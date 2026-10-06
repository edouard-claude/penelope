//! Heures calmes dans l'ordonnanceur (#296) : une planification sans `urgent` qui tombe
//! dans la plage n'est pas tirée ; elle reste due et part au premier passage après la
//! plage, une fois, avec la logique des créneaux manqués du rattrapage après veille
//! (`wake.rs`, #228). Les tirs retenus d'un même passage, et la file des livraisons
//! retenues ailleurs (`penelope_app::quiet`, alertes MCP), partent groupés : un message
//! par conversation, sous « 🌙 Pendant les heures calmes : ».
//!
//! ```text
//! passage pendant la plage
//!   planification due, pas urgente ─► reste due, `schedule.held` (une fois par créneau)
//!   `event` / `watch_file` sans urgent ─► passés ; curseur du journal gardé (pushed_from)
//! premier passage après la plage
//!   planification due ─► late_of(…, quiet) ─► tir avec la mention, créneaux fusionnés
//!   `event` ─► journal relu depuis pushed_from ; `watch_file` ─► empreinte comparée
//!   fournée + file persistée ─► un message par conversation, en-tête, puis les éléments
//! ```
//!
//! Fusion par planification (#318) : un prompt ou un workflow coûte un tour ou un run, il
//! ne part qu'**une** fois à la fin de la plage, quel que soit le nombre d'éléments.
//!
//! ```text
//! pendant la plage
//!   `mcp_poll` retenu, à sa cadence ─► éléments nouveaux ─► sa ligne de quiet_queue,
//!                                                         complétée (une par planification)
//! premier passage après la plage
//!   `mcp_poll`       ─► ligne accumulée + nouveautés du passage ─► un tir
//!   `mcp_subscribe`  ─► ressource relue tout de suite ─► un tir
//!   `event`          ─► événements de la nuit ─► un tir, chacun avec son heure
//!   `cron`/`interval` ─► un tir, « N occurrences manquées » (wake.rs)
//!   tours relâchés ─► couloir `heures_calmes` : l'un après l'autre
//! ```

use super::*;
use penelope_app::quiet as queue;
use penelope_kernel::event::EventDraft;

/// Variable d'un tir retenu : la mention courte (« prévue à 23h00 »), reprise par la
/// livraison groupée.
pub(super) const HELD: &str = "heures_calmes";

/// Clé kv : curseur du journal au premier passage où un déclencheur poussé (`event`,
/// `watch_file`) sans `urgent` a été retenu. Effacée quand ils ont rattrapé.
pub(super) const PUSHED_FROM: &str = "scheduler.quiet.pushed_from";

fn held_key(id: &str) -> String {
    format!("scheduler.quiet.held.{id}")
}

/// Vrai si les heures calmes retiennent cette planification maintenant.
pub(super) fn holds(s: &Services, sched: &Schedule) -> bool {
    !sched.is_urgent() && queue::is_quiet(s)
}

/// Note qu'un créneau dû attend la fin de la plage : `schedule.held`, une fois par créneau
/// (le passage suivant retrouve la même planification due).
pub(super) async fn mark_held(d: &Context, sched: &Schedule) -> anyhow::Result<()> {
    let s = &d.services;
    let key = held_key(&sched.id);
    let planned = sched.next_run.clone().unwrap_or_default();
    if s.kv_get(&key).await?.as_deref() == Some(planned.as_str()) {
        return Ok(());
    }
    s.kv_set(&key, &planned).await?;
    let until = s.config.config().quiet_range().map(|r| r.end_text());
    tracing::info!(schedule = %sched.id, planned, ?until, "créneau retenu par les heures calmes");
    s.events
        .append(EventDraft::new(
            "schedule.held",
            json!({"schedule": sched.id, "planned": planned, "until": until}),
        ))
        .await?;
    Ok(())
}

/// Le créneau retenu est parti : la marque s'efface.
pub(super) async fn clear_held(s: &Services, id: &str) {
    let _ = s.kv_delete(&held_key(id)).await;
    let _ = s.kv_delete(&polled_key(id)).await;
}

