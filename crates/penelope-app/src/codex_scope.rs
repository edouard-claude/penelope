//! Périmètre du fournisseur `codex` (décision 2 de l'issue #142).
//!
//! OpenAI tolère « un compte, un humain, un usage interactif », et traque la conversion
//! d'un abonnement en trafic automatisé. L'abonnement ne sert donc que les tours **ouverts
//! par le propriétaire** — un message Telegram ou CLI, et les sous-agents de ce tour.
//!
//! Tout ce qui tourne sans lui — planification à cible `prompt`, rêve nocturne, veille,
//! résumeur de compaction, relecture d'épisode, consolidation, classifieur, embeddings,
//! transcription, synthèse vocale, titre automatique, run de workflow — se replie sur le
//! modèle OpenRouter de l'alias, sans carte ni bruit, en laissant un événement.
//!
//! La garde est **unique** et se pose juste avant le choix du fournisseur : un travail de
//! fond nomme ce qu'il est, et reçoit en retour le modèle qu'il a le droit d'appeler.

use crate::services::Services;
use penelope_kernel::config::Config;
use penelope_kernel::event::EventDraft;
use serde_json::json;

/// Rôles qui tournent sans le propriétaire : leur alias ne doit jamais viser `codex:`.
///
/// Les autres rôles (`chat_default`, `code`, `image_*`) servent **dans** un tour du
/// propriétaire : ils restent autorisés.
pub const BACKGROUND_ROLES: &[&str] = &[
    "classifier",
    "compaction",
    "memory_review",
    "embedding",
    "stt",
    "tts",
];

/// Vrai si ce modèle passe par l'abonnement ChatGPT.
pub fn is_codex(model_id: &str) -> bool {
    penelope_llm::catalog::provider_of(model_id) == "codex"
}

/// Modèle effectivement appelable par un travail de fond. `work` nomme le travail, pour
/// la trace : `rêve`, `compaction`, `classifieur`, `workflow`…
///
/// Rend le modèle tel quel quand il ne vise pas l'abonnement — le cas courant, sans
/// aucun coût.
pub async fn background(s: &Services, model_id: &str, work: &str) -> String {
    if !is_codex(model_id) {
        return model_id.to_string();
    }
    let cfg = s.config.config();
    let replaced = replacement(&cfg, model_id);
    let _ = s
        .events
        .append(EventDraft::new(
            "llm.codex_scope_fallback",
            json!({
                "work": work,
                "asked": model_id,
                "served": replaced.clone().unwrap_or_else(|| model_id.to_string()),
                "replaced": replaced.is_some(),
            }),
        ))
        .await;
    match replaced {
        Some(m) => m,
        None => {
            // Aucun modèle hors abonnement dans toute la configuration : `model set`
            // refuse d'en arriver là, et `doctor` le signale. Plutôt que d'arrêter le
            // travail de fond, on le laisse passer, tracé.
            tracing::warn!(
                work,
                model = model_id,
                "aucun repli hors abonnement : le travail de fond reste sur `codex`"
            );
            model_id.to_string()
        }
    }
}

/// Modèle appelable pour un tour, d'après son origine : un message du propriétaire
/// (Telegram, CLI) garde l'abonnement, tout autre tour — planification à cible `prompt`,
/// veille, travail interne — se replie.
pub async fn for_origin(s: &Services, model_id: &str, origin: &crate::bus::Origin) -> String {
    match origin {
        crate::bus::Origin::Telegram { .. } | crate::bus::Origin::Cli => model_id.to_string(),
        crate::bus::Origin::Internal { source } => background(s, model_id, source).await,
    }
}

