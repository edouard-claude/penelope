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
//! cible  prompt  ─► tour « déclencheur » dans une session neuve à chaque exécution, réponse
//!                   dans le chat (ou le sujet) d'origine ; échec ou annulation : alerte
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
    cancelled_triggers(d).await?;
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
    // Un déclenchement manuel n'est jamais dédoublonné avec le passage prévu, ni avec une
    // relance précédente (« Relancer maintenant », issue #39).
    let manual: BTreeMap<String, String> = [(
        MANUAL.to_string(),
        penelope_kernel::ids::Ulid::new().to_string(),
    )]
    .into_iter()
    .collect();
    let result = match sched.kind {
        TriggerKind::McpPoll => poll(d, &sched).await,
        _ => fire(d, &sched, &[], &manual).await.map(|_| true),
    };
    let mut report = TickReport::default();
    finish(d, &sched, result, &mut report).await?;
    Ok(serde_json::to_value(report)?)
}

/// Variable d'un déclenchement manuel : rend sa clé de dédoublonnage unique.
const MANUAL: &str = "declenchement_manuel";

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
    // Seule une exécution menée à terme compte (issue #39) : un prompt déclenché ne l'est
    // qu'à la fin de son tour, un déclenchement en échec garde son erreur sans compter. Un
    // passage de sondage sans nouveauté compte, lui : il a fait tout son travail. Le passage
    // suivant est programmé dans tous les cas (aucun pour `watch_file` et `event`).
    let prompt = sched.target_kind() == Some(TargetKind::Prompt);
    match (&error, &result) {
        (None, Ok(true)) if prompt => s.schedules.advance(&sched.id, None).await?,
        (None, _) => s.schedules.mark_run(&sched.id, None).await?,
        (Some(e), _) => {
            s.schedules.advance(&sched.id, Some(e)).await?;
            if prompt {
                alert(d, sched, e).await;
            }
        }
    }
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
            // Une session par exécution (issue #39) : fermer la conversation où la
            // planification est née (`/new`, `/close`) ne la fait plus mourir en silence.
            let title = format!("{} · {}", label(d, sched).await, local_day(d));
            let session = s
                .sessions
                .create(SessionKind::Scheduled, Some(title))
                .await?
                .id
                .to_string();
            let dedup = format!(
                "sch:{}:{}{}:{}",
                sched.id,
                sched.next_run.clone().unwrap_or_default(),
                vars.get(MANUAL)
                    .map(|n| format!(":{n}"))
                    .unwrap_or_default(),
                items
                    .iter()
                    .map(|i| format!("{}@{}", i.id, i.fingerprint))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            // État consommé par le travail (« déjà vu », curseur) : gardé tel qu'avant le
            // tour, remis si l'exécution ne livre rien (issue #120).
            if let Some(path) = sched.target["etat"].as_str() {
                save_state(d, &session, path).await;
            }
            s.turns
                .enqueue(
                    &session,
                    TurnKind::Trigger,
                    json!({
                        "text": text,
                        "origin": origin.to_value(),
                        "schedule": sched.id,
                        "livrable": sched.target["livrable"],
                    }),
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

/// Crée une planification ; une planification active identique (même déclencheur, même
/// spécification, prompt quasi identique) est signalée dans la réponse (issue #39).
pub async fn create(
    s: &crate::runtime::Services,
    kind: penelope_workflow::TriggerKind,
    spec: Value,
    target: Value,
    dedup: Value,
) -> Result<Value, String> {
    let twins = s
        .schedules
        .similar(kind, &spec, &target)
        .await
        .map_err(|e| e.to_string())?;
    let sched = s.schedules.create(kind, spec, target, dedup).await?;
    let mut v = serde_json::to_value(&sched).map_err(|e| e.to_string())?;
    if !twins.is_empty() {
        let ids: Vec<&str> = twins.iter().map(|t| t.id.as_str()).collect();
        v["doublons"] = json!(ids);
        v["avertissement"] = json!(format!(
            "une planification identique est déjà active ({}) : supprimer l'une des deux \
             (`schedule_delete`) si ce n'est pas voulu",
            ids.join(", ")
        ));
    }
    Ok(v)
}

/// Nom lisible d'une planification : son libellé, sinon le titre de la conversation où
/// elle est née, sinon le début du prompt.
pub async fn label(d: &Daemon, sched: &Schedule) -> String {
    if let Some(l) = sched.target["label"]
        .as_str()
        .filter(|l| !l.trim().is_empty())
    {
        return l.trim().to_string();
    }
    if let Some(sid) = sched.target["origin_session"]
        .as_str()
        .or_else(|| sched.target["session_id"].as_str())
        && let Ok(Some(sess)) = d.services.sessions.get(sid).await
        && let Some(title) = sess.title.filter(|t| !t.trim().is_empty())
    {
        return title;
    }
    let text = sched.target["prompt"]
        .as_str()
        .or_else(|| sched.target["template"].as_str())
        .or_else(|| sched.target["workflowId"].as_str())
        .unwrap_or_default();
    let words: Vec<&str> = text.split_whitespace().take(6).collect();
    if words.is_empty() {
        sched.id.clone()
    } else {
        words.join(" ")
    }
}

/// Alerte : une planification n'a pas pu s'exécuter (issue #39). Jamais de silence.
pub async fn alert(d: &Arc<Daemon>, sched: &Schedule, reason: &str) {
    let text = format!(
        "⚠️ La planification « {} » n'a pas pu s'exécuter : {reason}",
        label(d, sched).await
    );
    let origin = target_origin(d, sched);
    let _ = d
        .services
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.failed",
            json!({"schedule": sched.id, "reason": reason}),
        ))
        .await;
    if let Some(tg) = d.hooks.telegram()
        && tg.schedule_alert(&origin, &sched.id, &text).await.is_ok()
    {
        return;
    }
    match d.hooks.messenger() {
        Some(m) => {
            if let Err(e) = m.send_text(&origin, &text).await {
                tracing::warn!(schedule = %sched.id, error = %e, "alerte de planification non envoyée");
            }
        }
        None => tracing::warn!(schedule = %sched.id, "{text}"),
    }
}

