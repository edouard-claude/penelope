//! Boutons : jetons inconnus ou déjà servis, fenêtres d'approbation, contradictions et
//! propositions de mémoire, effets incertains, OAuth.

use super::*;
use penelope_telegram::actions::kind as k;

async fn request(
    g: &TelegramGateway,
    kind: penelope_hitl::ApprovalKind,
    subject: &str,
    payload: Value,
) -> String {
    g.daemon
        .services
        .approvals
        .create(
            kind,
            subject,
            penelope_kernel::risk::RiskClass::Write,
            payload,
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap()
        .id
        .to_string()
}

async fn button_for(g: &TelegramGateway, action: &str, target: &str) -> String {
    g.actions
        .create(action, target, json!({}), 3_600_000, true)
        .await
        .unwrap()
        .token
}

async fn state(g: &TelegramGateway, id: &str) -> penelope_hitl::ApprovalState {
    g.daemon
        .services
        .approvals
        .get(id)
        .await
        .unwrap()
        .unwrap()
        .state
}

async fn notices(t: &MockTransport) -> Vec<String> {
    t.calls_to(tg::ANSWER_CALLBACK_QUERY)
        .await
        .iter()
        .filter_map(|c| c["text"].as_str().map(String::from))
        .collect()
}

/// Un jeton inconnu est dit inconnu ; un jeton déjà servi est dit traité et ses boutons
/// retirés ; une demande disparue retire les boutons sans rien dire.
#[tokio::test]
async fn unknown_used_and_orphan_buttons() {
    let (_d, g, t, _p) = gateway().await;
    press(&g, "jeton-inconnu").await;
    let id = request(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "fs_write",
        json!({}),
    )
    .await;
    let token = button_for(&g, k::DENY, &id).await;
    press(&g, &token).await;
    press(&g, &token).await;
    let seen = notices(&t).await;
    assert!(seen.contains(&"Action inconnue.".to_string()), "{seen:?}");
    assert!(seen.contains(&"Déjà traité.".to_string()), "{seen:?}");
    let orphan = button_for(&g, k::APPROVE, "a_disparue").await;
    let edits = t.calls_to(tg::EDIT_MESSAGE_REPLY_MARKUP).await.len();
    press(&g, &orphan).await;
    assert!(t.calls_to(tg::EDIT_MESSAGE_REPLY_MARKUP).await.len() > edits);
}

/// « Pour cette session » et « Toujours » approuvent ; « Toujours » laisse une règle ;
/// « Refuser avec raison » attend la raison dans le message suivant.
#[tokio::test]
async fn approval_windows_and_reasons() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let args = json!({"arguments": {"path": "note.txt", "content": "x"}});
    let run = request(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "fs_write",
        args.clone(),
    )
    .await;
    press(&g, &button_for(&g, k::APPROVE_RUN, &run).await).await;
    assert_eq!(
        state(&g, &run).await,
        penelope_hitl::ApprovalState::Approved
    );
    let always = request(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "fs_write",
        args.clone(),
    )
    .await;
    press(&g, &button_for(&g, k::APPROVE_ALWAYS, &always).await).await;
    assert_eq!(
        state(&g, &always).await,
        penelope_hitl::ApprovalState::Approved
    );
    assert!(
        !s.policies.active_rules().await.unwrap().is_empty(),
        "règle posée"
    );

    let reason = request(&g, penelope_hitl::ApprovalKind::ToolCall, "fs_write", args).await;
    press(&g, &button_for(&g, k::DENY_REASON, &reason).await).await;
    g.flush_outbox().await.unwrap();
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("Donne la raison du refus"))
    );
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|x| x.contains("✅ Pour cette session")),
        "{sent:?}"
    );
}

