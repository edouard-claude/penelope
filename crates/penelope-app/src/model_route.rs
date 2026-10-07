//! Modèle d'un tour, et l'accès des `Services` aux écarts annoncés (#333, #335).

use crate::model_watch::{Place, Watch, fallback_key, short};
use crate::ports::ProviderSource;
use crate::services::Services;
use penelope_llm::Provider;
use std::sync::Arc;

/// Les écarts, vus des services.
pub fn watch(s: &Services) -> Watch<'_> {
    Watch {
        store: &s.store,
        events: &s.events,
        now_ms: s.clock.now_ms(),
    }
}

/// Le modèle d'un tour et son fournisseur, avec les replis qui restent.
pub struct Route {
    pub model_id: String,
    pub provider: Arc<dyn Provider>,
    pub fallbacks: Vec<String>,
}

/// Modèle, fournisseur et replis d'un tour de conversation.
///
/// Un modèle **épinglé** par le propriétaire (`/model`) n'a pas de repli : il échoue en
/// le disant plutôt que de basculer (#333). Sinon, la chaîne du profil actif ; et si le
/// fournisseur du modèle choisi manque (compte Codex déconnecté, endpoint local éteint),
/// le premier repli joignable sert, annoncé, au lieu d'un échec avant la chaîne (#335).
pub async fn route(
    s: &Services,
    providers: &dyn ProviderSource,
    session_id: &str,
    alias: &str,
    model_id: &str,
) -> Result<Route, String> {
    let pinned = crate::helpers::pinned_model(s, session_id).await.is_some();
    let cfg = s.config.config();
    let fallbacks: Vec<String> = if pinned {
        Vec::new()
    } else {
        cfg.fallback_labels(alias)
            .iter()
            .filter_map(|l| cfg.alias_model(l).map(String::from))
            .filter(|m| m != model_id)
            .collect()
    };
    let error = match providers.provider_for(model_id).await {
        Ok(provider) => {
            return Ok(Route {
                model_id: model_id.to_string(),
                provider,
                fallbacks,
            });
        }
        Err(e) => e,
    };
    for (i, next) in fallbacks.iter().enumerate() {
        if let Ok(provider) = providers.provider_for(next).await {
            let text = format!(
                "⚠️ repli sur `{}` : `{}` injoignable ({error})",
                short(next),
                short(model_id)
            );
            let place = Place {
                session: Some(session_id),
                origin: None,
            };
            watch(s)
                .deviate(&place, &fallback_key(session_id), next, &text)
                .await;
            return Ok(Route {
                model_id: next.clone(),
                provider,
                fallbacks: fallbacks[i + 1..].to_vec(),
            });
        }
    }
    Err(error)
}

#[cfg(test)]
mod tests;