/// Jour du propriétaire, `JJ/MM`.
fn local_day(d: &Daemon) -> String {
    let s = &d.services;
    let utc = chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
    match s.config.config().owner.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => utc.with_timezone(&tz).format("%d/%m").to_string(),
        Err(_) => utc.format("%d/%m").to_string(),
    }
}

/// Clé de l'état d'une planification gardé avant son tour.
fn state_key(session_id: &str) -> String {
    format!("schedule.state.{session_id}")
}

/// Chemin d'un état ou d'un fichier livrable : absolu, ou relatif au premier workspace ;
/// jamais hors des workspaces.
fn resolve_in_workspace(d: &Daemon, path: &str) -> Option<std::path::PathBuf> {
    let workspaces = crate::executor::default_workspaces(&d.services);
    let p = std::path::Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspaces.first()?.join(p)
    };
    let full = penelope_platform::sandbox::normalise(&full);
    workspaces
        .iter()
        .any(|w| full.starts_with(w))
        .then_some(full)
}

/// Garde l'état tel qu'avant le tour (2 Mio au plus, texte).
async fn save_state(d: &Daemon, session_id: &str, path: &str) {
    let Some(full) = resolve_in_workspace(d, path) else {
        tracing::warn!(
            path,
            "état de planification hors des workspaces : non gardé"
        );
        return;
    };
    let saved = match std::fs::read(&full) {
        Ok(bytes) if bytes.len() <= 2 * 1024 * 1024 => match String::from_utf8(bytes) {
            Ok(text) => json!({"path": full, "content": text}),
            Err(_) => return,
        },
        Ok(_) => return,
        Err(_) => json!({"path": full, "content": null}),
    };
    let _ = d.kv_set(&state_key(session_id), &saved.to_string()).await;
}

/// Remet l'état d'avant le tour (`restore`), ou l'oublie : il est validé.
async fn settle_state(d: &Daemon, session_id: &str, restore: bool) {
    let key = state_key(session_id);
    let Ok(Some(raw)) = d.kv_get(&key).await else {
        return;
    };
    if restore && let Ok(v) = serde_json::from_str::<Value>(&raw) {
        let path = std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
        let done: Result<(), String> = match v["content"].as_str() {
            Some(text) => penelope_kernel::config::atomic_write(&path, text.as_bytes())
                .map_err(|e| e.to_string()),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
        };
        match done {
            Ok(()) => tracing::info!(path = %path.display(), "état de planification remis"),
            Err(e) => tracing::warn!(error = %e, "état de planification non remis"),
        }
    }
    let _ = d.kv_delete(&key).await;
}

