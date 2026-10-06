//! Déclencheurs sondés : `mcp_poll`, `watch_file`, `event`.

use super::*;

/// `mcp_poll` : appelle un outil MCP en lecture, extrait les éléments, déclenche la cible
/// pour les nouveaux (ou modifiés). Le premier passage amorce sans déclencher, sauf
/// `backfill`. `vars` : valeurs du passage (retard, heures calmes), reprises par chaque
/// tir ; `batch` : la fournée des heures calmes (#296). Ce que la plage a accumulé
/// (#318) part avec ce passage, en un seul tir pour un prompt ou un workflow.
pub(super) async fn poll(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    vars: &BTreeMap<String, String>,
    mut batch: Option<&mut Batch>,
) -> anyhow::Result<bool> {
    let s = &d.services;
    let items = fetch(d, ports, sched).await?;
    if sched.runs == 0 && !backfills(sched) {
        s.schedules.seed(&sched.id, &items, false).await?;
        return Ok(false);
    }
    let fresh = s
        .schedules
        .new_or_changed(&sched.id, &items, retriggers(sched))
        .await?;
    let stash = quiet::accumulated(s, sched).await?;
    let merge = stash.is_some() || vars.contains_key(quiet::HELD);
    let items = match &stash {
        Some((_, night)) => {
            let at = s.clock.now_rfc3339();
            let mut all = night.clone();
            all.extend(fresh.into_iter().map(|i| quiet::seen_at(s, i, &at)));
            all
        }
        None => fresh,
    };
    let groups = quiet::groups(s, sched, &items, merge);
    for group in &groups {
        fire(d, ports, sched, group, vars, batch.as_deref_mut()).await?;
    }
    if let Some((row, _)) = stash {
        penelope_app::quiet::forget(s, &[row]).await?;
    }
    Ok(!groups.is_empty())
}

/// `backfill` : le premier passage tire sur les éléments existants au lieu d'amorcer.
pub(super) fn backfills(sched: &Schedule) -> bool {
    sched.dedup.get("backfill").and_then(|b| b.as_bool()) == Some(true)
}

/// `retrigger_on_change` : un élément connu dont l'empreinte change tire de nouveau.
pub(super) fn retriggers(sched: &Schedule) -> bool {
    sched
        .dedup
        .get("retrigger_on_change")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
}

