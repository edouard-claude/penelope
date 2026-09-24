//! Méthodes de conversation et de session.

use super::*;

/// Délai maximal d'attente d'une réponse par `chat.send`.
const CHAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

impl Rpc {
    /// Un message, son arrêt ; le flux passe par la socket.
    pub(super) async fn chat(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
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
                // Les jobs d'outils de la session : ils tournent hors du tour, donc hors
                // de portée de `cancel_session` (issue #204).
                let jobs = s.jobs.cancel_session(&sid);
                Ok(json!({"session": sid, "stopped": stopped, "dropped": dropped, "jobs": jobs}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }

    /// Cycle de vie et réglages d'une session.
    pub(super) async fn sessions(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::SESSION_CLOSE => {
                let query = required_str(p, "session")?;
                let sess = crate::session_ops::resolve(s, &query)
                    .await
                    .map_err(anyhow::Error::msg)?;
                crate::session_ops::close(
                    &self.daemon.services,
                    self.daemon.providers.clone(),
                    &self.daemon.bus,
                    sess.id.as_str(),
                )
                .await
            }
            method::SESSION_PURGE => {
                let query = required_str(p, "session")?;
                let sess = crate::session_ops::resolve(s, &query)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let id = sess.id.to_string();
                // Le tour en cours est arrêté et la file vidée avant d'effacer.
                crate::session_ops::silence(
                    &self.daemon.services,
                    &self.daemon.bus,
                    &id,
                    "session purgée",
                )
                .await?;
                let reason = p
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("demande du propriétaire");
                crate::purge::session(&self.daemon.services, &id, reason).await
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
                self.daemon.services.kv_set("cli.session", &sid).await?;
                Ok(json!({"session": sid}))
            }
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
                crate::session_ops::fork(&self.daemon.services, &sid, title).await
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
                crate::session_ops::rewind(&self.daemon.services, &self.daemon.bus, &sid, turns)
                    .await
            }
            method::EXPORT => {
                let what = p.get("what").and_then(|w| w.as_str()).unwrap_or("session");
                let id = match (what, p.get("id").and_then(|i| i.as_str())) {
                    ("session", None) => Some(self.session_param(p).await?),
                    (_, id) => id.map(String::from),
                };
                crate::session_ops::export(&self.daemon.services, what, id.as_deref()).await
            }
            method::SESSION_EXPORT => {
                let sid = required_str(p, "session")?;
                let entries = s.context.history.load(&sid, 0).await?;
                Ok(serde_json::to_value(entries)?)
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
                        crate::session_project::set(s, &sid, None).await
                    }
                    Some(name) => crate::session_project::set(s, &sid, Some(name)).await,
                }
                let (project, how) = crate::session_project::of_session(s, &sid).await;
                let known: Vec<String> =
                    crate::session_project::known(s).await.into_iter().collect();
                Ok(json!({"session": sid, "project": project, "how": how, "known": known}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}
