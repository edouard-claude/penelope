//! Pièce jointe refusée par le fournisseur (issue #231) : une relance, sur la copie
//! envoyée seulement, sans l'image ; l'historique la garde, le tour suivant ne la
//! renvoie pas, le propriétaire apprend qu'elle n'a pas été lue.

use super::*;

const TOO_LARGE: &str = "messages.1.content.1.image.source.base64: image exceeds 5 MB \
                         maximum: 7340032 bytes > 5242880 bytes";
const PHOTO: &str = "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQ";

fn with_photo(text: &str) -> ChatMessage {
    ChatMessage {
        content: vec![
            Content::text(text),
            Content::ImageUrl {
                url: PHOTO.into(),
                detail: None,
            },
        ],
        ..ChatMessage::user("")
    }
}

fn images_in(req: &ChatRequest) -> usize {
    req.messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|c| matches!(c, Content::ImageUrl { .. }))
        .count()
}

/// Le fournisseur refuse toute requête qui porte une image.
fn refuses_images(p: &MockProvider) {
    p.set_responder(Some(Arc::new(|req: &ChatRequest| {
        if images_in(req) > 0 {
            Scripted::Error(LlmErrorKind::AttachmentRejected, TOO_LARGE.into())
        } else {
            Scripted::Text("Je n'ai pas pu voir la photo.".into())
        }
    })));
}

async fn conversation() -> MemoryConversation {
    let conv = MemoryConversation::new("Tu es Pénélope.", "bonjour");
    conv.record(&ChatMessage::assistant("Bonjour."), false)
        .await
        .unwrap();
    conv
}

async fn turn(
    s: &Arc<AgentServices>,
    p: &Arc<MockProvider>,
    sid: &str,
    conv: &MemoryConversation,
) -> TurnOutcome {
    AgentLoop::new(s.clone(), p.clone())
        .run_conversation(&spec(sid), conv, &exec(false), &NullSink)
        .await
        .unwrap()
}

/// Une photo refusée : une relance sans elle, une réponse qui le dit, l'historique
/// intact, le préfixe inchangé.
#[tokio::test]
async fn a_rejected_photo_is_dropped_from_the_sent_copy_once() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    refuses_images(&p);
    let conv = conversation().await;
    conv.record(&with_photo("que vois-tu ?"), false)
        .await
        .unwrap();

    let out = turn(&s, &p, &sid, &conv).await;
    let TurnOutcome::Answered { text, .. } = &out else {
        panic!("{out:?}");
    };
    assert!(text.starts_with("Je n'ai pas pu voir la photo."), "{text}");
    assert!(
        text.contains("Image non lue : refusée par le fournisseur (trop lourde)"),
        "{text}"
    );

    let reqs = p.requests();
    assert_eq!(reqs.len(), 2, "une seule relance");
    assert_eq!(images_in(&reqs[0]), 1);
    assert_eq!(images_in(&reqs[1]), 0);
    let last_user = reqs[1]
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .unwrap();
    assert!(
        last_user
            .text()
            .contains("[image retirée : refusée par le fournisseur (trop lourde)]"),
        "{:?}",
        last_user.content
    );
    // Le préfixe (prompt système) part à l'identique.
    assert_eq!(reqs[0].messages[0], reqs[1].messages[0]);

    // L'historique garde la photo : seule la copie envoyée l'a perdue.
    let kept = conv.messages();
    assert!(kept.iter().any(ChatMessage::has_images), "{kept:?}");
    assert!(
        !kept.iter().any(|m| m.text().contains("image retirée")),
        "la mention n'entre pas dans l'historique"
    );
    assert!(
        !kept.iter().any(|m| m.text().contains("Image non lue")),
        "la note au propriétaire n'entre pas dans l'historique"
    );

    let events = s
        .events
        .session_events_of_kind(&sid, TurnEventKind::AttachmentRejected.as_str())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["images"].as_array().unwrap().len(), 1);
    assert_eq!(events[0].payload["motif"], "trop lourde");
}