/// Ce que le tour devait livrer et n'a pas livré, s'il en déclarait un (issue #120).
async fn missing_deliverable(
    d: &Daemon,
    turn: &penelope_kernel::turn::Turn,
    outcome: &crate::agent::TurnOutcome,
) -> Option<String> {
    let s = &d.services;
    let wanted = turn.payload["livrable"].as_str()?.trim().to_string();
    let since = s
        .sessions
        .get(&turn.session_id)
        .await
        .ok()
        .flatten()
        .map(|x| x.created_at)
        .unwrap_or_default();
    if wanted == "message" {
        let answered = matches!(
            outcome,
            crate::agent::TurnOutcome::Answered { text, .. } if !text.trim().is_empty()
        );
        let channel = matches!(
            crate::bus::Origin::from_payload(&turn.payload),
            crate::bus::Origin::Telegram { .. }
        );
        // Un message envoyé par l'agent lui-même compte aussi.
        let sent = s
            .context
            .history
            .load(&turn.session_id, 0)
            .await
            .unwrap_or_default()
            .iter()
            .any(|e| {
                matches!(
                    e.message.name.as_deref(),
                    Some("send_message" | "send_file")
                ) && !e.message.text().starts_with("Erreur")
            });
        return (!(answered && channel || sent))
            .then(|| "aucun message envoyé au propriétaire".to_string());
    }
    if let Some(path) = wanted.strip_prefix("fichier:") {
        let Some(full) = resolve_in_workspace(d, path.trim()) else {
            return Some(format!("fichier `{}` hors des workspaces", path.trim()));
        };
        let since = chrono::DateTime::parse_from_rfc3339(&since)
            .map(std::time::SystemTime::from)
            .unwrap_or(std::time::UNIX_EPOCH);
        let written = std::fs::metadata(&full)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since);
        return (!written).then(|| format!("fichier `{}` non écrit", path.trim()));
    }
    if wanted == "run" {
        let started = s
            .runs
            .list(None, 50)
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.started_at >= since);
        return (!started).then(|| "aucun run lancé".to_string());
    }
    tracing::warn!(livrable = %wanted, "livrable de planification inconnu : ignoré");
    None
}

/// Fin du tour d'un prompt planifié, livrable compris (issue #120) : une exécution qui
/// répond sans livrer ce qu'elle a promis est un échec, prévenu comme ceux de #39, et
/// l'état qu'elle a consommé est remis pour que la suivante reprenne les mêmes éléments.
pub async fn trigger_outcome_of(
    d: &Arc<Daemon>,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
    turn: &penelope_kernel::turn::Turn,
) {
    use crate::agent::TurnOutcome;
    if let TurnOutcome::AwaitingApproval { .. } = outcome {
        return trigger_outcome(d, schedule_id, outcome).await;
    }
    let missing = match outcome {
        TurnOutcome::Answered { .. } => missing_deliverable(d, turn, outcome).await,
        _ => None,
    };
    let delivered = matches!(outcome, TurnOutcome::Answered { .. }) && missing.is_none();
    settle_state(d, &turn.session_id, !delivered).await;
    match missing {
        None => trigger_outcome(d, schedule_id, outcome).await,
        Some(what) => {
            let s = &d.services;
            let reason = format!("exécutée sans livrable : {what}");
            if let Err(e) = s.schedules.record_outcome(schedule_id, Some(&reason)).await {
                tracing::warn!(schedule = %schedule_id, error = %e, "issue de planification non enregistrée");
            }
            if let Ok(Some(sched)) = s.schedules.get(schedule_id).await {
                alert(d, &sched, &reason).await;
            }
        }
    }
}

