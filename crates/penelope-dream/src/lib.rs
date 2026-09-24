//! `penelope-dream` : la mémoire qui mûrit (épopée #208, T26).
//!
//! Entretien d'accueil (`onboarding`) ; la consolidation nocturne (`dream/`) et
//! l'ingestion de documents (`ingest`) y descendent quand leurs signatures ne citent plus
//! `Daemon`. Au-dessus de `penelope-app` et de `penelope-vault`, sous le daemon, qui
//! réexporte ces modules sous leurs anciens chemins jusqu'à T30.

#![forbid(unsafe_code)]

pub mod onboarding;

use penelope_app::ports::ProviderSource;
use penelope_app::services::Services;
use std::sync::Arc;

/// Ce que le rêve et l'ingestion lisent du daemon, sans le daemon : les services, les
/// providers de modèles et l'état du calcul des embeddings. Le daemon le construit
/// (`Daemon::dream`) ; les champs et méthodes portent les noms de `Daemon`, pour que les
/// corps déplacés ne changent pas.
#[derive(Clone)]
pub struct Context {
    pub services: Arc<Services>,
    pub providers: Arc<dyn ProviderSource>,
    pub embeddings: Arc<penelope_vault::embeddings::State>,
}

impl Context {
    /// Calcul des embeddings, comme `Daemon::embedder`.
    pub fn embedder(&self) -> penelope_vault::embeddings::Embedder {
        penelope_vault::embeddings::Embedder {
            services: self.services.clone(),
            providers: self.providers.clone(),
            state: self.embeddings.clone(),
        }
    }

    /// Provider d'un modèle, comme `Daemon::provider_for`.
    pub async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        self.providers.provider_for(model_id).await
    }
}

// Modules du socle et du vault, sous les chemins que les fichiers déplacés du daemon
// nomment encore (`crate::helpers`…).
pub(crate) use penelope_app::{helpers, machine};
pub(crate) use penelope_vault::vault_ops;
