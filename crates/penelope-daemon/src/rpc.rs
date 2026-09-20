//! Serveur RPC local (§2.7, §15) : JSON-RPC 2.0 en NDJSON sur socket de domaine.

use crate::agent::TurnOutcome;
use crate::bus::{BusKind, Origin};
use crate::runtime::{Daemon, Services};
use penelope_kernel::api::*;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Délai maximal d'attente d'une réponse par `chat.send`.
const CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// Routeur : une méthode, des paramètres, une valeur.
pub struct Rpc {
    pub daemon: Arc<Daemon>,
}

impl Rpc {
    pub fn new(daemon: Arc<Daemon>) -> Self {
        Rpc { daemon }
    }

    fn services(&self) -> &Services {
        &self.daemon.services
    }

    /// Traite une requête. Une méthode inconnue renvoie `-32601`, jamais une panique.
    pub async fn handle(&self, req: RpcRequest) -> RpcResponse {
        let id = req.id.clone();
        let params = req.params.clone().unwrap_or(json!({}));
        match Box::pin(self.dispatch(&req.method, &params)).await {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => {
                let code = classify(&e);
                RpcResponse::err(id, code, e.to_string())
            }
        }
    }

    /// Appel en processus, pour les canaux qui vivent dans le daemon (Telegram).
    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        Box::pin(self.dispatch(method, &params)).await
    }

    /// Le futur du répartiteur porte l'état de **toutes** les méthodes servies : construit
    /// sur la pile de l'appelant, il la faisait déborder au test dès qu'une branche
    /// s'ajoutait (issues #145 et #146). Ses deux appelants le mettent donc sur le tas —
    /// une allocation par appel RPC, et une méthode de plus ne coûte plus rien.
    async fn dispatch(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::STATUS => Ok(serde_json::to_value(self.daemon.status().await?)?),
            method::PATHS => Ok(json!({
                "config": s.platform.dirs.config(),
                "data": s.platform.dirs.data(),
                "state": s.platform.dirs.state(),
                "logs": s.platform.dirs.logs(),
                "cache": s.platform.dirs.cache(),
                "db": s.platform.dirs.db_path(),
                "socket": s.platform.dirs.socket_path(),
            })),
            method::DOCTOR => {
                let mut checks = crate::doctor::run(s).await;
                if let Some(sup) = self.daemon.hooks.mcp_supervisor() {
                    checks.extend(crate::doctor::mcp_checks(s, &sup).await);
                }
                checks.push(crate::doctor::embedding_check(&self.daemon).await);
                checks.push(crate::doctor::vault_index_check(s).await);
                checks.extend(crate::doctor::coherence_checks(s).await);
                checks.push(crate::doctor::logs_secret_check(s));
                checks.push(crate::doctor::stored_secret_check(s).await);
                checks.push(crate::vault_git::doctor_check(s));
                checks.push(crate::doctor::binary_signature_check(s));
                checks.push(crate::doctor::install_mode_check());
                checks.push(crate::doctor::pending_upgrade_check(s));
                checks.push(crate::doctor::schedules_check(s).await);
                checks.push(crate::voice::doctor_check(&self.daemon).await);
                checks.push(crate::backup::doctor_check(&self.daemon).await);
                // Boucles de fond relancées ou mortes (#84).
                checks.push(crate::tasks::doctor_check(&self.daemon));
                Ok(json!(checks))
            }
            method::METRICS => {
                // Les jauges se lisent au moment de la demande ; compteurs et
                // histogrammes s'accumulent pendant la vie du daemon (issue #103).
                use penelope_observe::metrics::gauge_set;
                gauge_set(
                    "penelope_approvals_pending",
                    &[],
                    s.approvals.count_pending().await? as f64,
                );
                gauge_set(
                    "penelope_effects_unknown",
                    &[],
                    s.effects
                        .count_by_state(penelope_kernel::effects::EffectState::Unknown)
                        .await? as f64,
                );
                gauge_set(
                    "penelope_rss_bytes",
                    &[],
                    crate::runtime::rss_mb() * 1024.0 * 1024.0,
                );
                Ok(json!({"text": penelope_observe::metrics::render()}))
            }
            method::SHUTDOWN => {
                self.daemon.handle.shutdown();
                Ok(json!({"ok": true}))
            }
            method::RESTART => {
                self.daemon.handle.request_restart();
                Ok(json!({"ok": true}))
            }
            method::UPGRADE => crate::upgrade::rpc(&self.daemon, p).await,
            method::IMPORT_HERMES => crate::hermes::rpc(&self.daemon, p).await,

            // ------------------------------------------------------------ chat
            method::CHAT_SEND => {
                let text = required_str(p, "text")?;
                let sid = self.session_param(p).await?;
                let id = self
                    .daemon
                    .enqueue_message(&sid, &text, &Origin::Cli, None)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("tour non créé"))?;
                let outcome =
                    tokio::time::timeout(CHAT_TIMEOUT, self.daemon.bus.wait_for(id.as_str()))
                        .await
                        .map_err(|_| anyhow::anyhow!("pas de réponse dans le délai"))??;
                Ok(outcome_json(&sid, id.as_str(), &outcome))
            }
            method::CHAT_STREAM | method::TAIL => Err(anyhow::anyhow!(
                "`{method}` répond en flux : l'appeler sur la socket locale"
            )),
            method::CHAT_STOP => {
                let sid = self.session_param(p).await?;
                // Le tour en cours, et ceux qui attendaient derrière lui (#100).
                let stopped = self.daemon.bus.cancel_session(&sid);
                let dropped = s
                    .turns
                    .cancel_pending(&sid, "arrêté par le propriétaire")
                    .await?;
                Ok(json!({"session": sid, "stopped": stopped, "dropped": dropped}))
            }
            method::SESSION_CLOSE => {
                let query = required_str(p, "session")?;
                let sess = crate::session_ops::resolve(s, &query)
                    .await
                    .map_err(anyhow::Error::msg)?;
                crate::session_ops::close(&self.daemon, sess.id.as_str()).await
            }
            method::SESSION_PURGE => {
                let query = required_str(p, "session")?;
                let sess = crate::session_ops::resolve(s, &query)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let id = sess.id.to_string();
                // Le tour en cours est arrêté et la file vidée avant d'effacer.
                crate::session_ops::silence(&self.daemon, &id, "session purgée").await?;
                let reason = p
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("demande du propriétaire");
                crate::purge::session(&self.daemon, &id, reason).await
            }
            method::SESSION_TITLE => {
                let sid = self.session_param(p).await?;
                let title = required_str(p, "title")?;
                let title = crate::titles::clean(&title)
                    .ok_or_else(|| anyhow::anyhow!("titre vide ou refusé"))?;
                s.sessions.require(&sid).await?;
                s.sessions.set_title(&sid, &title, false).await?;
                Ok(json!({"session": sid, "title": title}))
            }
            method::SESSION_SWITCH => {
                let sid = required_str(p, "session")?;
                s.sessions.require(&sid).await?;
                self.daemon.kv_set("cli.session", &sid).await?;
                Ok(json!({"session": sid}))
            }

            // ------------------------------------------------------------ sessions
            method::SESSION_LIST => {
                let sessions = s.sessions.list(None, 100).await?;
                Ok(serde_json::to_value(sessions)?)
            }
            method::SESSION_NEW => {
                let title = p.get("title").and_then(|t| t.as_str()).map(String::from);
                let sess = s
                    .sessions
                    .create(penelope_kernel::session::SessionKind::Chat, title)
                    .await?;
                Ok(serde_json::to_value(sess)?)
            }
            method::SESSION_COMPACT => {
                let sid = self.session_param(p).await?;
                let report = crate::compaction::compact(
                    &self.daemon,
                    &sid,
                    crate::compaction::Trigger::Manual,
                    None,
                )
                .await?;
                let mut v = serde_json::to_value(&report)?;
                v["text"] = json!(crate::compaction::report_text(&report));
                Ok(v)
            }
            method::SESSION_FORK => {
                let sid = self.session_param(p).await?;
                let title = p.get("title").and_then(|t| t.as_str()).map(String::from);
                crate::session_ops::fork(&self.daemon, &sid, title).await
            }
            method::SESSION_REWIND => {
                let sid = self.session_param(p).await?;
                let turns = p
                    .get("turns")
                    .and_then(|v| {
                        v.as_u64()
                            .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
                    })
                    .unwrap_or(1) as usize;
                crate::session_ops::rewind(&self.daemon, &sid, turns).await
            }
            method::EXPORT => {
                let what = p.get("what").and_then(|w| w.as_str()).unwrap_or("session");
                let id = match (what, p.get("id").and_then(|i| i.as_str())) {
                    ("session", None) => Some(self.session_param(p).await?),
                    (_, id) => id.map(String::from),
                };
                crate::session_ops::export(&self.daemon, what, id.as_deref()).await
            }
            method::STORE_REBUILD => crate::session_ops::rebuild(&self.daemon).await,
            method::SKILL_RELOAD => {
                // Skill déposée à l'instant : relue sans attendre la passe d'entretien.
                let generation = crate::runtime::reload_skills(s).await?;
                Ok(json!({"generation": generation, "skills": s.skills.all().len()}))
            }
            method::SKILL_ROLLBACK => {
                let name = required_str(p, "name")?;
                let root = s.platform.dirs.skills();
                let path =
                    penelope_skills::rollback_skill(&root, &name).map_err(anyhow::Error::msg)?;
                crate::runtime::reload_skills(s).await?;
                Ok(json!({"name": name, "restored": path}))
            }
            method::RESTORE => anyhow::bail!(
                "une restauration remplace la base : elle se fait daemon arrêté, `penelope stop` \
                 puis `penelope restore <sauvegarde>`"
            ),
            method::EVAL_RUN => anyhow::bail!(
                "les suites d'évaluation tournent depuis les sources : `penelope eval <suite>` \
                 dans le dépôt"
            ),
            method::SESSION_EXPORT => {
                let sid = required_str(p, "session")?;
                let entries = s.context.history.load(&sid, 0).await?;
                Ok(serde_json::to_value(entries)?)
            }

            // ------------------------------------------------------------ config
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
                let g = set_config_path(&self.daemon, &path, value)?;
                self.daemon.invalidate_providers().await;
                Ok(json!({"generation": g, "warnings": config_warnings(&self.daemon, &path)}))
            }

            // ------------------------------------------------------------ secrets
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

            // ------------------------------------------------------------ modèles
            // ------------------------------------------------------------ MCP
            method::MCP_LIST => {
                let sup = self.mcp()?;
                let servers: Vec<Value> = sup
                    .statuses()
                    .await
                    .into_iter()
                    .map(|st| {
                        json!({
                            "name": st.name,
                            "state": st.state.as_str(),
                            "transport": st.transport,
                            "tools": st.tool_count,
                            "running": st.running,
                            "lazy": st.lazy,
                            "keychain": st.keychain,
                            "protocol": st.protocol,
                            "calls": st.calls,
                            "errors": st.errors,
                            "p95_ms": st.p95_ms.round(),
                            "last_error": st.last_error,
                        })
                    })
                    .collect();
                let invalid: Vec<Value> = sup
                    .invalid()
                    .into_iter()
                    .map(|(file, error)| json!({"file": file, "error": error}))
                    .collect();
                Ok(json!({"servers": servers, "invalid": invalid, "dir": sup.dir()}))
            }
            method::MCP_SHOW => {
                let name = required_str(p, "name")?;
                self.mcp()?.show(&name).await.map_err(anyhow::Error::msg)
            }
            method::MCP_ADD => {
                let sup = self.mcp()?;
                let cfg = mcp_config_param(p)?;
                let name = cfg.name.clone();
                let report = sup.add(cfg, false).await.map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_EDIT => {
                let sup = self.mcp()?;
                let name = required_str(p, "name")?;
                let patch = p
                    .get("patch")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("paramètre `patch` manquant"))?;
                let report = sup.edit(&name, &patch).await.map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_RM => {
                let name = required_str(p, "name")?;
                let report = self
                    .mcp()?
                    .remove(&name)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"report": report}))
            }
            method::MCP_ENABLE | method::MCP_DISABLE => {
                let sup = self.mcp()?;
                let name = required_str(p, "name")?;
                let report = sup
                    .set_enabled(&name, method == method::MCP_ENABLE)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(mcp_change(&sup, &name, &report).await)
            }
            method::MCP_RESTART => {
                let name = required_str(p, "name")?;
                let status = self
                    .mcp()?
                    .restart(&name)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(serde_json::to_value(status)?)
            }
            method::MCP_TEST => {
                let sup = self.mcp()?;
                let cfg = match p.get("name").and_then(|n| n.as_str()) {
                    Some(name) if p.get("toml").is_none() => sup
                        .config_of(name)
                        .await
                        .ok_or_else(|| anyhow::anyhow!("serveur MCP introuvable : `{name}`"))?,
                    _ => mcp_config_param(p)?,
                };
                Ok(sup.test(&cfg).await)
            }
            method::MCP_LOGS => {
                let name = required_str(p, "name")?;
                let n = p.get("lines").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let lines = self
                    .mcp()?
                    .logs(&name, n.clamp(1, 500))
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"name": name, "lines": lines}))
            }
            method::SESSION_BUDGET => {
                let query = required_str(p, "session")?;
                let sess = crate::session_ops::resolve(s, &query)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let id = sess.id.to_string();
                if let Some(v) = p.get("usd").filter(|v| !v.is_null()) {
                    let usd = v
                        .as_f64()
                        .or_else(|| v.as_str().and_then(|x| x.replace(',', ".").parse().ok()))
                        .filter(|u| *u > 0.0);
                    s.sessions.set_budget(&id, usd).await?;
                }
                let cfg = s.config.config();
                let (daily, limit, _) = s.budget.limits(&cfg.budget, Some(&id), None).await?;
                Ok(json!({
                    "session": id,
                    "budget_usd": s.sessions.get(&id).await?.and_then(|x| x.budget_usd),
                    "limit_usd": limit,
                    "spent_usd": s.budget.spent_session(&id).await?,
                    "daily_limit_usd": daily,
                }))
            }
            method::SESSION_MODEL => {
                let sid = self.session_param(p).await?;
                match p.get("alias").and_then(|a| a.as_str()) {
                    None | Some("") => {}
                    Some("auto") => self.daemon.pin_model(&sid, None).await?,
                    Some(alias) => self.daemon.pin_model(&sid, Some(alias)).await?,
                }
                self.daemon.session_model_view(&sid).await
            }
            method::SESSION_MODE => {
                let sid = self.session_param(p).await?;
                use crate::approval_mode::{ApprovalMode, of_session, set};
                match p.get("mode").and_then(|m| m.as_str()).map(str::trim) {
                    None | Some("") => {}
                    Some("default" | "config") => set(s, &sid, None).await?,
                    Some(m) => {
                        let mode = ApprovalMode::parse(m).ok_or_else(|| {
                            anyhow::anyhow!("mode inconnu `{m}` : ask, reads ou auto")
                        })?;
                        set(s, &sid, Some(mode)).await?;
                    }
                }
                let mode = of_session(s, &sid).await;
                Ok(json!({"session": sid, "mode": mode.as_str(), "label": mode.label()}))
            }
            method::SESSION_PROJECT => {
                let sid = self.session_param(p).await?;
                match p.get("project").and_then(|m| m.as_str()).map(str::trim) {
                    None | Some("") => {}
                    Some("aucun" | "none" | "-") => {
                        crate::session_project::set(&self.daemon, &sid, None).await
                    }
                    Some(name) => crate::session_project::set(&self.daemon, &sid, Some(name)).await,
                }
                let (project, how) = crate::session_project::of_session(s, &sid).await;
                let known: Vec<String> =
                    crate::session_project::known(s).await.into_iter().collect();
                Ok(json!({"session": sid, "project": project, "how": how, "known": known}))
            }
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
                let g = self.daemon.publish_config("cli", |c| {
                    c.models.aliases.insert(alias.clone(), model.clone());
                    Ok(vec![format!("models.aliases.{alias}")])
                })?;
                self.daemon.invalidate_providers().await;
                // Un catalogue chargé permet de prévenir d'une faute de frappe.
                let bare = penelope_llm::catalog::strip_provider(&model);
                let known = s.catalog.is_empty() || s.catalog.get(bare).is_some();
                Ok(json!({"generation": g, "alias": alias, "model": model, "known": known}))
            }
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

            // ------------------------------------------------------------ HITL
            method::APPROVALS => Ok(serde_json::to_value(s.approvals.pending(50).await?)?),
            method::APPROVE => {
                let id = required_str(p, "id")?;
                let always = p.get("always").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut d = if always {
                    penelope_hitl::Decision::approve_always("cli")
                } else {
                    penelope_hitl::Decision::approve_once("cli")
                };
                // Effet incertain (#83) : « c'est fait » ou « relancer », à dire.
                match p.get("effect").and_then(|v| v.as_str()) {
                    Some("done") => d.choice = crate::agent::EFFECT_DONE.into(),
                    Some("retry") => d.choice = crate::agent::EFFECT_RETRY.into(),
                    Some(other) => anyhow::bail!("--effect {other} : attendu done ou retry"),
                    None => {}
                }
                self.decide_and_resume(&id, &d).await
            }
            method::DENY => {
                let id = required_str(p, "id")?;
                let reason = p.get("reason").and_then(|r| r.as_str()).map(String::from);
                self.decide_and_resume(&id, &penelope_hitl::Decision::deny("cli", reason))
                    .await
            }
            method::QUIET => {
                if let Some(range) = p
                    .get("range")
                    .or_else(|| p.get("arg"))
                    .and_then(|v| v.as_str())
                {
                    let r = range.to_string();
                    let g = self.daemon.publish_config("cli", move |c| {
                        c.telegram.quiet_hours = r.clone();
                        Ok(vec!["telegram.quiet_hours".into()])
                    })?;
                    Ok(json!({"quiet_hours": range, "generation": g}))
                } else {
                    Ok(json!({"quiet_hours": s.config.config().telegram.quiet_hours}))
                }
            }
            method::POLICIES => {
                // Chaque règle avec ce qui la rend inutile, s'il y a lieu (issue #111).
                let now = s.clock.now_ms();
                let rules: Vec<Value> = s
                    .policies
                    .active_rules()
                    .await?
                    .iter()
                    .map(|r| {
                        let mut v = serde_json::to_value(r).unwrap_or_default();
                        if let Some(note) = crate::approval_mode::rule_note(r, now) {
                            v["remarque"] = json!(note);
                        }
                        v
                    })
                    .collect();
                Ok(json!(rules))
            }
            method::POLICY_REVOKE => {
                let id = required_str(p, "id")?;
                Ok(json!({"revoked": s.policies.revoke(&id).await?}))
            }

            // ------------------------------------------------------------ mémoire
            method::MEM_SEARCH => {
                let q = required_str(p, "query")?;
                let hits = s
                    .memory
                    .search(&q, None, &penelope_memory::SearchFilter::explicit(), &[])
                    .await?;
                Ok(json!(
                    hits.iter()
                        .map(|h| json!({
                            "uid": h.entry.uid,
                            "text": h.entry.text,
                            "level": h.entry.level.as_str(),
                            "file": h.entry.file,
                            "score": h.score,
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::MEM_SHOW => {
                let uid = required_str(p, "uid")?;
                Ok(serde_json::to_value(s.memory.get(&uid).await?)?)
            }
            method::MEM_SIGNALS => {
                let uid = required_str(p, "uid")?;
                let entry = s
                    .memory
                    .get(&uid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("aucune entrée `{uid}`"))?;
                let sig = s.memory.signals_of(&uid).await?;
                Ok(json!({
                    "uid": uid,
                    "fichier": entry.file,
                    "texte": entry.text,
                    "rappels": sig.recalls,
                    "rappels_utiles": sig.useful_recalls,
                    "vu_sans_etre_retenu": sig.seen,
                    "succes": sig.successes,
                    "contradictions": sig.contradictions,
                    "dernier_rappel": sig.last_recall,
                    "requetes_distinctes": sig.distinct_queries.len(),
                    "facteur_usage": (penelope_memory::index::usage_factor(&sig) * 1000.0).round()
                        / 1000.0,
                }))
            }
            method::MCP_AUTH => {
                let name = required_str(p, "name")?;
                if let Some(callback) = p.get("callback").and_then(|c| c.as_str()) {
                    let server = crate::mcp_auth::complete(&self.daemon, callback)
                        .await
                        .map_err(anyhow::Error::msg)?;
                    if server != name {
                        anyhow::bail!("cette adresse autorise `{server}`, pas `{name}`");
                    }
                    let status = match self.daemon.hooks.mcp_supervisor() {
                        Some(sup) => sup
                            .restart(&server)
                            .await
                            .map(|st| serde_json::to_value(st).unwrap_or_default())
                            .unwrap_or_else(|e| json!({"error": e})),
                        None => Value::Null,
                    };
                    return Ok(json!({"server": server, "authorized": true, "status": status}));
                }
                let sup = self
                    .daemon
                    .hooks
                    .mcp_supervisor()
                    .ok_or_else(|| anyhow::anyhow!("superviseur MCP indisponible"))?;
                let cfg = sup
                    .config_of(&name)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("serveur MCP `{name}` inconnu"))?;
                let start = crate::mcp_auth::start(&self.daemon, &cfg, None)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let mut v = serde_json::to_value(&start)?;
                v["text"] = json!(crate::mcp_auth::prompt_text(&start));
                Ok(v)
            }
            method::MEM_HISTORY => {
                crate::dream::history(
                    s,
                    p.get("uid").and_then(|v| v.as_str()),
                    p.get("file").and_then(|v| v.as_str()),
                )
                .await
            }
            method::MEM_RESTORE => {
                let id = p
                    .get("id")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
                    })
                    .ok_or_else(|| anyhow::anyhow!("paramètre `id` (entier) obligatoire"))?;
                crate::dream::restore(s, id).await
            }
            method::MEM_REINDEX => {
                let vault = crate::conversation::vault_dir(s);
                crate::vault_ops::migrate_wiki(s, &vault)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let n = crate::vault_ops::reindex(s, &vault)
                    .await
                    .map_err(anyhow::Error::msg)?;
                // `embeddings` : tous les vecteurs recalculés avec le modèle courant.
                if p.get("embeddings").and_then(|v| v.as_bool()) == Some(true) {
                    let report = crate::embeddings::backfill(&self.daemon, true).await?;
                    return Ok(json!({"entries": n, "embeddings": report}));
                }
                crate::embeddings::spawn_backfill(self.daemon.clone());
                Ok(json!({"entries": n}))
            }
            method::MEM_RETRY_REJECTED => Ok(json!({
                "retried": s.candidates.retry_origin_rejections().await?,
            })),
            method::MEM_AUDIT => {
                let audit = crate::mem_audit::run(&self.daemon).await?;
                Ok(crate::mem_audit::to_json(&audit))
            }
            method::ONBOARD_NEXT | method::ONBOARD_ANSWER | method::ONBOARD_WRITE => {
                crate::onboarding::rpc(&self.daemon, method, p).await
            }
            method::MEM_FORGET => {
                let uid = required_str(p, "uid")?;
                let vault = crate::conversation::vault_dir(s);
                let done = crate::vault_ops::forget(s, &vault, &uid)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"uid": uid, "forgotten": done}))
            }
            method::MEM_SPLIT => mem_split(&self.daemon, p).await,
            method::MEM_CANDIDATES => mem_candidates(s).await,
            method::MEM_DREAM => {
                let dry_run = p.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
                let outcome = crate::dream::run(&self.daemon, dry_run).await?;
                let mut v = serde_json::to_value(&outcome)?;
                v["text"] = json!(outcome.report.render());
                Ok(v)
            }
            method::MEM_LEARNED => {
                let days = p
                    .get("days")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
                    })
                    .unwrap_or(7);
                Ok(json!(crate::dream::learned(s, days).await?))
            }
            method::VAULT_SYNC => {
                let day = s.clock.now_rfc3339()[..10].to_string();
                crate::dream::vault_sync(&self.daemon, &format!("sync: {day}"))
                    .await
                    .map_err(anyhow::Error::msg)
            }
            method::VAULT_CHECK => Ok(crate::dream::vault_check(s).await),
            method::VAULT_LINT => {
                let vault = crate::conversation::vault_dir(s);
                let (report, proposals) = crate::dream::wiki_review(s, &vault).await;
                let mut text = if report.is_clean() {
                    format!("✅ Wiki valide : {} note(s), aucun problème.", report.notes)
                } else {
                    format!(
                        "{} problème(s) sur {} note(s) :\n- {}",
                        report.problems(),
                        report.notes,
                        report.summary().join("\n- ")
                    )
                };
                if !proposals.is_empty() {
                    text.push_str(&format!("\nÀ trancher :\n- {}", proposals.join("\n- ")));
                }
                Ok(json!({"report": report, "proposals": proposals, "text": text}))
            }
            method::MEM_DIFF => {
                let since = p.get("since").and_then(|v| v.as_str()).unwrap_or_default();
                if !since.is_empty() && since != "dream" {
                    anyhow::bail!("`--since` n'accepte que `dream`");
                }
                crate::vault_git::diff(s, since == "dream")
                    .await
                    .map_err(anyhow::Error::msg)
            }
            method::INTENT_LIST => Ok(serde_json::to_value(s.intents.all().await?)?),
            method::INTENT_CANCEL => {
                let id = required_str(p, "id")?;
                Ok(json!({"cancelled": s.intents.cancel(&id).await?}))
            }

            // ------------------------------------------------------------ workflows
            method::WF_LIST => Ok(json!(
                s.workflows
                    .all()
                    .iter()
                    .map(|e| json!({
                        "id": e.workflow.metadata.id,
                        "name": e.workflow.metadata.name,
                        "scope": e.scope.as_str(),
                        "steps": e.workflow.steps.len(),
                        "runsHere": e.workflow.runs_on(s.platform.os_name()),
                    }))
                    .collect::<Vec<_>>()
            )),
            method::WF_SHOW => {
                let id = required_str(p, "id")?;
                let w = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
                Ok(json!({"definition": w, "graph": w.render_graph()}))
            }
            method::WF_VALIDATE => {
                let raw = required_str(p, "json")?;
                let stem = p.get("name").and_then(|n| n.as_str());
                let w = penelope_workflow::Workflow::from_json(&raw)
                    .map_err(|e| anyhow::anyhow!("JSON invalide : {e}"))?;
                let known = crate::runtime::workflow_known_with(
                    &s.config.config(),
                    &s.mcp_tools,
                    &s.workflows,
                )
                .await;
                let report = penelope_workflow::validate(&w, stem, &known);
                Ok(json!({
                    "valid": report.is_valid(),
                    "issues": report.issues.iter().map(|i| json!({
                        "path": i.path, "message": i.message,
                        "severity": format!("{:?}", i.severity).to_lowercase()
                    })).collect::<Vec<_>>(),
                }))
            }
            method::WF_RUNS => Ok(serde_json::to_value(s.runs.list(None, 50).await?)?),
            method::WF_TRACE => {
                let id = required_str(p, "run")?;
                Ok(json!(s.runs.trace(&id).await?))
            }
            method::WF_RUN => {
                let id = required_str(p, "id")?;
                let params = p.get("params").cloned().unwrap_or(json!({}));
                let origin = crate::scheduler::owner_origin(&self.daemon);
                let run = crate::workflow::start_run(&self.daemon, &id, params, &origin, None, 0)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(serde_json::to_value(run)?)
            }
            method::WF_CONTROL => {
                let run = required_str(p, "run")?;
                let op = required_str(p, "op")?;
                if op == "answer" {
                    let choice = required_str(p, "choice")?;
                    let current = s
                        .runs
                        .get(&run)
                        .await?
                        .ok_or_else(|| anyhow::anyhow!("run {run} introuvable"))?;
                    let visit = format!(
                        "{}.{}",
                        current.current_step.unwrap_or_default(),
                        current.iterations
                    );
                    crate::workflow::answer(
                        &self.daemon,
                        &run,
                        &visit,
                        &choice,
                        p.get("input").and_then(|i| i.as_str()),
                    )
                    .await?;
                    return Ok(json!({"run": run, "answered": choice}));
                }
                // Plafonds propres au run (issue #136).
                if op == "budget" {
                    return crate::workflow::raise_budget(
                        &self.daemon,
                        &run,
                        p.get("usd").and_then(|v| v.as_f64()),
                        p.get("tokens").and_then(|v| v.as_u64()),
                    )
                    .await;
                }
                let control = penelope_workflow::Control::parse(&op)
                    .ok_or_else(|| anyhow::anyhow!("opération inconnue : {op}"))?;
                let state = crate::workflow::control(&self.daemon, &run, &control).await?;
                Ok(json!({"state": state.as_str()}))
            }

            // ------------------------------------------------------------ schedules
            method::SCHEDULE_LIST => Ok(json!(crate::scheduler::listing(s).await?)),
            // Nouvelle destination, sans recréer la planification (#124) : `private`, ou
            // `chat_id` et `topic_id`.
            method::SCHEDULE_MOVE => {
                let id = required_str(p, "id")?;
                let (chat_id, topic_id) =
                    if p.get("private").and_then(|v| v.as_bool()) == Some(true) {
                        (s.config.config().owner.telegram_user_id, None)
                    } else {
                        let chat = p.get("chat_id").and_then(|v| v.as_i64()).ok_or_else(|| {
                            anyhow::anyhow!("`chat_id` (et `topic_id`) ou `private: true`")
                        })?;
                        (chat, p.get("topic_id").and_then(|v| v.as_i64()))
                    };
                let to = crate::scheduler::retarget(s, &id, chat_id, topic_id)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"id": id, "destination": to}))
            }
            method::SCHEDULE_ADD => {
                let kind = penelope_workflow::TriggerKind::parse(&required_str(p, "kind")?)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "kind inconnu : cron, interval, mcp_poll, watch_file ou event"
                        )
                    })?;
                crate::scheduler::create(
                    s,
                    kind,
                    p.get("spec").cloned().unwrap_or(json!({})),
                    p.get("target").cloned().unwrap_or(json!({})),
                    p.get("dedup").cloned().unwrap_or(json!({})),
                )
                .await
                .map_err(anyhow::Error::msg)
            }
            method::SCHEDULE_RUN_NOW => {
                let id = required_str(p, "id")?;
                crate::scheduler::run_now(&self.daemon, &id).await
            }
            method::SCHEDULE_PAUSE => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "paused")
                    .await?;
                Ok(json!({"ok": true}))
            }
            method::SCHEDULE_RESUME => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "active")
                    .await?;
                Ok(json!({"ok": true}))
            }
            method::SCHEDULE_RM => {
                s.schedules
                    .set_state(&required_str(p, "id")?, "deleted")
                    .await?;
                Ok(json!({"ok": true}))
            }

            // ------------------------------------------------------------ skills
            method::SKILL_LIST => Ok(json!(
                s.skills
                    .all()
                    .iter()
                    .map(|k| json!({
                        "name": k.name, "version": k.version, "scope": k.scope.as_str(),
                        "description": k.description
                    }))
                    .collect::<Vec<_>>()
            )),
            method::SKILL_INSTALL => skill_install(&self.daemon, p).await,
            method::SKILL_SHOW => {
                let name = required_str(p, "name")?;
                Ok(serde_json::to_value(s.skills.get(&name))?)
            }

            // ------------------------------------------------------------ données
            method::AUDIT_VERIFY => Ok(serde_json::to_value(s.events.verify().await?)?),
            method::USAGE => {
                let by = p.get("by").and_then(|b| b.as_str()).unwrap_or("session");
                if !penelope_kernel::budget::USAGE_AXES.contains(&by) {
                    anyhow::bail!(
                        "axe inconnu `{by}` : {}",
                        penelope_kernel::budget::USAGE_AXES.join(", ")
                    );
                }
                let session = p.get("session").and_then(|v| v.as_str());
                let since = p.get("since").and_then(|v| v.as_str());
                let limit = p
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(20)
                    .clamp(1, 500);
                let rows = s.budget.report(by, session, since, limit).await?;
                Ok(json!(
                    rows.into_iter()
                        .map(|r| json!({
                            "key": r.key,
                            "label": r.label,
                            "costUsd": round_usd(r.cost_usd),
                            "tokens": r.tokens,
                            "calls": r.calls,
                            "estimated": r.estimated,
                            "last": r.last_ts,
                            "promptTokens": r.prompt,
                            "cachedTokens": r.cached,
                            "completionTokens": r.completion,
                            "cacheRatio": (r.cache_ratio() * 1000.0).round() / 1000.0,
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::BACKUP => {
                // Sans `push`, l'ancien comportement : un instantané de la base, local.
                let push = p.get("push").and_then(|v| v.as_bool()).unwrap_or(false);
                let full = push || p.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                if full {
                    let media = p.get("media").and_then(|v| v.as_bool());
                    return crate::backup::run(&self.daemon, push, media).await;
                }
                let dest = s.platform.dirs.data().join("backups").join(format!(
                    "penelope-{}.db",
                    s.clock.now_rfc3339().replace(':', "-")
                ));
                let took = s.store.snapshot_to(dest.clone()).await?;
                Ok(json!({"path": dest, "snapshot_ms": took.as_millis() as u64}))
            }

            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}

/// Montant lisible : six décimales suffisent à distinguer un appel d'un autre.
pub fn round_usd(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

/// Déclaration de serveur passée en paramètre : `toml` (texte d'un fichier `mcp.d`) ou
/// `config` (objet JSON).
fn mcp_config_param(p: &Value) -> anyhow::Result<penelope_mcp::config::ServerConfig> {
    if let Some(raw) = p.get("toml").and_then(|v| v.as_str()) {
        let default_name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let mut v = penelope_mcp::config::parse_file(raw, default_name)?;
        if v.len() != 1 {
            anyhow::bail!(
                "une déclaration à la fois : le fichier en contient {}",
                v.len()
            );
        }
        let cfg = v.remove(0);
        if cfg.name.is_empty() {
            anyhow::bail!("paramètre `name` manquant (ou champ `name` dans la déclaration)");
        }
        return Ok(cfg);
    }
    match p.get("config") {
        Some(c) => Ok(serde_json::from_value(c.clone())?),
        None => anyhow::bail!("paramètre `toml` manquant"),
    }
}

/// Résultat d'une modification : ce qui a changé et l'état du serveur après coup.
async fn mcp_change(
    sup: &crate::mcp::McpSupervisor,
    name: &str,
    report: &crate::mcp::ReloadReport,
) -> Value {
    let status = sup.statuses().await.into_iter().find(|s| s.name == name);
    json!({"report": report, "status": status})
}

impl Rpc {
    fn mcp(&self) -> anyhow::Result<Arc<crate::mcp::McpSupervisor>> {
        self.daemon
            .hooks
            .mcp_supervisor()
            .ok_or_else(|| anyhow::anyhow!("superviseur MCP non démarré dans ce daemon"))
    }

    /// Session visée : paramètre `session`, sinon la session courante de la CLI.
    async fn session_param(&self, p: &Value) -> anyhow::Result<String> {
        match p.get("session").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => {
                self.daemon.services.sessions.require(s).await?;
                Ok(s.to_string())
            }
            _ => self.daemon.chat_session_for(&Origin::Cli).await,
        }
    }

    /// Tranche une approbation et remet la suite du tour en file, sur le canal de la
    /// session : un tour né sur Telegram y répond, même approuvé depuis la CLI.
    async fn decide_and_resume(
        &self,
        id: &str,
        decision: &penelope_hitl::Decision,
    ) -> anyhow::Result<Value> {
        let s = self.services();
        if s.approvals.get(id).await?.is_none() {
            anyhow::bail!("demande {id} introuvable");
        }
        let before = s.approvals.get(id).await?.map(|a| a.state);
        crate::agent::decide_approval(s, id, decision).await?;
        let a = s
            .approvals
            .get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("demande {id} introuvable"))?;
        if before != Some(penelope_hitl::ApprovalState::Pending) {
            anyhow::bail!(
                "demande {id} déjà tranchée : {} via {}",
                a.state.as_str(),
                a.decided_via.clone().unwrap_or_default()
            );
        }
        // Propositions de mémoire : la décision s'applique ici, aucun tour à reprendre.
        if a.kind == penelope_hitl::ApprovalKind::MemoryProposal {
            if a.state == penelope_hitl::ApprovalState::Approved {
                let written = crate::ingest::apply_memory_proposal(&self.daemon, id).await?;
                let mut v = serde_json::to_value(&a)?;
                v["written"] = json!(written);
                return Ok(v);
            }
            return Ok(serde_json::to_value(a)?);
        }
        if let Some(sid) = &a.session_id {
            let origin = match s.sessions.get(sid).await? {
                Some(sess) if sess.tg_chat_id.is_some() => Origin::Telegram {
                    chat_id: sess.tg_chat_id.unwrap_or_default(),
                    topic_id: sess.tg_topic_id,
                    message_id: None,
                },
                _ => Origin::Cli,
            };
            self.daemon.enqueue_resume(sid, id, &origin).await?;
        }
        Ok(serde_json::to_value(a)?)
    }

    /// Méthodes à réponse en flux : `chat.stream` et `tail`. Les événements partent en
    /// notifications JSON-RPC (sans `id`), la réponse finale clôt l'échange.
    /// Le client d'un tour CLI est parti : le tour s'arrête, qu'il tourne déjà ou attende
    /// encore son tour (#100). Un tour Telegram n'est jamais concerné : il n'a pas de
    /// client de flux.
    async fn abandon(&self, session_id: &str, turn_id: &str) {
        if !self.daemon.bus.cancel_turn(session_id, turn_id) {
            let _ = self.daemon.services.turns.cancel_if_pending(turn_id).await;
        }
        tracing::info!(
            session = session_id,
            turn = turn_id,
            "client parti : tour annulé"
        );
    }

    /// Flux d'un tour (`chat.stream`) ou de tous (`tail`). `closed` se résout quand le
    /// client ferme sa connexion : un tour lancé par la CLI que personne ne lira plus est
    /// annulé, outils compris, au lieu de tourner et de facturer (issue #100).
    pub async fn handle_streaming<W, C>(
        &self,
        req: RpcRequest,
        out: &mut W,
        closed: C,
    ) -> anyhow::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
        C: std::future::Future<Output = ()>,
    {
        let params = req.params.clone().unwrap_or(json!({}));
        let mut rx = self.daemon.bus.subscribe();
        tokio::pin!(closed);
        match req.method.as_str() {
            method::TAIL => loop {
                let ev = tokio::select! {
                    ev = rx.recv() => ev,
                    _ = &mut closed => return Ok(()),
                };
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return Ok(()),
                };
                if let Some(se) = to_stream_event(&ev) {
                    write_line(out, &notification(&se)).await?;
                }
            },
            _ => {
                let text = required_str(&params, "text")?;
                let sid = self.session_param(&params).await?;
                let id = self
                    .daemon
                    .enqueue_message(&sid, &text, &Origin::Cli, None)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("tour non créé"))?;
                let mut done = self.daemon.bus.wait_for(id.as_str());
                loop {
                    tokio::select! {
                        outcome = &mut done => {
                            // Les derniers fragments éventuels, puis la réponse.
                            while let Ok(ev) = rx.try_recv() {
                                if ev.turn_id == id.as_str()
                                    && let Some(se) = to_stream_event(&ev) {
                                        write_line(out, &notification(&se)).await?;
                                    }
                            }
                            let outcome = outcome?;
                            let resp = RpcResponse::ok(req.id.clone(), outcome_json(&sid, id.as_str(), &outcome));
                            write_line(out, &serde_json::to_value(resp)?).await?;
                            return Ok(());
                        }
                        _ = &mut closed => {
                            self.abandon(&sid, id.as_str()).await;
                            return Ok(());
                        }
                        ev = rx.recv() => {
                            match ev {
                                Ok(ev) if ev.turn_id == id.as_str() => {
                                    if matches!(ev.kind, BusKind::Finished(_)) {
                                        continue;
                                    }
                                    if let Some(se) = to_stream_event(&ev)
                                        && let Err(e) = write_line(out, &notification(&se)).await
                                    {
                                        // Client parti entre deux fragments.
                                        self.abandon(&sid, id.as_str()).await;
                                        return Err(e);
                                    }
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                Err(_) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Issue d'un tour, pour la CLI.
pub fn outcome_json(session_id: &str, turn_id: &str, o: &TurnOutcome) -> Value {
    let (kind, extra) = match o {
        TurnOutcome::Answered {
            text,
            iterations,
            cost_usd,
        } => (
            "answered",
            json!({"text": text, "iterations": iterations, "cost_usd": cost_usd}),
        ),
        TurnOutcome::AwaitingApproval { approval_id } => {
            ("awaiting_approval", json!({"approval_id": approval_id}))
        }
        TurnOutcome::LoopAborted {
            report,
            answer,
            choices,
        } => (
            "loop_aborted",
            json!({"report": report, "answer": answer, "choices": choices}),
        ),
        TurnOutcome::Cancelled => ("cancelled", json!({})),
        TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        } => (
            "budget_exceeded",
            json!({
                "scope": scope,
                "spent_usd": spent_usd,
                "limit_usd": limit_usd,
                "text": crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
            }),
        ),
        TurnOutcome::Failed { error } => ("failed", json!({"error": error})),
    };
    let mut v = json!({"session": session_id, "turn": turn_id, "outcome": kind});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    v
}

fn to_stream_event(ev: &crate::bus::BusEvent) -> Option<StreamEvent> {
    use crate::agent::TurnEvent as T;
    let session_id = ev.session_id.clone();
    Some(match &ev.kind {
        BusKind::Event(T::Delta(text)) => StreamEvent::Delta {
            session_id,
            text: text.clone(),
        },
        BusKind::Event(T::Reasoning(text)) => StreamEvent::Reasoning {
            session_id,
            text: text.clone(),
        },
        BusKind::Event(T::ToolCall { name, args }) => StreamEvent::ToolCall {
            session_id,
            name: name.clone(),
            args: args.clone(),
        },
        BusKind::Event(T::ToolResult { name, ok, preview }) => StreamEvent::ToolResult {
            session_id,
            name: name.clone(),
            ok: *ok,
            preview: preview.clone(),
        },
        BusKind::Event(T::Approval { id, tool, risk, .. }) => StreamEvent::Approval {
            id: id.clone(),
            kind: "tool_call".into(),
            subject: tool.clone(),
            risk: risk.as_str().into(),
        },
        BusKind::Finished(TurnOutcome::Answered { text, .. }) => StreamEvent::Done {
            session_id,
            text: text.clone(),
        },
        BusKind::Finished(TurnOutcome::Failed { error }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: error.clone(),
        },
        // L'événement d'approbation est déjà parti pendant le tour.
        BusKind::Finished(TurnOutcome::AwaitingApproval { .. }) => return None,
        BusKind::Finished(TurnOutcome::Cancelled) => StreamEvent::Error {
            session_id: Some(session_id),
            message: "génération arrêtée".into(),
        },
        // Le rapport technique reste dans les événements et les journaux (issue #31).
        BusKind::Finished(TurnOutcome::LoopAborted {
            answer, choices, ..
        }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: format!("{answer}\n\nSuites possibles : {}", choices.join(" · ")),
        },
        BusKind::Finished(TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: crate::agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
        },
        _ => return None,
    })
}

fn notification(se: &StreamEvent) -> Value {
    json!({"jsonrpc": JSONRPC, "method": "event", "params": se})
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(
    out: &mut W,
    v: &Value,
) -> anyhow::Result<()> {
    let mut body = serde_json::to_string(v)?;
    body.push('\n');
    out.write_all(body.as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

fn required_str(p: &Value, key: &str) -> anyhow::Result<String> {
    p.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("paramètre `{key}` manquant"))
}

/// Installe des skills tierces et rend ce qui a été posé, avec ce qui manque (#146).
async fn skill_install(d: &Arc<Daemon>, p: &Value) -> anyhow::Result<Value> {
    let source = required_str(p, "source")?;
    let force = p.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let (src, installed, missing) = crate::skill_install::install(d, &source, force).await?;
    Ok(json!({
        "source": src.label(),
        "installed": installed,
        "missing": missing,
        "report": crate::skill_install::report(&src, &installed, &missing),
    }))
}

/// Candidats en attente, **et** questions sans réponse : elles ne repassent pas en
/// consolidation, elles doivent rester visibles (issue #145).
async fn mem_candidates(s: &Services) -> anyhow::Result<Value> {
    let mut v = s.candidates.pending(None).await?;
    v.extend(s.candidates.in_question().await?);
    Ok(serde_json::to_value(v)?)
}

/// Propose le découpage d'une entrée fourre-tout : une carte, jamais une écriture (#145).
async fn mem_split(d: &Arc<Daemon>, p: &Value) -> anyhow::Result<Value> {
    let uid = required_str(p, "uid")?;
    let id = crate::mem_split::propose(d, &uid).await?;
    Ok(json!({"approval": id}))
}

fn classify(e: &anyhow::Error) -> i32 {
    let m = e.to_string();
    if m.contains("méthode inconnue") {
        METHOD_NOT_FOUND
    } else if m.contains("manquant") {
        INVALID_PARAMS
    } else if m.contains("introuvable") {
        NOT_FOUND
    } else if m.contains("déjà tranchée") {
        CONFLICT
    } else {
        INTERNAL_ERROR
    }
}

/// `config set a.b.c = valeur` : applique une modification par chemin.
pub(crate) fn set_config_path(daemon: &Daemon, path: &str, value: Value) -> anyhow::Result<u64> {
    let path_owned = path.to_string();
    let generation = daemon.publish_config("cli", move |c| {
        let mut v = serde_json::to_value(&*c).map_err(penelope_kernel::KernelError::Json)?;
        let parts: Vec<&str> = path_owned.split('.').collect();
        let mut cur = &mut v;
        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                let obj = cur.as_object_mut().ok_or_else(|| {
                    penelope_kernel::KernelError::config(format!("chemin invalide : {path_owned}"))
                })?;
                // Une table à clés libres accepte une entrée nouvelle (#128) ; ailleurs,
                // une clé absente est une faute de frappe.
                let parent = parts[..i].join(".");
                if !obj.contains_key(*part)
                    && !penelope_kernel::config::MAP_PATHS.contains(&parent.as_str())
                {
                    return Err(penelope_kernel::KernelError::config(format!(
                        "clé inconnue : {path_owned}"
                    )));
                }
                // Une valeur seule vaut une liste d'un élément, comme pour `mcp edit`
                // (issue #138).
                let value = match obj.get(*part) {
                    Some(current) => {
                        penelope_kernel::config::list_value(&path_owned, current, value.clone())
                            .map_err(penelope_kernel::KernelError::config)?
                    }
                    None => value.clone(),
                };
                obj.insert((*part).to_string(), value);
            } else {
                cur = cur.get_mut(part).ok_or_else(|| {
                    penelope_kernel::KernelError::config(format!("clé inconnue : {path_owned}"))
                })?;
            }
        }
        let updated: penelope_kernel::Config =
            serde_json::from_value(v).map_err(penelope_kernel::KernelError::Json)?;
        // Un réglage qui annule sa propre intention est refusé, nommément (issue #16).
        let refused = penelope_kernel::coherence::new_refusals(c, &updated, &path_owned);
        if !refused.is_empty() {
            return Err(penelope_kernel::KernelError::config(format!(
                "réglage refusé : {}",
                refused
                    .iter()
                    .map(|r| r.message.clone())
                    .collect::<Vec<_>>()
                    .join(" ; ")
            )));
        }
        *c = updated;
        Ok(vec![path_owned.clone()])
    })?;
    Ok(generation)
}

/// Avertissements de cohérence qui touchent un réglage, après son écriture.
pub(crate) fn config_warnings(daemon: &Daemon, path: &str) -> Vec<String> {
    penelope_kernel::coherence::contradictions(&daemon.services.config.config())
        .into_iter()
        .filter(|c| c.concerns(path))
        .map(|c| c.message)
        .collect()
}

/// Sert la socket locale jusqu'à l'arrêt du daemon.
pub async fn serve(daemon: Arc<Daemon>) -> anyhow::Result<()> {
    let path = daemon.services.platform.dirs.socket_path();
    let listener = penelope_platform::ipc::IpcListener::bind(&path).await?;
    serve_on(daemon, listener).await
}

/// Sert une socket déjà ouverte. Le daemon l'ouvre **avant** de lancer ses boucles : un
/// second daemon s'arrête ainsi avant d'avoir touché à la file des tours.
pub async fn serve_on(
    daemon: Arc<Daemon>,
    listener: penelope_platform::ipc::IpcListener,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    tracing::info!(
        socket = %daemon.services.platform.dirs.socket_path().display(),
        "RPC à l'écoute"
    );
    let rpc = Arc::new(Rpc::new(daemon.clone()));
    // Jeton de session : sans lui, la socket ne sert rien (issue #91).
    let token: Arc<str> = listener.token().into();

    loop {
        if daemon.handle.is_shutting_down() {
            return Ok(());
        }
        let accept = tokio::time::timeout(std::time::Duration::from_millis(500), listener.accept());
        let stream = match accept.await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "connexion RPC refusée");
                continue;
            }
            Err(_) => continue, // délai : on revérifie l'arrêt
        };

        let rpc = rpc.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<RpcRequest>(&line) {
                    // Même utilisateur ne veut pas dire propriétaire : un processus confiné
                    // n'a pas le jeton, rien ne s'exécute pour lui.
                    Ok(req)
                        if !penelope_platform::ipc::tokens_match(req.auth.as_deref(), &token) =>
                    {
                        tracing::warn!(methode = %req.method, "requête RPC sans jeton valide refusée");
                        RpcResponse::err(
                            req.id,
                            penelope_kernel::api::DENIED,
                            "unauthorized : jeton RPC absent ou invalide (daemon redémarré ? \
                             relancer la commande)",
                        )
                    }
                    Ok(req) if req.method == method::CHAT_STREAM || req.method == method::TAIL => {
                        let id = req.id.clone();
                        // Fin de la connexion côté client : plus personne ne lira ce flux.
                        let closed = async { while let Ok(Some(_)) = lines.next_line().await {} };
                        if let Err(e) = rpc.handle_streaming(req, &mut write, closed).await {
                            let r = RpcResponse::err(id, classify(&e), e.to_string());
                            let _ = write_line(
                                &mut write,
                                &serde_json::to_value(r).unwrap_or_default(),
                            )
                            .await;
                        }
                        continue;
                    }
                    Ok(req) => rpc.handle(req).await,
                    Err(e) => RpcResponse::err(None, PARSE_ERROR, e.to_string()),
                };
                let mut body = serde_json::to_string(&response).unwrap_or_default();
                body.push('\n');
                if write.write_all(body.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    async fn rpc() -> (tempfile::TempDir, Rpc) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        (dir, Rpc::new(Arc::new(Daemon::from_services(s))))
    }

    async fn call(rpc: &Rpc, method: &str, params: Value) -> RpcResponse {
        rpc.handle(RpcRequest::new(1, method, params)).await
    }

    /// #54 : un modèle qui n'appelle pas d'outils est refusé pour un alias de
    /// conversation, et accepté pour un rôle de service. `doctor` le signale ensuite.
    #[tokio::test]
    async fn a_model_without_tool_calling_is_refused_for_a_conversation_alias() {
        let (_d, r) = rpc().await;
        let s = &r.daemon.services;
        let mut sans =
            penelope_llm::catalog::ModelInfo::minimal("vieux/modele", "openrouter", 8192);
        sans.supported_parameters.clear();
        s.catalog.upsert(vec![
            sans,
            penelope_llm::catalog::ModelInfo::minimal("bon/modele", "openrouter", 128_000),
        ]);

        let refus = call(
            &r,
            method::MODEL_SET,
            json!({"alias": "main", "model": "openrouter:vieux/modele"}),
        )
        .await;
        let message = refus.error.expect("refus attendu").message;
        assert!(message.contains("n'appelle pas d'outils"), "{message}");

        // Un rôle de service n'appelle pas d'outils : le même modèle y est bienvenu.
        let ok = call(
            &r,
            method::MODEL_SET,
            json!({"alias": "stt", "model": "openrouter:vieux/modele"}),
        )
        .await;
        assert!(ok.error.is_none(), "{:?}", ok.error);

        // Configuration déjà en place : `doctor` le dit.
        r.daemon
            .publish_config("test", |c| {
                c.models
                    .aliases
                    .insert("main".into(), "openrouter:vieux/modele".into());
                Ok(vec!["models.aliases.main".into()])
            })
            .unwrap();
        let checks = crate::doctor::run(s).await;
        let tools = checks
            .iter()
            .find(|c| c.id == "models.tools")
            .expect("contrôle des outils");
        assert!(!tools.ok, "{tools:?}");
    }

    /// #142 : l'abonnement ChatGPT ne sert que les tours du propriétaire — un alias de
    /// rôle de fond ne peut pas le viser, et un préfixe mal écrit est refusé au lieu de
    /// partir en silence chez OpenRouter.
    #[tokio::test]
    async fn a_background_alias_cannot_aim_at_the_subscription() {
        let (_d, r) = rpc().await;
        for alias in ["stt", "embedding", "tts", "summarizer"] {
            let refus = call(
                &r,
                method::MODEL_SET,
                json!({"alias": alias, "model": "codex:gpt-6-astra"}),
            )
            .await;
            let message = refus
                .error
                .unwrap_or_else(|| panic!("{alias} : refus attendu"))
                .message;
            assert!(
                message.contains("abonnement ChatGPT"),
                "{alias} : {message}"
            );
        }
        // La conversation, elle, a le droit : c'est le propriétaire qui parle.
        let ok = call(
            &r,
            method::MODEL_SET,
            json!({"alias": "main", "model": "codex:gpt-6-astra"}),
        )
        .await;
        assert!(ok.error.is_none(), "{:?}", ok.error);

        // Un préfixe inconnu est une faute de frappe, pas un modèle OpenRouter.
        let refus = call(
            &r,
            method::MODEL_SET,
            json!({"alias": "main", "model": "codx:gpt-6"}),
        )
        .await;
        assert!(
            refus.error.expect("refus").message.contains("codx"),
            "le préfixe est nommé"
        );
    }

    /// #142 (lot 3) : `doctor` dit l'état du fournisseur Codex, signale un alias de rôle
    /// de fond qui l'aurait contourné, et ne se tait jamais sur l'identité empruntée.
    #[tokio::test]
    async fn doctor_reports_the_codex_provider() {
        let (_d, r) = rpc().await;
        let s = &r.daemon.services;
        // Éteint et sans alias : rien à dire.
        assert!(crate::doctor::codex_checks(s).await.is_empty());

        r.daemon
            .publish_config("test", |c| {
                c.providers.codex.enabled = true;
                c.models
                    .aliases
                    .insert("summarizer".into(), "codex:gpt-6-astra".into());
                Ok(vec!["providers.codex.enabled".into()])
            })
            .unwrap();
        let checks = crate::doctor::codex_checks(s).await;
        let by = |id: &str| {
            checks
                .iter()
                .find(|c| c.id == id)
                .unwrap_or_else(|| panic!("contrôle `{id}` attendu"))
                .clone()
        };
        let provider = by("provider.codex");
        assert!(!provider.ok, "activé sans compte connecté");
        assert!(provider.detail.contains("aucun compte"), "{provider:?}");

        let identity = by("provider.codex.identity");
        assert!(identity.detail.contains("codex_cli_rs"), "{identity:?}");
        assert!(
            identity.detail.contains("toléré") && identity.detail.contains("jamais garanti"),
            "l'avertissement ne se tait pas : {identity:?}"
        );

        let scope = by("provider.codex.scope");
        assert!(!scope.ok);
        assert!(scope.detail.contains("compaction"), "{scope:?}");

        // Connecté : l'état le dit, et le périmètre reste signalé.
        crate::codex_auth::store(
            s,
            &crate::codex_auth::Grant {
                access_token: "a".into(),
                refresh_token: "rr".into(),
                plan_type: "pro".into(),
                email: "moi@example.test".into(),
                expires_at: s.clock.now_ms() + 3_600_000,
                last_refresh: s.clock.now_ms(),
                ..Default::default()
            },
        )
        .unwrap();
        let checks = crate::doctor::codex_checks(s).await;
        let provider = checks
            .iter()
            .find(|c| c.id == "provider.codex")
            .expect("contrôle");
        assert!(provider.ok, "{provider:?}");
        assert!(provider.detail.contains("plan pro"), "{provider:?}");
    }

    /// #142 : sans compte connecté, `model auth codex --status` le dit ; connecté, une
    /// seconde connexion exige une déconnexion explicite.
    #[tokio::test]
    async fn only_one_chatgpt_account_at_a_time() {
        let (_d, r) = rpc().await;
        let empty = call(
            &r,
            method::MODEL_AUTH,
            json!({"provider": "codex", "action": "status"}),
        )
        .await;
        assert!(empty.error.is_none());
        assert!(empty.result.expect("état")["status"].is_null());

        crate::codex_auth::store(
            &r.daemon.services,
            &crate::codex_auth::Grant {
                access_token: "a".into(),
                refresh_token: "r".into(),
                account_id: "acc_1".into(),
                plan_type: "pro".into(),
                email: "moi@example.test".into(),
                expires_at: r.daemon.services.clock.now_ms() + 3_600_000,
                last_refresh: r.daemon.services.clock.now_ms(),
                ..Default::default()
            },
        )
        .unwrap();
        let busy = call(
            &r,
            method::MODEL_AUTH,
            json!({"provider": "codex", "action": "start"}),
        )
        .await;
        let message = busy.error.expect("refus").message;
        assert!(
            message.contains("moi@example.test") && message.contains("logout"),
            "{message}"
        );

        // Un autre fournisseur ne se connecte pas par compte.
        let other = call(
            &r,
            method::MODEL_AUTH,
            json!({"provider": "openrouter", "action": "status"}),
        )
        .await;
        assert!(other.error.expect("refus").message.contains("seul `codex`"));
    }

    #[tokio::test]
    async fn mcp_servers_are_administered_over_rpc() {
        use crate::mcp::testing::{FakeConnector, server, tool};
        let (_d, r) = rpc().await;
        let without = call(&r, method::MCP_LIST, json!({})).await;
        assert!(without.error.unwrap().message.contains("non démarré"));

        let fake = Arc::new(FakeConnector::default());
        fake.serve(
            "forge",
            server(Arc::new(std::sync::Mutex::new(vec![tool(
                "create_pr",
                json!({}),
            )]))),
        );
        let sup = crate::mcp::McpSupervisor::new(r.daemon.services.clone(), fake.clone());
        r.daemon.hooks.set_mcp(sup.clone());

        let toml = "command = \"/opt/mcp/forge\"\ntimeout = \"20s\"\n";
        let tested = call(&r, method::MCP_TEST, json!({"toml": toml, "name": "forge"})).await;
        let tested = tested.result.unwrap();
        assert_eq!(tested["ok"], true, "{tested}");
        assert_eq!(tested["tools"], 1);
        assert!(sup.statuses().await.is_empty(), "un essai n'ajoute rien");

        let added = call(&r, method::MCP_ADD, json!({"toml": toml, "name": "forge"})).await;
        let added = added.result.unwrap();
        assert_eq!(added["report"]["added"][0], "forge");
        assert_eq!(added["status"]["tool_count"], 1);

        let list = call(&r, method::MCP_LIST, json!({})).await.result.unwrap();
        assert_eq!(list["servers"][0]["name"], "forge");
        assert_eq!(list["servers"][0]["state"], "ready");

        let edited = call(
            &r,
            method::MCP_EDIT,
            json!({"name": "forge", "patch": {"timeout": "45s"}}),
        )
        .await;
        assert!(edited.error.is_none(), "{:?}", edited.error);
        assert_eq!(sup.config_of("forge").await.unwrap().timeout, "45s");

        let show = call(&r, method::MCP_SHOW, json!({"name": "forge"}))
            .await
            .result
            .unwrap();
        assert_eq!(show["tools"][0]["name"], "mcp__forge__create_pr");

        let status = call(&r, method::STATUS, json!({})).await.result.unwrap();
        assert_eq!(
            (status["mcp_ready"].clone(), status["mcp_total"].clone()),
            (json!(1), json!(1))
        );

        let rm = call(&r, method::MCP_RM, json!({"name": "forge"})).await;
        assert_eq!(rm.result.unwrap()["report"]["removed"][0], "forge");
        let missing = call(&r, method::MCP_RESTART, json!({"name": "forge"})).await;
        assert!(missing.error.unwrap().message.contains("inconnu"));
    }

    #[tokio::test]
    async fn status_and_paths() {
        let (_d, r) = rpc().await;
        let resp = call(&r, method::STATUS, json!({})).await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["config_generation"], 1);

        let resp = call(&r, method::PATHS, json!({})).await;
        let v = resp.result.unwrap();
        assert!(v["data"].as_str().unwrap().ends_with("/data"));
        assert!(v["socket"].as_str().unwrap().ends_with("rpc.sock"));
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let (_d, r) = rpc().await;
        let resp = call(&r, "methode.inventee", json!({})).await;
        assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn missing_parameter_returns_32602() {
        let (_d, r) = rpc().await;
        let resp = call(&r, method::MEM_SEARCH, json!({})).await;
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[tokio::test]
    async fn sessions_can_be_created_and_listed() {
        let (_d, r) = rpc().await;
        call(&r, method::SESSION_NEW, json!({"title":"refonte"})).await;
        let resp = call(&r, method::SESSION_LIST, json!({})).await;
        let list = resp.result.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["title"], "refonte");
    }

    #[tokio::test]
    async fn config_get_set_and_status() {
        let (_d, r) = rpc().await;
        let before = call(&r, method::CONFIG_GET, json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(before["budget"]["daily_usd"], 20.0);

        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path":"budget.daily_usd","value":50.0}),
        )
        .await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        assert_eq!(resp.result.unwrap()["generation"], 2);

        let after = call(&r, method::CONFIG_GET, json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(after["budget"]["daily_usd"], 50.0);

        let st = call(&r, method::CONFIG_STATUS, json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(st["generation"], 2);
        assert!(st["subsystems"]["mcp"].is_object());
    }

    /// Issue #16 : un réglage qui annule sa propre intention est refusé nommément ; celui
    /// qui en rend un autre inutile passe avec un avertissement.
    #[tokio::test]
    async fn self_cancelling_settings_are_refused_or_warned() {
        let (_d, r) = rpc().await;
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "tools.http_allowlist", "value": ["http://192.168.0.10:8080"]}),
        )
        .await;
        let err = resp.error.expect("refus").message;
        assert!(
            err.contains("réglage refusé") && err.contains("192.168.0.10"),
            "{err}"
        );
        let cfg = call(&r, method::CONFIG_GET, json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(
            cfg["tools"]["http_allowlist"],
            json!([]),
            "rien n'est écrit"
        );

        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "budget.session_usd", "value": 50.0}),
        )
        .await;
        let v = resp.result.expect("accepté");
        assert!(
            v["warnings"][0]
                .as_str()
                .unwrap()
                .contains("ne sera jamais atteint"),
            "{v}"
        );

        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "models.roles.classifier", "value": "absent"}),
        )
        .await;
        assert!(resp.error.unwrap().message.contains("n'existe pas"));
    }

    #[tokio::test]
    async fn unknown_config_path_is_refused() {
        let (_d, r) = rpc().await;
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path":"budget.inexistant","value":1}),
        )
        .await;
        let err = resp.error.expect("refus").message;
        assert!(err.contains("clé inconnue : budget.inexistant"), "{err}");
        // La lecture du fichier tolère les clés inconnues (#76), la saisie non.
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path":"futur.actif","value":true}),
        )
        .await;
        let err = resp.error.expect("refus").message;
        assert!(err.contains("clé inconnue : futur.actif"), "{err}");
    }

    /// #128 : une entrée nouvelle d'une table à clés libres se pose (`image_locate` absent
    /// d'une configuration écrite avant #125), vérifiée comme les autres ; une clé de
    /// structure inconnue reste refusée.
    #[tokio::test]
    async fn a_new_role_can_be_set_on_an_older_configuration() {
        let (_dir, r) = rpc().await;
        let d = r.daemon.clone();
        d.publish_config("test", |c| {
            c.models.roles.remove("image_locate");
            c.models.aliases.insert(
                "pointage".into(),
                "openrouter:bytedance/ui-tars-1.5-7b".into(),
            );
            Ok(vec!["models.roles".into()])
        })
        .unwrap();
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "models.roles.image_locate", "value": "pointage"}),
        )
        .await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        assert_eq!(
            d.services
                .config
                .config()
                .models
                .roles
                .get("image_locate")
                .map(String::as_str),
            Some("pointage")
        );
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "models.roles.image_describe", "value": "inconnu"}),
        )
        .await;
        assert!(resp.error.is_some(), "un alias inconnu reste refusé");
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "models.inexistant", "value": 1}),
        )
        .await;
        assert!(resp.error.unwrap().message.contains("clé inconnue"));
    }

    /// #138 : `config set` suit la même règle que `mcp edit` : une valeur seule remplit
    /// une liste.
    #[tokio::test]
    async fn config_set_takes_a_single_value_for_a_list() {
        let (_dir, r) = rpc().await;
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "sandbox.allow_keychain_for", "value": "mailbridge"}),
        )
        .await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        assert_eq!(
            r.daemon.services.config.config().sandbox.allow_keychain_for,
            vec!["mailbridge".to_string()]
        );
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path": "sandbox.allow_keychain_for", "value": 3}),
        )
        .await;
        assert!(resp.error.unwrap().message.contains("une liste"));
    }

    #[tokio::test]
    async fn workflow_validation_reports_paths() {
        let (_d, r) = rpc().await;
        let bad = json!({
            "metadata": {"id":"demo"},
            "entryStep": "absente",
            "steps": [{"id":"un","type":"shell","command":"echo x",
                       "transitions":[{"goto":"$done"}]}]
        });
        let resp = call(
            &r,
            method::WF_VALIDATE,
            json!({"json": bad.to_string(), "name":"demo"}),
        )
        .await;
        let v = resp.result.unwrap();
        assert_eq!(v["valid"], false);
        assert!(
            v["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["path"] == "/entryStep")
        );
    }

    #[tokio::test]
    async fn bundled_workflows_are_listed() {
        let (_d, r) = rpc().await;
        let v = call(&r, method::WF_LIST, json!({})).await.result.unwrap();
        let ids: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["id"].as_str())
            .collect();
        assert!(ids.contains(&"ticket-to-deploy"));
        assert!(ids.contains(&"deploy-generic"));
    }

    #[tokio::test]
    async fn audit_verify_is_reachable() {
        let (_d, r) = rpc().await;
        let v = call(&r, method::AUDIT_VERIFY, json!({}))
            .await
            .result
            .unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn approvals_flow_over_rpc() {
        let (_d, r) = rpc().await;
        let a = r
            .services()
            .approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                json!({}),
                vec!["Autoriser".into()],
                None,
                None,
                false,
            )
            .await
            .unwrap();

        let list = call(&r, method::APPROVALS, json!({})).await.result.unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);

        let resp = call(&r, method::APPROVE, json!({"id": a.id.0})).await;
        assert!(resp.error.is_none());
        // Une seconde décision est un conflit.
        let resp = call(&r, method::DENY, json!({"id": a.id.0})).await;
        assert_eq!(resp.error.unwrap().code, CONFLICT);
    }

    #[tokio::test]
    async fn shutdown_and_restart_flags() {
        let (_d, r) = rpc().await;
        call(&r, method::RESTART, json!({})).await;
        assert!(r.daemon.handle.wants_restart());
        assert!(r.daemon.handle.is_shutting_down());
    }

    /// #103 : le registre de métriques a un lecteur, et un tour y laisse sa trace.
    #[tokio::test]
    async fn metrics_are_readable_over_rpc() {
        let (_d, r) = rpc().await;
        penelope_observe::metrics::register_default_metrics();
        penelope_observe::metrics::counter_inc(
            "penelope_turns_total",
            &[("outcome", "answered")],
            1.0,
        );
        let text = call(&r, method::METRICS, json!({})).await.result.unwrap()["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("penelope_turns_total"), "{text}");
        assert!(text.contains("penelope_approvals_pending"), "{text}");
    }

    /// #111 : le mode d'approbation d'une session se lit et se change ; les règles
    /// inutiles portent leur remarque.
    #[tokio::test]
    async fn the_approval_mode_and_useless_rules_are_readable() {
        let (_dir, r) = rpc().await;
        let s = &r.daemon.services;
        let sid = r
            .daemon
            .chat_session_for(&crate::bus::Origin::Cli)
            .await
            .unwrap();
        let v = call(&r, method::SESSION_MODE, json!({"session": sid}))
            .await
            .result
            .unwrap();
        assert_eq!(v["mode"], "reads");
        let v = call(
            &r,
            method::SESSION_MODE,
            json!({"session": sid, "mode": "ask"}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(v["mode"], "ask");
        assert!(
            call(
                &r,
                method::SESSION_MODE,
                json!({"session": sid, "mode": "yolo"})
            )
            .await
            .error
            .is_some()
        );
        let v = call(
            &r,
            method::SESSION_MODE,
            json!({"session": sid, "mode": "default"}),
        )
        .await
        .result
        .unwrap();
        assert_eq!(v["mode"], "reads");

        for family in ["cd", "ls", "cargo test", "PASS=\"$(cut"] {
            s.policies
                .create_rule(
                    penelope_hitl::RuleScope::Tool,
                    Some("shell_exec"),
                    None,
                    Some(json!({"command": {penelope_hitl::policy::CMD_PREFIX_OP: family}})),
                    penelope_kernel::risk::PolicyDecision::Auto,
                    penelope_kernel::risk::PolicyWindow::Always,
                    None,
                )
                .await
                .unwrap();
        }
        let rules = call(&r, method::POLICIES, json!({})).await.result.unwrap();
        let note = |family: &str| {
            rules
                .as_array()
                .unwrap()
                .iter()
                .find(|x| x["arg_match"]["command"][penelope_hitl::policy::CMD_PREFIX_OP] == family)
                .map(|x| x["remarque"].clone())
                .unwrap()
        };
        assert!(note("cd").as_str().unwrap().contains("composée"));
        assert!(note("ls").as_str().unwrap().contains("lectures"));
        assert!(note("PASS=\"$(cut").as_str().unwrap().contains("jamais"));
        assert!(note("cargo test").is_null(), "règle utile, sans remarque");
    }

    /// #105 : les signaux d'une entrée se lisent, avec le facteur qu'ils donnent.
    #[tokio::test]
    async fn memory_signals_are_readable() {
        let (_dir, r) = rpc().await;
        let s = &r.daemon.services;
        s.memory
            .upsert(
                &penelope_memory::index::simple_entry(
                    "u1",
                    "Le client Martin est basé à Lyon",
                    penelope_memory::Level::Cure,
                    "2026-09-16",
                ),
                &penelope_memory::Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z"),
            )
            .await
            .unwrap();
        for _ in 0..10 {
            s.memory
                .record_recall("u1", "où est Martin ?", true)
                .await
                .unwrap();
        }
        let v = call(&r, method::MEM_SIGNALS, json!({"uid": "u1"}))
            .await
            .result
            .unwrap();
        assert_eq!(v["rappels"], 10);
        assert_eq!(v["rappels_utiles"], 10);
        assert!(v["facteur_usage"].as_f64().unwrap() > 1.15, "{v}");
        let missing = call(&r, method::MEM_SIGNALS, json!({"uid": "u2"})).await;
        assert!(missing.error.is_some());
    }

    #[tokio::test]
    async fn every_declared_method_is_either_served_or_explicitly_absent() {
        let (_d, r) = rpc().await;
        let mut unimplemented = Vec::new();
        for m in method::ALL {
            let resp = call(&r, m, json!({})).await;
            if let Some(e) = resp.error
                && e.code == METHOD_NOT_FOUND
            {
                unimplemented.push(*m);
            }
        }
        // Les méthodes non encore servies sont connues et listées : elles ne doivent pas
        // apparaître silencieusement.
        let expected: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        let actual: std::collections::BTreeSet<&str> = unimplemented.into_iter().collect();
        assert_eq!(
            actual, expected,
            "la liste des méthodes non servies a changé : mettre à jour docs/progress.md"
        );
    }
}
