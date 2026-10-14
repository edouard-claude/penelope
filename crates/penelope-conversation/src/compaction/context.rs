//! Ce que la compaction lit du daemon, sans le daemon (épopée #208, T23).

use super::{Trigger, compact};
use penelope_app::bus::Bus;
use penelope_app::ports::ProviderSource;
use penelope_app::services::Services;
use std::sync::Arc;

/// Les services, les providers de modèles, le bus des tours (un résumé prêt attend la
/// fin du tour actif) et l'état des compactions. Le daemon le construit
/// (`compaction::context_of`) ; les champs et méthodes portent les noms de `Daemon`,
/// pour que les corps de la compaction ne changent pas.
#[derive(Clone)]
pub struct Context {
    pub services: Arc<Services>,
    pub providers: Arc<dyn ProviderSource>,
    pub bus: Arc<Bus>,
    pub compaction: Arc<super::State>,
}

impl Context {
    /// Provider d'un modèle, comme `Daemon::provider_for`.
    pub async fn provider_for(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn penelope_llm::Provider>, String> {
        self.providers.provider_for(model_id).await
    }

    /// Alias épinglé sur une session, comme `Daemon::pinned_model`.
    pub async fn pinned_model(&self, session_id: &str) -> Option<penelope_llm::StickyModel> {
        penelope_app::helpers::pinned_model(&self.services, session_id).await
    }
}

/// Compaction sur dépassement de fenêtre, offerte à la conversation d'un tour.
pub struct OverflowCompactor {
    pub context: Context,
    pub turn_id: Option<String>,
}

#[async_trait::async_trait]
impl penelope_app::conversation::Compactor for OverflowCompactor {
    async fn compact_now(&self, session_id: &str) -> anyhow::Result<bool> {
        let r = compact(
            &self.context,
            session_id,
            Trigger::Overflow,
            self.turn_id.as_deref(),
        )
        .await?;
        Ok(r.published > 0)
    }
}

/// Les ports d'une mission (#364) : le tour suivant en file, la compaction entre deux tours
/// (#365), le compte rendu sur le canal de la mission.
pub struct MissionPorts {
    pub context: Context,
    pub intake: Arc<dyn penelope_app::engine::TurnIntake>,
    pub messenger: Option<Arc<dyn penelope_app::ports::Messenger>>,
}

#[async_trait::async_trait]
impl penelope_app::mission::MissionPorts for MissionPorts {
    async fn enqueue(
        &self,
        session_id: &str,
        text: &str,
        origin: &penelope_app::bus::Origin,
        dedup: String,
    ) {
        if let Err(e) = self
            .intake
            .enqueue_message(session_id, text, origin, Some(dedup))
            .await
        {
            tracing::error!(error = %e, session = session_id, "relance de mission non mise en file");
        }
    }

    async fn compact(&self, session_id: &str) {
        // Le seuil de la compaction de fond décide : sous le seuil, rien n'est résumé. Le
        // résumé est publié au début du tour suivant (`publish_pending`).
        if let Err(e) = compact(&self.context, session_id, Trigger::Background, None).await {
            tracing::warn!(error = %e, session = session_id, "compaction de mission");
        }
    }

    async fn notify(&self, origin: &penelope_app::bus::Origin, text: &str) {
        if !origin.is_channel() {
            return;
        }
        if let Some(m) = &self.messenger
            && let Err(e) = m.send_text(origin, text).await
        {
            tracing::warn!(error = %e, "compte rendu de mission non envoyé");
        }
    }
}
