//! Méthodes de configuration, de secrets et de modèles.

use super::*;
use crate::helpers::set_config_path;

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

    /// Alias, catalogue et routage des modèles.
    pub(super) async fn models(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::MODEL_LIST => {
                // D'abord ce que l'utilisateur a configuré, ensuite le catalogue du provider.
                let filter = p
                    .get("filter")
                    .and_then(|f| f.as_str())
                    .filter(|f| !f.trim().is_empty());
                let cfg = s.config.config();
                let per_m = |x: f64| (x * 1_000_000.0 * 100.0).round() / 100.0;
                let aliases: Vec<Value> = cfg
                    .models
                    .aliases
                    .iter()
                    .map(|(alias, model)| {
                        let info = s.catalog.get(penelope_llm::catalog::strip_provider(model));
                        json!({
                            "alias": alias,
                            "model": model,
                            "context": info.as_ref().map(|i| i.context_window),
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
                                "usd_per_m_in": per_m(m.price_prompt),
                                "usd_per_m_out": per_m(m.price_completion),
                                "tools": m.supports_tools(),
                            })
                        })
                        .collect(),
                    None => Vec::new(),
                };
                let routing = &cfg.models.routing;
                let model_of = |alias: &str| cfg.alias_model(alias).unwrap_or("?").to_string();
                let routing_view = json!({
                    "classifier": routing.classifier,
                    "default": {
                        "alias": cfg.role_alias("chat_default"),
                        "model": model_of(&cfg.role_alias("chat_default")),
                    },
                    "low": {"alias": routing.low, "model": model_of(&routing.low)},
                    "medium": {"alias": routing.medium, "model": model_of(&routing.medium)},
                    "high": {"alias": routing.high, "model": model_of(&routing.high)},
                    "classifier_model": model_of(&cfg.role_alias("classifier")),
                    "fallback": routing.fallback,
                });
                // Abonnement ChatGPT : plan, compte et jauge, là où on regarde les
                // modèles — son coût en dollars est nul par construction (#142).
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
                Ok(json!({
                    "aliases": aliases,
                    "routing": routing_view,
                    "catalog_size": s.catalog.len(),
                    "models": models,
                    "codex": codex,
                    "note": if s.catalog.is_empty() {
                        "catalogue pas encore chargé : le daemon le télécharge au démarrage dès qu'une clé est posée"
                    } else if filter.is_none() {
                        "ajouter un filtre pour chercher dans le catalogue, par exemple `penelope model list --filter glm`"
                    } else { "" },
                }))
            }
            method::MODEL_SET => {
                let alias = required_str(p, "alias")?;
                let model = required_str(p, "model")?;
                // Un identifiant mal écrit (`codx:`) partait en silence chez OpenRouter
                // (#142) : il est refusé ici comme à la validation de configuration.
                penelope_kernel::config::check_model_id(&model).map_err(anyhow::Error::msg)?;
                // L'abonnement ChatGPT ne sert que les tours du propriétaire (décision 2
                // de #142) : un alias de rôle de fond ne peut pas le viser.
                let background =
                    crate::codex_scope::background_roles_of(&s.config.config(), &alias);
                if crate::codex_scope::is_codex(&model) && !background.is_empty() {
                    anyhow::bail!(crate::codex_scope::refusal(&alias, &model, &background));
                }
                // Un alias qui sert un rôle à outils doit viser un modèle qui en appelle :
                // l'émulation n'existe plus (issue #54, décision 0009).
                let bare_new = penelope_llm::catalog::strip_provider(&model);
                if s.catalog.get(bare_new).map(|i| i.supports_tools()) == Some(false)
                    && crate::doctor::alias_needs_tools(&s.config.config(), &alias)
                {
                    anyhow::bail!(
                        "`{model}` n'appelle pas d'outils : l'alias `{alias}` sert un rôle qui \
                         en a besoin. Choisir un modèle avec tool calling, ou donner ce \
                         modèle à un alias sans outils."
                    );
                }
                // Un rôle d'extraction structurée (consolidation, relecture d'épisode)
                // donné à un modèle qui ne sait pas couper son raisonnement dépense son
                // budget de sortie à réfléchir et ne rend rien (issue #152) : c'est un
                // avertissement, pas un refus — le propriétaire peut avoir ses raisons.
                let reasoning_note = s
                    .catalog
                    .get(bare_new)
                    .filter(|_| crate::doctor::alias_serves_extraction(&s.config.config(), &alias))
                    .and_then(|i| {
                        (i.lightest_effort().as_deref() != Some("none")).then(|| {
                            format!(
                                "`{model}` impose un raisonnement (effort le plus faible : {}) \
                                 et l'alias `{alias}` sert la consolidation : son budget de \
                                 sortie partira en réflexion, et la passe nocturne peut ne \
                                 rien rendre",
                                i.lightest_effort().unwrap_or_else(|| "?".into())
                            )
                        })
                    });
                let g = self.daemon.publish_config("cli", |c| {
                    c.models.aliases.insert(alias.clone(), model.clone());
                    Ok(vec![format!("models.aliases.{alias}")])
                })?;
                self.daemon.invalidate_providers().await;
                // Un catalogue chargé permet de prévenir d'une faute de frappe.
                let bare = penelope_llm::catalog::strip_provider(&model);
                let known = s.catalog.is_empty() || s.catalog.get(bare).is_some();
                let mut out = json!({"generation": g, "alias": alias, "model": model,
                                     "known": known});
                // La clé n'apparaît que s'il y a quelque chose à dire : le rendu générique
                // du CLI imprime toutes les clés, un `null` ferait une ligne vide.
                if let Some(w) = reasoning_note
                    && let Some(o) = out.as_object_mut()
                {
                    o.insert("avertissement".into(), json!(w));
                }
                Ok(out)
            }
            method::MODEL_ROUTE_TEST => {
                let text = required_str(p, "text")?;
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
}
