//! Tests de la classification des updates et des aides du canal.

use super::*;
use crate::mock::updates;

const OWNER: i64 = 42;

fn group(chat_id: i64) -> Access {
    Access {
        owner_id: OWNER,
        allowed_chats: vec![chat_id],
    }
}

fn in_supergroup(mut u: serde_json::Value, chat_id: i64, title: &str) -> serde_json::Value {
    u["message"]["chat"] = serde_json::json!({"id": chat_id, "type": "supergroup", "title": title});
    u
}

/// #113 : dans un groupe autorisé par son identifiant, l'administrateur anonyme parle
/// au nom du propriétaire ; hors liste, silence avec l'identifiant ; un tiers reste
/// refusé ; l'anonyme d'un autre groupe ne passe pas.
#[test]
fn allowed_chats_open_a_group_and_accept_the_anonymous_admin() {
    let chat = -1_001_234_567_890;
    let mut anon = in_supergroup(
        updates::in_topic(updates::text_message(1, 0, ANONYMOUS_ADMIN_ID, "go"), 7),
        chat,
        "Chantiers",
    );
    anon["message"]["sender_chat"] = serde_json::json!({"id": chat, "type": "supergroup"});
    match classify(&anon, &group(chat)) {
        Incoming::Text {
            from_id, topic_id, ..
        } => {
            assert_eq!(from_id, OWNER);
            assert_eq!(topic_id, Some(7));
        }
        other => panic!("{other:?}"),
    }
    match classify(&anon, &Access::owner_only(OWNER)) {
        Incoming::ForeignChat {
            chat_id,
            chat_type,
            title,
            ..
        } => {
            assert_eq!(chat_id, chat);
            assert_eq!(chat_type, "supergroup");
            assert_eq!(title, "Chantiers");
        }
        other => panic!("{other:?}"),
    }
    let stranger = in_supergroup(updates::text_message(2, 0, 999, "salut"), chat, "Chantiers");
    assert!(matches!(
        classify(&stranger, &group(chat)),
        Incoming::Unauthorized { from_id: 999, .. }
    ));
    let mut other_anon = anon.clone();
    other_anon["message"]["sender_chat"] = serde_json::json!({"id": -1_009, "type": "channel"});
    assert!(matches!(
        classify(&other_anon, &group(chat)),
        Incoming::Unauthorized { .. }
    ));
    let owner = in_supergroup(
        updates::text_message(3, 0, OWNER, "bonjour"),
        chat,
        "Chantiers",
    );
    assert!(matches!(
        classify(&owner, &group(chat)),
        Incoming::Text { .. }
    ));
}

#[test]
fn text_messages_are_classified() {
    let u = updates::text_message(1, OWNER, OWNER, "bonjour");
    match classify(&u, &Access::owner_only(OWNER)) {
        Incoming::Text {
            text, forwarded, ..
        } => {
            assert_eq!(text, "bonjour");
            assert!(!forwarded);
        }
        other => panic!("{other:?}"),
    }
}

/// CA 14 : un utilisateur non autorisé est ignoré.
#[test]
fn ca_14_5_unauthorized_users_are_rejected() {
    let u = updates::text_message(1, 999, 999, "coucou");
    assert!(matches!(
        classify(&u, &Access::owner_only(OWNER)),
        Incoming::Unauthorized { from_id: 999, .. }
    ));
    let c = updates::callback(2, 999, "a:abc", 1);
    assert!(matches!(
        classify(&c, &Access::owner_only(OWNER)),
        Incoming::Unauthorized { .. }
    ));
}

#[test]
fn groups_are_refused_unless_listed() {
    let mut u = updates::text_message(1, -100, OWNER, "salut");
    u["message"]["chat"]["type"] = serde_json::json!("supergroup");
    assert!(matches!(
        classify(&u, &Access::owner_only(OWNER)),
        Incoming::ForeignChat { chat_id: -100, .. }
    ));
    assert!(matches!(classify(&u, &group(-100)), Incoming::Text { .. }));
}

#[test]
fn commands_are_parsed() {
    for (input, name, args) in [
        ("/status", "status", ""),
        ("/model gpt", "model", "gpt"),
        ("/mcp@penelope_bot add redmine", "mcp", "add redmine"),
        ("/New Nom Du Sujet", "new", "Nom Du Sujet"),
    ] {
        let (n, a) = parse_command(input).expect(input);
        assert_eq!(n, name);
        assert_eq!(a, args);
    }
    assert!(parse_command("pas une commande").is_none());
    assert!(parse_command("/").is_none());
    assert!(parse_command("/1+1").is_none());
}

