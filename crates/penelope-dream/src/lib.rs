//! `penelope-dream` : la mémoire qui mûrit (épopée #208, T26).
//!
//! Entretien d'accueil (`onboarding`) ; la consolidation nocturne (`dream/`) et
//! l'ingestion de documents (`ingest`) y descendent quand leurs signatures ne citent plus
//! `Daemon`. Au-dessus de `penelope-app` et de `penelope-vault`, sous le daemon, qui
//! réexporte ces modules sous leurs anciens chemins jusqu'à T30.

#![forbid(unsafe_code)]

pub mod ingest;
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

/// Ce que le digest du matin lit au-dessus du rêve, transmis en données : l'orchestrateur
/// (planifications) et le compactage ne sont pas des dépendances de la crate.
#[derive(Debug, Clone, Default)]
pub struct DigestInputs {
    /// Planifications actives en échec, une ligne `- libellé : erreur` chacune.
    pub failing_schedules: Vec<String>,
    /// Sessions dont le résumé échoue : (titre, échecs en 24 h, coût par tour).
    pub struggling_sessions: Vec<(String, u32, Option<f64>)>,
    /// Ce qui part aujourd'hui, une ligne chacun.
    pub due_today: Vec<String>,
}

/// Qui fournit au digest ses entrées d'au-dessus du rêve, au moment de l'écrire : les
/// crons système (`system_crons`) le déclenchent sans connaître l'orchestrateur ni le
/// compactage. Le daemon l'implémente (`dream::DigestFeed`).
#[async_trait::async_trait]
pub trait DigestSource: Send + Sync {
    async fn digest_inputs(&self) -> DigestInputs;
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
pub(crate) use penelope_app::{helpers, machine, media, ports};
pub(crate) use penelope_vault::{concepts, vault_ops};