/// Couloir des tours relâchés à la fin de la plage (#318) : ils partent l'un après
/// l'autre, jamais plusieurs à la fois dans un même sujet.
pub(super) const QUIET_LANE: &str = "heures_calmes";

fn polled_key(id: &str) -> String {
    format!("scheduler.quiet.polled.{id}")
}

/// Ce qui coûte un tour ou un run : fusionné en un seul tir à la fin de la plage. Une
/// notification part déjà dans le message groupé.
fn costly(sched: &Schedule) -> bool {
    matches!(
        sched.target_kind(),
        Some(TargetKind::Prompt | TargetKind::Workflow)
    )
}

/// Les tirs d'un passage : un seul pour une planification coûteuse qui rattrape la plage
/// (`merge`), sinon le regroupement ordinaire de la planification.
pub(super) fn groups(
    s: &Services,
    sched: &Schedule,
    items: &[PolledItem],
    merge: bool,
) -> Vec<Vec<PolledItem>> {
    if merge && costly(sched) && !items.is_empty() {
        return vec![items.to_vec()];
    }
    s.schedules.coalesce(sched, items)
}

/// Vrai si `event` doit fusionner ses événements en un tir (`vars` d'un rattrapage).
pub(super) fn merges(sched: &Schedule, vars: &BTreeMap<String, String>) -> bool {
    vars.contains_key(HELD) && costly(sched)
}

/// L'élément, avec l'heure locale où il a été vu (`observed_at`, « 22h05 »).
pub(super) fn seen_at(s: &Services, mut item: PolledItem, at: &str) -> PolledItem {
    let hour = Value::String(queue::local_hour(s, at));
    item.value = match item.value {
        Value::Object(mut o) => {
            o.insert("observed_at".into(), hour);
            Value::Object(o)
        }
        other => json!({"observed_at": hour, "value": other}),
    };
    item
}

fn to_json(item: &PolledItem) -> Value {
    json!({"id": item.id, "fingerprint": item.fingerprint, "value": item.value})
}

fn from_json(v: &Value) -> PolledItem {
    PolledItem {
        id: v["id"].as_str().unwrap_or_default().to_string(),
        fingerprint: v["fingerprint"].as_str().unwrap_or_default().to_string(),
        value: v["value"].clone(),
    }
}

/// Ce qu'un `mcp_poll` a accumulé pendant la plage : sa ligne et ses éléments.
pub(super) async fn accumulated(
    s: &Services,
    sched: &Schedule,
) -> anyhow::Result<Option<(i64, Vec<PolledItem>)>> {
    Ok(queue::accumulated(s, &sched.id)
        .await?
        .map(|(id, items)| (id, items.iter().map(from_json).collect())))
}

/// Un `mcp_poll` retenu sonde quand même, à sa cadence (#318) : ses éléments nouveaux
/// rejoignent sa ligne de la file, avec leur heure, sans rien tirer. Une source qui ne
/// rend que ses derniers éléments ne perd ainsi rien de la nuit. Une planification pas
/// encore amorcée attend la fin de la plage ; un sondage en échec se tait, le passage de
/// la fin de la plage le dira.
pub(super) async fn accumulate(d: &Context, ports: &Ports, sched: &Schedule) -> anyhow::Result<()> {
    let s = &d.services;
    if sched.runs == 0 && !backfills(sched) {
        return Ok(());
    }
    let now = s.clock.now_ms();
    let key = polled_key(&sched.id);
    let last = s.kv_get(&key).await?.and_then(|v| v.parse::<i64>().ok());
    let tz = s.config.config().owner.timezone.clone();
    if last.is_some_and(|t| sched.next_after(t, &tz).is_none_or(|next| next > now)) {
        return Ok(());
    }
    s.kv_set(&key, &now.to_string()).await?;
    let items = match fetch(d, ports, sched).await {
        Ok(items) => items,
        Err(e) => {
            tracing::warn!(schedule = %sched.id, error = %e, "sondage de nuit en échec");
            return Ok(());
        }
    };
    let fresh = s
        .schedules
        .new_or_changed(&sched.id, &items, retriggers(sched))
        .await?;
    let at = s.clock.now_rfc3339();
    let fresh: Vec<Value> = fresh
        .into_iter()
        .map(|i| to_json(&seen_at(s, i, &at)))
        .collect();
    queue::accumulate(s, &sched.id, &target_origin(s, sched), fresh).await
}

