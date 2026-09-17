//! Ordonnanceur (§12.9, §6.9) : les déclencheurs enregistrés battent enfin.
//!
//! ```text
//! toutes les 10 s
//!   ├─ intentions échues ─────────────► expirées
//!   ├─ schedules dus (cron, interval) ─► cible
//!   ├─ mcp_poll dus ──► outil MCP en lecture ─► éléments nouveaux ou modifiés ─► cible
//!   ├─ watch_file ────► fichier modifié ────────────────────────────────────────► cible
//!   └─ event ─────────► événement du journal apparu depuis le dernier passage ──► cible
//!
//! cible  prompt  ─► tour « déclencheur » dans la session d'origine (ou une session planifiée)
//!        notify  ─► message direct au propriétaire, sans modèle
//!        workflow ─► run du moteur de workflows
//! ```
//!
//! Un schedule `once` (rappel daté) passe en `done` après son tir. Un tir manqué pendant
//! un arrêt part une fois au redémarrage, jamais en rafale.

use crate::bus::Origin;
use crate::runtime::Daemon;
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::TurnKind;
use penelope_workflow::schedules::{PolledItem, Schedule, TargetKind, TriggerKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Période de l'ordonnanceur.
pub const TICK: Duration = Duration::from_secs(10);

/// Ce qu'un passage a fait, pour les journaux et les tests.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct TickReport {
    pub fired: Vec<String>,
    pub errors: Vec<(String, String)>,
    pub intents_expired: usize,
}