/// Modèle de repli d'un modèle `codex:` : la chaîne `models.routing.fallback` de son
/// alias d'abord, le modèle de conversation ensuite, rien enfin.
fn replacement(cfg: &Config, model_id: &str) -> Option<String> {
    let alias = cfg
        .models
        .aliases
        .iter()
        .find(|(_, m)| m.as_str() == model_id)
        .map(|(a, _)| a.clone());
    if let Some(alias) = &alias {
        for to in cfg.models.routing.fallback.get(alias).into_iter().flatten() {
            if let Some(m) = cfg.alias_model(to).filter(|m| !is_codex(m)) {
                return Some(m.to_string());
            }
        }
    }
    cfg.alias_model(&cfg.role_alias("chat_default"))
        .filter(|m| !is_codex(m))
        .map(String::from)
}

/// Rôles de fond servis par cet alias, s'il y en a : `model set` refuse de leur donner
/// l'abonnement, et `doctor` signale ceux qui l'auraient contourné.
pub fn background_roles_of(cfg: &Config, alias: &str) -> Vec<String> {
    cfg.models
        .roles
        .iter()
        .filter(|(role, a)| a.as_str() == alias && BACKGROUND_ROLES.contains(&role.as_str()))
        .map(|(role, _)| role.clone())
        .collect()
}

/// Pourquoi un alias de rôle de fond ne peut pas viser l'abonnement.
pub fn refusal(alias: &str, model: &str, roles: &[String]) -> String {
    format!(
        "`{model}` passe par l'abonnement ChatGPT, qui ne sert que les tours ouverts par \
         le propriétaire (issue #142) ; l'alias `{alias}` sert {} ({}), qui tourne sans \
         lui. Lui donner un modèle OpenRouter, ou choisir un autre alias.",
        if roles.len() > 1 {
            "des rôles de fond"
        } else {
            "un rôle de fond"
        },
        roles.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut c = Config::sample(1);
        c.models
            .aliases
            .insert("code".into(), "codex:gpt-6-astra".into());
        c.models.aliases.insert(
            "fast".into(),
            "openrouter:deepseek/deepseek-v4-flash".into(),
        );
        c
    }

    /// #142 : un travail de fond qui vise l'abonnement retombe sur la chaîne de repli de
    /// son alias, sinon sur le modèle de conversation.
    #[test]
    fn a_background_model_falls_back_outside_the_subscription() {
        let mut c = cfg();
        c.models
            .routing
            .fallback
            .insert("code".into(), vec!["fast".into()]);
        assert_eq!(
            replacement(&c, "codex:gpt-6-astra").as_deref(),
            Some("openrouter:deepseek/deepseek-v4-flash"),
            "la chaîne de repli de l'alias d'abord"
        );

        // Sans chaîne de repli : le modèle de conversation.
        let c = cfg();
        assert_eq!(
            replacement(&c, "codex:gpt-6-astra").as_deref(),
            Some("openrouter:deepseek/deepseek-v4-pro")
        );

        // Tout sur l'abonnement : plus rien à proposer.
        let mut c = cfg();
        for alias in ["main", "fast", "summarizer"] {
            c.models
                .aliases
                .insert(alias.into(), "codex:gpt-6-astra".into());
        }
        assert!(replacement(&c, "codex:gpt-6-astra").is_none());
    }

    /// #142 : les rôles qui tournent sans le propriétaire sont nommés, les autres non —
    /// `code` et `image_describe` servent dans son tour.
    #[test]
    fn background_roles_are_named() {
        let c = cfg();
        assert_eq!(
            background_roles_of(&c, "fast"),
            ["classifier", "memory_review"]
        );
        assert_eq!(background_roles_of(&c, "summarizer"), ["compaction"]);
        assert!(
            background_roles_of(&c, "main").is_empty(),
            "la conversation"
        );
        assert!(background_roles_of(&c, "reasoning").is_empty(), "`code`");
        assert!(background_roles_of(&c, "vision").is_empty(), "les images");
        let r = refusal("fast", "codex:gpt-6-astra", &["classifier".into()]);
        assert!(r.contains("classifier") && r.contains("abonnement"), "{r}");
    }
}
