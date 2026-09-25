//! Ce que `self_status` lit du daemon : le module vit dans `penelope-executor` (épopée
//! #208, T24). Reste ici ce que le daemon sert par `Admin` et que l'exécuteur ne peut pas
//! lire lui-même : l'état Codex, dans `penelope-ops`.

use penelope_app::services::Services;
use serde_json::{Value, json};

/// État du fournisseur `codex` : compte, plan, jauges du plan (issue #142). Le coût en
/// dollars n'y veut rien dire — un abonnement ne facture pas l'appel —, c'est le quota
/// qui borne. Servi par `Admin::codex_view`.
pub async fn codex_view(s: &Services, cfg: &penelope_kernel::config::Config) -> Value {
    let status = penelope_ops::codex_auth::status(s).ok().flatten();
    let quota = penelope_ops::codex_quota::snapshot(s).await;
    json!({
        "enabled": cfg.providers.codex.enabled,
        "connected": status.as_ref().map(|st| st.connected).unwrap_or(false),
        "plan": status.as_ref().map(|st| st.plan.clone()),
        "account": status.as_ref().map(|st| st.account.clone()),
        "disconnected": status.as_ref().and_then(|st| st.disconnected.clone()),
        "quota": quota
            .as_ref()
            .map(|q| penelope_ops::codex_quota::gauge_line(q, s.clock.now_ms())),
        "note": "l'abonnement ne facture pas l'appel : la limite est le quota du plan, \
                 pas `budget.*`",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Daemon;
    use penelope_executor::selfknow::{Admin, status};
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    /// T24 : servis par `Admin` depuis que `self_status` vit dans l'exécuteur, la mémoire
    /// du processus, l'état Codex et la taille du contexte gardent les valeurs que le
    /// daemon calculait lui-même avant la sortie.
    #[tokio::test]
    async fn the_daemon_admin_serves_what_self_status_read_directly() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Daemon::from_services(s.clone());
        let v = status(&s, "s1", None, Some(&d as &dyn Admin), "all")
            .await
            .unwrap();
        let rss = v["penelope"]["rss_mb"].as_f64().expect("rss_mb chiffré");
        assert!(rss > 0.0, "{v}");
        assert_eq!(
            v["config"]["providers"]["codex"],
            codex_view(&s, &s.config.config()).await
        );
        assert_eq!(v["config"]["providers"]["codex"]["enabled"], json!(false));
        assert_eq!(
            v["costs"]["context"],
            penelope_conversation::compaction::context_view(&s, "s1", None)
                .await
                .unwrap()
        );
        assert!(v["costs"]["context"].is_object(), "{v}");
    }
}
