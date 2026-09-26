//! Le contexte de l'orchestrateur sur le daemon : le moteur de workflows et
//! l'ordonnanceur vivent dans `penelope-orchestrator` (épopée #208, T27), où ils
//! reçoivent un [`Context`] au lieu du daemon. Ses appelants (coureur, RPC, superviseur,
//! passerelle) tiennent un `Arc<Daemon>` ; ce module est le seul endroit qui en dérive le
//! contexte : les registres ne suffisent pas, il y faut ses providers, son bus, l'état
//! de ses runs, les ports de la boucle et le daemon lui-même comme `Admin`.

use crate::runtime::Daemon;
use penelope_orchestrator::{Context, WorkflowOrchestrator};
use std::sync::Arc;

/// Le contexte de l'orchestrateur sur le daemon : ses services, ses providers, l'état de
/// ses runs, les ports de la boucle sur ses modules (`agent::services_of`), et le daemon
/// lui-même comme `Admin` de l'exécuteur des étapes.
pub fn context_of(d: &Arc<Daemon>) -> Context {
    Context {
        services: d.services.clone(),
        providers: d.providers.clone(),
        handle: d.handle.clone(),
        bus: d.bus.clone(),
        workflows: d.workflows.clone(),
        embeddings: d.embeddings.clone(),
        agent: crate::agent::judged(&d.services, d.providers.clone()),
        admin: Some(d.clone()),
    }
}

/// L'orchestrateur offert aux outils et à l'ordonnanceur, sur le contexte du daemon.
pub fn orchestrator_of(d: &Arc<Daemon>) -> WorkflowOrchestrator {
    WorkflowOrchestrator {
        context: context_of(d),
    }
}