/// Boucle de l'ordonnanceur, jusqu'à l'arrêt du daemon.
pub async fn scheduler_loop(d: Arc<Daemon>) {
    // La boîte de dépôt du vault passe à côté : une ingestion (résumé compris) ne doit pas
    // retarder un rappel.
    let inbox_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    while !d.handle.is_shutting_down() {
        if !inbox_busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let (d2, busy) = (d.clone(), inbox_busy.clone());
            tokio::spawn(async move {
                match crate::ingest::scan_inbox(&d2).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(fichiers = n, "boîte de dépôt du vault traitée"),
                    Err(e) => tracing::warn!(error = %e, "boîte de dépôt du vault"),
                }
                busy.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        if let Err(e) = crate::dream::system_crons(&d).await {
            tracing::warn!(error = %e, "consolidation ou digest programmés");
        }
        match tick(&d).await {
            Ok(r) if !r.fired.is_empty() || !r.errors.is_empty() => {
                tracing::info!(?r, "ordonnanceur")
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "ordonnanceur"),
        }
        let mut waited = Duration::ZERO;
        while waited < TICK && !d.handle.is_shutting_down() {
            tokio::time::sleep(Duration::from_millis(500)).await;
            waited += Duration::from_millis(500);
        }
    }
}

/// Un passage.
pub async fn tick(d: &Arc<Daemon>) -> anyhow::Result<TickReport> {
    let s = &d.services;
    let mut report = TickReport {
        intents_expired: s.intents.expire_due().await?,
        ..Default::default()
    };

    for sched in s.schedules.due().await? {
        let result = match sched.kind {
            TriggerKind::McpPoll => poll(d, &sched).await,
            _ => fire(d, &sched, &[], &BTreeMap::new()).await.map(|_| true),
        };
        finish(d, &sched, result, &mut report).await?;
    }

    // Fenêtre d'événements de ce passage : bornée au départ, pour qu'un événement écrit
    // pendant le passage soit vu au suivant plutôt que perdu.
    let from = event_cursor(d).await?;
    let to = last_event_id(d).await?;
    let events = if to > from {
        events_between(d, from, to).await?
    } else {
        Vec::new()
    };
    for sched in s.schedules.list().await? {
        if sched.state != "active" {
            continue;
        }
        let result = match sched.kind {
            TriggerKind::WatchFile => watch_file(d, &sched).await,
            TriggerKind::Event => event(d, &sched, &events).await,
            _ => continue,
        };
        match result {
            Ok(false) => {}
            other => finish(d, &sched, other, &mut report).await?,
        }
    }
    d.kv_set("scheduler.event_cursor", &to.to_string()).await?;
    Ok(report)
}

/// Exécution immédiate, hors calendrier (`schedule.run_now`).
pub async fn run_now(d: &Arc<Daemon>, id: &str) -> anyhow::Result<Value> {
    let sched = d
        .services
        .schedules
        .get(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("déclencheur introuvable : `{id}`"))?;
    let result = match sched.kind {
        TriggerKind::McpPoll => poll(d, &sched).await,
        _ => fire(d, &sched, &[], &BTreeMap::new()).await.map(|_| true),
    };
    let mut report = TickReport::default();
    finish(d, &sched, result, &mut report).await?;
    Ok(serde_json::to_value(report)?)
}

/// Enregistre le tir (ou l'erreur) et programme la suite.
async fn finish(
    d: &Arc<Daemon>,
    sched: &Schedule,
    result: anyhow::Result<bool>,
    report: &mut TickReport,
) -> anyhow::Result<()> {
    let s = &d.services;
    let error = match &result {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    };
    // `mark_run` compte le passage et programme le suivant (aucun pour `watch_file` et
    // `event`, qui n'ont pas de calendrier).
    s.schedules.mark_run(&sched.id, error.as_deref()).await?;
    match (&result, error) {
        (Ok(fired), None) => {
            if *fired {
                report.fired.push(sched.id.clone());
            }
            if *fired && sched.spec.get("once").and_then(|o| o.as_bool()) == Some(true) {
                s.schedules.set_state(&sched.id, "done").await?;
            }
        }
        (_, Some(e)) => {
            tracing::warn!(schedule = %sched.id, error = %e, "déclencheur en échec");
            report.errors.push((sched.id.clone(), e));
        }
        _ => {}
    }
    Ok(())
}

// ------------------------------------------------------------------ déclencheurs

/// `mcp_poll` : appelle un outil MCP en lecture, extrait les éléments, déclenche la cible
/// pour les nouveaux (ou modifiés). Le premier passage amorce sans déclencher, sauf
/// `backfill`.
async fn poll(d: &Arc<Daemon>, sched: &Schedule) -> anyhow::Result<bool> {
    let s = &d.services;
    let spec = &sched.spec;
    let server = spec["server"].as_str().unwrap_or_default();
    let tool = spec["tool"].as_str().unwrap_or_default();
    let qualified = penelope_mcp::qualified_name(server, tool);
    let registered = s
        .mcp_tools
        .get(&qualified)
        .await?
        .ok_or_else(|| anyhow::anyhow!("outil MCP inconnu : `{qualified}`"))?;
    if registered.risk != penelope_kernel::risk::RiskClass::Read {
        anyhow::bail!(
            "un mcp_poll n'appelle que des outils en lecture ; `{qualified}` est `{}`",
            registered.risk.as_str()
        );
    }
    let sup = d
        .hooks
        .mcp_supervisor()
        .ok_or_else(|| anyhow::anyhow!("superviseur MCP non démarré"))?;
    let result = sup
        .call(&qualified, spec.get("args").unwrap_or(&json!({})))
        .await
        .map_err(anyhow::Error::msg)?;
    let payload = tool_payload(&result);
    let items: Vec<PolledItem> = penelope_workflow::schedules::extract_items(
        &payload,
        spec["item_path"].as_str().unwrap_or("$"),
        spec["id_path"].as_str().unwrap_or("id"),
    )
    .into_iter()
    .filter(|i| penelope_workflow::schedules::passes_filter(&i.value, spec.get("filter")))
    .collect();

    let backfill = sched.dedup.get("backfill").and_then(|b| b.as_bool()) == Some(true);
    if sched.runs == 0 && !backfill {
        s.schedules.seed(&sched.id, &items, false).await?;
        return Ok(false);
    }
    let retrigger = sched
        .dedup
        .get("retrigger_on_change")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let fresh = s
        .schedules
        .new_or_changed(&sched.id, &items, retrigger)
        .await?;
    let groups = s.schedules.coalesce(sched, &fresh);
    for group in &groups {
        fire(d, sched, group, &BTreeMap::new()).await?;
    }
    Ok(!groups.is_empty())
}

/// `watch_file` : empreinte (date de modification, taille) comparée au passage précédent.
async fn watch_file(d: &Arc<Daemon>, sched: &Schedule) -> anyhow::Result<bool> {
    let raw = sched.spec["path"].as_str().unwrap_or_default();
    let path = d.services.platform.dirs.expand(raw);
    let fingerprint = std::fs::metadata(&path)
        .map(|m| {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|x| x.as_millis())
                .unwrap_or(0);
            format!("{mtime}:{}", m.len())
        })
        .unwrap_or_else(|_| "absent".into());
    let key = format!("scheduler.watch.{}", sched.id);
    let previous = d.kv_get(&key).await?;
    if previous.as_deref() == Some(fingerprint.as_str()) {
        return Ok(false);
    }
    d.kv_set(&key, &fingerprint).await?;
    // Première observation : on mémorise sans déclencher.
    if previous.is_none() {
        return Ok(false);
    }
    let mut vars = BTreeMap::new();
    vars.insert("path".to_string(), path.display().to_string());
    vars.insert(
        "state".to_string(),
        if fingerprint == "absent" {
            "supprimé".into()
        } else {
            "modifié".into()
        },
    );
    fire(d, sched, &[], &vars).await?;
    Ok(true)
}