/// #145 : « Ignorer » une contradiction la tranche en refus ; « Exception » sans
/// contexte demande dans quel cas ; « Rien » d'une proposition l'écarte.
#[tokio::test]
async fn memory_cards_are_decided_from_telegram() {
    let (_d, g, t, _p) = gateway().await;
    let clash = json!({"contradiction": true, "existing_uid": "X1", "existing": "Café",
                       "proposed": "Thé", "candidates": []});
    let ignore = request(
        &g,
        penelope_hitl::ApprovalKind::MemoryProposal,
        "mémoire",
        clash.clone(),
    )
    .await;
    press(&g, &button_for(&g, k::MEMORY_REJECT, &ignore).await).await;
    assert_eq!(
        state(&g, &ignore).await,
        penelope_hitl::ApprovalState::Denied
    );
    let exception = request(
        &g,
        penelope_hitl::ApprovalKind::MemoryProposal,
        "mémoire",
        clash,
    )
    .await;
    press(
        &g,
        &button_for(&g, k::MEMORY_AS_EXCEPTION, &exception).await,
    )
    .await;
    assert_eq!(
        state(&g, &exception).await,
        penelope_hitl::ApprovalState::Approved
    );

    let facts = request(
        &g,
        penelope_hitl::ApprovalKind::MemoryProposal,
        "mémoire",
        json!({"items": ["Le client Martin est à Lyon"], "source": "notes"}),
    )
    .await;
    press(&g, &button_for(&g, k::MEMORY_REJECT, &facts).await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter().any(|x| x.contains("Dans quel cas ?")),
        "{sent:?}"
    );
    assert!(
        sent.iter().any(|x| x.contains("Propositions écartées")),
        "{sent:?}"
    );
}

/// Carte OAuth : « J'ai l'adresse » demande de la coller, « Relancer » refait la carte.
#[tokio::test]
async fn oauth_card_buttons() {
    let (_d, g, t, _p) = gateway().await;
    press(&g, &button_for(&g, k::OAUTH_PASTED, "drive").await).await;
    press(&g, &button_for(&g, k::OAUTH_RETRY, "drive").await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|x| x.contains("Colle ici l'adresse complète")),
        "{sent:?}"
    );
    assert!(sent.iter().any(|x| x.contains("inconnu")), "{sent:?}");
}

/// #219 : « Ignorer » une contradiction applique le refus (le candidat est écarté) au lieu
/// de répondre « Déjà tranché » ; une seconde carte sur la même demande, elle, l'est.
#[tokio::test]
async fn ignoring_a_contradiction_applies_it() {
    let (_d, g, t, _p) = gateway().await;
    let clash = json!({"contradiction": true, "existing_uid": "X1", "existing": "Café",
                       "proposed": "Thé", "candidates": []});
    let id = request(
        &g,
        penelope_hitl::ApprovalKind::MemoryProposal,
        "mémoire",
        clash,
    )
    .await;
    press(&g, &button_for(&g, k::MEMORY_REJECT, &id).await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(
        sent.iter()
            .any(|x| x.contains("Ignoré : la mémoire ne change pas")),
        "{sent:?}"
    );
    assert!(!sent.iter().any(|x| x.contains("Déjà tranché")), "{sent:?}");
    press(&g, &button_for(&g, k::MEMORY_ACCEPT, &id).await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(sent.iter().any(|x| x.contains("Déjà tranché")), "{sent:?}");
}

/// #219 : un second refus sur une demande déjà refusée depuis Telegram dit « Déjà
/// tranché », il ne refuse pas une seconde fois.
#[tokio::test]
async fn a_second_deny_says_already_decided() {
    let (_d, g, t, _p) = gateway().await;
    let id = request(
        &g,
        penelope_hitl::ApprovalKind::ToolCall,
        "fs_write",
        json!({}),
    )
    .await;
    press(&g, &button_for(&g, k::DENY, &id).await).await;
    press(&g, &button_for(&g, k::DENY, &id).await).await;
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert_eq!(
        sent.iter().filter(|x| x.contains("Refusé")).count(),
        1,
        "{sent:?}"
    );
    assert!(sent.iter().any(|x| x.contains("Déjà tranché")), "{sent:?}");
}
