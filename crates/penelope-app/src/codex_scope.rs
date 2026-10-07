//! Périmètre du fournisseur `codex` (décision 2 de l'issue #142), devenu un réglage du
//! profil en #333 (décision 0021).
//!
//! OpenAI tolère « un compte, un humain, un usage interactif », et traque la conversion
//! d'un abonnement en trafic automatisé. Avec `codex_background = "deny"` (la valeur de
//! toute configuration migrée), l'abonnement ne sert que les tours **ouverts par le
//! propriétaire** : un message Telegram ou CLI, les sous-agents de ce tour, un run de
//! workflow qu'il lance lui-même (gate « vas-y », `/run`, CLI).
//!
//! Tout ce qui tourne sans lui (planification, rêve nocturne, veille, résumeur de
//! compaction, relecture d'épisode, consolidation, classifieur, embeddings, transcription,
//! synthèse vocale, titre automatique, run de workflow planifié) passe sur le modèle de
//! repli, **en le disant** une fois dans la conversation (`model_watch`). Avec `allow`,
//! tout passe par l'abonnement, et le risque est écrit une fois, dans `/model` et la doc.
//!
//! La garde est **unique** et se pose juste avant le choix du fournisseur : un travail de
//! fond nomme ce qu'il est, et reçoit en retour le modèle qu'il a le droit d'appeler.

use crate::bus::Origin;
use crate::model_route::watch;
use crate::model_watch::{Place, short};
use crate::services::Services;
use penelope_kernel::config::Config;
use penelope_kernel::event::EventDraft;
use serde_json::json;

/// Rôles qui tournent sans le propriétaire : sous la garde, ils ne visent pas `codex:`.
///
/// Les autres rôles (`chat_default`, `code`, `workflow`, `image_*`) servent **dans** un
/// tour du propriétaire ; un run planifié est gardé par son origine, pas par son rôle.
pub const BACKGROUND_ROLES: &[&str] = &[
    "classifier",
    "title",
    "dream",
    "compaction",
    "memory_review",
    "approval_judge",
    "embedding",
    "stt",
    "tts",
];

/// Vrai si ce modèle passe par l'abonnement ChatGPT.
pub fn is_codex(model_id: &str) -> bool {
    penelope_llm::catalog::provider_of(model_id) == "codex"
}

/// Modèle effectivement appelable par un travail de fond. `work` nomme le travail, pour
/// la trace et l'annonce : `rêve`, `compaction`, `classifieur`, `workflow`…
///
/// Rend le modèle tel quel quand il ne vise pas l'abonnement : le cas courant, sans
/// aucun coût.
pub async fn background(s: &Services, model_id: &str, work: &str) -> String {
    guarded(s, model_id, work, "travail de fond", None).await
}

/// [`background`], en disant pourquoi le travail est gardé et où l'annoncer (`None` : le
/// foyer du propriétaire).
pub async fn guarded(
    s: &Services,
    model_id: &str,
    work: &str,
    why: &str,
    origin: Option<&Origin>,
) -> String {
    if !is_codex(model_id) {
        return model_id.to_string();
    }
    let cfg = s.config.config();
    let (watch, key) = (watch(s), format!("guard.{work}"));
    let place = Place {
        session: None,
        origin,
    };
    if cfg.codex_background_allowed() {
        let text = format!(
            "✅ `{work}` repasse par `{}` : garde Codex levée",
            short(model_id)
        );
        watch.settle(&place, &key, model_id, &text).await;
        return model_id.to_string();
    }
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
        Some(m) => {
            let text = format!("⚠️ `{work}` part sur `{}` : garde Codex ({why})", short(&m));
            watch.deviate(&place, &key, &m, &text).await;
            m
        }
        None => {
            // Aucun modèle hors abonnement dans le profil : `model set` refuse d'en
            // arriver là, et `doctor` le signale. Plutôt que d'arrêter le travail de
            // fond, on le laisse passer, tracé.
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
/// (canal, CLI) garde l'abonnement, tout autre tour (planification à cible `prompt`,
/// veille, travail interne) passe par la garde.
pub async fn for_origin(s: &Services, model_id: &str, origin: &Origin) -> String {
    match origin {
        Origin::Internal { source } => guarded(s, model_id, source, "tâche planifiée", None).await,
        _ => model_id.to_string(),
    }
}

/// Ce que la garde ferait d'un travail de fond sur ce modèle, sans rien appeler ni
/// annoncer : `Some(repli)` quand elle s'applique (`/model`, « Tout voir »).
pub fn preview(cfg: &Config, model_id: &str) -> Option<String> {
    (is_codex(model_id) && !cfg.codex_background_allowed())
        .then(|| replacement(cfg, model_id))
        .flatten()
}

/// Modèle de repli d'un modèle `codex:` : la chaîne du profil actif pour son alias
/// d'abord, le principal ensuite, rien enfin.
fn replacement(cfg: &Config, model_id: &str) -> Option<String> {
    let alias = cfg
        .models
        .aliases
        .iter()
        .find(|(_, m)| m.as_str() == model_id)
        .map(|(a, _)| a.clone())
        .unwrap_or_else(|| model_id.to_string());
    for to in cfg.fallback_labels(&alias) {
        if let Some(m) = cfg.alias_model(&to).filter(|m| !is_codex(m)) {
            return Some(m.to_string());
        }
    }
    cfg.alias_model(&cfg.primary_label())
        .filter(|m| !is_codex(m))
        .map(String::from)
}

/// Rôles de fond servis par cet alias dans le profil actif, s'il y en a : sous la garde,
/// `model set` refuse de leur donner l'abonnement, et `doctor` signale ceux qui
/// l'auraient contourné. Garde levée : aucun.
pub fn background_roles_of(cfg: &Config, alias: &str) -> Vec<String> {
    if cfg.codex_background_allowed() {
        return Vec::new();
    }
    BACKGROUND_ROLES
        .iter()
        .filter(|role| cfg.role_alias(role) == alias)
        .map(|role| role.to_string())
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
            ["classifier", "title", "memory_review", "approval_judge"]
        );
        assert_eq!(
            background_roles_of(&c, "summarizer"),
            ["dream", "compaction"]
        );
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
