//! T20 (épopée #208) : reprise après crash. Un tour ouvert au moment de l'arrêt est
//! fermé `interrupted` au démarrage ; le tour rejoué ouvre sa propre borne, tentative
//! suivante, et retrouve l'appel resté sans résultat.
//!
//! Avec eux, les effets incertains après un crash (#83) : sortis de `agent/tests/`
//! (épopée #208, T09), ils passent par `Daemon::recover`.

use super::*;
use crate::agent::decide_approval;
use penelope_agent::{
    AgentLoop, EFFECT_DONE, EFFECT_RETRY, MemoryConversation, NullSink, effect_kind,
};
use penelope_app::services::Services;
use penelope_context::journal::{Provenance, TurnIdentity, UserSource, started_payload};
use penelope_hitl::Decision;
use penelope_kernel::effects::{EffectSpec, Planned};
use penelope_kernel::event::EventDraft;
use penelope_llm::types::ToolDef;
use penelope_tools::ToolOutcome;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingExecutor {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl penelope_agent::ToolExecutor for CountingExecutor {
    async fn execute(
        &self,
        name: &str,
        _args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutcome::ok(json!({"tool": name, "ok": true})))
    }
}

fn exec(_fail: bool) -> CountingExecutor {
    CountingExecutor {
        calls: AtomicUsize::new(0),
    }
}

pub(super) async fn setup() -> (tempfile::TempDir, Arc<Services>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let p = Arc::new(MockProvider::new());
    (dir, s, p)
}

fn spec(session_id: &str) -> TurnSpec {
    TurnSpec {
        session_id: session_id.to_string(),
        run_id: None,
        turn_id: Some("t_test".into()),
        model_id: "mock/model".into(),
        fallback_models: vec![],
        tools: vec![ToolDef::new("fs_read", "lire", json!({"type":"object"}))],
        allowed_tools: vec![],
        cancel: CancelToken::new(),
    }
}

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

async fn session(s: &Services) -> String {
    s.sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string()
}

#[tokio::test]
async fn a_turn_open_at_the_crash_is_closed_as_interrupted_then_replayed() {
    let (_dir, s, p) = setup().await;
    let d = Arc::new(Daemon::from_services(s.clone()));
    d.set_provider_override(p.clone());
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    d.enqueue_message(&sid, "quelle heure est-il ?", &Origin::Cli, None)
        .await
        .unwrap();

    // Le tour a ouvert sa borne, écrit le message et la réponse qui appelle un outil ;
    // le processus meurt avant le résultat.
    let t = s.turns.claim("runner-0").await.unwrap().unwrap();
    let id = TurnIdentity {
        turn_id: t.id.to_string(),
        kind: "message".into(),
        attempt: t.attempts,
    };
    s.events
        .append(
            EventDraft::new(
                "turn.started",
                started_payload(None, Some(t.id.as_str()), Some(&id)),
            )
            .session(&sid),
        )
        .await
        .unwrap();
    let history = &s.context.history;
    let prov = Provenance::queued(UserSource::Owner, t.id.as_str(), &t.enqueued_at);
    history
        .append_queued(
            &sid,
            &ChatMessage::user("quelle heure est-il ?"),
            5,
            0,
            &prov,
        )
        .await
        .unwrap();
    // Le message est au journal sous l'identifiant du tour : le rejeu ne le réécrit pas
    // (T16, plus de clé `turn.recorded.*`).
    assert!(history.recorded(&sid, t.id.as_str()).await.unwrap());
    let asks = ChatMessage::assistant("").with_tool_calls(vec![call("c1", "time_now", json!({}))]);
    let prov = Provenance {
        turn: Some(t.id.to_string()),
        step: 1,
        ..Default::default()
    };
    history
        .append_as(&sid, &asks, 5, 0, false, None, &prov)
        .await
        .unwrap();

    // Redémarrage, deux fois : une seule fermeture.
    d.recover().await.unwrap();
    d.recover().await.unwrap();
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let finished: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "turn.finished")
        .collect();
    assert_eq!(finished.len(), 1, "{events:?}");
    assert_eq!(
        finished[0].payload,
        json!({"reason": "interrupted", "turn_id": t.id.to_string(), "kind": "message",
               "attempt": t.attempts, "origin_turn": t.id.to_string()})
    );

    // Le tour rejoué : tentative suivante, l'appel en attente est résolu avant le modèle.
    p.reply("Il est quatre heures.");
    let again = s.turns.claim("runner-0").await.unwrap().unwrap();
    assert_eq!(again.id, t.id);
    assert_eq!(again.attempts, t.attempts + 1);
    let out = d.run_turn(&again).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let started: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "turn.started")
        .map(|e| e.payload["attempt"].clone())
        .collect();
    assert_eq!(started, vec![json!(t.attempts), json!(t.attempts + 1)]);
    let result = events
        .iter()
        .find(|e| e.kind == "conv.tool_result")
        .expect("l'appel sans résultat est résolu au rejeu");
    assert_eq!(result.payload["call_id"], "c1");
    let users = events.iter().filter(|e| e.kind == "conv.user").count();
    assert_eq!(users, 1, "le message n'est pas réécrit");
    let last = events.iter().rfind(|e| e.kind == "turn.finished").unwrap();
    assert_eq!(last.payload["reason"], "answered");
    assert_eq!(last.payload["attempt"], json!(t.attempts + 1));
}

