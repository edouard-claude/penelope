//! Ce que le moteur de workflows et l'ordonnanceur lisent au-dessus d'eux, sans le daemon
//! (épopée #208, T27) : services, providers, signal d'arrêt, bus des tours, état des runs,
//! embeddings, services de la boucle d'agent, administration du processus.

use penelope_agent::AgentServices;
use penelope_app::bus::Bus;
use penelope_app::ports::{Admin, Handle, ProviderSource};
use penelope_app::services::Services;
use std::sync::Arc;

/// Contexte de l'orchestrateur. Le daemon le construit (`workflow::context_of`) ; les tests
/// le montent sur `Services::for_tests`.
#[derive(Clone)]
pub struct Context {
    pub services: Arc<Services>,
    pub providers: Arc<dyn ProviderSource>,
    pub handle: Handle,
    /// Réveil de la file des tours, après un prompt planifié.
    pub bus: Arc<Bus>,
    /// Runs pilotés par ce processus.
    pub workflows: Arc<super::State>,
    pub embeddings: Arc<penelope_vault::embeddings::State>,
    /// Les services de la boucle d'agent, ports compris : étapes `agent` et sous-agents
    /// l'appellent directement (T11).
    pub agent: Arc<AgentServices>,
    /// Ce que `self_status` lit du processus, pour l'exécuteur des étapes. Absent en test.
    pub admin: Option<Arc<dyn Admin>>,
}

impl Context {
    /// Provider d'un modèle, comme `Daemon::provider_for`.
    pub async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        self.providers.provider_for(model_id).await
    }

    /// Calcul des embeddings, comme `Daemon::embedder`.
    pub fn embedder(&self) -> penelope_vault::embeddings::Embedder {
        penelope_vault::embeddings::Embedder {
            services: self.services.clone(),
            providers: self.providers.clone(),
            state: self.embeddings.clone(),
        }
    }

    /// Contexte du rêve et de l'ingestion, comme `Daemon::dream`.
    pub fn dream(&self) -> penelope_dream::Context {
        penelope_dream::Context {
            services: self.services.clone(),
            providers: self.providers.clone(),
            embeddings: self.embeddings.clone(),
        }
    }
}
