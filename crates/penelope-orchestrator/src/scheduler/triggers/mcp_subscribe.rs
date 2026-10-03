//! `mcp_subscribe` (#293) : une ressource MCP suivie par abonnement, au lieu d'un sondage.
//!
//! ```text
//! chaque passage (10 s)
//!   ├─ subscribe_resource(serveur, uri) ─► posé (ou reposé après reconnexion) : abonné
//!   │                                     serveur sans resources.subscribe : sondage
//!   │                                     serveur injoignable : perdu (digest après 1 h)
//!   ├─ notification `mcp.resource_updated` au journal ─► ouvre la fenêtre (30 s)
//!   ├─ fenêtre close, sondage dû, ou reprise ─► resources/read ─► éléments
//!   └─ nouveaux ou modifiés (seen_items, comme mcp_poll) ─► cible, sous plafond horaire ;
//!      le surplus d'un prompt ou d'un workflow part en notification sans modèle
//! ```
//!
//! L'état propre au déclencheur (`scheduler.mcp_subscribe.<id>`) : mode, fenêtre, dernière
//! lecture, tirs de l'heure, depuis quand l'abonnement est perdu.

use super::*;
use penelope_workflow::schedules::subscribe::{
    DEFAULT_EVERY_MS, DEFAULT_MAX_PER_HOUR, DEFAULT_WINDOW_MS,
};
use serde::{Deserialize, Serialize};

/// Événement du journal écrit par le superviseur à chaque `notifications/resources/updated`.
pub const RESOURCE_UPDATED: &str = "mcp.resource_updated";

/// Abonnement perdu au-delà de ce délai : le digest le dit.
pub const LOST_ALERT_AFTER_MS: i64 = 3_600_000;

/// État d'un `mcp_subscribe`, gardé dans le `kv`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeState {
    /// `subscribe`, `poll` (repli) ou `lost` (serveur injoignable).
    #[serde(default)]
    pub mode: String,
    /// Première notification de la fenêtre ouverte, ms epoch.
    pub window_since_ms: Option<i64>,
    /// Dernière lecture de la ressource, ms epoch.
    pub last_read_ms: Option<i64>,
    /// Depuis quand le serveur est injoignable, ms epoch.
    pub lost_since_ms: Option<i64>,
    /// Ce que le serveur a répondu la dernière fois qu'il était injoignable.
    pub last_error: Option<String>,
    /// Tirs de l'heure glissante (prompt ou workflow), ms epoch.
    #[serde(default)]
    pub fired_ms: Vec<i64>,
    /// Les éléments existants ont été marqués vus (ou `backfill` a tiré dessus).
    #[serde(default)]
    pub seeded: bool,
}

fn key(id: &str) -> String {
    format!("scheduler.mcp_subscribe.{id}")
}

