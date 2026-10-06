//! Rafraîchissement du catalogue des modèles, fournisseur par fournisseur (#324).
//!
//! ```text
//!   alias ──▶ endpoint (openrouter, codex, local, extra:<nom>) ──▶ un modèle témoin
//!                         │
//!                         └─▶ provider_for(témoin).fetch_models() ──▶ catalogue
//! ```
//!
//! Jusqu'à la 1.0.44, seul le fournisseur de `chat_default` était rafraîchi, plus Codex
//! si un alias le visait. `main` passé sur Codex, le catalogue d'OpenRouter n'était plus
//! jamais chargé : les modèles des autres alias n'avaient pas de fenêtre connue, et leur
//! seuil de compaction retombait sur le repli (128 000), quelle que soit leur fenêtre
//! réelle. Chaque endpoint visé par un alias a maintenant son échéance : la cadence de
//! `providers.openrouter.catalog_refresh` après un succès, une minute après un échec (une
//! clé posée pendant que le daemon tourne, un serveur local relancé).

use crate::ports::{Handle, ProviderSource};
use crate::services::Services;
use penelope_kernel::config::Config;
use penelope_llm::catalog::{provider_of, strip_provider};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Délai avant de retenter un endpoint dont le catalogue n'a pas pu être lu.
pub const RETRY: Duration = Duration::from_secs(60);

/// Cadence par défaut, si `providers.openrouter.catalog_refresh` ne se lit pas.
const DEFAULT_EVERY: Duration = Duration::from_secs(6 * 3600);

/// L'endpoint qui sert un modèle, tel que `ProviderSet::get` le choisit ; `None` si aucun
/// fournisseur actif ne le sert.
pub fn endpoint_of(cfg: &Config, model: &str) -> Option<String> {
    let p = &cfg.providers;
    match provider_of(model) {
        "codex" => p.codex.enabled.then(|| "codex".into()),
        "openrouter" if p.openrouter.enabled => Some("openrouter".into()),
        "openrouter" => p.local.enabled.then(|| "local".into()),
        _ => {
            let bare = strip_provider(model);
            p.extra
                .iter()
                .find(|(_, e)| e.enabled && e.models.iter().any(|m| m == bare))
                .map(|(name, _)| format!("extra:{name}"))
                .or_else(|| p.local.enabled.then(|| "local".into()))
        }
    }
}

/// Les endpoints à rafraîchir, chacun avec le premier modèle d'alias qui le vise : celui
/// de `chat_default` d'abord, puis les alias dans l'ordre (les rôles nomment des alias).
pub fn targets(cfg: &Config) -> BTreeMap<String, String> {
    let main = cfg.alias_model(&cfg.role_alias("chat_default"));
    let mut out = BTreeMap::new();
    for model in main
        .into_iter()
        .chain(cfg.models.aliases.values().map(String::as_str))
    {
        if let Some(endpoint) = endpoint_of(cfg, model) {
            out.entry(endpoint).or_insert_with(|| model.to_string());
        }
    }
    out
}

/// Échéances des endpoints, d'une passe à l'autre.
#[derive(Debug, Default)]
pub struct Refresher {
    due: BTreeMap<String, Instant>,
}

impl Refresher {
    /// Rafraîchit les endpoints échus ; rend le nombre d'endpoints lus avec succès.
    pub async fn pass(
        &mut self,
        cfg: &Config,
        providers: &dyn ProviderSource,
        now: Instant,
    ) -> usize {
        let every =
            penelope_kernel::config::parse_duration(&cfg.providers.openrouter.catalog_refresh)
                .unwrap_or(DEFAULT_EVERY);
        let targets = targets(cfg);
        // Un endpoint retiré de la configuration n'a plus d'échéance.
        self.due.retain(|k, _| targets.contains_key(k));
        let mut fresh = 0;
        for (endpoint, model) in &targets {
            if self.due.get(endpoint).is_some_and(|t| *t > now) {
                continue;
            }
            let ok = match providers.provider_for(model).await {
                Ok(p) => match p.fetch_models().await {
                    Ok(models) => {
                        tracing::info!(%endpoint, n = models.len(), "catalogue de modèles à jour");
                        true
                    }
                    Err(e) => {
                        tracing::warn!(%endpoint, error = %e, "catalogue de modèles indisponible");
                        false
                    }
                },
                Err(e) => {
                    tracing::info!(%endpoint, error = %e, "catalogue en attente d'une clé");
                    false
                }
            };
            fresh += usize::from(ok);
            self.due
                .insert(endpoint.clone(), now + if ok { every } else { RETRY });
        }
        fresh
    }
}

/// Boucle de fond : une passe par minute, chaque endpoint à son échéance, jusqu'à l'arrêt.
pub async fn run(s: &Services, providers: &dyn ProviderSource, handle: &Handle) {
    let mut refresher = Refresher::default();
    while !handle.is_shutting_down() {
        refresher
            .pass(&s.config.config(), providers, Instant::now())
            .await;
        // Par petites tranches, pour réagir vite à l'arrêt.
        let step = Duration::from_millis(500);
        let mut waited = Duration::ZERO;
        while waited < RETRY && !handle.is_shutting_down() {
            tokio::time::sleep(step).await;
            waited += step;
        }
    }
}

#[cfg(test)]
mod tests;
