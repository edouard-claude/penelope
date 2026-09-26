//! Exécution d'un tour de conversation, de la file à la réponse (§3.3, §10.3).
//!
//! Un tour : message utilisateur écrit une seule fois, choix du modèle (alias collant,
//! classifieur à la première demande), prompt assemblé, boucle d'agent sur le
//! transcript persistant, événements publiés sur le bus.

use crate::runtime::{Core, Daemon};
use penelope_agent::{AgentLoop, TurnOutcome, TurnSink, TurnSpec};
use penelope_app::bus::{BusSink, Origin};
use penelope_app::codex_scope;
use penelope_app::engine::{SessionModels, Transcriber, TurnIntake};
use penelope_app::helpers::{last_model_key, pin_key};
use penelope_context::journal::{Provenance, UserSource};
use penelope_conversation::{SessionConversation, TurnInbox, compaction};
use penelope_executor::executor::{NativeToolExecutor, ToolEnv};
use penelope_executor::executor::{chat_tool_defs, default_workspaces};
use penelope_executor::tools_on_demand;
use penelope_kernel::ids::TurnId;
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::{Turn, TurnKind};
use penelope_llm::router::{CLASSIFIER_PROMPT, Classification, RouteReason};
use penelope_llm::types::{ChatMessage, ChatRequest};
use penelope_llm::{CancelToken, RouteInput, Router, StickyModel, collect_stream};
use penelope_vault::{embeddings, review, usage_feedback};
use serde_json::{Value, json};
use std::sync::Arc;

