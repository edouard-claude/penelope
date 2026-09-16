//! `penelope-telegram` : client Bot API typé, rendu, templates, CTA, formulaires (§14).

#![forbid(unsafe_code)]

pub mod actions;
pub mod api;
pub mod commands;
pub mod error;
pub mod forms;
pub mod html;
pub mod mock;
pub mod render;
pub mod templates;
pub mod topics;

pub use actions::{Action, ActionStore, ClickOutcome};
pub use api::{Bot, BotTransport, RenderMode};
pub use error::{TgError, TgResult};
pub use html::{html_to_plain, markdown_to_html};
pub use render::{Block, ButtonSpec, split_message, to_blocks, to_html};
pub use templates::{Template, TemplateRegistry};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Classification d'un update entrant (§14.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Incoming {
    Text {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        topic_id: Option<i64>,
        text: String,
        /// Réponse à une carte : le contexte de la carte est injecté.
        reply_to: Option<i64>,
        forwarded: bool,
    },
    Command {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        topic_id: Option<i64>,
        command: String,
        args: String,
    },
    Photo {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        file_ids: Vec<String>,
        media_group: Option<String>,
        caption: Option<String>,
    },
    Document {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        file_id: String,
        file_name: String,
    },
    Voice {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        file_id: String,
    },
    Callback {
        update_id: i64,
        from_id: i64,
        callback_id: String,
        data: String,
        message_id: i64,
        chat_id: i64,
    },
    /// URL collée contenant `code=` et `state=` : flux OAuth `paste_back` (§8.5).
    OAuthCallback {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        url: String,
    },
    Edited {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        text: String,
    },
    StoppedGeneration {
        update_id: i64,
        chat_id: i64,
        /// Entier dans l'API réelle (`MessageGenerationStopped.draft_id`).
        draft_id: i64,
    },
    /// Update d'un utilisateur non autorisé : ignoré, journalisé, alerte au propriétaire.
    Unauthorized {
        update_id: i64,
        from_id: i64,
    },
    Ignored {
        update_id: i64,
        reason: String,
    },
}

impl Incoming {
    pub fn update_id(&self) -> i64 {
        match self {
            Incoming::Text { update_id, .. }
            | Incoming::Command { update_id, .. }
            | Incoming::Photo { update_id, .. }
            | Incoming::Document { update_id, .. }
            | Incoming::Voice { update_id, .. }
            | Incoming::Callback { update_id, .. }
            | Incoming::OAuthCallback { update_id, .. }
            | Incoming::Edited { update_id, .. }
            | Incoming::StoppedGeneration { update_id, .. }
            | Incoming::Unauthorized { update_id, .. }
            | Incoming::Ignored { update_id, .. } => *update_id,
        }
    }

    pub fn from_id(&self) -> Option<i64> {
        match self {
            Incoming::Text { from_id, .. }
            | Incoming::Command { from_id, .. }
            | Incoming::Photo { from_id, .. }
            | Incoming::Document { from_id, .. }
            | Incoming::Voice { from_id, .. }
            | Incoming::Callback { from_id, .. }
            | Incoming::OAuthCallback { from_id, .. }
            | Incoming::Edited { from_id, .. }
            | Incoming::Unauthorized { from_id, .. } => Some(*from_id),
            _ => None,
        }
    }
}