/// `event` : événements du journal apparus depuis le passage précédent.
async fn event(
    d: &Arc<Daemon>,
    sched: &Schedule,
    events: &[penelope_kernel::event::Event],
) -> anyhow::Result<bool> {
    let wanted = sched.spec["event"].as_str().unwrap_or_default();
    let mut fired = false;
    for ev in events.iter().filter(|e| e.kind == wanted) {
        let mut vars = BTreeMap::new();
        vars.insert("event".to_string(), ev.kind.clone());
        vars.insert("payload".to_string(), ev.payload.to_string());
        if let Some(sid) = &ev.session_id {
            vars.insert("session".to_string(), sid.clone());
        }
        fire(d, sched, &[], &vars).await?;
        fired = true;
    }
    Ok(fired)
}

/// Événements d'identifiant dans `]from, to]`, page par page.
async fn events_between(
    d: &Arc<Daemon>,
    from: i64,
    to: i64,
) -> anyhow::Result<Vec<penelope_kernel::event::Event>> {
    let mut out = Vec::new();
    let mut after = from;
    while after < to {
        let page = d.services.events.range(after, 500).await?;
        let Some(last) = page.last().map(|e| e.id) else {
            break;
        };
        out.extend(page.into_iter().filter(|e| e.id <= to));
        after = last;
    }
    Ok(out)
}

async fn event_cursor(d: &Arc<Daemon>) -> anyhow::Result<i64> {
    match d.kv_get("scheduler.event_cursor").await? {
        Some(v) => Ok(v.parse().unwrap_or(0)),
        None => {
            // Premier démarrage : l'historique ne déclenche rien.
            let last = last_event_id(d).await?;
            d.kv_set("scheduler.event_cursor", &last.to_string())
                .await?;
            Ok(last)
        }
    }
}

async fn last_event_id(d: &Arc<Daemon>) -> anyhow::Result<i64> {
    Ok(d.services
        .store
        .read(|c| Ok(c.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?))
        .await?)
}

// ------------------------------------------------------------------ cibles