/// Fin du tour d'un prompt planifié : l'exécution compte si le modèle a répondu, sinon la
/// raison est gardée et le propriétaire prévenu (issue #39).
pub async fn trigger_outcome(
    d: &Arc<Daemon>,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
) {
    use crate::agent::TurnOutcome;
    let s = &d.services;
    let Ok(Some(sched)) = s.schedules.get(schedule_id).await else {
        return;
    };
    let (error, warn) = match outcome {
        TurnOutcome::Answered { .. } => (None, false),
        // La carte d'approbation est partie : l'exécution reprendra après la décision.
        TurnOutcome::AwaitingApproval { .. } => return,
        TurnOutcome::LoopAborted { .. } => (
            Some("boucle d'outils arrêtée (réponse envoyée)".to_string()),
            false,
        ),
        TurnOutcome::Failed { error } => (Some(format!("erreur du modèle : {error}")), true),
        TurnOutcome::BudgetExceeded { scope, .. } => {
            (Some(format!("budget {scope} atteint")), true)
        }
        TurnOutcome::Cancelled => (Some("exécution interrompue".to_string()), true),
    };
    if let Err(e) = s
        .schedules
        .record_outcome(schedule_id, error.as_deref())
        .await
    {
        tracing::warn!(schedule = %schedule_id, error = %e, "issue de planification non enregistrée");
    }
    if let (Some(reason), true) = (error, warn) {
        alert(d, &sched, &reason).await;
    }
}