impl Daemon {
    /// Exécute un tour complet et publie son issue.
    pub async fn run_turn(self: &Arc<Self>, turn: &Turn) -> TurnOutcome {
        if !turn.merged_messages.is_empty()
            && let Err(error) = self
                .services
                .events
                .append(
                    penelope_kernel::event::EventDraft::new(
                        "turn.merged",
                        json!({"turn": turn.id.as_str(), "count": turn.merged_messages.len(), "phase": "queued"}),
                    )
                    .session(&turn.session_id),
                )
                .await
        {
            tracing::warn!(turn = %turn.id, %error, "événement de fusion non enregistré");
        }
        let origin = Origin::from_payload(&turn.payload);
        let active = self.bus.begin(turn.id.as_str(), &turn.session_id, &origin);
        let sink = BusSink {
            bus: self.bus.clone(),
            turn_id: turn.id.as_str().to_string(),
            session_id: turn.session_id.clone(),
            origin: origin.clone(),
        };
        // Un tour tombé avant sa boucle est borné ici : chaque sortie a son `turn.finished`.
        let meta = penelope_agent::TurnMeta::of(turn);
        let result = self
            .execute_turn(turn, &origin, active.cancel.clone(), &sink, &meta)
            .await;
        crate::agent::close_unopened(&self.services, &turn.session_id, &meta, &result).await;
        let outcome = match result {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(turn = %turn.id, error = %e, "tour en échec");
                TurnOutcome::Failed {
                    error: e.to_string(),
                }
            }
        };
        self.bus.end(&turn.session_id, turn.id.as_str());
        self.handle.record_turn();
        // Un tour a pu écrire en mémoire ou créer une intention : vecteurs manquants.
        embeddings::spawn_backfill(self.embedder());
        // Retour d'usage (#105) : un souvenir servi n'est utile que si la réponse le
        // reprend ; un tour qui attend une approbation garde sa liste pour sa reprise.
        match &outcome {
            TurnOutcome::Answered { text: answer, .. } => {
                let said = turn
                    .payload
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                usage_feedback::judge(&self.services, &turn.session_id, said, answer).await;
            }
            TurnOutcome::AwaitingApproval { .. } => {}
            _ => usage_feedback::forget(&self.services, &turn.session_id).await,
        }
        // Apprentissage continu (§6.6) : revue de fond des échanges qui le méritent.
        if let (TurnKind::Message, TurnOutcome::Answered { text: answer, .. }) =
            (turn.kind, &outcome)
        {
            let said = turn
                .payload
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            // Un accord court (« ok », « go ») relit la proposition qu'il accepte (#108).
            let previous = if review::is_short_agreement(said) {
                review::previous_answer(&self.services, &turn.session_id).await
            } else {
                None
            };
            if self.services.config.config().memory.review_max_candidates > 0
                && let Some(matter) = review::review_matter(said, previous.as_deref())
            {
                review::spawn(
                    self.services.clone(),
                    self.providers.clone(),
                    turn.session_id.clone(),
                    turn.id.to_string(),
                    said.to_string(),
                    answer.clone(),
                    matter,
                );
            }
            // Premier échange d'une session sans titre : un titre lisible (issue #2).
            if self.services.config.config().context.auto_title
                && !said.trim().is_empty()
                && let Ok(Some(sess)) = self.services.sessions.get(&turn.session_id).await
                && penelope_conversation::titles::wants_title(&sess)
            {
                penelope_conversation::titles::spawn(
                    self.services.clone(),
                    self.providers.clone(),
                    self.hooks.delivery(),
                    turn.session_id.clone(),
                    said.to_string(),
                    answer.clone(),
                );
            }
        }
        // Frontière de tour : résumé prêt publié, compaction de fond lancée si besoin.
        compaction::after_turn(&crate::compaction::context_of(self), &turn.session_id).await;
        outcome
    }

    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (engine/turn.rs)
    async fn execute_turn(
        self: &Arc<Self>,
        turn: &Turn,
        origin: &Origin,
        cancel: CancelToken,
        sink: &dyn TurnSink,
        meta: &penelope_agent::TurnMeta,
    ) -> anyhow::Result<TurnOutcome> {
        let s = self.services.clone();
        let cfg = s.config.config();
        let session = s.sessions.require(&turn.session_id).await?;
        let text = turn
            .payload
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        let images: Vec<std::path::PathBuf> = turn
            .payload
            .get("images")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(std::path::PathBuf::from))
                    .collect()
            })
            .unwrap_or_default();

        // 0. Frontière d'épisode (§6.6) : inactivité ou changement de sujet, vérifiés une
        // seule fois par message, avant qu'il soit écrit. Le message du tour entre au
        // journal sous l'identifiant du tour, qui dit s'il est déjà écrit (§2.7, T16).
        let history = &s.context.history;
        let recorded = history.recorded(&turn.session_id, turn.id.as_str()).await?;
        let prov = |source| Provenance::queued(source, turn.id.as_str(), &turn.enqueued_at);
        let mut episode = session.episode_seq;
        if turn.kind == TurnKind::Message && !recorded {
            let (s, p) = (self.services.clone(), self.providers.clone());
            episode = penelope_vault::episodes::before_message(s, p, &session, &text).await?;
        }

        // 1. Le message utilisateur, écrit une seule fois même si le tour est rejoué. Avec
        // des photos, il attend le choix du modèle : lui les montrer ou les faire décrire.
        if turn.kind != TurnKind::Resume && !text.trim().is_empty() && images.is_empty() {
            let (content, source) = match turn.kind {
                TurnKind::Trigger => (
                    format!("[déclencheur planifié] {text}"),
                    UserSource::Trigger,
                ),
                TurnKind::Nudge => (format!("[relance] {text}"), UserSource::Nudge),
                _ => (text.clone(), UserSource::Owner),
            };
            let tokens = s.context.estimator.text_tokens("default", &content);
            let user = ChatMessage::user(content);
            history
                .append_queued(&turn.session_id, &user, tokens, episode, &prov(source))
                .await?;
        }
        if turn.kind == TurnKind::Message {
            for merged in &turn.merged_messages {
                let Some(message) = merged.payload.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let tokens = s.context.estimator.text_tokens("default", message);
                let prov =
                    Provenance::queued(UserSource::Merged, merged.id.as_str(), &merged.enqueued_at);
                let user = ChatMessage::user(message);
                s.context
                    .history
                    .append_queued(&turn.session_id, &user, tokens, episode, &prov)
                    .await?;
            }
        }

        // Tour d'origine : une reprise après approbation compte pour la requête initiale.
        let origin_turn = match turn.kind {
            TurnKind::Resume => {
                let approval = match turn.payload.get("approval_id").and_then(|a| a.as_str()) {
                    Some(id) => s.approvals.get(id).await?,
                    None => None,
                };
                approval
                    .and_then(|a| {
                        a.payload
                            .get("turn_id")
                            .and_then(|t| t.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_else(|| turn.id.to_string())
            }
            _ => turn.id.to_string(),
        };

        // 2. Modèle : alias collant, sinon routage.
        let classified = if text.trim().is_empty() && !images.is_empty() {
            "(photo)".to_string()
        } else {
            text.clone()
        };
        // Le vecteur du message ne dépend pas du modèle choisi : les deux allers-retours
        // partent ensemble au lieu de s'attendre (issue #74).
        let want_vector = turn.kind == TurnKind::Message;
        let ((alias, model_id), vector) = tokio::join!(
            self.select_model(&session, &classified, &origin_turn),
            async {
                if want_vector {
                    embeddings::query_vector(&self.embedder(), &text).await
                } else {
                    None
                }
            }
        );

        // Photos : montrées au modèle de la session s'il lit les images, sinon décrites par
        // le rôle `image_describe` et jointes en texte (§10.4).
        if turn.kind == TurnKind::Message && !images.is_empty() && !recorded {
            let message = self
                .photo_message(&text, &images, &model_id, &turn.session_id, &origin_turn)
                .await;
            let tokens = s.context.estimator.message_tokens(&model_id, &message);
            history
                .append_queued(
                    &turn.session_id,
                    &message,
                    tokens,
                    episode,
                    &prov(UserSource::Photo),
                )
                .await?;
        }

        // 3. Provider. L'abonnement ChatGPT ne sert que les tours du propriétaire : une
        // planification ou un travail interne se replie ici, sans bruit (#142).
        let model_id = codex_scope::for_origin(&self.services, &model_id, origin).await;
        let provider = match self.provider_for(&model_id).await {
            Ok(p) => p,
            Err(error) => return Ok(TurnOutcome::Failed { error }),
        };

        // 4. Prompt et transcript.
        let mcp_lines = match self.hooks.mcp() {
            Some(m) => m.server_lines().await,
            None => Vec::new(),
        };
        // Reprise d'une session froide au-delà du seuil : résumée avant l'appel (issue #40).
        compaction::before_turn(
            &crate::compaction::context_of(self),
            &turn.session_id,
            &model_id,
            Some(&origin_turn),
        )
        .await;
        let (mut tiers, recalled) = penelope_conversation::build_turn_prompt(
            &s,
            &text,
            &mcp_lines,
            None,
            (session.kind == SessionKind::Chat).then_some((turn.session_id.as_str(), episode)),
            vector.clone(),
        )
        .await;
        usage_feedback::served(
            &s,
            &turn.session_id,
            session.kind == SessionKind::Chat,
            &recalled,
            &text,
        )
        .await;
        if let Some(block) = self.intents_block(turn, &text, vector.as_deref()).await? {
            tiers.volatile.push_str("\n\n");
            tiers.volatile.push_str(&block);
        }
        // Cache de prompt (issue #17) : le contexte volatil reste avec son message, et un
        // préfixe modifié attend que le cache soit froid.
        crate::cache_audit::freeze_volatile(&s, &turn.session_id, &mut tiers).await?;
        crate::cache_audit::stable_prefix(&s, &turn.session_id, &mut tiers).await?;
        // Un résumé prêt depuis le tour précédent (ou avant un redémarrage) est publié
        // avant de construire la projection.
        if let Err(e) =
            compaction::publish_pending(&crate::compaction::context_of(self), &turn.session_id)
                .await
        {
            tracing::warn!(session = %turn.session_id, error = %e, "résumé en attente non publié");
        }
        let conv = SessionConversation::new(s.clone(), &turn.session_id, &model_id, tiers, episode)
            .with_compactor(Arc::new(compaction::OverflowCompactor {
                context: crate::compaction::context_of(self),
                turn_id: Some(origin_turn.clone()),
            }));
        let inbox = TurnInbox::for_turn(&s, turn, self.hooks.delivery(), &cancel);

        // 5. Outils.
        let mut exec = NativeToolExecutor::new(
            s.clone(),
            ToolEnv {
                session_id: turn.session_id.clone(),
                run_id: None,
                origin: origin.clone(),
                workspaces: default_workspaces(&s),
                in_workflow: false,
                turn_model: Some(penelope_executor::selfknow::TurnModel {
                    alias: alias.clone(),
                    model_id: model_id.clone(),
                }),
            },
        );
        exec.admin = Some(self.core.clone() as Arc<dyn penelope_executor::selfknow::Admin>);
        exec.messenger = self.hooks.messenger();
        exec.mcp = self.hooks.mcp();
        exec.orchestrator = self.hooks.orchestrator();

        let router = Router::new(s.catalog.clone());
        let spec = TurnSpec {
            session_id: turn.session_id.clone(),
            run_id: None,
            turn_id: Some(origin_turn),
            model_id: model_id.clone(),
            fallback_models: router
                .fallback_chain(&cfg, &alias)
                .into_iter()
                .map(|d| d.model_id)
                .collect(),
            tools: {
                // Noyau + outils à la demande découverts par la session (#104).
                let discovered = tools_on_demand::exposed_for_turn(&s, &turn.session_id).await;
                let mut tools = chat_tool_defs(&discovered);
                if let Some(m) = self.hooks.mcp() {
                    tools.extend(m.eager_tools().await);
                }
                tools
            },
            allowed_tools: Vec::new(),
            cancel,
        };

        let outcome = AgentLoop::new(crate::agent::judged(&s, self.providers.clone()), provider)
            .with_inbox(inbox)
            .run_conversation_as(&spec, Some(meta), &conv, &exec, sink)
            .await;
        // Estimation locale ou prompt réellement facturé : l'un ou l'autre au-delà du seuil
        // demande la compaction de fond (issue #40).
        compaction::after_answer(
            &crate::compaction::context_of(self),
            &turn.session_id,
            &model_id,
            spec.turn_id.clone(),
            conv.wants_compaction(),
        )
        .await;
        outcome
    }
}

/// Session de chat active d'un canal ; créée au besoin. Elle reste dans engine.rs : la
/// liaison de session nomme encore le canal (`[channel.allowed]`) jusqu'à T37.
async fn chat_session(
    s: &penelope_app::services::Services,
    origin: &Origin,
) -> anyhow::Result<String> {
    match origin {
        Origin::Telegram {
            chat_id, topic_id, ..
        } => {
            if let Some(sess) = s.sessions.find_by_topic(*chat_id, *topic_id).await? {
                return Ok(sess.id.to_string());
            }
            let sess = s.sessions.create(SessionKind::Chat, None).await?;
            s.sessions
                .bind_telegram(sess.id.as_str(), *chat_id, *topic_id)
                .await?;
            Ok(sess.id.to_string())
        }
        _ => {
            if let Some(id) = s.kv_get("cli.session").await?
                && let Some(sess) = s.sessions.get(&id).await?
                && sess.state == "active"
            {
                return Ok(id);
            }
            let sess = s
                .sessions
                .create(SessionKind::Chat, Some("CLI".into()))
                .await?;
            s.kv_set("cli.session", sess.id.as_str()).await?;
            Ok(sess.id.to_string())
        }
    }
}

impl Core {
    /// Intentions armées que ce message réveille (§6.9) : elles tirent une fois, et leur
    /// texte rejoint le contexte volatil du tour. Un tour rejoué retrouve le même bloc
    /// sans tirer une seconde fois.
    async fn intents_block(
        &self,
        turn: &Turn,
        text: &str,
        vector: Option<&[f32]>,
    ) -> anyhow::Result<Option<String>> {
        if turn.kind != TurnKind::Message || text.trim().is_empty() {
            return Ok(None);
        }
        let key = format!("turn.intents.{}", turn.id);
        if let Some(saved) = self.services.kv_get(&key).await? {
            return Ok((!saved.is_empty()).then_some(saved));
        }
        let s = &self.services;
        let max = s.config.config().memory.intents.max_per_turn.max(1);
        let matches = s.intents.matching(text, vector, 0.5, max).await?;
        let mut lines = Vec::new();
        for i in &matches {
            s.intents.fire(&i.id).await?;
            s.events
                .append(
                    penelope_kernel::event::EventDraft::new(
                        "intent.fired",
                        json!({"intent": i.id, "texte": i.texte}),
                    )
                    .session(&turn.session_id),
                )
                .await?;
            lines.push(format!("- {} (intention {})", i.texte, i.id));
        }
        let block = if lines.is_empty() {
            String::new()
        } else {
            format!(
                "Intentions armées que ce message concerne, à honorer dans la réponse :\n{}",
                lines.join("\n")
            )
        };
        s.kv_set(&key, &block).await?;
        Ok((!block.is_empty()).then_some(block))
    }
}

mod admin;
mod aliases;
mod classify;
mod intake;
mod media;
mod models;
pub use aliases::conversation_aliases;
use classify::classification_schema;
pub use classify::parse_classification;

#[cfg(test)]
mod tests;