/// Classe un update brut. `owner_id` est la **liste blanche d'un seul élément** (§13.4).
pub fn classify(update: &Value, owner_id: i64, allow_groups: bool) -> Incoming {
    let update_id = update
        .get("update_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if let Some(cb) = update.get("callback_query") {
        let from_id = cb
            .get("from")
            .and_then(|f| f.get("id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if from_id != owner_id {
            return Incoming::Unauthorized { update_id, from_id };
        }
        return Incoming::Callback {
            update_id,
            from_id,
            callback_id: cb
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            data: cb
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            message_id: cb
                .get("message")
                .and_then(|m| m.get("message_id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            chat_id: cb
                .get("message")
                .and_then(|m| m.get("chat"))
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        };
    }

    if let Some(s) = update.get("stopped_message_generation") {
        return Incoming::StoppedGeneration {
            update_id,
            chat_id: s
                .get("chat")
                .and_then(|c| c.get("id"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            draft_id: s.get("draft_id").and_then(|v| v.as_i64()).unwrap_or(0),
        };
    }

    let (msg, edited) = match (update.get("message"), update.get("edited_message")) {
        (Some(m), _) => (m, false),
        (None, Some(m)) => (m, true),
        _ => {
            return Incoming::Ignored {
                update_id,
                reason: "type d'update non traité".into(),
            };
        }
    };

    let from_id = msg
        .get("from")
        .and_then(|f| f.get("id"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if from_id != owner_id {
        return Incoming::Unauthorized { update_id, from_id };
    }
    let chat_type = msg
        .get("chat")
        .and_then(|c| c.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("private");
    if chat_type != "private" && !allow_groups {
        return Incoming::Ignored {
            update_id,
            reason: format!("conversation `{chat_type}` refusée : le bot est privé"),
        };
    }

    let chat_id = msg
        .get("chat")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let message_id = msg.get("message_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let topic_id = msg.get("message_thread_id").and_then(|v| v.as_i64());
    let text = msg
        .get("text")
        .or_else(|| msg.get("caption"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if edited {
        return Incoming::Edited {
            update_id,
            chat_id,
            from_id,
            message_id,
            text,
        };
    }

    if let Some(photos) = msg.get("photo").and_then(|v| v.as_array()) {
        return Incoming::Photo {
            update_id,
            chat_id,
            from_id,
            message_id,
            file_ids: photos
                .iter()
                .filter_map(|p| p.get("file_id").and_then(|v| v.as_str()).map(String::from))
                .collect(),
            media_group: msg
                .get("media_group_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            caption: (!text.is_empty()).then(|| text.clone()),
        };
    }
    if let Some(d) = msg.get("document") {
        return Incoming::Document {
            update_id,
            chat_id,
            from_id,
            message_id,
            file_id: d
                .get("file_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            file_name: d
                .get("file_name")
                .and_then(|v| v.as_str())
                .unwrap_or("fichier")
                .to_string(),
        };
    }
    if let Some(v) = msg.get("voice").or_else(|| msg.get("audio")) {
        return Incoming::Voice {
            update_id,
            chat_id,
            from_id,
            message_id,
            file_id: v
                .get("file_id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
        };
    }

    if looks_like_oauth_callback(&text) {
        return Incoming::OAuthCallback {
            update_id,
            chat_id,
            from_id,
            url: text,
        };
    }

    if let Some((command, args)) = parse_command(&text) {
        return Incoming::Command {
            update_id,
            chat_id,
            from_id,
            message_id,
            topic_id,
            command,
            args,
        };
    }

    Incoming::Text {
        update_id,
        chat_id,
        from_id,
        message_id,
        topic_id,
        text,
        reply_to: msg
            .get("reply_to_message")
            .and_then(|r| r.get("message_id"))
            .and_then(|v| v.as_i64()),
        forwarded: msg.get("forward_origin").is_some() || msg.get("forward_from").is_some(),
    }
}

/// `/commande argument` → `("commande", "argument")`. Gère `/cmd@bot`.
pub fn parse_command(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let rest = t.strip_prefix('/')?;
    let (head, args) = match rest.split_once(char::is_whitespace) {
        Some((h, a)) => (h, a.trim()),
        None => (rest, ""),
    };
    let name = head.split('@').next().unwrap_or(head).to_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((name, args.to_string()))
}

/// Une URL collée qui porte `code=` et `state=` déclenche le flux OAuth `paste_back`.
pub fn looks_like_oauth_callback(text: &str) -> bool {
    let t = text.trim();
    (t.starts_with("http://") || t.starts_with("https://") || t.starts_with("code="))
        && t.contains("code=")
        && t.contains("state=")
}

/// Fenêtre de regroupement d'un album (§14.4 : `media_group_id`, 1,5 s).
pub const MEDIA_GROUP_WINDOW_MS: i64 = 1500;

/// Intervalle minimal entre deux mises à jour de draft (§14.2).
pub const DRAFT_INTERVAL_MS: u64 = 700;

/// Durée de vie d'un draft côté Telegram.
pub const DRAFT_TTL_MS: u64 = 30_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::updates;

    const OWNER: i64 = 42;

    #[test]
    fn text_messages_are_classified() {
        let u = updates::text_message(1, OWNER, OWNER, "bonjour");
        match classify(&u, OWNER, false) {
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
            classify(&u, OWNER, false),
            Incoming::Unauthorized { from_id: 999, .. }
        ));
        let c = updates::callback(2, 999, "a:abc", 1);
        assert!(matches!(
            classify(&c, OWNER, false),
            Incoming::Unauthorized { .. }
        ));
    }

    #[test]
    fn groups_are_refused_unless_configured() {
        let mut u = updates::text_message(1, -100, OWNER, "salut");
        u["message"]["chat"]["type"] = serde_json::json!("supergroup");
        assert!(matches!(
            classify(&u, OWNER, false),
            Incoming::Ignored { .. }
        ));
        assert!(matches!(classify(&u, OWNER, true), Incoming::Text { .. }));
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
        match classify(&u, OWNER, false) {
            Incoming::Command { command, .. } => assert_eq!(command, "doctor"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn photos_albums_documents_and_voice() {
        match classify(&updates::photo(1, OWNER, OWNER, Some("g1")), OWNER, false) {
            Incoming::Photo {
                media_group,
                file_ids,
                ..
            } => {
                assert_eq!(media_group.as_deref(), Some("g1"));
                assert_eq!(file_ids.len(), 1);
            }
            other => panic!("{other:?}"),
        }
        match classify(
            &updates::document(2, OWNER, OWNER, "note.pdf"),
            OWNER,
            false,
        ) {
            Incoming::Document { file_name, .. } => assert_eq!(file_name, "note.pdf"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify(&updates::voice(3, OWNER, OWNER), OWNER, false),
            Incoming::Voice { .. }
        ));
    }

    #[test]
    fn forwarded_messages_are_flagged() {
        let u = updates::forwarded(1, OWNER, OWNER, "contenu venu d'ailleurs");
        match classify(&u, OWNER, false) {
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

        let u = updates::text_message(1, OWNER, OWNER, url);
        assert!(matches!(
            classify(&u, OWNER, false),
            Incoming::OAuthCallback { .. }
        ));
    }

    #[test]
    fn edited_messages_and_stop_are_recognised() {
        assert!(matches!(
            classify(&updates::edited(1, OWNER, OWNER, "corrigé"), OWNER, false),
            Incoming::Edited { .. }
        ));
        assert!(matches!(
            classify(&updates::stopped_generation(2, OWNER, 71), OWNER, false),
            Incoming::StoppedGeneration { .. }
        ));
    }

    #[test]
    fn topic_is_carried() {
        let u = updates::in_topic(updates::text_message(1, OWNER, OWNER, "x"), 7);
        match classify(&u, OWNER, false) {
            Incoming::Text { topic_id, .. } => assert_eq!(topic_id, Some(7)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_update_kinds_are_ignored_not_fatal() {
        let u = serde_json::json!({"update_id": 9, "poll": {"id": "p"}});
        assert!(matches!(
            classify(&u, OWNER, false),
            Incoming::Ignored { .. }
        ));
    }
}