/// État d'une planification, tel que le `kv` le garde.
pub async fn subscription_state(s: &Services, id: &str) -> SubscribeState {
    s.kv_get(&key(id))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

async fn save(s: &Services, id: &str, st: &SubscribeState) -> anyhow::Result<()> {
    s.kv_set(&key(id), &serde_json::to_string(st)?).await
}

/// Abonnement perdu depuis plus d'une heure : depuis quand, et pourquoi, pour le digest.
pub async fn lost_for(s: &Services, sched: &Schedule) -> Option<(i64, String)> {
    let st = subscription_state(s, &sched.id).await;
    let since = st.lost_since_ms?;
    (s.clock.now_ms() - since >= LOST_ALERT_AFTER_MS)
        .then(|| (since, st.last_error.unwrap_or_default()))
}

/// Un passage pour une planification `mcp_subscribe`. `events` : les événements du journal
/// apparus depuis le passage précédent ; `force` : relire sans attendre (« Relire
/// maintenant »). Rend vrai si la cible a tiré.
pub(in crate::scheduler) async fn subscribe(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    events: &[penelope_kernel::event::Event],
    force: bool,
) -> anyhow::Result<bool> {
    let s = &d.services;
    let spec = &sched.spec;
    let server = spec["server"].as_str().unwrap_or_default();
    let uri = spec["uri"].as_str().unwrap_or_default();
    let every_ms = spec["every_ms"].as_u64().unwrap_or(DEFAULT_EVERY_MS) as i64;
    let window_ms = spec["window_ms"].as_u64().unwrap_or(DEFAULT_WINDOW_MS) as i64;
    let max_per_hour = spec["max_per_hour"]
        .as_u64()
        .unwrap_or(DEFAULT_MAX_PER_HOUR) as usize;
    let now = s.clock.now_ms();
    let mut st = subscription_state(s, &sched.id).await;

    // 1. L'abonnement, posé ou reposé : le superviseur dit s'il tient.
    let mut read_now = force;
    let subscribed = match ports.mcp.get() {
        Some(m) => m.subscribe_resource(server, uri).await,
        None => Err("superviseur MCP non démarré".to_string()),
    };
    match subscribed {
        Ok(true) => {
            if st.mode != "subscribe" {
                // Reprise après une coupure (ou premier passage) : ce qui a bougé pendant
                // le silence n'a pas été notifié, on relit.
                read_now = st.seeded;
                restored(d, sched, &st).await;
            }
            st.mode = "subscribe".into();
            st.lost_since_ms = None;
            st.last_error = None;
        }
        Ok(false) => {
            if st.mode == "lost" {
                restored(d, sched, &st).await;
            }
            st.mode = "poll".into();
            st.lost_since_ms = None;
            st.last_error = None;
        }
        Err(e) => {
            if st.lost_since_ms.is_none() {
                tracing::warn!(schedule = %sched.id, server, uri, error = %e, "abonnement MCP perdu");
                let _ = s
                    .events
                    .append(penelope_kernel::event::EventDraft::new(
                        "schedule.subscription_lost",
                        json!({"schedule": sched.id, "server": server, "uri": uri, "error": e}),
                    ))
                    .await;
                st.lost_since_ms = Some(now);
            }
            st.mode = "lost".into();
            st.last_error = Some(e);
        }
    }

    // 2. Les notifications de ce passage ouvrent (ou prolongent) la fenêtre.
    let notified = events.iter().any(|e| {
        e.kind == RESOURCE_UPDATED && e.payload["server"] == server && e.payload["uri"] == uri
    });
    if notified && st.window_since_ms.is_none() {
        st.window_since_ms = Some(now);
    }

    // 3. Lire ? Fenêtre close, sondage dû, reprise, ou amorçage.
    let window_closed = st.window_since_ms.is_some_and(|w| now - w >= window_ms);
    let poll_due = st.mode == "poll" && st.last_read_ms.is_none_or(|t| now - t >= every_ms);
    let seeding = !st.seeded && st.mode != "lost";
    if st.mode == "lost" || !(read_now || window_closed || poll_due || seeding) {
        save(s, &sched.id, &st).await?;
        return Ok(false);
    }

    let read = match ports.mcp.get() {
        Some(m) => m.read_resource(server, uri).await,
        None => Err("superviseur MCP non démarré".to_string()),
    };
    let payload = match read {
        Ok(v) => resource_payload(&v),
        Err(e) => {
            // La fenêtre reste ouverte : le passage suivant réessaie.
            save(s, &sched.id, &st).await?;
            anyhow::bail!(e);
        }
    };
    st.window_since_ms = None;
    st.last_read_ms = Some(now);
    let items = extract(spec, uri, &payload);

    let backfill = sched.dedup.get("backfill").and_then(|b| b.as_bool()) == Some(true);
    if !st.seeded {
        st.seeded = true;
        if !backfill {
            s.schedules.seed(&sched.id, &items, false).await?;
            save(s, &sched.id, &st).await?;
            return Ok(false);
        }
    }
    // Sans `id_path`, l'élément est la ressource entière : chaque changement compte.
    let retrigger = sched
        .dedup
        .get("retrigger_on_change")
        .and_then(|b| b.as_bool())
        .unwrap_or(spec.get("id_path").is_none());
    let fresh = s
        .schedules
        .new_or_changed(&sched.id, &items, retrigger)
        .await?;
    if fresh.is_empty() {
        save(s, &sched.id, &st).await?;
        return Ok(false);
    }

    // 4. Tirer, sous plafond horaire pour ce qui coûte un tour ou un run.
    let mut vars = BTreeMap::new();
    vars.insert("server".to_string(), server.to_string());
    vars.insert("uri".to_string(), uri.to_string());
    st.fired_ms.retain(|t| now - *t < 3_600_000);
    let capped_kind = sched.target_kind() != Some(TargetKind::Notify);
    let mut overflow: Vec<PolledItem> = Vec::new();
    let mut fired = false;
    for group in s.schedules.coalesce(sched, &fresh) {
        if capped_kind && st.fired_ms.len() >= max_per_hour {
            overflow.extend(group);
            continue;
        }
        fire(d, ports, sched, &group, &vars).await?;
        st.fired_ms.push(now);
        fired = true;
    }
    if !overflow.is_empty() {
        capped(d, ports, sched, &overflow, max_per_hour).await;
    }
    save(s, &sched.id, &st).await?;
    Ok(fired || !overflow.is_empty())
}

/// L'abonnement qui était perdu tient de nouveau : au journal, et au propriétaire s'il
/// avait été prévenu (une heure ou plus).
async fn restored(d: &Context, sched: &Schedule, st: &SubscribeState) {
    let Some(since) = st.lost_since_ms else {
        return;
    };
    let s = &d.services;
    let lasted_ms = s.clock.now_ms() - since;
    tracing::info!(schedule = %sched.id, lasted_ms, "abonnement MCP rétabli");
    let _ = s
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.subscription_restored",
            json!({"schedule": sched.id, "lasted_ms": lasted_ms}),
        ))
        .await;
}

