//! Exécution des étapes d'un scénario : message (joué, ou tué pendant un appel d'outil),
//! mise en file, commandes de session, approbation, dernier appel facturé, historique
//! semé. Les issues rendues deviennent les lignes `outcome` du monde.

use super::super::Crash;
use super::lifecycle::shut_down;
use super::{CRASH_WAIT, HEARTBEAT, Harness, outcome_json};
use anyhow::Context as _;
use penelope_app::bus::Origin;
use penelope_app::engine::TurnIntake;
use penelope_conversation::compaction;
use penelope_daemon::runner;
use penelope_kernel::budget::UsageRecord;
use penelope_kernel::turn::Turn;
use penelope_llm::types::ChatMessage;
use penelope_ops::{purge, session_ops};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

impl Harness<'_> {
    pub(super) async fn message(
        &mut self,
        text: &str,
        crash: Option<Crash>,
        steer: Option<&str>,
    ) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        d.enqueue_message(&self.session, text, &Origin::Cli, None)
            .await?
            .context("tour non créé")?;
        let turn = self.claim().await?.context("aucun tour à réclamer")?;
        let merged = turn.merged_messages.len();
        match crash {
            None => {
                let out = match steer {
                    Some(steer) => self.steer_during_tool(turn, steer).await?,
                    None => runner::process(&d, turn, HEARTBEAT).await,
                };
                let mut v = outcome_json(&out);
                if merged > 0 {
                    v["merged"] = json!(merged);
                }
                Ok(v)
            }
            Some(Crash::DuringTool) => {
                // Notre propre copie du daemon ne doit pas survivre au processus mort.
                drop(d);
                self.crash_during_tool(turn).await?;
                Ok(json!({"outcome": "crash", "detail": "processus mort pendant l'appel d'outil"}))
            }
        }
    }

    /// Le tour part dans une tâche ; dès que l'outil simulé est appelé (réponse du modèle
    /// écrite, effet en vol, résultat absent), la tâche est tuée et les services détruits :
    /// c'est le processus qui meurt entre la réponse et le résultat d'outil.
    async fn crash_during_tool(&mut self, turn: Turn) -> anyhow::Result<()> {
        let life = self.life.take().context("processus déjà mort")?;
        let gateway = life
            .gateway
            .clone()
            .context("`crash = \"during_tool\"` demande un outil MCP simulé (`[[mcp_tools]]`)")?;
        gateway.block.store(true, Ordering::SeqCst);
        let d = life.daemon.clone();
        let task = tokio::spawn(async move { runner::process(&d, turn, HEARTBEAT).await });
        tokio::time::timeout(CRASH_WAIT, gateway.called.notified())
            .await
            .context("l'outil simulé n'a pas été appelé : rien à interrompre")?;
        task.abort();
        let _ = task.await;
        shut_down(life, &self.spec.name).await;
        self.crashed = true;
        Ok(())
    }

    /// Le tour part dans une tâche ; dès que l'outil simulé est appelé, le propriétaire
    /// écrit `text`, puis l'outil répond : le message arrive pendant l'appel (T13).
    async fn steer_during_tool(
        &mut self,
        turn: Turn,
        text: &str,
    ) -> anyhow::Result<penelope_agent::TurnOutcome> {
        let life = self.life.as_ref().context("processus mort")?;
        let gateway = life
            .gateway
            .clone()
            .context("`steer` demande un outil MCP simulé (`[[mcp_tools]]`)")?;
        gateway.block.store(true, Ordering::SeqCst);
        let d = life.daemon.clone();
        let task = tokio::spawn(async move { runner::process(&d, turn, HEARTBEAT).await });
        tokio::time::timeout(CRASH_WAIT, gateway.called.notified())
            .await
            .context("l'outil simulé n'a pas été appelé : aucun appel pendant lequel écrire")?;
        self.enqueue(text).await?;
        gateway.block.store(false, Ordering::SeqCst);
        gateway.release.notify_one();
        Ok(task.await?)
    }

    pub(super) async fn enqueue(&mut self, text: &str) -> anyhow::Result<Value> {
        self.daemon()?
            .enqueue_message(&self.session, text, &Origin::Cli, None)
            .await?
            .context("tour non créé")?;
        Ok(json!({"outcome": "queued"}))
    }

    pub(super) async fn command(&mut self, command: &str) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        let (name, args) = command
            .trim()
            .split_once(' ')
            .map(|(n, a)| (n, a.trim()))
            .unwrap_or((command.trim(), ""));
        match name {
            "/compact" => {
                let r = compaction::compact(
                    &penelope_daemon::compaction::context_of(&d),
                    &self.session,
                    compaction::Trigger::Manual,
                    None,
                )
                .await?;
                Ok(json!({
                    "text": compaction::report_text(&r),
                    "published": r.published,
                    "messages": r.messages,
                    "remaining_batches": r.remaining_batches,
                    "deferred": r.deferred,
                    "skipped": r.skipped,
                }))
            }
            "/fork" => {
                let title = (!args.is_empty()).then(|| args.to_string());
                let v = session_ops::fork(&d.services, &self.session, title).await?;
                self.session = v["session"]
                    .as_str()
                    .context("fork sans identifiant")?
                    .to_string();
                Ok(v)
            }
            "/rewind" => {
                let turns: usize = if args.is_empty() { 1 } else { args.parse()? };
                session_ops::rewind(&d.services, &d.bus, &self.session, turns).await
            }
            "/purge" => purge::session(&d.services, &self.session, "scénario").await,
            other => anyhow::bail!(
                "commande inconnue `{other}` : /compact, /fork [titre], /rewind [n], /purge"
            ),
        }
    }

    pub(super) async fn approve(&mut self) -> anyhow::Result<Value> {
        let d = self.daemon()?;
        let s = self.services()?;
        let pending = s.approvals.pending(50).await?;
        let approval = pending
            .iter()
            .find(|a| a.session_id.as_deref() == Some(self.session.as_str()))
            .context("aucune demande en attente pour la session")?;
        let id = approval.id.0.clone();
        let decision = penelope_hitl::Decision::approve_once("cli");
        let resumed = penelope_daemon::agent::decide_approval(&s, &id, &decision).await?;
        d.enqueue_resume(&self.session, &id, &Origin::Cli)
            .await?
            .context("reprise non créée")?;
        let turn = self.claim().await?.context("aucune reprise à réclamer")?;
        let mut v = outcome_json(&runner::process(&d, turn, HEARTBEAT).await);
        v["approval"] = json!(id);
        v["resumable"] = json!(resumed);
        Ok(v)
    }

    pub(super) async fn usage(&mut self, prompt: u64) -> anyhow::Result<Value> {
        let s = self.services()?;
        let model = s
            .config
            .config()
            .alias_model("main")
            .context("alias `main`")?
            .to_string();
        s.budget
            .record(UsageRecord {
                session_id: Some(self.session.clone()),
                model: model.clone(),
                provider: "mock".into(),
                role: Some("chat".into()),
                prompt,
                ..Default::default()
            })
            .await?;
        Ok(json!({"model": model, "prompt": prompt}))
    }

    pub(super) async fn seed(
        &mut self,
        exchanges: usize,
        user: &str,
        assistant: &str,
        filler: &str,
        tokens: u64,
    ) -> anyhow::Result<Value> {
        let s = self.services()?;
        let fill = |template: &str, i: usize| {
            template
                .replace("{i}", &i.to_string())
                .replace("{filler}", filler)
        };
        for i in 1..=exchanges {
            s.context
                .history
                .append(
                    &self.session,
                    &ChatMessage::user(fill(user, i)),
                    tokens,
                    0,
                    false,
                    None,
                )
                .await?;
            s.context
                .history
                .append(
                    &self.session,
                    &ChatMessage::assistant(fill(assistant, i)),
                    tokens,
                    0,
                    false,
                    None,
                )
                .await?;
        }
        Ok(json!({"messages": exchanges * 2, "tokens_each": tokens}))
    }
}
