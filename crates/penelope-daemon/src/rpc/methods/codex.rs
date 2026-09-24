//! Connexion d'un fournisseur à compte : aujourd'hui Codex (issue #142).

use super::*;

impl Rpc {
    /// Connexion d'un fournisseur à compte (`model.auth`).
    pub(super) async fn codex(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            // Connexion d'un fournisseur à compte : aujourd'hui Codex (issue #142). Ni le
            // code d'appareil ni les jetons ne passent par une carte d'approbation ou par
            // `tg_outbox` (#134) : ils repartent au canal qui a demandé, et rien d'autre.
            method::MODEL_AUTH => {
                let provider = p
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("codex")
                    .to_string();
                if provider != "codex" {
                    anyhow::bail!(
                        "`{provider}` ne se connecte pas par compte : seul `codex` le fait"
                    );
                }
                let action = p
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("start")
                    .to_string();
                match action.as_str() {
                    "status" => Ok(json!({
                        "provider": provider,
                        "status": crate::codex_auth::status(s).map_err(anyhow::Error::msg)?,
                        "enabled": s.config.config().providers.codex.enabled,
                    })),
                    "logout" => {
                        crate::codex_auth::logout(s)
                            .await
                            .map_err(anyhow::Error::msg)?;
                        // Le fournisseur s'éteint avec le compte : un alias `codex:` se
                        // replie au lieu d'échouer à chaque tour.
                        let g = self.daemon.publish_config("cli", |c| {
                            c.providers.codex.enabled = false;
                            Ok(vec!["providers.codex.enabled".into()])
                        })?;
                        self.daemon.invalidate_providers().await;
                        Ok(json!({"provider": provider, "connected": false, "generation": g}))
                    }
                    "start" => {
                        // Un seul compte à la fois : un second exigerait une rotation de
                        // jetons que rien ne surveille, et OpenAI traque exactement ça.
                        if let Some(st) =
                            crate::codex_auth::status(s).map_err(anyhow::Error::msg)?
                            && st.connected
                        {
                            anyhow::bail!(
                                "déjà connecté au compte {} (plan {}) : se déconnecter \
                                 d'abord avec `penelope model auth codex --logout`",
                                st.account,
                                st.plan
                            );
                        }
                        let login = crate::codex_auth::start_pending(s)
                            .await
                            .map_err(anyhow::Error::msg)?;
                        Ok(json!({
                            "provider": provider,
                            "user_code": login.user_code,
                            "url": login.verification_url,
                            "expires_at_ms": login.expires_at_ms,
                        }))
                    }
                    "wait" => {
                        let grant = crate::codex_auth::wait_pending(s)
                            .await
                            .map_err(anyhow::Error::msg)?;
                        let g = self.daemon.publish_config("cli", |c| {
                            c.providers.codex.enabled = true;
                            Ok(vec!["providers.codex.enabled".into()])
                        })?;
                        self.daemon.invalidate_providers().await;
                        Ok(json!({
                            "provider": provider,
                            "connected": true,
                            "plan": grant.plan_type,
                            "account": if grant.email.is_empty() { grant.account_id } else { grant.email },
                            "generation": g,
                        }))
                    }
                    other => anyhow::bail!(
                        "action `{other}` inconnue : `start`, `wait`, `status` ou `logout`"
                    ),
                }
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}
