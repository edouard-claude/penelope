//! Exécution d'un tour de conversation, de la file à la réponse (§3.3, §10.3).
//!
//! Un tour : message utilisateur écrit une seule fois, choix du modèle (alias collant,
//! classifieur à la première demande), prompt assemblé, boucle d'agent sur le
//! transcript persistant, événements publiés sur le bus.

use crate::agent::{AgentLoop, TurnEvent, TurnOutcome, TurnSink, TurnSpec};
use crate::bus::{Bus, BusEvent, BusKind, Origin};
use crate::conversation::SessionConversation;
use crate::executor::{NativeToolExecutor, ToolEnv, chat_tool_defs, default_workspaces};
use crate::runtime::Daemon;
use penelope_kernel::ids::TurnId;
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::{Turn, TurnKind};
use penelope_llm::router::{CLASSIFIER_PROMPT, Classification, RouteReason};
use penelope_llm::types::{ChatMessage, ChatRequest};
use penelope_llm::{CancelToken, RouteInput, Router, StickyModel, collect_stream};
use serde_json::{Value, json};
use std::sync::Arc;

fn pin_key(session_id: &str) -> String {
    format!("session.model_pin.{session_id}")
}

pub(crate) fn last_model_key(session_id: &str) -> String {
    format!("session.model_last.{session_id}")
}

/// Frontière qui a fait reclasser le dernier message (#82), vide sinon.
fn last_model_why_key(session_id: &str) -> String {
    format!("session.model_last.why.{session_id}")
}

/// Libellé d'une frontière, pour `/model`.
fn boundary_label(b: &str) -> &'static str {
    match b {
        "compaction" => "contexte compacté",
        "episode" => "nouvel épisode",
        _ => "pause au-delà de la durée du cache",
    }
}

