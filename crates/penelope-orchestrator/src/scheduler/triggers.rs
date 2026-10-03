//! Déclencheurs sondés : `mcp_poll`, `watch_file`, `event`.

use super::*;

/// `mcp_poll` : appelle un outil MCP en lecture, extrait les éléments, déclenche la cible
/// pour les nouveaux (ou modifiés). Le premier passage amorce sans déclencher, sauf
/// `backfill`. `vars` : valeurs du passage (retard, heures calmes), reprises par chaque
/// tir ; `batch` : la fournée des heures calmes (#296).
pub(super) async fn poll(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    vars: &BTreeMap<String, String>,
    mut batch: Option<&mut Batch>,
) -> anyhow::Result<bool> {
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
        fire(d, ports, sched, group, vars, batch.as_deref_mut()).await?;
    }
    Ok(!groups.is_empty())
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
    let mut fired = false;
    for ev in events.iter().filter(|e| e.kind == wanted) {
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
