//! Exécution d'un tour de conversation, de la file à la réponse (§3.3, §10.3).
//!
//! Un tour : message utilisateur écrit une seule fois, choix du modèle (alias collant,
//! classifieur à la première demande), prompt assemblé, boucle d'agent sur le
//! transcript persistant, événements publiés sur le bus.

use crate::agent::{AgentLoop, TurnEvent, TurnOutcome, TurnSink, TurnSpec};
use crate::bus::{Bus, BusEvent, BusKind, Origin};
use crate::conversation::{SessionConversation, build_tiers};
use crate::executor::{NativeToolExecutor, ToolEnv, default_workspaces, tool_defs};
use crate::runtime::Daemon;
use penelope_kernel::ids::TurnId;
use penelope_kernel::session::SessionKind;
use penelope_kernel::turn::{Turn, TurnKind};
use penelope_llm::router::{CLASSIFIER_PROMPT, Classification, RouteReason};
use penelope_llm::types::{ChatMessage, ChatRequest};
use penelope_llm::{CancelToken, RouteInput, Router, StickyModel, collect_stream};
use penelope_store::rusqlite::params;
use serde_json::{Value, json};
use std::sync::Arc;

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

    /// Remet en file la suite d'un tour suspendu par une approbation.
    pub async fn enqueue_resume(
        &self,
        session_id: &str,
        approval_id: &str,
        origin: &Origin,
    ) -> anyhow::Result<Option<TurnId>> {
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
                if let Some(id) = self.kv_get("cli.session").await? {
                    if let Some(sess) = s.sessions.get(&id).await? {
                        if sess.state == "active" {
                            return Ok(id);
                        }
                    }
                }
                let sess = s
                    .sessions
                    .create(SessionKind::Chat, Some("CLI".into()))
                    .await?;
                self.kv_set("cli.session", sess.id.as_str()).await?;
                Ok(sess.id.to_string())
            }
        }
    }

    /// Exécute un tour complet et publie son issue.
    pub async fn run_turn(self: &Arc<Self>, turn: &Turn) -> TurnOutcome {
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
        outcome
    }

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

        // 1. Le message utilisateur, écrit une seule fois même si le tour est rejoué.
        if turn.kind != TurnKind::Resume && !text.trim().is_empty() {
            let flag = format!("turn.recorded.{}", turn.id);
            if self.kv_get(&flag).await?.is_none() {
                let content = match turn.kind {
                    TurnKind::Trigger => format!("[déclencheur planifié] {text}"),
                    TurnKind::Nudge => format!("[relance] {text}"),
                    _ => text.clone(),
                };
                let tokens = s.context.estimator.text_tokens("default", &content);
                s.context
                    .history
                    .append(
                        &turn.session_id,
                        &ChatMessage::user(content),
                        tokens,
                        session.episode_seq,
                        false,
                        None,
                    )
                    .await?;
                self.kv_set(&flag, "1").await?;
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
        let (alias, model_id) = self.select_model(&session, &text, &origin_turn).await;

        // 3. Provider.
        let provider = match self.provider_for(&model_id).await {
            Ok(p) => p,
            Err(error) => return Ok(TurnOutcome::Failed { error }),
        };

        // 4. Prompt et transcript.
        let mcp_lines = match self.hooks.mcp() {
            Some(m) => m.server_lines().await,
            None => Vec::new(),
        };
        let tiers = build_tiers(&s, &text, &mcp_lines, None).await;
        let conv = SessionConversation::new(
            s.clone(),
            &turn.session_id,
            &model_id,
            tiers,
            session.episode_seq,
        );

        // 5. Outils.
        let mut exec = NativeToolExecutor::new(
            s.clone(),
            ToolEnv {
                session_id: turn.session_id.clone(),
                run_id: None,
                origin: origin.clone(),
                workspaces: default_workspaces(&s),
                in_workflow: false,
            },
        );
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
            tools: tool_defs(false, !mcp_lines.is_empty()),
            allowed_tools: Vec::new(),
            cancel,
        };

        AgentLoop::new(s.clone(), provider)
            .run_conversation(&spec, &conv, &exec, sink)
            .await
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
        let input = RouteInput {
            message: text.to_string(),
            sticky,
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
        }
        (decision.alias, decision.model_id)
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

    pub async fn kv_get(&self, key: &str) -> anyhow::Result<Option<String>> {
        let k = key.to_string();
        Ok(self
            .services
            .store
            .read(move |c| {
                let mut st = c.prepare("SELECT v FROM kv WHERE k = ?1")?;
                let mut rows = st.query([&k])?;
                Ok(match rows.next()? {
                    Some(r) => Some(r.get::<_, String>(0)?),
                    None => None,
                })
            })
            .await?)
    }

    pub async fn kv_set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let (k, v) = (key.to_string(), value.to_string());
        self.services
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v) VALUES(?1, ?2)
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                    params![k, v],
                )?;
                Ok(())
            })
            .await?;
        Ok(())
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
    let end = text.rfind('}')?;
    let mut v: Value = serde_json::from_str(&text[start..=end]).ok()?;
    // Un domaine trop bavard ne doit pas invalider la complexité.
    if let Some(d) = v.get("domain").and_then(|d| d.as_str()) {
        if d.chars().count() > 40 {
            let short: String = d.chars().take(40).collect();
            v["domain"] = json!(short);
        }
    }
    let errors = penelope_kernel::schema::validate(&penelope_llm::router::classifier_schema(), &v);
    if !errors.is_empty() {
        return None;
    }
    serde_json::from_value(v).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use penelope_llm::types::ToolCall;

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        (dir, d, p)
    }

    async fn claim(d: &Daemon) -> Turn {
        d.services.turns.claim("test").await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_message_is_answered_and_the_transcript_persists() {
        let (_dir, d, p) = daemon().await;
        // Classifieur puis réponse.
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("Bonjour ! Que puis-je faire ?");
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "bonjour", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        let out = d.run_turn(&turn).await;
        match out {
            TurnOutcome::Answered { text, .. } => assert!(text.contains("Bonjour")),
            other => panic!("{other:?}"),
        }
        // Le verrou de session tient jusqu'à la fin du tour : c'est le runner qui le rend.
        d.services.turns.complete(&turn).await.unwrap();

        let history = d.services.context.history.load(&sid, 0).await.unwrap();
        let texts: Vec<String> = history.iter().map(|e| e.message.text()).collect();
        assert_eq!(texts, vec!["bonjour", "Bonjour ! Que puis-je faire ?"]);

        // Le classifieur a choisi `fast` pour ce message, sans le rendre collant : un
        // « bonjour » ne doit pas enfermer la session sur le petit modèle.
        let cfg = d.services.config.config();
        let fast = cfg.alias_model("fast").unwrap().to_string();
        let main = cfg.alias_model("main").unwrap().to_string();
        assert_eq!(p.requests()[1].model, fast);
        let sess = d.services.sessions.require(&sid).await.unwrap();
        assert_eq!(sess.model_alias, None);

        // Les deux appels du tour (classifieur et réponse) sont attribués à la requête.
        let by_turn = d
            .services
            .budget
            .report("turn", Some(&sid), None, 10)
            .await
            .unwrap();
        assert_eq!(by_turn.len(), 1);
        assert_eq!(by_turn[0].key, turn.id.to_string());
        assert_eq!(by_turn[0].calls, 2);
        assert_eq!(by_turn[0].label.as_deref(), Some("bonjour"));
        let by_role = d
            .services
            .budget
            .report("role", Some(&sid), None, 10)
            .await
            .unwrap();
        let mut roles: Vec<&str> = by_role.iter().map(|r| r.key.as_str()).collect();
        roles.sort_unstable();
        assert_eq!(roles, vec!["chat", "classifier"]);

        // Le second message est reclassé ; `medium` part sur `main`, qui, lui, colle.
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Toujours là.");
        d.enqueue_message(&sid, "tu es là ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        d.run_turn(&turn).await;
        d.services.turns.complete(&turn).await.unwrap();
        let last = p.requests().last().unwrap().clone();
        assert_eq!(last.model, main);
        assert_eq!(last.session_id.as_deref(), Some(sid.as_str()));
        let seen: Vec<String> = last.messages.iter().map(|m| m.text()).collect();
        assert!(
            seen.iter().any(|t| t == "bonjour"),
            "l'ancien message reste intact"
        );
        assert!(
            seen.iter().any(|t| t.ends_with("tu es là ?")),
            "le dernier porte le contexte volatil en tête"
        );
        let sess = d.services.sessions.require(&sid).await.unwrap();
        assert_eq!(sess.model_alias.as_deref(), Some("main"));
        assert_eq!(p.call_count(), 4);

        // Troisième message : `main` est collant, pas de nouvel appel au classifieur.
        p.reply("Encore là.");
        d.enqueue_message(&sid, "et maintenant ?", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        d.run_turn(&turn).await;
        assert_eq!(p.call_count(), 5, "un seul appel de plus : la réponse");
    }

    #[tokio::test]
    async fn replaying_a_turn_does_not_duplicate_the_user_message() {
        let (_dir, d, p) = daemon().await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("première");
        p.reply("seconde");
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "salut", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        d.run_turn(&turn).await;
        d.run_turn(&turn).await;
        let users = d
            .services
            .context
            .history
            .load(&sid, 0)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.message.text() == "salut")
            .count();
        assert_eq!(users, 1);
    }

    /// Chemin complet d'une approbation : suspension, décision, reprise en file.
    #[tokio::test]
    async fn an_approval_suspends_then_a_resume_turn_finishes_the_work() {
        let (_dir, d, p) = daemon().await;
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "fs_write".into(),
                arguments: json!({"path": "hello.txt", "content": "bonjour"}),
            }],
        ));
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "écris hello.txt", &Origin::Cli, None)
            .await
            .unwrap();
        let mut events = d.bus.subscribe();
        let turn = claim(&d).await;
        let approval_id = match d.run_turn(&turn).await {
            TurnOutcome::AwaitingApproval { approval_id } => approval_id,
            other => panic!("{other:?}"),
        };
        d.services.turns.complete(&turn).await.unwrap();

        // La carte d'approbation est passée sur le bus.
        let mut saw_card = false;
        while let Ok(ev) = events.try_recv() {
            if let BusKind::Event(TurnEvent::Approval { tool, .. }) = &ev.kind {
                assert_eq!(tool, "fs_write");
                saw_card = true;
            }
        }
        assert!(saw_card);

        crate::agent::decide_approval(
            &d.services,
            &approval_id,
            &penelope_hitl::Decision::approve_once("cli"),
        )
        .await
        .unwrap();
        d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
            .await
            .unwrap();
        p.reply("C'est écrit.");
        let resume = claim(&d).await;
        assert_eq!(resume.kind, TurnKind::Resume);
        match d.run_turn(&resume).await {
            TurnOutcome::Answered { text, .. } => assert_eq!(text, "C'est écrit."),
            other => panic!("{other:?}"),
        }
        let ws = default_workspaces(&d.services);
        assert_eq!(
            std::fs::read_to_string(ws[0].join("hello.txt")).unwrap(),
            "bonjour"
        );
    }

    #[tokio::test]
    async fn a_missing_key_fails_with_an_actionable_message() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s));
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "bonjour", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        match d.run_turn(&turn).await {
            TurnOutcome::Failed { error } => {
                assert!(error.contains("secret set openrouter_api_key"), "{error}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn telegram_chats_get_their_own_bound_session() {
        let (_dir, d, _p) = daemon().await;
        let a = Origin::Telegram {
            chat_id: 42,
            topic_id: None,
            message_id: Some(1),
        };
        let b = Origin::Telegram {
            chat_id: 42,
            topic_id: Some(7),
            message_id: Some(2),
        };
        let sa = d.chat_session_for(&a).await.unwrap();
        assert_eq!(
            d.chat_session_for(&a).await.unwrap(),
            sa,
            "même chat, même session"
        );
        let sb = d.chat_session_for(&b).await.unwrap();
        assert_ne!(sa, sb, "un sujet a sa propre session");
    }

    #[tokio::test]
    async fn disabling_the_classifier_brings_every_session_back_to_main() {
        let (_dir, d, p) = daemon().await;
        p.reply(r#"{"complexity":"low"}"#);
        p.reply("réponse rapide");
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.enqueue_message(&sid, "salut", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        d.run_turn(&turn).await;
        d.services.turns.complete(&turn).await.unwrap();
        assert_eq!(
            p.requests().last().unwrap().model,
            d.services.config.config().alias_model("fast").unwrap()
        );

        d.publish_config("test", |c| {
            c.models.routing.classifier = false;
            Ok(vec!["models.routing.classifier".into()])
        })
        .unwrap();
        p.reply("réponse de main");
        d.enqueue_message(&sid, "encore", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        d.run_turn(&turn).await;
        assert_eq!(
            p.requests().last().unwrap().model,
            d.services.config.config().alias_model("main").unwrap(),
            "la session routée vers `fast` repasse sur `main`"
        );
    }

    #[test]
    fn classifications_are_parsed_even_with_surrounding_text() {
        let c =
            parse_classification("Voici : {\"complexity\":\"high\",\"needs_tools\":true}").unwrap();
        assert_eq!(c.complexity, penelope_llm::Complexity::High);
        assert!(parse_classification("{\"complexity\":\"énorme\"}").is_none());
        assert!(parse_classification("pas de json").is_none());
    }
}