#[test]
fn command_updates_are_routed() {
    let u = updates::text_message(1, OWNER, OWNER, "/doctor");
    match classify(&u, &Access::owner_only(OWNER)) {
        Incoming::Command { command, .. } => assert_eq!(command, "doctor"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn photos_albums_documents_and_voice() {
    match classify(
        &updates::photo(1, OWNER, OWNER, Some("g1")),
        &Access::owner_only(OWNER),
    ) {
        Incoming::Photo {
            media_group, sizes, ..
        } => {
            assert_eq!(media_group.as_deref(), Some("g1"));
            assert_eq!(
                sizes,
                vec![PhotoSize {
                    file_id: "f1".into(),
                    width: 100,
                    height: 100,
                    file_size: Some(1000),
                }]
            );
        }
        other => panic!("{other:?}"),
    }
    match classify(
        &updates::document(2, OWNER, OWNER, "note.pdf"),
        &Access::owner_only(OWNER),
    ) {
        Incoming::Document { file_name, .. } => assert_eq!(file_name, "note.pdf"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        classify(&updates::voice(3, OWNER, OWNER), &Access::owner_only(OWNER)),
        Incoming::Voice { .. }
    ));
}

/// #242 : la plus grande taille qui tient sous la limite, pas forcément la dernière.
#[test]
fn the_largest_photo_size_within_the_limit_is_chosen() {
    let size = |id: &str, side: u32, bytes: Option<i64>| PhotoSize {
        file_id: id.into(),
        width: side,
        height: side * 3 / 4,
        file_size: bytes,
    };
    let sizes = vec![
        size("s", 320, Some(20_000)),
        size("m", 1_280, Some(3_000_000)),
        size("x", 4_000, Some(12_000_000)),
    ];
    let limit = 10 * 1024 * 1024;
    assert_eq!(largest_photo_within(&sizes, limit).unwrap().file_id, "m");
    assert_eq!(
        largest_photo_within(&sizes, 50_000_000).unwrap().file_id,
        "x"
    );
    assert!(largest_photo_within(&sizes[2..], limit).is_none());
    assert_eq!(
        largest_photo_within(&[size("n", 800, None)], limit)
            .unwrap()
            .file_id,
        "n"
    );
    assert!(largest_photo_within(&[], limit).is_none());
}

#[test]
fn forwarded_messages_are_flagged() {
    let u = updates::forwarded(1, OWNER, OWNER, "contenu venu d'ailleurs");
    match classify(&u, &Access::owner_only(OWNER)) {
        Incoming::Text { forwarded, .. } => assert!(forwarded),
        other => panic!("{other:?}"),
    }
}

#[test]
fn pasted_oauth_url_is_detected() {
    let url = "http://127.0.0.1:7777/oauth/callback?code=abc&state=xyz";
    assert!(looks_like_oauth_callback(url));
    assert!(!looks_like_oauth_callback("https://example.com/page"));
    assert!(!looks_like_oauth_callback("code=abc"));
    // Schéma perdu au copier-coller : toujours un retour OAuth.
    assert!(looks_like_oauth_callback(
        "127.0.0.1:7777/oauth/callback?code=abc&state=xyz"
    ));
    assert!(looks_like_oauth_callback(
        "localhost:8765/callback?state=xyz&code=abc"
    ));
    assert!(looks_like_oauth_callback("code=abc&state=xyz"));
    assert!(!looks_like_oauth_callback(
        "le paramètre code= et le state= de la doc"
    ));
    let bare = updates::text_message(2, OWNER, OWNER, "127.0.0.1:7777/cb?code=a&state=b");
    assert!(matches!(
        classify(&bare, &Access::owner_only(OWNER)),
        Incoming::OAuthCallback { .. }
    ));

    let u = updates::text_message(1, OWNER, OWNER, url);
    assert!(matches!(
        classify(&u, &Access::owner_only(OWNER)),
        Incoming::OAuthCallback { .. }
    ));
}

#[test]
fn oauth_callback_keeps_its_topic_and_falls_back_to_text() {
    // Issue #320 : une URL d'outil tiers collée dans un sujet garde son sujet et son
    // texte, pour repartir en message ordinaire si aucune autorisation ne l'attend.
    let url = "http://localhost:8080/callback?code=c&state=s";
    let u = updates::in_topic(updates::text_message(3, OWNER, OWNER, url), 7);
    let incoming = classify(&u, &Access::owner_only(OWNER));
    match &incoming {
        Incoming::OAuthCallback { topic_id, text, .. } => {
            assert_eq!(*topic_id, Some(7));
            assert_eq!(text, url);
        }
        other => panic!("{other:?}"),
    }
    match incoming.into_text() {
        Incoming::Text {
            topic_id,
            text,
            message_id,
            ..
        } => {
            assert_eq!(topic_id, Some(7));
            assert_eq!(text, url);
            assert_eq!(message_id, 30);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn edited_messages_and_stop_are_recognised() {
    assert!(matches!(
        classify(
            &updates::edited(1, OWNER, OWNER, "corrigé"),
            &Access::owner_only(OWNER)
        ),
        Incoming::Edited { .. }
    ));
    assert!(matches!(
        classify(
            &updates::stopped_generation(2, OWNER, 71),
            &Access::owner_only(OWNER)
        ),
        Incoming::StoppedGeneration { .. }
    ));
}

#[test]
fn topic_is_carried() {
    let u = updates::in_topic(updates::text_message(1, OWNER, OWNER, "x"), 7);
    match classify(&u, &Access::owner_only(OWNER)) {
        Incoming::Text { topic_id, .. } => assert_eq!(topic_id, Some(7)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn unknown_update_kinds_are_ignored_not_fatal() {
    let u = serde_json::json!({"update_id": 9, "poll": {"id": "p"}});
    assert!(matches!(
        classify(&u, &Access::owner_only(OWNER)),
        Incoming::Ignored { .. }
    ));
}