/// Alias proposés pour la conversation : les étages du routage et le rôle
/// `chat_default`, puis les alias ajoutés à la main, jamais ceux réservés à la
/// compaction, aux images, aux embeddings ou à la transcription.
pub fn conversation_aliases(cfg: &penelope_kernel::config::Config) -> Vec<String> {
    const NOT_CHAT: &[&str] = &[
        "compaction",
        "image_generate",
        "image_describe",
        "embedding",
        "stt",
    ];
    let reserved: std::collections::BTreeSet<&str> = cfg
        .models
        .roles
        .iter()
        .filter(|(role, _)| NOT_CHAT.contains(&role.as_str()))
        .map(|(_, alias)| alias.as_str())
        .collect();
    let routing = &cfg.models.routing;
    let mut out: Vec<String> = Vec::new();
    for a in [
        routing.low.clone(),
        routing.medium.clone(),
        routing.high.clone(),
        cfg.role_alias("chat_default"),
    ] {
        if cfg.alias_model(&a).is_some() && !out.contains(&a) {
            out.push(a);
        }
    }
    for a in cfg.models.aliases.keys() {
        if !reserved.contains(a.as_str()) && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

#[async_trait::async_trait]
impl crate::selfknow::Admin for Daemon {
    fn uptime_s(&self) -> u64 {
        self.handle.uptime_s(self.services.clock.now_ms())
    }

    async fn set_config(&self, path: &str, value: Value) -> Result<u64, String> {
        let g =
            crate::rpc::set_config_path(&self.services, path, value).map_err(|e| e.to_string())?;
        self.invalidate_providers().await;
        Ok(g)
    }

    async fn backup_status(&self) -> Result<Value, String> {
        Ok(crate::backup::status(self).await)
    }

    async fn send_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        args: &Value,
    ) -> Result<Value, String> {
        crate::voice::tool(self, session_id, origin, args).await
    }

    async fn memory_search(&self) -> Value {
        crate::embeddings::search_mode(self)
            .await
            .unwrap_or_else(|e| json!({"error": e.to_string()}))
    }

    async fn mcp_servers(&self) -> Value {
        let Some(sup) = self.hooks.mcp_supervisor() else {
            return json!("superviseur MCP non démarré");
        };
        json!({
            "dir": sup.dir(),
            "servers": sup.statuses().await,
            "invalid": sup.invalid(),
            "admin": "penelope mcp list|show|restart|logs|test ; déclarations dans mcp.d/*.toml, prises en compte à chaud",
        })
    }
}

/// Sink qui publie les événements d'un tour sur le bus.
pub struct BusSink {
    pub bus: Arc<Bus>,
    pub turn_id: String,
    pub session_id: String,
    pub origin: Origin,
}

impl TurnSink for BusSink {
    fn emit(&self, event: TurnEvent) {
        self.bus.publish(BusEvent {
            turn_id: self.turn_id.clone(),
            session_id: self.session_id.clone(),
            origin: self.origin.clone(),
            kind: BusKind::Event(event),
        });
    }
}

impl Daemon {
    /// Met un message en file pour une session. `dedup` rend l'ajout idempotent.
    pub async fn enqueue_message(
        &self,
        session_id: &str,
        text: &str,
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        let payload = json!({"text": text, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(session_id, TurnKind::Message, payload, dedup, 0)
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    /// Met en file un message accompagné de photos (chemins enregistrés par
    /// [`crate::media::save_photo`]).
    pub async fn enqueue_message_with_images(
        &self,
        session_id: &str,
        text: &str,
        images: &[std::path::PathBuf],
        origin: &Origin,
        dedup: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        let images: Vec<String> = images
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        let payload = json!({"text": text, "images": images, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(session_id, TurnKind::Message, payload, dedup, 0)
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    /// Relance la réponse d'une session sur son transcript, sans nouveau message
    /// (bouton « Réessayer » après un échec).
    pub async fn enqueue_retry(
        &self,
        session_id: &str,
        origin: &Origin,
        token: &str,
    ) -> anyhow::Result<Option<TurnId>> {
        let payload = json!({"retry": true, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(
                session_id,
                TurnKind::Resume,
                payload,
                Some(format!("retry:{token}")),
                10,
            )
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    /// Remet en file la suite d'un tour suspendu par une approbation.
    pub async fn enqueue_resume(
        &self,
        session_id: &str,
        approval_id: &str,
        origin: &Origin,
    ) -> anyhow::Result<Option<TurnId>> {
        // Un run de workflow reprend par son pilote, pas par un tour de conversation.
        if let Ok(Some(sess)) = self.services.sessions.get(session_id).await
            && sess.kind == penelope_kernel::session::SessionKind::WorkflowRun
        {
            self.workflows.wake();
            return Ok(None);
        }
        let payload = json!({"approval_id": approval_id, "origin": origin.to_value()});
        let id = self
            .services
            .turns
            .enqueue(
                session_id,
                TurnKind::Resume,
                payload,
                Some(format!("resume:{approval_id}")),
                10,
            )
            .await?;
        self.bus.notify_enqueued();
        Ok(id)
    }

    /// Session de chat active d'un canal ; créée au besoin.
    pub async fn chat_session_for(&self, origin: &Origin) -> anyhow::Result<String> {
        let s = &self.services;
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
                if let Some(id) = self.services.kv_get("cli.session").await?
                    && let Some(sess) = s.sessions.get(&id).await?
                    && sess.state == "active"
                {
                    return Ok(id);
                }
                let sess = s
                    .sessions
                    .create(SessionKind::Chat, Some("CLI".into()))
                    .await?;
                self.services
                    .kv_set("cli.session", sess.id.as_str())
                    .await?;
                Ok(sess.id.to_string())
            }
        }
    }

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
        let outcome = match self
            .execute_turn(turn, &origin, active.cancel.clone(), &sink)
            .await
        {
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
        crate::embeddings::spawn_backfill(self.clone());
        // Retour d'usage (#105) : un souvenir servi n'est utile que si la réponse le
        // reprend ; un tour qui attend une approbation garde sa liste pour sa reprise.
        match &outcome {
            TurnOutcome::Answered { text: answer, .. } => {
                let said = turn
                    .payload
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default();
                crate::usage_feedback::judge(&self.services, &turn.session_id, said, answer).await;
            }
            TurnOutcome::AwaitingApproval { .. } => {}
            _ => crate::usage_feedback::forget(&self.services, &turn.session_id).await,
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
            let previous = if crate::review::is_short_agreement(said) {
                crate::review::previous_answer(&self.services, &turn.session_id).await
            } else {
                None
            };
            if self.services.config.config().memory.review_max_candidates > 0
                && let Some(matter) = crate::review::review_matter(said, previous.as_deref())
            {
                crate::review::spawn(
                    self.clone(),
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
                && crate::titles::wants_title(&sess)
            {
                crate::titles::spawn(
                    self.clone(),
                    turn.session_id.clone(),
                    said.to_string(),
                    answer.clone(),
                );
            }
        }
        // Frontière de tour : résumé prêt publié, compaction de fond lancée si besoin.
        crate::compaction::after_turn(self, &turn.session_id).await;
        outcome
    }

    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (engine/turn.rs)
    async fn execute_turn(
        self: &Arc<Self>,
        turn: &Turn,
        origin: &Origin,
        cancel: CancelToken,
        sink: &dyn TurnSink,
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
        // seule fois par message, avant qu'il soit écrit.
        let mut episode = session.episode_seq;
        if turn.kind == TurnKind::Message
            && self
                .services
                .kv_get(&format!("turn.recorded.{}", turn.id))
                .await?
                .is_none()
        {
            episode = crate::episodes::before_message(self, &session, &text).await?;
        }

        // 1. Le message utilisateur, écrit une seule fois même si le tour est rejoué. Avec
        // des photos, il attend le choix du modèle : lui les montrer ou les faire décrire.
        if turn.kind != TurnKind::Resume && !text.trim().is_empty() && images.is_empty() {
            let flag = format!("turn.recorded.{}", turn.id);
            if self.services.kv_get(&flag).await?.is_none() {
                let content = match turn.kind {
                    TurnKind::Trigger => format!("[déclencheur planifié] {text}"),
                    TurnKind::Nudge => format!("[relance] {text}"),
                    _ => text.clone(),
                };
                let tokens = s.context.estimator.text_tokens("default", &content);
                if turn.kind == TurnKind::Message {
                    s.context
                        .history
                        .append_user_turn_at(
                            &turn.session_id,
                            turn.id.as_str(),
                            &content,
                            &turn.enqueued_at,
                            tokens,
                            episode,
                        )
                        .await?;
                } else {
                    s.context
                        .history
                        .append(
                            &turn.session_id,
                            &ChatMessage::user(content),
                            tokens,
                            episode,
                            false,
                            None,
                        )
                        .await?;
                }
                self.services.kv_set(&flag, "1").await?;
            }
        }
        if turn.kind == TurnKind::Message {
            for merged in &turn.merged_messages {
                let Some(message) = merged.payload.get("text").and_then(Value::as_str) else {
                    continue;
                };
                let tokens = s.context.estimator.text_tokens("default", message);
                s.context
                    .history
                    .append_user_turn_at(
                        &turn.session_id,
                        merged.id.as_str(),
                        message,
                        &merged.enqueued_at,
                        tokens,
                        episode,
                    )
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
                    crate::embeddings::query_vector(self, &text).await
                } else {
                    None
                }
            }
        );

        // Photos : montrées au modèle de la session s'il lit les images, sinon décrites par
        // le rôle `image_describe` et jointes en texte (§10.4).
        if turn.kind == TurnKind::Message && !images.is_empty() {
            let flag = format!("turn.recorded.{}", turn.id);
            if self.services.kv_get(&flag).await?.is_none() {
                let message = self
                    .photo_message(&text, &images, &model_id, &turn.session_id, &origin_turn)
                    .await;
                let tokens = s.context.estimator.message_tokens(&model_id, &message);
                s.context
                    .history
                    .append(&turn.session_id, &message, tokens, episode, false, None)
                    .await?;
                self.services.kv_set(&flag, "1").await?;
            }
        }

        // 3. Provider. L'abonnement ChatGPT ne sert que les tours du propriétaire : une
        // planification ou un travail interne se replie ici, sans bruit (#142).
        let model_id = crate::codex_scope::for_origin(self, &model_id, origin).await;
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
        crate::compaction::before_turn(self, &turn.session_id, &model_id, Some(&origin_turn)).await;
        let (mut tiers, recalled) = crate::conversation::build_turn_prompt(
            &s,
            &text,
            &mcp_lines,
            None,
            (session.kind == SessionKind::Chat).then_some((turn.session_id.as_str(), episode)),
            vector.clone(),
        )
        .await;
        crate::usage_feedback::served(
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
        crate::cache_audit::stable_prefix(self, &turn.session_id, &mut tiers).await?;
        // Un résumé prêt depuis le tour précédent (ou avant un redémarrage) est publié
        // avant de construire la projection.
        if let Err(e) = crate::compaction::publish_pending(self, &turn.session_id).await {
            tracing::warn!(session = %turn.session_id, error = %e, "résumé en attente non publié");
        }
        let mut conv =
            SessionConversation::new(s.clone(), &turn.session_id, &model_id, tiers, episode)
                .with_compactor(Arc::new(crate::compaction::OverflowCompactor {
                    daemon: self.clone(),
                    turn_id: Some(origin_turn.clone()),
                }));
        if turn.kind == TurnKind::Message {
            conv = conv.with_merge_turn(turn.clone(), self.hooks.telegram(), cancel.clone());
        }

        // 5. Outils.
        let mut exec = NativeToolExecutor::new(
            s.clone(),
            ToolEnv {
                session_id: turn.session_id.clone(),
                run_id: None,
                origin: origin.clone(),
                workspaces: default_workspaces(&s),
                in_workflow: false,
                turn_model: Some(crate::selfknow::TurnModel {
                    alias: alias.clone(),
                    model_id: model_id.clone(),
                }),
            },
        );
        exec.admin = Some(self.clone() as Arc<dyn crate::selfknow::Admin>);
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
                let discovered =
                    crate::tools_on_demand::exposed_for_turn(&s, &turn.session_id).await;
                let mut tools = chat_tool_defs(&discovered);
                if let Some(m) = self.hooks.mcp() {
                    tools.extend(m.eager_tools().await);
                }
                tools
            },
            allowed_tools: Vec::new(),
            cancel,
        };

        let outcome = AgentLoop::new(s.clone(), provider)
            .run_conversation(&spec, &conv, &exec, sink)
            .await;
        // Estimation locale ou prompt réellement facturé : l'un ou l'autre au-delà du seuil
        // demande la compaction de fond (issue #40).
        crate::compaction::after_answer(
            self,
            &turn.session_id,
            &model_id,
            spec.turn_id.clone(),
            conv.wants_compaction(),
        )
        .await;
        outcome
    }

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
        self.services.kv_set(&key, &block).await?;
        Ok((!block.is_empty()).then_some(block))
    }

    /// Transcrit un audio avec le modèle du rôle `stt` (§14.4) et en compte le coût.
    ///
    /// Un alias `openai_compat:…` vise le serveur local (`providers.local`) : s'il n'est
    /// pas activé, on le dit plutôt que d'envoyer l'audio à OpenRouter par défaut.
    pub async fn transcribe(
        &self,
        audio: Vec<u8>,
        filename: &str,
        session_id: &str,
    ) -> Result<String, String> {
        let s = &self.services;
        let cfg = s.config.config();
        let alias = cfg.role_alias("stt");
        let model = cfg
            .alias_model(&alias)
            .ok_or_else(|| format!("aucun modèle pour l'alias `{alias}` du rôle `stt`"))?
            .to_string();
        if penelope_llm::catalog::provider_of(&model) != "openrouter"
            && !cfg.providers.local.enabled
            && self.provider_override_active().is_none()
        {
            return Err(format!(
                "l'alias `{alias}` vise un serveur local (`{model}`) mais `providers.local` \
                 n'est pas activé : `penelope config set providers.local.enabled true`, ou \
                 transcrire via OpenRouter : `penelope model set {alias} \
                 openrouter:openai/whisper-large-v3`"
            ));
        }
        let model = crate::codex_scope::background(self, &model, "transcription").await;
        let provider = self.provider_for(&model).await?;
        let language = Some(cfg.owner.language.clone()).filter(|l| !l.is_empty());
        let t = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            provider.transcribe(&model, audio, filename, language.as_deref()),
        )
        .await
        .map_err(|_| "transcription trop longue (plus de 3 min)".to_string())?
        .map_err(|e| format!("{} ({model})", e.message))?;
        let _ = s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(session_id.to_string()),
                model: penelope_llm::catalog::strip_provider(&model).to_string(),
                provider: provider.name().to_string(),
                role: Some("stt".into()),
                cost_usd: t.cost_usd.unwrap_or(0.0),
                estimated: t.cost_usd.is_none(),
                ..Default::default()
            })
            .await;
        Ok(t.text)
    }

    /// Message utilisateur d'un tour avec photos.
    async fn photo_message(
        &self,
        text: &str,
        images: &[std::path::PathBuf],
        model_id: &str,
        session_id: &str,
        turn_id: &str,
    ) -> ChatMessage {
        let s = &self.services;
        let mut urls = Vec::new();
        let mut unreadable = 0;
        for p in images {
            match crate::media::data_url(p) {
                Ok(u) => urls.push(u),
                Err(e) => {
                    tracing::warn!(error = %e, "photo illisible");
                    unreadable += 1;
                }
            }
        }
        let count = if urls.len() > 1 {
            format!("{} photos", urls.len())
        } else {
            "une photo".to_string()
        };
        let mut request = if text.trim().is_empty() {
            format!("(Le propriétaire envoie {count}, sans légende.)")
        } else {
            text.trim().to_string()
        };
        if unreadable > 0 {
            request.push_str(&format!(
                "
({unreadable} photo(s) illisible(s) ignorée(s).)"
            ));
        }
        // Le chemin reste : `image_inspect` y relit un texte ou y pointe un élément (#125).
        let saved: Vec<String> = images.iter().map(|p| p.display().to_string()).collect();
        if !saved.is_empty() {
            request.push_str(&format!(
                "\n(photo enregistrée : {} ; `image_inspect` pour y lire le texte ou y \
                 pointer un élément)",
                saved.join(", ")
            ));
        }
        let sees = s
            .catalog
            .get(penelope_llm::catalog::strip_provider(model_id))
            .map(|i| i.accepts_images())
            .unwrap_or(false);
        if sees && !urls.is_empty() {
            let mut content = vec![penelope_llm::types::Content::text(request)];
            content.extend(
                urls.into_iter()
                    .map(|url| penelope_llm::types::Content::ImageUrl { url, detail: None }),
            );
            return ChatMessage {
                content,
                ..ChatMessage::user("")
            };
        }
        if urls.is_empty() {
            return ChatMessage::user(request);
        }
        let description = match self.describe_images(&urls, text, session_id, turn_id).await {
            Ok(d) => d,
            Err(e) => format!("(description impossible : {e})"),
        };
        ChatMessage::user(format!(
            "{request}

[{count} : description par le modèle de vision, le modèle de la              conversation ne lit pas les images]
{description}"
        ))
    }

    /// Décrit des images avec le modèle du rôle `image_describe` (alias `vision`).
    pub async fn describe_images(
        &self,
        urls: &[String],
        caption: &str,
        session_id: &str,
        turn_id: &str,
    ) -> Result<String, String> {
        let request = if caption.trim().is_empty() {
            "Décris ces images.".to_string()
        } else {
            format!("Légende du propriétaire : {}", caption.trim())
        };
        crate::vision::ask(
            self,
            crate::vision::Task::Describe,
            urls,
            &request,
            None,
            session_id,
            turn_id,
        )
        .await
        .map(|a| a.text)
    }

    /// Choisit l'alias et le modèle d'un tour (§10.3).
    ///
    /// L'alias est collant pour la session, mais le modèle est **relu** dans la
    /// configuration à chaque tour : `penelope model set main …` s'applique partout.
    pub async fn select_model(
        &self,
        session: &penelope_kernel::session::Session,
        text: &str,
        turn_id: &str,
    ) -> (String, String) {
        let s = &self.services;
        let cfg = s.config.config();
        let router = Router::new(s.catalog.clone());
        let routing = &cfg.models.routing;

        // Sans classifieur, pas d'alias collant : `main` (rôle `chat_default`) s'applique
        // aussitôt à toutes les sessions, y compris celles routées avant le changement.
        // Avec lui, l'alias « léger » (`low`) ne colle jamais : un « bonjour » ne doit pas
        // enfermer la session sur le petit modèle, le message suivant est reclassé.
        let sticky = session
            .model_alias
            .as_ref()
            .filter(|_| routing.classifier)
            .filter(|a| **a != routing.low)
            .and_then(|a| {
                cfg.alias_model(a).map(|id| StickyModel {
                    alias: a.clone(),
                    model_id: id.to_string(),
                })
            });
        let pinned = self.pinned_model(session.id.as_str()).await;
        // Frontière (#82) : cache froid, compaction ou nouvel épisode depuis le dernier
        // appel. Le préfixe change de toute façon, le collant n'y gagne rien : le message
        // repasse par le classifieur, qui peut monter ou descendre d'étage.
        let boundary = match (&sticky, &pinned) {
            (Some(_), None) => self.model_boundary(session.id.as_str()).await,
            _ => None,
        };
        let input = RouteInput {
            message: text.to_string(),
            pinned,
            sticky,
            at_boundary: boundary.is_some(),
            ..Default::default()
        };

        let decision = match router.route_deterministic(&cfg, &input) {
            Some(d) => d,
            None => match self.classify(text, session.id.as_str(), turn_id).await {
                Some(c) => router.route_with_classification(&cfg, &c),
                None => router.default_decision(&cfg),
            },
        };

        tracing::info!(
            session = %session.id,
            alias = %decision.alias,
            model = %decision.model_id,
            reason = ?decision.reason,
            frontiere = boundary.unwrap_or("aucune"),
            "modèle choisi"
        );
        // Seul le choix « de conversation » devient collant, pas un détour ponctuel
        // (image, vision) ni le petit modèle.
        let persist = match decision.reason {
            RouteReason::Default => true,
            RouteReason::Classifier => decision.alias != routing.low,
            _ => false,
        };
        if persist && session.model_alias.as_deref() != Some(decision.alias.as_str()) {
            let _ = s
                .sessions
                .set_model(session.id.as_str(), &decision.alias, &decision.model_id)
                .await;
        } else if !persist && boundary.is_some() && decision.reason == RouteReason::Classifier {
            // Reclassé en « simple » à une frontière : l'ancien collant ne revient pas au
            // message suivant.
            let _ = s.sessions.clear_model(session.id.as_str()).await;
        }
        // Dernier choix, pour que `/model` dise qui a répondu en dernier, et pourquoi il
        // a pu changer.
        let _ = self
            .services
            .kv_set(&last_model_key(session.id.as_str()), &decision.alias)
            .await;
        let _ = self
            .services
            .kv_set(
                &last_model_why_key(session.id.as_str()),
                boundary.map(boundary_label).unwrap_or(""),
            )
            .await;
        (decision.alias, decision.model_id)
    }

    /// Frontière franchie depuis le dernier appel de conversation de la session (#82) :
    /// cache du fournisseur expiré, contexte compacté, ou épisode clos.
    async fn model_boundary(&self, session_id: &str) -> Option<&'static str> {
        let s = &self.services;
        let previous = crate::cache_audit::previous_call(s, session_id)
            .await
            .ok()?;
        let Some(previous) = previous else {
            return Some("cache");
        };
        if s.clock.now_ms() - previous.ts_ms >= crate::cache_audit::CACHE_TTL_MS {
            return Some("cache");
        }
        let since = chrono::DateTime::from_timestamp_millis(previous.ts_ms)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let sid = session_id.to_string();
        let kinds: Vec<String> = s
            .store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT DISTINCT kind FROM events WHERE session_id = ?1 AND ts >= ?2
                       AND kind IN ('context.compacted', 'memory.episode_closed')",
                )?;
                let rows = st.query_map(penelope_store::rusqlite::params![sid, since], |r| {
                    r.get::<_, String>(0)
                })?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
            .ok()?;
        if kinds.iter().any(|k| k == "context.compacted") {
            Some("compaction")
        } else if kinds.iter().any(|k| k == "memory.episode_closed") {
            Some("episode")
        } else {
            None
        }
    }

    /// Alias épinglé sur une session, s'il existe encore dans la configuration.
    pub async fn pinned_model(&self, session_id: &str) -> Option<StickyModel> {
        let alias = self
            .services
            .kv_get(&pin_key(session_id))
            .await
            .ok()
            .flatten()
            .filter(|a| !a.is_empty())?;
        let cfg = self.services.config.config();
        match cfg.alias_model(&alias) {
            Some(id) => Some(StickyModel {
                alias,
                model_id: id.to_string(),
            }),
            None => {
                tracing::warn!(session = session_id, alias = %alias, "alias épinglé disparu de la configuration");
                None
            }
        }
    }

    /// Épingle un alias sur une session, ou revient à l'automatique (`None`).
    pub async fn pin_model(&self, session_id: &str, alias: Option<&str>) -> anyhow::Result<()> {
        if let Some(a) = alias {
            let cfg = self.services.config.config();
            if cfg.alias_model(a).is_none() {
                anyhow::bail!(
                    "alias inconnu `{a}` ; alias disponibles : {}",
                    conversation_aliases(&cfg).join(", ")
                );
            }
        }
        self.services
            .kv_set(&pin_key(session_id), alias.unwrap_or(""))
            .await
    }

    /// État du modèle d'une session : épinglé ou automatique, dernier alias utilisé, choix.
    pub async fn session_model_view(&self, session_id: &str) -> anyhow::Result<Value> {
        let cfg = self.services.config.config();
        let model_of = |a: &str| cfg.alias_model(a).map(String::from);
        let pinned = self.pinned_model(session_id).await;
        let last = self
            .services
            .kv_get(&last_model_key(session_id))
            .await?
            .filter(|a| !a.is_empty());
        let why = self
            .services
            .kv_get(&last_model_why_key(session_id))
            .await?
            .filter(|a| !a.is_empty());
        let choices: Vec<Value> = conversation_aliases(&cfg)
            .into_iter()
            .map(|a| json!({"alias": a, "model": model_of(&a)}))
            .collect();
        Ok(json!({
            "session": session_id,
            "mode": if pinned.is_some() { "épinglé" } else { "automatique" },
            "pinned": pinned.as_ref().map(|p| p.alias.clone()),
            "pinned_model": pinned.as_ref().map(|p| p.model_id.clone()),
            "last_alias": last,
            "last_model": last.as_deref().and_then(model_of),
            "last_boundary": why,
            "classifier": cfg.models.routing.classifier,
            "choices": choices,
        }))
    }

    /// Classifieur de complexité : un petit modèle, une réponse JSON, 8 s au plus.
    ///
    /// Raisonnement réduit au minimum que le modèle accepte, sortie structurée quand le
    /// modèle la supporte, et coût enregistré comme celui de la conversation.
    async fn classify(
        &self,
        text: &str,
        session_id: &str,
        turn_id: &str,
    ) -> Option<Classification> {
        if text.trim().is_empty() {
            return None;
        }
        let s = &self.services;
        let cfg = s.config.config();
        let alias = cfg.role_alias("classifier");
        let model_id = cfg.alias_model(&alias)?.to_string();
        let model_id = crate::codex_scope::background(self, &model_id, "classifieur").await;
        let provider = self.provider_for(&model_id).await.ok()?;
        let info = s
            .catalog
            .get(penelope_llm::catalog::strip_provider(&model_id));
        let effort = info.as_ref().and_then(|i| i.lightest_effort());
        let structured = info
            .as_ref()
            .map(|i| i.supports_structured_output())
            .unwrap_or(false);
        let req = ChatRequest {
            model: model_id.clone(),
            messages: vec![
                ChatMessage::system(CLASSIFIER_PROMPT),
                ChatMessage::user(text.chars().take(2_000).collect::<String>()),
            ],
            stream: true,
            // Sans raisonnement, 300 tokens suffisent ; avec, il lui faut de la marge
            // pour ne pas finir à vide (`finish_reason: length`).
            max_tokens: Some(if effort.as_deref() == Some("none") {
                300
            } else {
                2_000
            }),
            reasoning_effort: effort,
            response_format: structured.then(classification_schema),
            session_id: Some(session_id.to_string()),
            ..Default::default()
        };
        let call = async {
            let rx = provider.chat_stream(req, CancelToken::new()).await.ok()?;
            collect_stream(rx, &model_id, provider.name(), &s.catalog)
                .await
                .ok()
        };
        let response = tokio::time::timeout(std::time::Duration::from_secs(8), call)
            .await
            .ok()
            .flatten()?;
        let _ = s
            .budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(session_id.to_string()),
                turn_id: Some(turn_id.to_string()),
                model: response.model.clone(),
                provider: response.provider.clone(),
                role: Some("classifier".into()),
                generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
                upstream: response.upstream.clone(),
                finish: Some(format!("{:?}", response.finish).to_lowercase()),
                prompt: response.usage.prompt,
                completion: response.usage.completion,
                cached: response.usage.cached,
                cache_write: response.usage.cache_write,
                reasoning: response.usage.reasoning,
                cost_usd: response.cost_usd,
                estimated: response.cost_estimated,
                ..Default::default()
            })
            .await;
        parse_classification(&response.message.text())
    }
}

