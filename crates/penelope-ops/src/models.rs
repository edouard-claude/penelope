//! Méthodes `model.*` (#334) : la vue des profils et des rôles, et leurs modifications,
//! sorties du daemon. Le daemon route l'appel et reconstruit les fournisseurs après une
//! modification ; tout le reste est ici, pour `/model` comme pour `penelope model`.
//!
//! ```text
//!  model.list ──► profils, table rôle → modèle effectif → raison, écarts des 24 h
//!  model.set ───► primary | routing.<étage> | capacité | rôle de voix | rôle | alias
//!  model.unset ─► retire une surcharge, une capacité, un étage ou un modèle de voix
//!  model.profile ► use | new | copy | rename | rm
//! ```

use penelope_app::services::Services;
use penelope_kernel::api::method;
use penelope_kernel::config::{Capability, Config, ROLES, VOICE_ROLES, role_spec};
use serde_json::{Value, json};

mod edit;
pub use edit::{profile, set, unset};

/// Vrai si la méthode modifie la configuration : le daemon reconstruit alors les
/// fournisseurs.
pub fn mutates(m: &str) -> bool {
    [
        method::MODEL_SET,
        method::MODEL_UNSET,
        method::MODEL_PROFILE,
    ]
    .contains(&m)
}

/// Aiguillage des méthodes `model.*` (sauf `model.auth`, qui reste au daemon).
pub async fn rpc(s: &Services, m: &str, p: &Value) -> anyhow::Result<Value> {
    match m {
        method::MODEL_LIST => Ok(list(s, filter(p)).await),
        method::MODEL_SET => set(s, p),
        method::MODEL_UNSET => unset(s, p),
        method::MODEL_PROFILE => profile(s, p),
        method::MODEL_ROUTE_TEST => {
            let text = str_of(p, "text")?;
            let cfg = s.config.config();
            let router = penelope_llm::Router::new(s.catalog.clone());
            let input = penelope_llm::RouteInput {
                message: text,
                ..Default::default()
            };
            let d = router
                .route_deterministic(&cfg, &input)
                .unwrap_or_else(|| router.default_decision(&cfg));
            Ok(serde_json::to_value(d)?)
        }
        other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
    }
}

pub(crate) fn str_of(p: &Value, key: &str) -> anyhow::Result<String> {
    p.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("paramètre `{key}` manquant"))
}

fn filter(p: &Value) -> Option<&str> {
    p.get("filter")
        .and_then(Value::as_str)
        .filter(|f| !f.trim().is_empty())
}

fn per_m(x: f64) -> f64 {
    (x * 1_000_000.0 * 100.0).round() / 100.0
}

/// Une ligne de « Tout voir » : le rôle, sa famille, le modèle effectif et sa raison ;
/// `guarded` dit où la garde Codex l'envoie quand elle s'applique.
pub fn role_rows(s: &Services, cfg: &Config) -> Vec<Value> {
    let mut names: Vec<String> = ROLES.iter().map(|r| r.name.to_string()).collect();
    // Une surcharge d'un rôle que ce binaire ne connaît pas (une étape `memoire`) se voit.
    for extra in cfg.models.active().overrides.keys() {
        if !names.contains(extra) {
            names.push(extra.clone());
        }
    }
    names
        .iter()
        .map(|name| {
            let r = cfg.resolve_role_with(name, &s.catalog);
            let spec = role_spec(name);
            let background = penelope_app::codex_scope::BACKGROUND_ROLES.contains(&name.as_str());
            let guarded = r
                .model
                .as_deref()
                .filter(|_| background)
                .and_then(|m| penelope_app::codex_scope::preview(cfg, m));
            let reason = if guarded.is_some() {
                penelope_kernel::config::ModelReason::CodexGuard
            } else {
                r.reason
            };
            json!({
                "role": name,
                "family": spec.map_or("background", |s| s.family.key()),
                "name": spec.map_or(name.as_str(), |s| s.label),
                "label": r.label,
                "model": r.model,
                "reason": reason,
                "icon": reason.icon(),
                "why": reason.label(),
                "guarded": guarded,
            })
        })
        .collect()
}