/// Le tour suivant ne renvoie pas la photo refusée : un seul appel, sans elle.
#[tokio::test]
async fn the_next_turn_does_not_resend_the_rejected_photo() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    refuses_images(&p);
    let conv = conversation().await;
    conv.record(&with_photo("que vois-tu ?"), false)
        .await
        .unwrap();
    turn(&s, &p, &sid, &conv).await;

    conv.record(&ChatMessage::user("et maintenant ?"), false)
        .await
        .unwrap();
    let out = turn(&s, &p, &sid, &conv).await;
    let TurnOutcome::Answered { text, .. } = &out else {
        panic!("{out:?}");
    };
    assert!(
        !text.contains("Image non lue"),
        "déjà dit au tour précédent"
    );
    let reqs = p.requests();
    assert_eq!(reqs.len(), 3, "aucun nouveau refus");
    assert_eq!(images_in(&reqs[2]), 0);
    assert!(conv.messages().iter().any(ChatMessage::has_images));
}

/// Une autre image, jamais refusée, part encore : seule celle du refus est retirée.
#[tokio::test]
async fn a_new_photo_is_still_sent_after_a_refusal() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::Error(
        LlmErrorKind::AttachmentRejected,
        TOO_LARGE.into(),
    ));
    let conv = conversation().await;
    conv.record(&with_photo("que vois-tu ?"), false)
        .await
        .unwrap();
    turn(&s, &p, &sid, &conv).await;

    let other = ChatMessage {
        content: vec![
            Content::text("et celle-ci ?"),
            Content::ImageUrl {
                url: "data:image/png;base64,iVBORw0KGgo".into(),
                detail: None,
            },
        ],
        ..ChatMessage::user("")
    };
    conv.record(&other, false).await.unwrap();
    turn(&s, &p, &sid, &conv).await;
    let reqs = p.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(images_in(&reqs[2]), 1, "la nouvelle photo part");
}

/// Une seule relance : un second refus fait échouer le tour, en le disant.
#[tokio::test]
async fn a_second_refusal_fails_the_turn() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.set_responder(Some(Arc::new(|_: &ChatRequest| {
        Scripted::Error(LlmErrorKind::AttachmentRejected, TOO_LARGE.into())
    })));
    let conv = conversation().await;
    conv.record(&with_photo("que vois-tu ?"), false)
        .await
        .unwrap();
    let out = turn(&s, &p, &sid, &conv).await;
    let TurnOutcome::Failed { error } = &out else {
        panic!("{out:?}");
    };
    assert!(error.contains("image jointe"), "{error}");
    assert_eq!(p.call_count(), 2);
}

/// Sans image dans la requête, rien à retirer : l'échec est immédiat.
#[tokio::test]
async fn a_refusal_without_images_is_not_retried() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::Error(
        LlmErrorKind::AttachmentRejected,
        TOO_LARGE.into(),
    ));
    let out = turn(&s, &p, &sid, &conversation().await).await;
    assert!(matches!(out, TurnOutcome::Failed { .. }), "{out:?}");
    assert_eq!(p.call_count(), 1);
}

/// Ce qui marchait reste : un 400 ordinaire n'est pas relancé, l'image reste envoyée.
#[tokio::test]
async fn a_plain_bad_request_is_not_an_image_refusal() {
    let (_d, s, p) = setup().await;
    let sid = session(&s).await;
    p.push(Scripted::Error(
        LlmErrorKind::BadRequest,
        "invalid tool schema".into(),
    ));
    let conv = conversation().await;
    conv.record(&with_photo("que vois-tu ?"), false)
        .await
        .unwrap();
    let out = turn(&s, &p, &sid, &conv).await;
    assert!(matches!(out, TurnOutcome::Failed { .. }), "{out:?}");
    assert_eq!(p.call_count(), 1);
    let events = s
        .events
        .session_events_of_kind(&sid, TurnEventKind::AttachmentRejected.as_str())
        .await
        .unwrap();
    assert!(events.is_empty());
}