/// Tours de prompts planifiés annulés dans la file sans jamais tourner (session fermée,
/// `/stop`) : erreur gardée, propriétaire prévenu. Le premier passage prend l'instant sans
/// rien signaler.
async fn cancelled_triggers(d: &Arc<Daemon>) -> anyhow::Result<()> {
    const CURSOR: &str = "scheduler.cancelled_cursor";
    let s = &d.services;
    let now = s.clock.now_rfc3339();
    let Some(cursor) = d.kv_get(CURSOR).await? else {
        d.kv_set(CURSOR, &now).await?;
        return Ok(());
    };
    let since = cursor.clone();
    let rows: Vec<(String, String, Option<String>)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT payload, finished_at, last_error FROM turn_queue
                 WHERE kind = 'trigger' AND state = 'cancelled' AND finished_at > ?1
                 ORDER BY finished_at",
            )?;
            let rows = st.query_map([&since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let mut last = cursor;
    for (payload, finished, error) in rows {
        last = last.max(finished);
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let Some(id) = payload["schedule"].as_str() else {
            continue;
        };
        let Some(sched) = s.schedules.get(id).await? else {
            continue;
        };
        let reason = format!(
            "tour annulé ({})",
            error.unwrap_or_else(|| "sans raison".into())
        );
        s.schedules.record_outcome(id, Some(&reason)).await?;
        alert(d, &sched, &reason).await;
    }
    d.kv_set(CURSOR, &last).await?;
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

    /// #120 : un prompt qui déclare un message pour livrable et répond sans rien envoyer
    /// est un échec prévenu, et son état « déjà vu » est remis ; le même tour avec message
    /// compte et valide l'état ; sans livrable déclaré, rien ne change.
    #[tokio::test]
    async fn a_silent_scheduled_run_is_a_failure_and_its_state_is_restored() {
        let (_dir, d, clock, rec) = daemon().await;
        let s = &d.services;
        let origin = Origin::Telegram {
            chat_id: 42,
            topic_id: Some(7),
            message_id: None,
        };
        let ws = crate::executor::default_workspaces(s)[0].clone();
        std::fs::create_dir_all(ws.join("veille")).unwrap();
        std::fs::write(ws.join("veille/seen.json"), r#"["a","b"]"#).unwrap();
        let make = |livrable: Value| {
            let origin = origin.to_value();
            async move {
                s.schedules
                    .create(
                        TriggerKind::Interval,
                        json!({"every_ms": 3_600_000}),
                        json!({"type": "prompt", "prompt": "Prépare la veille",
                               "origin": origin, "livrable": livrable,
                               "etat": "veille/seen.json"}),
                        json!({}),
                    )
                    .await
                    .unwrap()
            }
        };
        let run = |text: &'static str| {
            let d = d.clone();
            async move {
                let turn = d.services.turns.claim("t").await.unwrap().expect("tour");
                // Le travail consomme l'état.
                let ws = crate::executor::default_workspaces(&d.services)[0].clone();
                std::fs::write(ws.join("veille/seen.json"), r#"["a","b","c","d"]"#).unwrap();
                let outcome = crate::agent::TurnOutcome::Answered {
                    text: text.into(),
                    iterations: 1,
                    cost_usd: 0.0,
                };
                d.services.turns.complete(&turn).await.unwrap();
                let schedule = turn.payload["schedule"].as_str().unwrap().to_string();
                trigger_outcome_of(&d, &schedule, &outcome, &turn).await;
                schedule
            }
        };

        let muet = make(json!("message")).await;
        clock.advance_ms(3_600_500);
        tick(&d).await.unwrap();
        run("").await;
        let after = s.schedules.get(&muet.id).await.unwrap().unwrap();
        assert!(
            after
                .last_error
                .as_deref()
                .unwrap_or_default()
                .contains("sans livrable"),
            "{after:?}"
        );
        assert!(
            rec.texts()
                .iter()
                .any(|t| t.contains("aucun message envoyé")),
            "{:?}",
            rec.texts()
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("veille/seen.json")).unwrap(),
            r#"["a","b"]"#,
            "état remis : la suivante reprend les mêmes éléments"
        );
        let digest = crate::dream::digest_text(&d).await.unwrap();
        assert!(digest.contains("planification(s) en échec"), "{digest}");
        s.schedules.set_state(&muet.id, "paused").await.unwrap();

        let parle = make(json!("message")).await;
        clock.advance_ms(3_600_500);
        tick(&d).await.unwrap();
        let alerts = rec.texts().len();
        run("Six items cette semaine.").await;
        let after = s.schedules.get(&parle.id).await.unwrap().unwrap();
        assert!(after.last_error.is_none() && after.runs == 1, "{after:?}");
        assert_eq!(rec.texts().len(), alerts, "pas d'alerte");
        assert_eq!(
            std::fs::read_to_string(ws.join("veille/seen.json")).unwrap(),
            r#"["a","b","c","d"]"#,
            "état validé"
        );
        s.schedules.set_state(&parle.id, "paused").await.unwrap();

        let libre = make(Value::Null).await;
        clock.advance_ms(3_600_500);
        tick(&d).await.unwrap();
        run("").await;
        let after = s.schedules.get(&libre.id).await.unwrap().unwrap();
        assert!(
            after.last_error.is_none() && after.runs == 1,
            "comme avant : {after:?}"
        );
    }

    /// Issue #39 : un prompt planifié s'exécute dans une session neuve, titrée d'après la
    /// planification, même si la conversation où il est né a été fermée ; il ne compte
    /// qu'une fois son tour terminé.
    #[tokio::test]
    async fn a_recurring_prompt_survives_the_closing_of_its_conversation() {
        let (_dir, d, clock, _rec) = daemon().await;
        let s = &d.services;
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        s.sessions
            .set_title(&sid, "Veille agents IA", false)
            .await
            .unwrap();
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
                       "origin_session": sid, "origin": origin.to_value()}),
                json!({}),
            )
            .await
            .unwrap();
        // La conversation d'origine est fermée (`/new`, `/close`).
        s.sessions.set_state(&sid, "closed").await.unwrap();

        clock.advance_ms(3_600_500);
        tick(&d).await.unwrap();
        tick(&d).await.unwrap();
        assert_eq!(
            s.turns.pending_count().await.unwrap(),
            1,
            "une occurrence, un tour"
        );
        let turn = s.turns.claim("t").await.unwrap().expect("tour non annulé");
        assert_eq!(turn.kind, TurnKind::Trigger);
        assert_ne!(turn.session_id, sid, "session neuve");
        let session = s.sessions.get(&turn.session_id).await.unwrap().unwrap();
        assert_eq!(session.kind, SessionKind::Scheduled);
        assert!(
            session
                .title
                .as_deref()
                .unwrap()
                .starts_with("Veille agents IA · "),
            "{:?}",
            session.title
        );
        assert_eq!(turn.payload["text"], "Fais le point sur mes tickets");
        assert_eq!(Origin::from_payload(&turn.payload), origin);
        let pending = s.schedules.get(&sched.id).await.unwrap().unwrap();
        assert_eq!(pending.runs, 0, "rien n'est compté avant la fin du tour");
        assert!(pending.next_run.is_some());

        let answered = crate::agent::TurnOutcome::Answered {
            text: "Trois tickets ouverts.".into(),
            iterations: 1,
            cost_usd: 0.0,
        };
        s.turns.complete(&turn).await.unwrap();
        trigger_outcome(&d, &sched.id, &answered).await;
        let done = s.schedules.get(&sched.id).await.unwrap().unwrap();
        assert_eq!(done.runs, 1);
        assert!(done.last_run.is_some() && done.last_error.is_none());

        run_now(&d, &sched.id).await.unwrap();
        assert_eq!(
            s.turns.pending_count().await.unwrap(),
            1,
            "exécution immédiate"
        );
    }

    /// Issue #39 : un tour planifié annulé dans la file ou en échec n'est jamais silencieux.
    #[tokio::test]
    async fn a_cancelled_or_failed_scheduled_prompt_warns_the_owner() {
        let (_dir, d, clock, rec) = daemon().await;
        let s = &d.services;
        let sched = s
            .schedules
            .create(
                TriggerKind::Interval,
                json!({"every_ms": 3_600_000}),
                json!({"type": "prompt", "label": "Veille du matin", "prompt": "Veille"}),
                json!({}),
            )
            .await
            .unwrap();
        tick(&d).await.unwrap();
        // Un ancien tour, dans une session fermée : la file l'annule sans le jouer.
        let closed = s
            .sessions
            .create(SessionKind::Chat, None)
            .await
            .unwrap()
            .id
            .to_string();
        s.sessions.set_state(&closed, "closed").await.unwrap();
        clock.advance_ms(1_000);
        s.turns
            .enqueue(
                &closed,
                TurnKind::Trigger,
                json!({"text": "Veille", "schedule": sched.id}),
                None,
                5,
            )
            .await
            .unwrap();
        assert!(s.turns.claim("t").await.unwrap().is_none());
        clock.advance_ms(1_000);
        tick(&d).await.unwrap();
        let texts = rec.texts();
        assert!(
            texts.iter().any(|t| t
                .contains("⚠️ La planification « Veille du matin » n'a pas pu s'exécuter")
                && t.contains("session fermée")),
            "{texts:?}"
        );
        let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
        assert!(
            after
                .last_error
                .as_deref()
                .unwrap()
                .contains("session fermée")
        );
        assert_eq!(after.runs, 0);
        tick(&d).await.unwrap();
        assert_eq!(rec.texts().len(), texts.len(), "une seule alerte");

        trigger_outcome(
            &d,
            &sched.id,
            &crate::agent::TurnOutcome::Failed {
                error: "fournisseur indisponible".into(),
            },
        )
        .await;
        let texts = rec.texts();
        assert!(
            texts
                .last()
                .unwrap()
                .contains("erreur du modèle : fournisseur indisponible"),
            "{texts:?}"
        );
        let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
        assert!(after.last_error.as_deref().unwrap().contains("fournisseur"));
        let doctor = crate::doctor::schedules_check(s).await;
        assert!(
            !doctor.ok && doctor.detail.contains(&sched.id),
            "{doctor:?}"
        );
    }

    /// Issue #39 : une planification identique déjà active est signalée à la création.
    #[tokio::test]
    async fn a_duplicate_schedule_is_reported_at_creation() {
        let (_dir, d, _clock, _rec) = daemon().await;
        let s = &d.services;
        let spec = json!({"expr": "30 8 * * *"});
        let first = create(
            s,
            TriggerKind::Cron,
            spec.clone(),
            json!({"type": "prompt", "prompt": "Charge la skill veille et exécute le protocole de veille"}),
            json!({}),
        )
        .await
        .unwrap();
        assert!(first.get("doublons").is_none());
        let twin = create(
            s,
            TriggerKind::Cron,
            spec.clone(),
            json!({"type": "prompt", "prompt": "Charge la skill veille et exécute le protocole de veille."}),
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(twin["doublons"], json!([first["id"]]));
        assert!(
            twin["avertissement"]
                .as_str()
                .unwrap()
                .contains("identique")
        );
        let other = create(
            s,
            TriggerKind::Cron,
            spec,
            json!({"type": "prompt", "prompt": "Résume mes mails de la nuit"}),
            json!({}),
        )
        .await
        .unwrap();
        assert!(other.get("doublons").is_none());
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