/// L'appel d'un `mcp_poll` : l'outil MCP en lecture, ses éléments extraits et filtrés.
pub(super) async fn fetch(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
) -> anyhow::Result<Vec<PolledItem>> {
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
    let sup = ports
        .mcp
        .get()
        .ok_or_else(|| anyhow::anyhow!("superviseur MCP non démarré"))?;
    let result = sup
        // Un `mcp_poll` tourne sans le propriétaire : une élicitation n'aurait pas de
        // conversation où revenir, le canal prendra son repli (issue #143).
        .call_tool(
            &qualified,
            spec.get("args").unwrap_or(&json!({})),
            Default::default(),
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let payload = tool_payload(&result);
    Ok(penelope_workflow::schedules::extract_items(
        &payload,
        spec["item_path"].as_str().unwrap_or("$"),
        spec["id_path"].as_str().unwrap_or("id"),
    )
    .into_iter()
    .filter(|i| penelope_workflow::schedules::passes_filter(&i.value, spec.get("filter")))
    .collect())
}

/// Empreinte d'un fichier surveillé (date de modification, taille), ou `absent`.
fn file_fingerprint(d: &Context, sched: &Schedule) -> (std::path::PathBuf, String) {
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
    (path, fingerprint)
}

fn watch_key(sched: &Schedule) -> String {
    format!("scheduler.watch.{}", sched.id)
}

/// `watch_file` retenu par les heures calmes (#296) : la première observation est prise
/// quand même, sans déclencher, pour qu'un changement de la nuit se voie à la fin de la
/// plage ; une empreinte déjà connue n'est pas touchée.
pub(super) async fn watch_file_seed(d: &Context, sched: &Schedule) -> anyhow::Result<()> {
    let key = watch_key(sched);
    if d.services.kv_get(&key).await?.is_none() {
        let (_, fingerprint) = file_fingerprint(d, sched);
        d.services.kv_set(&key, &fingerprint).await?;
    }
    Ok(())
}

/// `watch_file` : empreinte (date de modification, taille) comparée au passage précédent.
pub(super) async fn watch_file(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    base: &BTreeMap<String, String>,
    batch: Option<&mut Batch>,
) -> anyhow::Result<bool> {
    let (path, fingerprint) = file_fingerprint(d, sched);
    let key = watch_key(sched);
    let previous = d.services.kv_get(&key).await?;
    if previous.as_deref() == Some(fingerprint.as_str()) {
        return Ok(false);
    }
    d.services.kv_set(&key, &fingerprint).await?;
    // Première observation : on mémorise sans déclencher.
    if previous.is_none() {
        return Ok(false);
    }
    let mut vars = base.clone();
    vars.insert("path".to_string(), path.display().to_string());
    vars.insert(
        "state".to_string(),
        if fingerprint == "absent" {
            "supprimé".into()
        } else {
            "modifié".into()
        },
    );
    fire(d, ports, sched, &[], &vars, batch).await?;
    Ok(true)
}

/// `event` : événements du journal apparus depuis le passage précédent.
pub(super) async fn event(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    events: &[penelope_kernel::event::Event],
    base: &BTreeMap<String, String>,
    mut batch: Option<&mut Batch>,
) -> anyhow::Result<bool> {
    let wanted = sched.spec["event"].as_str().unwrap_or_default();
    let matching: Vec<&penelope_kernel::event::Event> =
        events.iter().filter(|e| e.kind == wanted).collect();
    // Rattrapage des heures calmes (#318) : un prompt ou un workflow tire une fois, avec
    // les événements de la nuit en éléments, chacun avec son heure.
    if quiet::merges(sched, base) && !matching.is_empty() {
        let items: Vec<PolledItem> = matching
            .iter()
            .map(|ev| {
                let item = PolledItem {
                    id: ev.id.to_string(),
                    fingerprint: String::new(),
                    value: json!({"event": ev.kind, "payload": ev.payload}),
                };
                quiet::seen_at(&d.services, item, &ev.ts)
            })
            .collect();
        let mut vars = base.clone();
        vars.insert("event".to_string(), wanted.to_string());
        fire(d, ports, sched, &items, &vars, batch).await?;
        return Ok(true);
    }
    let mut fired = false;
    for ev in matching {
        let mut vars = base.clone();
        vars.insert("event".to_string(), ev.kind.clone());
        vars.insert("payload".to_string(), ev.payload.to_string());
        if let Some(sid) = &ev.session_id {
            vars.insert("session".to_string(), sid.clone());
        }
        fire(d, ports, sched, &[], &vars, batch.as_deref_mut()).await?;
        fired = true;
    }
    Ok(fired)
}

/// Événements d'identifiant dans `]from, to]`, page par page.
pub(super) async fn events_between(
    d: &Context,
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

pub(super) async fn event_cursor(d: &Context) -> anyhow::Result<i64> {
    match d.services.kv_get("scheduler.event_cursor").await? {
        Some(v) => Ok(v.parse().unwrap_or(0)),
        None => {
            // Premier démarrage : l'historique ne déclenche rien.
            let last = last_event_id(d).await?;
            d.services
                .kv_set("scheduler.event_cursor", &last.to_string())
                .await?;
            Ok(last)
        }
    }
}

pub(super) async fn last_event_id(d: &Context) -> anyhow::Result<i64> {
    Ok(d.services
        .store
        .read(|c| Ok(c.query_row("SELECT COALESCE(MAX(id), 0) FROM events", [], |r| r.get(0))?))
        .await?)
}

pub(super) mod mcp_subscribe;
pub(super) use mcp_subscribe::subscribe;
