//! Méthodes de configuration, de secrets et de modèles.

use super::*;
use penelope_app::helpers::set_config_path;

impl Rpc {
    /// Configuration et secrets.
    pub(super) async fn config(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::CONFIG_GET => Ok(serde_json::to_value(&*s.config.config())?),
            method::CONFIG_STATUS => Ok(json!({
                "generation": s.config.generation(),
                "path": s.config.path(),
                "subsystems": s.config.apply_results(),
            })),
            method::CONFIG_RELOAD => {
                let g = s.config.reload_from_disk()?;
                // Un fichier édité à la main vaut un `config set` : les fournisseurs se
                // reconstruisent avec `[providers.*]` relu (#313).
                self.daemon.invalidate_providers().await;
                Ok(json!({"generation": g.generation, "changed": g.changed}))
            }
            method::CONFIG_SET => {
                let path = required_str(p, "path")?;
                let value = p.get("value").cloned().unwrap_or(Value::Null);
                let g = set_config_path(&self.daemon.services, &path, value)?;
                self.daemon.invalidate_providers().await;
                Ok(json!({"generation": g, "warnings": config_warnings(&self.daemon, &path)}))
            }
            method::SECRET_LIST => Ok(json!(s.platform.secrets.list()?)),
            method::SECRET_BACKEND => Ok(json!({"backend": s.platform.secrets.backend()})),
            method::SECRET_RM => {
                s.platform.secrets.delete(&required_str(p, "name")?)?;
                self.daemon.invalidate_providers().await;
                Ok(json!({"ok": true}))
            }
            method::SECRET_SET => {
                // La valeur ne transite que par la socket locale (0600), jamais par un canal
                // de conversation.
                let name = required_str(p, "name")?;
                penelope_platform::validate_secret_name(&name)?;
                let value = required_str(p, "value")?;
                s.platform.secrets.set(&name, value.trim())?;
                self.daemon.invalidate_providers().await;
                Ok(json!({"name": name, "stored": true}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }

    /// Profils, rôles, catalogue et routage des modèles (`penelope_ops::models`, #334) ;
    /// une modification reconstruit les fournisseurs.
    pub(super) async fn models(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let out = penelope_ops::models::rpc(self.services(), method, p).await?;
        if penelope_ops::models::mutates(method) {
            self.daemon.invalidate_providers().await;
        }
        Ok(out)
    }
}
