//! Façade de `self_status` et `config_set` : le module vit dans `penelope-executor`
//! (épopée #208, T24). Reste ici ce que le daemon sert par `Admin` et que l'exécuteur ne
//! peut pas lire lui-même : l'état Codex, dans `penelope-ops`.

pub use penelope_executor::selfknow::*;

use crate::runtime::Services;
use serde_json::{Value, json};

/// État du fournisseur `codex` : compte, plan, jauges du plan (issue #142). Le coût en
/// dollars n'y veut rien dire — un abonnement ne facture pas l'appel —, c'est le quota
/// qui borne. Servi par `Admin::codex_view`.
pub async fn codex_view(s: &Services, cfg: &penelope_kernel::config::Config) -> Value {
    let status = crate::codex_auth::status(s).ok().flatten();
    let quota = crate::codex_quota::snapshot(s).await;
    json!({
        "enabled": cfg.providers.codex.enabled,
        "connected": status.as_ref().map(|st| st.connected).unwrap_or(false),
        "plan": status.as_ref().map(|st| st.plan.clone()),
        "account": status.as_ref().map(|st| st.account.clone()),
        "disconnected": status.as_ref().and_then(|st| st.disconnected.clone()),
        "quota": quota
            .as_ref()
            .map(|q| crate::codex_quota::gauge_line(q, s.clock.now_ms())),
        "note": "l'abonnement ne facture pas l'appel : la limite est le quota du plan, \
                 pas `budget.*`",
    })
}