/// Plafond horaire atteint : les éléments en surplus partent en notification, sans
/// modèle, pour que rien ne se perde en silence.
async fn capped(d: &Context, ports: &Ports, sched: &Schedule, items: &[PolledItem], max: usize) {
    let s = &d.services;
    let _ = s
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.capped",
            json!({"schedule": sched.id, "items": items.len(), "max_per_hour": max}),
        ))
        .await;
    let text = format!(
        "⏳ « {} » : {max} déclenchements dans l'heure, plafond atteint ; {} élément(s) \
         notifié(s) sans traitement :\n{}",
        label(s, sched).await,
        items.len(),
        items_lines(items)
    );
    let origin = target_origin(s, sched);
    match ports.messenger.get() {
        Some(m) => {
            if let Err(e) = m.send_text(&origin, &text).await {
                tracing::warn!(schedule = %sched.id, error = %e, "surplus non notifié");
            }
        }
        None => tracing::warn!(schedule = %sched.id, "{text}"),
    }
}

/// Données d'une ressource lue : le premier contenu texte, décodé en JSON s'il en est,
/// sinon le texte ; un contenu binaire seul donne son `uri`.
pub fn resource_payload(read: &Value) -> Value {
    let contents = read["contents"].as_array().cloned().unwrap_or_default();
    let text = contents
        .iter()
        .find_map(|c| c.get("text").and_then(|t| t.as_str()));
    match text {
        Some(t) => serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string())),
        None => contents
            .first()
            .and_then(|c| c.get("uri"))
            .cloned()
            .unwrap_or(Value::Null),
    }
}

/// Éléments d'une ressource : une liste adressée par `item_path` et `id_path`, comme
/// `mcp_poll` ; sans `id_path`, la ressource entière, identifiée par son `uri`.
fn extract(spec: &Value, uri: &str, payload: &Value) -> Vec<PolledItem> {
    let items = match spec.get("id_path").and_then(|p| p.as_str()) {
        Some(id_path) => penelope_workflow::schedules::extract_items(
            payload,
            spec["item_path"].as_str().unwrap_or("$"),
            id_path,
        ),
        None => {
            let value = match spec["item_path"].as_str() {
                Some(p) => {
                    penelope_workflow::conditions::json_path(payload, p).unwrap_or(Value::Null)
                }
                None => payload.clone(),
            };
            vec![PolledItem {
                id: uri.to_string(),
                fingerprint: penelope_kernel::canonical::canonical_hash(&value),
                value,
            }]
        }
    };
    items
        .into_iter()
        .filter(|i| penelope_workflow::schedules::passes_filter(&i.value, spec.get("filter")))
        .collect()
}
