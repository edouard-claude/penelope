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

use crate::bus::ChannelDelivery;
use crate::bus::Origin;
use crate::helpers::owner_origin_of;
use crate::ports::{McpAdmin, Messenger, Slot};
use crate::runtime::{Daemon, Services};
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::TurnKind;
use penelope_workflow::schedules::{PolledItem, Schedule, TargetKind, TriggerKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Période de l'ordonnanceur.
pub const TICK: Duration = Duration::from_secs(10);

/// Branchements que l'ordonnanceur reçoit de la composition : canal du propriétaire,
/// livraison des alertes, MCP pour le digest. Lus au moment de s'en servir.
#[derive(Clone, Default)]
pub struct Ports {
    pub messenger: Slot<dyn Messenger>,
    pub delivery: Slot<dyn ChannelDelivery>,
    pub mcp: Slot<dyn McpAdmin>,
    pub orchestrator: Slot<dyn crate::executor::Orchestrator>,
}

/// Ce qu'un passage a fait, pour les journaux et les tests.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct TickReport {
    pub fired: Vec<String>,
    pub errors: Vec<(String, String)>,
    pub intents_expired: usize,
}

/// Boucle de l'ordonnanceur, jusqu'à l'arrêt du daemon.
pub async fn scheduler_loop(d: Arc<Daemon>, ports: Ports) {
    // La boîte de dépôt du vault passe à côté : une ingestion (résumé compris) ne doit pas
    // retarder un rappel.
    let inbox_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    while !d.handle.is_shutting_down() {
        if !inbox_busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let (d2, busy, messenger) = (d.clone(), inbox_busy.clone(), ports.messenger.clone());
            tokio::spawn(async move {
                match crate::ingest::scan_inbox(&d2.dream(), &messenger).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(fichiers = n, "boîte de dépôt du vault traitée"),
                    Err(e) => tracing::warn!(error = %e, "boîte de dépôt du vault"),
                }
                busy.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        let (dream, feed) = (d.dream(), Arc::new(crate::dream::DigestFeed(d.clone())));
        let crons = penelope_dream::system_crons(&dream, feed, &ports.messenger, &ports.mcp);
        if let Err(e) = crons.await {
            tracing::warn!(error = %e, "consolidation ou digest programmés");
        }
        match tick(&d, &ports).await {
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
pub async fn tick(d: &Arc<Daemon>, ports: &Ports) -> anyhow::Result<TickReport> {
    let s = &d.services;
    let mut report = TickReport {
        intents_expired: s.intents.expire_due().await?,
        ..Default::default()
    };

    for sched in s.schedules.due().await? {
        let result = match sched.kind {
            TriggerKind::McpPoll => poll(d, ports, &sched).await,
            _ => fire(d, ports, &sched, &[], &BTreeMap::new())
                .await
                .map(|_| true),
        };
        finish(d, ports, &sched, result, &mut report).await?;
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
            TriggerKind::WatchFile => watch_file(d, ports, &sched).await,
            TriggerKind::Event => event(d, ports, &sched, &events).await,
            _ => continue,
        };
        match result {
            Ok(false) => {}
            other => finish(d, ports, &sched, other, &mut report).await?,
        }
    }
    s.kv_set("scheduler.event_cursor", &to.to_string()).await?;
    cancelled_triggers(d, ports).await?;
    Ok(report)
}

/// Exécution immédiate, hors calendrier (`schedule.run_now`).
pub async fn run_now(d: &Arc<Daemon>, ports: &Ports, id: &str) -> anyhow::Result<Value> {
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
        TriggerKind::McpPoll => poll(d, ports, &sched).await,
        _ => fire(d, ports, &sched, &[], &manual).await.map(|_| true),
    };
    let mut report = TickReport::default();
    finish(d, ports, &sched, result, &mut report).await?;
    Ok(serde_json::to_value(report)?)
}

/// Variable d'un déclenchement manuel : rend sa clé de dédoublonnage unique.
const MANUAL: &str = "declenchement_manuel";

/// Enregistre le tir (ou l'erreur) et programme la suite.
async fn finish(
    d: &Arc<Daemon>,
    ports: &Ports,
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
                alert(d, ports, sched, e).await;
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

mod fire;
mod origin;
mod outcome;
mod templating;
mod triggers;
use fire::fire;
pub use fire::{alert, create, label};
use origin::target_origin;
pub use origin::{destination, due_today, listing, place_name, retarget};
use outcome::{cancelled_triggers, save_state};
pub use outcome::{final_already_sent, repeats, trigger_outcome, trigger_outcome_of};
use templating::{items_lines, substitute, template_params, tool_payload};
use triggers::{event, event_cursor, events_between, last_event_id, poll, watch_file};

#[cfg(test)]
mod tests;