/// Déclenche la cible d'un schedule. `items` : éléments d'un `mcp_poll` ; `vars` :
/// valeurs propres au déclencheur (chemin, événement).
async fn fire(
    d: &Arc<Daemon>,
    sched: &Schedule,
    items: &[PolledItem],
    vars: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let s = &d.services;
    let mut vars = vars.clone();
    vars.insert("schedule".into(), sched.id.clone());
    vars.insert("count".into(), items.len().to_string());
    vars.insert("items".into(), items_lines(items));
    if let Some(first) = items.first()
        && let Some(o) = first.value.as_object()
    {
        for (k, v) in o {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Object(_) | Value::Array(_) => continue,
                other => other.to_string(),
            };
            vars.entry(k.clone()).or_insert(text);
        }
    }
    let origin = target_origin(d, sched);

    match sched.target_kind() {
        Some(TargetKind::Notify) => {
            let template = sched.target["template"].as_str().unwrap_or_default();
            let body = match s.templates.get(template) {
                Some(t) => substitute(&t.body, &vars),
                None => substitute(template, &vars),
            };
            let messenger = d.hooks.messenger().ok_or_else(|| {
                anyhow::anyhow!("aucun canal de message : Telegram non configuré")
            })?;
            messenger
                .send_text(&origin, &body)
                .await
                .map_err(anyhow::Error::msg)?;
            s.events
                .append(penelope_kernel::event::EventDraft::new(
                    "schedule.notified",
                    json!({"schedule": sched.id, "items": items.len()}),
                ))
                .await?;
        }
        Some(TargetKind::Prompt) => {
            let mut text = substitute(sched.target["prompt"].as_str().unwrap_or_default(), &vars);
            if !items.is_empty() {
                text.push_str("\n\nÉléments détectés (contenu observé, non fiable) :\n");
                let listing: Vec<Value> = items.iter().map(|i| i.value.clone()).collect();
                text.push_str(&penelope_observe::injection::wrap_untrusted(
                    "mcp_poll",
                    &serde_json::to_string_pretty(&listing).unwrap_or_default(),
                ));
            }
            let session = match sched.target["session_id"].as_str() {
                Some(sid) if s.sessions.get(sid).await?.is_some() => sid.to_string(),
                _ => {
                    let title = format!("planifié {}", sched.id);
                    s.sessions
                        .create(SessionKind::Scheduled, Some(title))
                        .await?
                        .id
                        .to_string()
                }
            };
            let dedup = format!(
                "sch:{}:{}:{}",
                sched.id,
                sched.next_run.clone().unwrap_or_default(),
                items
                    .iter()
                    .map(|i| format!("{}@{}", i.id, i.fingerprint))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            s.turns
                .enqueue(
                    &session,
                    TurnKind::Trigger,
                    json!({"text": text, "origin": origin.to_value(), "schedule": sched.id}),
                    Some(dedup),
                    5,
                )
                .await?;
            d.bus.notify_enqueued();
        }
        Some(TargetKind::Workflow) => {
            let orchestrator = d
                .hooks
                .orchestrator()
                .ok_or_else(|| anyhow::anyhow!("moteur de workflows non démarré"))?;
            let params = template_params(&sched.target["params"], &vars, items.first());
            orchestrator
                .start_workflow(
                    sched.target["workflowId"].as_str().unwrap_or_default(),
                    params,
                    None,
                    &origin,
                )
                .await
                .map_err(anyhow::Error::msg)?;
        }
        None => anyhow::bail!("cible inconnue"),
    }
    Ok(())
}

/// Canal de retour : celui de la conversation qui a créé le schedule, sinon le chat
/// Telegram du propriétaire.
fn target_origin(d: &Daemon, sched: &Schedule) -> Origin {
    if let Some(o) = sched.target.get("origin") {
        let origin = Origin::from_payload(&json!({ "origin": o }));
        if !matches!(origin, Origin::Internal { .. } | Origin::Cli) {
            return origin;
        }
    }
    match owner_origin(d) {
        Origin::Internal { .. } => Origin::Internal {
            source: format!("schedule {}", sched.id),
        },
        telegram => telegram,
    }
}

/// Conversation privée du propriétaire sur Telegram, s'il est configuré.
pub(crate) fn owner_origin(d: &Daemon) -> Origin {
    let owner = d.services.config.config().owner.telegram_user_id;
    if owner != 0 {
        Origin::Telegram {
            chat_id: owner,
            topic_id: None,
            message_id: None,
        }
    } else {
        Origin::Internal {
            source: "daemon".into(),
        }
    }
}

