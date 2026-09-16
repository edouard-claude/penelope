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
        match self.dispatch(&req.method, &params).await {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => {
                let code = classify(&e);
                RpcResponse::err(id, code, e.to_string())
            }
        }
    }

    /// Appel en processus, pour les canaux qui vivent dans le daemon (Telegram).
    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.dispatch(method, &params).await
    }

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
            method::DOCTOR => Ok(json!(crate::doctor::run(s).await)),
            method::SHUTDOWN => {
                self.daemon.handle.shutdown();
                Ok(json!({"ok": true}))
            }
            method::RESTART => {
                self.daemon.handle.request_restart();
                Ok(json!({"ok": true}))
            }

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
                Ok(json!({"session": sid, "stopped": self.daemon.bus.cancel_session(&sid)}))
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
                Ok(json!({"generation": g}))
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
            method::SESSION_MODEL => {
                let sid = self.session_param(p).await?;
                match p.get("alias").and_then(|a| a.as_str()) {
                    None | Some("") => {}
                    Some("auto") => self.daemon.pin_model(&sid, None).await?,
                    Some(alias) => self.daemon.pin_model(&sid, Some(alias)).await?,
                }
                self.daemon.session_model_view(&sid).await
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
                Ok(json!({
                    "aliases": aliases,
                    "routing": routing_view,
                    "catalog_size": s.catalog.len(),
                    "models": models,
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
                let d = if always {
                    penelope_hitl::Decision::approve_always("cli")
                } else {
                    penelope_hitl::Decision::approve_once("cli")
                };
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
            method::POLICIES => Ok(serde_json::to_value(s.policies.active_rules().await?)?),
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
                let known = crate::runtime::workflow_known(&s.config.config(), &s.mcp_tools).await;
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
            method::WF_CONTROL => {
                let run = required_str(p, "run")?;
                let op = required_str(p, "op")?;
                let control = penelope_workflow::Control::parse(&op)
                    .ok_or_else(|| anyhow::anyhow!("opération inconnue : {op}"))?;
                let state = s.runs.control(&run, &control).await?;
                Ok(json!({"state": state.as_str()}))
            }

            // ------------------------------------------------------------ schedules
            method::SCHEDULE_LIST => Ok(serde_json::to_value(s.schedules.list().await?)?),
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
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::BACKUP => {
                let dest = s.platform.dirs.data().join("backups").join(format!(
                    "penelope-{}.db",
                    s.clock.now_rfc3339().replace(':', "-")
                ));
                s.store.backup_to(&dest)?;
                Ok(json!({"path": dest}))
            }

            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}

/// Montant lisible : six décimales suffisent à distinguer un appel d'un autre.
pub fn round_usd(x: f64) -> f64 {
    (x * 1_000_000.0).round() / 1_000_000.0
}

impl Rpc {
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
    pub async fn handle_streaming<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        req: RpcRequest,
        out: &mut W,
    ) -> anyhow::Result<()> {
        let params = req.params.clone().unwrap_or(json!({}));
        let mut rx = self.daemon.bus.subscribe();
        match req.method.as_str() {
            method::TAIL => loop {
                let ev = match rx.recv().await {
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
                                if ev.turn_id == id.as_str() {
                                    if let Some(se) = to_stream_event(&ev) {
                                        write_line(out, &notification(&se)).await?;
                                    }
                                }
                            }
                            let outcome = outcome?;
                            let resp = RpcResponse::ok(req.id.clone(), outcome_json(&sid, id.as_str(), &outcome));
                            write_line(out, &serde_json::to_value(resp)?).await?;
                            return Ok(());
                        }
                        ev = rx.recv() => {
                            match ev {
                                Ok(ev) if ev.turn_id == id.as_str() => {
                                    if matches!(ev.kind, BusKind::Finished(_)) {
                                        continue;
                                    }
                                    if let Some(se) = to_stream_event(&ev) {
                                        write_line(out, &notification(&se)).await?;
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
        TurnOutcome::LoopAborted { report } => ("loop_aborted", json!({"report": report})),
        TurnOutcome::Cancelled => ("cancelled", json!({})),
        TurnOutcome::BudgetExceeded { scope } => ("budget_exceeded", json!({"scope": scope})),
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
        BusKind::Finished(TurnOutcome::LoopAborted { report }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: format!("boucle détectée, tour arrêté\n{report}"),
        },
        BusKind::Finished(TurnOutcome::BudgetExceeded { scope }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: format!("budget `{scope}` atteint"),
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
fn set_config_path(daemon: &Daemon, path: &str, value: Value) -> anyhow::Result<u64> {
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
                if !obj.contains_key(*part) {
                    return Err(penelope_kernel::KernelError::config(format!(
                        "clé inconnue : {path_owned}"
                    )));
                }
                obj.insert((*part).to_string(), value.clone());
            } else {
                cur = cur.get_mut(part).ok_or_else(|| {
                    penelope_kernel::KernelError::config(format!("clé inconnue : {path_owned}"))
                })?;
            }
        }
        *c = serde_json::from_value(v).map_err(penelope_kernel::KernelError::Json)?;
        Ok(vec![path_owned.clone()])
    })?;
    Ok(generation)
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
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<RpcRequest>(&line) {
                    Ok(req) if req.method == method::CHAT_STREAM || req.method == method::TAIL => {
                        let id = req.id.clone();
                        if let Err(e) = rpc.handle_streaming(req, &mut write).await {
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

    #[tokio::test]
    async fn unknown_config_path_is_refused() {
        let (_d, r) = rpc().await;
        let resp = call(
            &r,
            method::CONFIG_SET,
            json!({"path":"budget.inexistant","value":1}),
        )
        .await;
        assert!(resp.error.is_some());
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

    #[tokio::test]
    async fn every_declared_method_is_either_served_or_explicitly_absent() {
        let (_d, r) = rpc().await;
        let mut unimplemented = Vec::new();
        for m in method::ALL {
            let resp = call(&r, m, json!({})).await;
            if let Some(e) = resp.error {
                if e.code == METHOD_NOT_FOUND {
                    unimplemented.push(*m);
                }
            }
        }
        // Les méthodes non encore servies sont connues et listées : elles ne doivent pas
        // apparaître silencieusement.
        let expected: std::collections::BTreeSet<&str> = [
            "session.fork",
            "session.rewind",
            "session.compact",
            "mcp.list",
            "mcp.show",
            "mcp.add",
            "mcp.edit",
            "mcp.rm",
            "mcp.enable",
            "mcp.disable",
            "mcp.restart",
            "mcp.test",
            "mcp.auth",
            "mcp.logs",
            "skill.rollback",
            "wf.run",
            "schedule.add",
            "schedule.run_now",
            "mem.history",
            "mem.restore",
            "mem.reindex",
            "mem.forget",
            "mem.candidates",
            "mem.dream",
            "mem.learned",
            "vault.sync",
            "vault.check",
            "import.hermes",
            "export",
            "restore",
            "store.rebuild",
            "eval.run",
            "upgrade",
        ]
        .into_iter()
        .collect();
        let actual: std::collections::BTreeSet<&str> = unimplemented.into_iter().collect();
        assert_eq!(
            actual, expected,
            "la liste des méthodes non servies a changé : mettre à jour docs/progress.md"
        );
    }
}