/// Un `git push` était en vol quand le daemon est tombé : l'effet est `dispatching`,
/// l'appel n'a pas de résultat. Le « redémarrage » le passe en `unknown`.
async fn crashed_push() -> (
    tempfile::TempDir,
    Arc<Services>,
    Arc<MockProvider>,
    String,
    MemoryConversation,
    String,
) {
    let (d, s, p) = setup().await;
    let sid = session(&s).await;
    let conv = MemoryConversation::new("Tu es Pénélope.", "pousse la branche");
    let args = json!({"command": "git push origin main"});
    conv.record(
        &ChatMessage::assistant("").with_tool_calls(vec![call("c1", "shell_exec", args.clone())]),
        false,
    )
    .await
    .unwrap();
    let id = match s
        .effects
        .plan(
            EffectSpec::new(effect_kind("shell_exec"), "shell_exec", args)
                .session(&sid)
                .step("c1"),
        )
        .await
        .unwrap()
    {
        Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    s.effects.dispatching(&id).await.unwrap();
    let daemon = crate::runtime::Daemon::from_services(s.clone());
    daemon.recover().await.unwrap();
    // Un second redémarrage ne crée pas de seconde demande.
    daemon.recover().await.unwrap();
    let pending = s.approvals.pending(10).await.unwrap();
    assert_eq!(pending.len(), 1, "une demande par effet : {pending:?}");
    assert_eq!(pending[0].kind, penelope_hitl::ApprovalKind::EffectUnknown);
    let approval = pending[0].id.0.clone();
    (d, s, p, sid, conv, approval)
}

/// #83 : la reprise attend la décision, « C'est fait » rejoue sans relancer, et
/// aucune règle n'est créée, même demandée « toujours ».
#[tokio::test]
async fn an_uncertain_effect_marked_done_is_replayed_not_rerun() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(crate::agent::services_of(&s), p.clone());
    assert_eq!(
        loop_
            .run_conversation(&spec(&sid), &conv, &e, &NullSink)
            .await
            .unwrap(),
        TurnOutcome::AwaitingApproval {
            approval_id: approval.clone()
        },
        "la reprise attend la décision"
    );
    let d = Decision {
        choice: EFFECT_DONE.into(),
        ..Decision::approve_always("telegram")
    };
    assert!(loop_.decide_approval(&approval, &d).await.unwrap());
    assert!(
        s.policies.active_rules().await.unwrap().is_empty(),
        "aucune règle"
    );

    p.reply("poussé");
    let out = loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(e.calls.load(Ordering::SeqCst), 0, "jamais relancé");
    let result = conv.messages()[2].text();
    assert!(result.contains("fait"), "{result}");
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Completed)
            .await
            .unwrap(),
        1
    );
}

/// #83 : « Relancer » remet l'effet en `planned` : la reprise l'exécute, une fois.
#[tokio::test]
async fn an_uncertain_effect_retried_runs_once() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(crate::agent::services_of(&s), p.clone());
    let d = Decision {
        choice: EFFECT_RETRY.into(),
        ..Decision::approve_once("cli")
    };
    assert!(loop_.decide_approval(&approval, &d).await.unwrap());
    p.reply("relancé");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 1);
}

/// #83 : « Ignorer » laisse l'effet tel quel (`failed`) et le modèle l'apprend.
#[tokio::test]
async fn an_uncertain_effect_ignored_is_not_rerun() {
    let (_d, s, p, sid, conv, approval) = crashed_push().await;
    let e = exec(false);
    let loop_ = AgentLoop::new(crate::agent::services_of(&s), p.clone());
    assert!(
        !loop_
            .decide_approval(&approval, &Decision::deny("telegram", None))
            .await
            .unwrap()
    );
    p.reply("compris");
    loop_
        .run_conversation(&spec(&sid), &conv, &e, &NullSink)
        .await
        .unwrap();
    assert_eq!(e.calls.load(Ordering::SeqCst), 0);
    let result = conv.messages()[2].text();
    assert!(result.contains("sans relance"), "{result}");
    assert_eq!(
        s.effects
            .count_by_state(penelope_kernel::effects::EffectState::Failed)
            .await
            .unwrap(),
        1
    );
    // Un « Autoriser » sans choix d'effet est refusé, sans rien trancher.
    let (_d2, s2, p2, _sid2, _conv2, approval2) = crashed_push().await;
    let e2 = decide_approval(&s2, &approval2, &Decision::approve_once("cli"))
        .await
        .unwrap_err();
    assert!(e2.to_string().contains("--effect"), "{e2}");
    assert_eq!(s2.approvals.pending(10).await.unwrap().len(), 1);
    drop(p2);
}