/// Les profils, l'actif en tête de sa ligne.
pub fn profile_rows(cfg: &Config) -> Vec<Value> {
    let m = &cfg.models;
    m.profile_names()
        .iter()
        .filter_map(|n| m.profile_named(n).map(|p| (n, p.into_owned())))
        .map(|(n, p)| {
            json!({
                "name": n,
                "active": n == m.active_name(),
                "derived": m.is_derived(n),
                "primary": p.primary,
                "model": cfg.alias_model(&p.primary),
                "codex_background": p.codex_background,
                "overrides": p.overrides.len(),
            })
        })
        .collect()
}

/// `model.list` : alias, profils, rôles, écarts, catalogue filtré, abonnement Codex.
pub async fn list(s: &Services, filter: Option<&str>) -> Value {
    let cfg = s.config.config();
    let aliases: Vec<Value> = cfg
        .models
        .aliases
        .iter()
        .map(|(alias, model)| {
            let info = s.catalog.get(model);
            let (window, source) = s.catalog.window(model);
            json!({
                "alias": alias,
                "model": model,
                "context": info.as_ref().map(|_| window),
                "context_source": source,
                "usd_per_m_in": info.as_ref().map(|i| per_m(i.price_prompt)),
                "usd_per_m_out": info.as_ref().map(|i| per_m(i.price_completion)),
                "known": if s.catalog.is_empty() { Value::Null } else { json!(info.is_some()) },
            })
        })
        .collect();
    let models: Vec<Value> = match filter {
        Some(f) => s
            .catalog
            .list(Some(f))
            .into_iter()
            .take(50)
            .map(|m| {
                json!({
                    "id": m.id,
                    "context": m.context_window,
                    "context_source": m.window_source,
                    "usd_per_m_in": per_m(m.price_prompt),
                    "usd_per_m_out": per_m(m.price_completion),
                    "tools": m.supports_tools(),
                })
            })
            .collect(),
        None => Vec::new(),
    };
    let tier = |t| {
        let label = cfg.routing_label(t);
        json!({"alias": label, "model": cfg.alias_model(&label).unwrap_or("?")})
    };
    use penelope_kernel::config::Tier;
    let primary = cfg.primary_label();
    let routing = json!({
        "classifier": cfg.models.routing.classifier,
        "adaptive": cfg.adaptive_routing(),
        "default": {"alias": primary, "model": cfg.alias_model(&primary).unwrap_or("?")},
        "low": tier(Tier::Low),
        "medium": tier(Tier::Medium),
        "high": tier(Tier::High),
        "classifier_model": cfg.role_model("classifier").unwrap_or_else(|| "?".into()),
        "fallback": cfg.models.active().fallback.clone(),
    });
    let codex = match crate::codex_auth::status(s).ok().flatten() {
        Some(st) => json!({
            "connected": st.connected,
            "plan": st.plan,
            "account": st.account,
            "disconnected": st.disconnected,
            "quota": crate::codex_quota::snapshot(s)
                .await
                .map(|q| crate::codex_quota::gauge_line(&q, s.clock.now_ms())),
        }),
        None => Value::Null,
    };
    let deviations = penelope_app::model_route::watch(s).recent().await;
    json!({
        "profile": cfg.models.active_name(),
        "profiles": profile_rows(&cfg),
        "primary": {"label": primary, "model": cfg.alias_model(&primary)},
        "codex_background": cfg.models.active().codex_background,
        "codex_risk": penelope_kernel::config::CODEX_RISK,
        "roles": role_rows(s, &cfg),
        "voice_roles": VOICE_ROLES,
        "capabilities": Capability::ALL.map(Capability::key),
        "deviations": deviations,
        "aliases": aliases,
        "routing": routing,
        "catalog_size": s.catalog.len(),
        "models": models,
        "codex": codex,
        "note": if s.catalog.is_empty() {
            "catalogue pas encore chargé : le daemon le télécharge au démarrage dès qu'une clé est posée"
        } else if filter.is_none() {
            "ajouter un filtre pour chercher dans le catalogue, par exemple `penelope model list --filter glm`"
        } else { "" },
    })
}

#[cfg(test)]
mod tests;