/// Variables d'un déclencheur poussé qui rattrape à la fin de la plage.
pub(super) fn pushed_vars(s: &Services) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    let end = s
        .config
        .config()
        .quiet_range()
        .map(|r| r.end_text())
        .unwrap_or_default();
    vars.insert(
        LATE.to_string(),
        format!(
            "{} déclencheur retenu, constaté à la fin de la plage ({end}).",
            queue::HEADER
        ),
    );
    vars.insert(
        HELD.to_string(),
        "constaté à la fin des heures calmes".to_string(),
    );
    vars
}

/// Une livraison de la fournée : où, quoi, et la planification `notify` qu'elle porte
/// (son événement `schedule.notified` attend l'envoi), ou la ligne de la file persistée.
struct Item {
    origin: Origin,
    text: String,
    notified: Option<(String, usize)>,
    queued: Option<i64>,
}

/// Tirs retenus d'un passage, à livrer groupés.
#[derive(Default)]
pub(super) struct Batch {
    items: Vec<Item>,
}

impl Batch {
    pub(super) fn push(&mut self, origin: Origin, text: String, notified: Option<(String, usize)>) {
        self.items.push(Item {
            origin,
            text,
            notified,
            queued: None,
        });
    }
}

/// Livre la fournée du passage et, hors de la plage, la file persistée : un message par
/// conversation, l'en-tête puis les éléments dans l'ordre. Une ligne de la file n'est
/// oubliée qu'une fois partie ; une notification qui ne part pas est un échec de sa
/// planification, comme un envoi direct.
pub(super) async fn flush(
    d: &Context,
    ports: &Ports,
    batch: Batch,
    report: &mut TickReport,
) -> anyhow::Result<()> {
    let s = &d.services;
    let mut items = batch.items;
    if !queue::is_quiet(s) {
        for h in queue::held(s).await? {
            items.push(Item {
                origin: h.origin,
                text: format!(
                    "{}\n(reçu à {})",
                    h.text,
                    queue::local_hour(s, &h.created_at)
                ),
                notified: None,
                queued: Some(h.id),
            });
        }
    }
    if items.is_empty() {
        return Ok(());
    }
    let Some(m) = ports.messenger.get() else {
        tracing::warn!(
            items = items.len(),
            "fin des heures calmes sans canal de message : livraisons gardées"
        );
        return Ok(());
    };
    let mut groups: Vec<(Origin, Vec<Item>)> = Vec::new();
    for item in items {
        match groups.iter_mut().find(|(o, _)| *o == item.origin) {
            Some((_, group)) => group.push(item),
            None => groups.push((item.origin.clone(), vec![item])),
        }
    }
    let mut delivered = 0usize;
    let mut forget = Vec::new();
    for (origin, group) in groups {
        let body: Vec<&str> = group.iter().map(|i| i.text.as_str()).collect();
        let text = format!("{}\n\n{}", queue::HEADER, body.join("\n\n"));
        match m.send_text(&origin, &text).await {
            Ok(()) => {
                delivered += group.len();
                for item in group {
                    if let Some((id, count)) = item.notified {
                        s.events
                            .append(EventDraft::new(
                                "schedule.notified",
                                json!({"schedule": id, "items": count, "quiet": true}),
                            ))
                            .await?;
                    }
                    forget.extend(item.queued);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "livraison groupée des heures calmes non envoyée");
                for item in group {
                    if let Some((id, _)) = item.notified {
                        s.schedules.record_outcome(&id, Some(&e)).await?;
                        report.errors.push((id, e.clone()));
                    }
                }
            }
        }
    }
    queue::forget(s, &forget).await?;
    if delivered > 0 {
        s.events
            .append(EventDraft::new(
                "quiet.delivered",
                json!({"items": delivered, "queued": forget.len()}),
            ))
            .await?;
    }
    Ok(())
}
