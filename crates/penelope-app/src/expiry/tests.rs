use super::*;
use crate::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use penelope_kernel::ids::TurnId;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Les reprises demandées, par session.
#[derive(Default)]
struct Resumes(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl TurnIntake for Resumes {
    async fn enqueue_message(
        &self,
        _: &str,
        _: &str,
        _: &Origin,
        _: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        unreachable!()
    }
    async fn enqueue_message_with_images(
        &self,
        _: &str,
        _: &str,
        _: &[PathBuf],
        _: &Origin,
        _: Option<String>,
    ) -> anyhow::Result<Option<TurnId>> {
        unreachable!()
    }
    async fn enqueue_retry(&self, _: &str, _: &Origin, _: &str) -> anyhow::Result<Option<TurnId>> {
        unreachable!()
    }
    async fn enqueue_resume(
        &self,
        session_id: &str,
        _: &str,
        _: &Origin,
    ) -> anyhow::Result<Option<TurnId>> {
        self.0.lock().unwrap().push(session_id.into());
        Ok(None)
    }
    async fn chat_session_for(&self, _: &Origin) -> anyhow::Result<String> {
        unreachable!()
    }
}

async fn ask(
    s: &Services,
    session: &str,
    call: &str,
    turn: &str,
) -> penelope_hitl::ApprovalRequest {
    s.approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "mcp__mail__move_email",
            penelope_kernel::risk::RiskClass::Destructive,
            json!({"call_id": call, "turn_id": turn}),
            vec![],
            Some(session),
            None,
            false,
        )
        .await
        .unwrap()
}

/// #399 : une approbation échue reprend son tour une fois. Si le tour repris redemande et
/// que la seconde carte échoue aussi, il s'arrête et le dit, au lieu de redemander chaque
/// jour ; une session close ne reprend jamais, et sa fermeture annule ses cartes.
#[tokio::test]
async fn an_expired_approval_resumes_its_turn_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let s = Services::for_tests(dir.path().to_path_buf(), Arc::new(clock.clone()))
        .await
        .unwrap();
    let (intake, rec) = (Resumes::default(), RecordingMessenger::new());
    let new_session = || async {
        s.sessions
            .create(penelope_kernel::session::SessionKind::Scheduled, None)
            .await
            .unwrap()
            .id
            .to_string()
    };
    let day = 25 * 3_600_000;
    let expire = || async {
        clock.advance_ms(day);
        for a in s.approvals.expire_due().await.unwrap() {
            settle(&s, &intake, Some(rec.as_ref()), &a, &Origin::Cli)
                .await
                .unwrap();
        }
    };

    let sid = new_session().await;
    ask(&s, &sid, "c1", "t_origine").await;
    expire().await;
    assert_eq!(
        *intake.0.lock().unwrap(),
        std::slice::from_ref(&sid),
        "première reprise"
    );

    // Le tour repris redemande, sans réponse non plus.
    ask(&s, &sid, "c1", "t_origine").await;
    expire().await;
    assert_eq!(intake.0.lock().unwrap().len(), 1, "pas de seconde reprise");
    assert!(
        rec.texts().iter().any(|t| t.contains("la tâche s'arrête")),
        "{:?}",
        rec.texts()
    );

    // Session close : sa carte est annulée, rien ne reprend.
    let other = new_session().await;
    let card = ask(&s, &other, "c2", "t_autre").await;
    s.sessions.set_state(&other, "closed").await.unwrap();
    assert_eq!(
        s.approvals
            .cancel_for_session(&other, "session fermée")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        s.approvals
            .get(card.id.as_str())
            .await
            .unwrap()
            .unwrap()
            .state,
        penelope_hitl::ApprovalState::Cancelled
    );
    ask(&s, &other, "c3", "t_autre2").await;
    expire().await;
    assert_eq!(intake.0.lock().unwrap().len(), 1, "session close");
}
