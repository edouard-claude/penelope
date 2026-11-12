//! #464 : la carte d'une demande dont le demandeur a disparu. Avant, elle restait
//! cliquable : le propriétaire lisait « approuvé », et rien ne se passait.

use super::*;
use penelope_executor::jobs::{self, NewJob};
use penelope_hitl::{ApprovalKind, ApprovalRequest, ApprovalState};

/// Une demande d'outil posée par le sous-agent d'un job d'arrière-plan, qui l'attend.
async fn waiting(g: &TelegramGateway, tool: &str) -> (ApprovalRequest, String) {
    let s = &g.daemon.services;
    let session = s
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    let store = jobs::store(s);
    let job = store
        .create(NewJob {
            session_id: session.clone(),
            run_id: None,
            turn_id: None,
            call_id: None,
            tool: "sub_agent_spawn".into(),
            request: json!({"prompt": "range le dossier"}),
            effect_id: None,
        })
        .await
        .unwrap()
        .id;
    let a = s
        .approvals
        .create(
            ApprovalKind::ToolCall,
            tool,
            penelope_kernel::risk::RiskClass::Write,
            json!({"arguments": {"path": "notes.md"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            Some(&session),
            None,
            false,
        )
        .await
        .unwrap();
    s.kv_set(&jobs::child_approval_key(a.id.as_str()), &job)
        .await
        .unwrap();
    store.set_waiting(&job, true).await.unwrap();
    (a, job)
}

async fn card(g: &TelegramGateway, t: &MockTransport, a: &ApprovalRequest) -> Value {
    g.send_approval_card(OWNER, None, a).await.unwrap();
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await.pop();
    sent.expect("carte envoyée")
}

async fn state(g: &TelegramGateway, a: &ApprovalRequest) -> ApprovalState {
    let s = &g.daemon.services;
    s.approvals.get(a.id.as_str()).await.unwrap().unwrap().state
}

async fn queued_turns(g: &TelegramGateway) -> i64 {
    g.daemon
        .services
        .store
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM turn_queue", [], |r| r.get(0))?))
        .await
        .unwrap()
}

/// Le daemon a redémarré : le job du sous-agent est perdu, sa carte est restée dans la
/// conversation. Un clic dit qu'il n'y a plus rien à approuver, la demande est retirée
/// et aucun tour n'est lancé.
#[tokio::test]
async fn a_click_on_a_card_whose_job_is_gone_says_so() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let (a, job) = waiting(&g, "fs_write").await;
    let buttons = inline_buttons(&card(&g, &t, &a).await);
    assert_eq!(jobs::store(&s).recover_on_boot().await.unwrap().len(), 1);
    t.clear().await;

    press(&g, &buttons[0].1).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("Plus rien à approuver") && sent[0].contains(&job),
        "{sent:?}"
    );
    assert!(!sent[0].contains('✅'), "{sent:?}");
    assert_eq!(state(&g, &a).await, ApprovalState::Expired);
    let key = jobs::child_approval_key(a.id.as_str());
    assert!(s.kv_get(&key).await.unwrap().is_none(), "clé retirée");
    assert_eq!(queued_turns(&g).await, 0);

    // L'autre bouton de la même carte, puis une carte relancée après coup : pareil.
    press(&g, &buttons[1].1).await;
    let again = inline_buttons(&card(&g, &t, &a).await);
    press(&g, &again[0].1).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    let told = sent.iter().filter(|x| x.contains("Plus rien à approuver"));
    assert_eq!(told.count(), 3, "{sent:?}");
    assert_eq!(state(&g, &a).await, ApprovalState::Expired);
}

/// Le retrait désarme les cartes déjà parties, relance comprise : leur texte dit que la
/// demande est retirée et leurs boutons disparaissent. La carte d'une autre demande et
/// celle d'un job qui attend encore ne sont pas touchées.
#[tokio::test]
async fn a_withdrawn_request_disarms_its_cards() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let (a, job) = waiting(&g, "fs_write").await;
    let (other, _) = waiting(&g, "fs_delete").await;
    let first = card(&g, &t, &a).await;
    card(&g, &t, &a).await;
    let kept = inline_buttons(&card(&g, &t, &other).await);
    t.clear().await;

    let messenger = Some(&*g as &dyn Messenger);
    let sweep = penelope_orchestrator::withdrawn::sweep;
    assert!(
        sweep(&s, messenger).await.unwrap().is_empty(),
        "ils attendent"
    );
    assert!(t.calls_to(tg::EDIT_MESSAGE_TEXT).await.is_empty());

    let cancelled = penelope_mcp::tasks::TaskState::Cancelled;
    jobs::store(&s).finish(&job, cancelled, None).await.unwrap();
    assert_eq!(sweep(&s, messenger).await.unwrap(), vec![a.id.to_string()]);
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert_eq!(edits.len(), 2, "la carte et sa relance : {edits:?}");
    for edit in &edits {
        let text = edit["text"].as_str().unwrap();
        assert!(text.starts_with(first["text"].as_str().unwrap()), "{text}");
        assert!(text.contains("⌛ <i>Demande retirée"), "{text}");
        assert!(inline_buttons(edit).is_empty());
    }
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.is_empty(), "sans bruit : {sent:?}");

    // L'autre demande se tranche comme avant.
    press(&g, &kept[0].1).await;
    assert_eq!(state(&g, &other).await, ApprovalState::Approved);
}