/// Extrait la classification d'une réponse, même entourée de texte.
/// `response_format` du classifieur : schéma strict (toutes les propriétés requises,
/// aucune autre admise), comme l'exigent les providers à sortie structurée stricte.
fn classification_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "classification",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "complexity": {"type": "string", "enum": ["low", "medium", "high"]},
                    "needs_tools": {"type": "boolean"},
                    "domain": {"type": "string"}
                },
                "required": ["complexity", "needs_tools", "domain"],
                "additionalProperties": false
            }
        }
    })
}

pub fn parse_classification(text: &str) -> Option<Classification> {
    let start = text.find('{')?;
    // Une réponse coupée peut porter une `}` avant sa première `{` : `text[30..=10]`
    // paniquerait (« slice index starts at 30 but ends at 11 », issue #130).
    let end = text.rfind('}').filter(|e| *e > start)?;
    let mut v: Value = serde_json::from_str(&text[start..=end]).ok()?;
    // Un domaine trop bavard ne doit pas invalider la complexité.
    if let Some(d) = v.get("domain").and_then(|d| d.as_str())
        && d.chars().count() > 40
    {
        let short: String = d.chars().take(40).collect();
        v["domain"] = json!(short);
    }
    let errors = penelope_kernel::schema::validate(&penelope_llm::router::classifier_schema(), &v);
    if !errors.is_empty() {
        return None;
    }
    serde_json::from_value(v).ok()
}

#[cfg(test)]
mod tests;