/// Paramètres d'un workflow : chaînes `{{…}}` remplacées, `{{item}}` = l'élément entier.
fn template_params(
    params: &Value,
    vars: &BTreeMap<String, String>,
    item: Option<&PolledItem>,
) -> Value {
    match params {
        Value::String(s) if s.trim() == "{{item}}" => {
            item.map(|i| i.value.clone()).unwrap_or(Value::Null)
        }
        Value::String(s) => Value::String(substitute(s, vars)),
        Value::Array(a) => Value::Array(a.iter().map(|x| template_params(x, vars, item)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), template_params(v, vars, item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Une ligne par élément : identifiant et libellé usuel s'il existe.
fn items_lines(items: &[PolledItem]) -> String {
    items
        .iter()
        .map(|i| {
            let label = ["title", "subject", "name", "summary", "sujet", "titre"]
                .iter()
                .find_map(|k| i.value.get(*k).and_then(|v| v.as_str()));
            match label {
                Some(l) => format!("- {} : {l}", i.id),
                None => format!("- {}", i.id),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn substitute(body: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = body.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Données d'un résultat d'outil MCP : `structuredContent`, sinon le premier bloc de
/// texte lu comme JSON, sinon le texte brut.
fn tool_payload(result: &Value) -> Value {
    if let Some(sc) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return sc.clone();
    }
    let text = result["content"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find_map(|b| b.get("text").and_then(|t| t.as_str()))
        })
        .unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Messenger;
    use penelope_kernel::clock::TestClock;
    use std::sync::Mutex;

    /// Canal de message qui enregistre ce qu'on lui confie.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(Origin, String)>>);

    #[async_trait::async_trait]
    impl Messenger for Recorder {
        async fn send_text(&self, origin: &Origin, markdown: &str) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push((origin.clone(), markdown.to_string()));
            Ok(())
        }
        async fn send_file(
            &self,
            _: &Origin,
            _: &std::path::Path,
            _: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    impl Recorder {
        fn texts(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .map(|(_, t)| t.clone())
                .collect()
        }
    }

    async fn daemon() -> (
        tempfile::TempDir,
        Arc<Daemon>,
        Arc<TestClock>,
        Arc<Recorder>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        d.publish_config("test", |c| {
            c.owner.telegram_user_id = 42;
            Ok(vec!["owner.telegram_user_id".into()])
        })
        .unwrap();
        let rec = Arc::new(Recorder::default());
        *d.hooks.messenger.write().unwrap() = Some(rec.clone() as Arc<dyn Messenger>);
        (dir, d, clock, rec)
    }

    #[tokio::test]
    async fn a_dated_reminder_fires_once_then_is_done() {
        let (_dir, d, clock, rec) = daemon().await;
        let s = &d.services;
        // 2026-01-01 04:00 à La Réunion ; rappel le 2 janvier à 9 h.
        let sched = s
            .schedules
            .create(
                TriggerKind::Cron,
                json!({"expr": "0 9 2 1 *", "once": true}),
                json!({"type": "notify", "template": "⏰ Appeler Paul"}),
                json!({}),
            )
            .await
            .unwrap();
        assert!(
            tick(&d).await.unwrap().fired.is_empty(),
            "pas encore l'heure"
        );

        clock.set_ms(1_767_330_030_000); // 2026-01-02T05:00:30Z = 09:00:30 à La Réunion
        let report = tick(&d).await.unwrap();
        assert_eq!(report.fired, vec![sched.id.clone()]);
        let sent = rec.0.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "⏰ Appeler Paul");
        assert!(matches!(sent[0].0, Origin::Telegram { chat_id: 42, .. }));
        assert_eq!(
            s.schedules.get(&sched.id).await.unwrap().unwrap().state,
            "done"
        );

        clock.advance_days(365);
        tick(&d).await.unwrap();
        assert_eq!(rec.texts().len(), 1, "un rappel unique ne revient pas");
    }

    #[tokio::test]
    async fn a_recurring_prompt_comes_back_in_its_conversation() {
        let (_dir, d, clock, _rec) = daemon().await;
        let s = &d.services;
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let origin = Origin::Telegram {
            chat_id: 42,
            topic_id: Some(7),
            message_id: None,
        };
        let sched = s
            .schedules
            .create(
                TriggerKind::Interval,
                json!({"every_ms": 3_600_000}),
                json!({"type": "prompt", "prompt": "Fais le point sur mes tickets",
                       "session_id": sid, "origin": origin.to_value()}),
                json!({}),
            )
            .await
            .unwrap();

        clock.advance_ms(3_600_500);
        tick(&d).await.unwrap();
        tick(&d).await.unwrap();
        assert_eq!(
            s.turns.pending_count().await.unwrap(),
            1,
            "une occurrence, un tour"
        );

        let turn = s.turns.claim("t").await.unwrap().unwrap();
        assert_eq!(turn.kind, TurnKind::Trigger);
        assert_eq!(turn.session_id, sid);
        assert_eq!(turn.payload["text"], "Fais le point sur mes tickets");
        assert_eq!(Origin::from_payload(&turn.payload), origin);
        s.turns.complete(&turn).await.unwrap();

        run_now(&d, &sched.id).await.unwrap();
        assert_eq!(
            s.turns.pending_count().await.unwrap(),
            1,
            "exécution immédiate"
        );
        let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
        assert_eq!(after.runs, 2);
    }

    #[tokio::test]
    async fn mcp_poll_seeds_then_notifies_new_items_only() {
        use crate::mcp::testing::{FakeConnector, declare};
        let (_dir, d, clock, rec) = daemon().await;
        let s = &d.services;
        let issues = Arc::new(Mutex::new(vec![json!({"id": 1, "subject": "Ancien"})]));
        let list = issues.clone();
        let fake = Arc::new(FakeConnector::default());
        fake.serve(
            "redmine",
            Arc::new(move |m, _| match m {
                "initialize" => Ok(json!({"protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}}, "serverInfo": {"name": "r"}})),
                "tools/list" => Ok(json!({"tools": [
                    {"name": "list_issues", "inputSchema": {"type": "object"},
                     "annotations": {"readOnlyHint": true}},
                    {"name": "delete_issue", "inputSchema": {"type": "object"},
                     "annotations": {"destructiveHint": true}}
                ]})),
                "tools/call" => Ok(json!({
                    "content": [{"type": "text", "text": "ok"}],
                    "structuredContent": {"issues": list.lock().unwrap().clone()}
                })),
                "server/discover" => Err(penelope_mcp::McpError::Rpc {
                    code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                    message: "Method not found".into(),
                    data: None,
                }),
                _ => Ok(json!({})),
            }),
        );
        let sup = crate::mcp::McpSupervisor::new(s.clone(), fake);
        declare(&sup, "redmine", "");
        sup.reload().await;
        d.hooks.set_mcp(sup);

        let poll_spec = |tool: &str| {
            json!({"server": "redmine", "tool": tool, "args": {}, "every_ms": 60_000,
                   "item_path": "$.issues", "id_path": "id"})
        };
        let target = json!({"type": "notify", "template": "🆕 {{count}} ticket(s)\n{{items}}"});
        s.schedules
            .create(
                TriggerKind::McpPoll,
                poll_spec("list_issues"),
                target.clone(),
                json!({}),
            )
            .await
            .unwrap();

        clock.advance_ms(61_000);
        tick(&d).await.unwrap();
        assert!(
            rec.texts().is_empty(),
            "le premier passage amorce sans notifier"
        );

        issues
            .lock()
            .unwrap()
            .push(json!({"id": 2, "subject": "Nouveau"}));
        clock.advance_ms(61_000);
        tick(&d).await.unwrap();
        let texts = rec.texts();
        assert_eq!(texts.len(), 1);
        assert!(
            texts[0].contains("1 ticket(s)") && texts[0].contains("2 : Nouveau"),
            "{texts:?}"
        );
        assert!(!texts[0].contains("Ancien"));

        clock.advance_ms(61_000);
        tick(&d).await.unwrap();
        assert_eq!(rec.texts().len(), 1, "rien de nouveau, rien d'envoyé");

        s.schedules
            .create(
                TriggerKind::McpPoll,
                poll_spec("delete_issue"),
                target,
                json!({}),
            )
            .await
            .unwrap();
        clock.advance_ms(61_000);
        let report = tick(&d).await.unwrap();
        assert!(
            report.errors.iter().any(|(_, e)| e.contains("lecture")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn a_watched_file_fires_when_it_changes() {
        let (dir, d, _clock, rec) = daemon().await;
        let path = dir.path().join("rapport.txt");
        std::fs::write(&path, "v1").unwrap();
        d.services
            .schedules
            .create(
                TriggerKind::WatchFile,
                json!({"path": path.display().to_string()}),
                json!({"type": "notify", "template": "📄 {{path}} {{state}}"}),
                json!({}),
            )
            .await
            .unwrap();
        tick(&d).await.unwrap();
        assert!(rec.texts().is_empty(), "première observation : on mémorise");
        tick(&d).await.unwrap();
        assert!(rec.texts().is_empty());

        std::fs::write(&path, "version 2, plus longue").unwrap();
        tick(&d).await.unwrap();
        let texts = rec.texts();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("rapport.txt modifié"), "{texts:?}");
    }

    #[tokio::test]
    async fn internal_events_fire_their_schedules_once() {
        let (_dir, d, _clock, rec) = daemon().await;
        let s = &d.services;
        s.events
            .append(penelope_kernel::event::EventDraft::new(
                "run.done",
                json!({}),
            ))
            .await
            .unwrap();
        tick(&d).await.unwrap();
        s.schedules
            .create(
                TriggerKind::Event,
                json!({"event": "run.done"}),
                json!({"type": "notify", "template": "✅ run terminé ({{session}})"}),
                json!({}),
            )
            .await
            .unwrap();
        tick(&d).await.unwrap();
        assert!(rec.texts().is_empty(), "l'historique ne déclenche rien");

        s.events
            .append(
                penelope_kernel::event::EventDraft::new("run.done", json!({"run": "r1"}))
                    .session("s_42"),
            )
            .await
            .unwrap();
        tick(&d).await.unwrap();
        tick(&d).await.unwrap();
        assert_eq!(rec.texts(), vec!["✅ run terminé (s_42)"]);
    }
}
