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
        topic_id: Option<i64>,
        /// Tailles de la photo, de la plus petite à la plus grande.
        file_ids: Vec<String>,
        /// Poids de la plus grande taille, s'il est annoncé.
        file_size: Option<i64>,
        media_group: Option<String>,
        caption: Option<String>,
    },
    Document {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        topic_id: Option<i64>,
        file_id: String,
        file_name: String,
        mime_type: Option<String>,
        file_size: Option<i64>,
        caption: Option<String>,
    },
    Voice {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        topic_id: Option<i64>,
        file_id: String,
        /// Nom d'origine d'un fichier audio (`audio.file_name`) ; absent pour un vocal.
        file_name: Option<String>,
        mime_type: Option<String>,
        file_size: Option<i64>,
        duration: Option<i64>,
    },
    Callback {
        update_id: i64,
        from_id: i64,
        callback_id: String,
        data: String,
        message_id: i64,
        chat_id: i64,
        /// Sujet du forum du message cliqué.
        topic_id: Option<i64>,
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
    /// Message d'un groupe absent de `telegram.allowed_chats` : ignoré en silence, mais
    /// journalisé avec son identifiant pour qu'on puisse l'autoriser (issue #113).
    ForeignChat {
        update_id: i64,
        chat_id: i64,
        chat_type: String,
        title: String,
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
            | Incoming::ForeignChat { update_id, .. }
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

/// `from.id` d'un message écrit en administrateur anonyme : le bot `GroupAnonymousBot`
/// de Telegram, la conversation étant dans `sender_chat`.
pub const ANONYMOUS_ADMIN_ID: i64 = 1_087_968_824;

/// Qui peut parler à Pénélope, et où (§13.4, issue #113) : le propriétaire, en privé et
/// dans les seules conversations de groupe nommées par leur identifiant.
#[derive(Debug, Clone, Default)]
pub struct Access {
    pub owner_id: i64,
    pub allowed_chats: Vec<i64>,
}

impl Access {
    pub fn owner_only(owner_id: i64) -> Self {
        Access {
            owner_id,
            allowed_chats: Vec::new(),
        }
    }
}

/// Classe un update brut. Le propriétaire est la **liste blanche d'un seul élément**
/// (§13.4) ; un groupe n'est ouvert que s'il figure dans `access.allowed_chats`, et y
/// parler en administrateur anonyme vaut propriétaire (issue #113).
#[allow(clippy::too_many_lines)] // gel 0.17 : classification des mises à jour Telegram
pub fn classify(update: &Value, access: &Access) -> Incoming {
    let owner_id = access.owner_id;
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
        let chat_id = cb
            .get("message")
            .and_then(|m| m.get("chat"))
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let anonymous = from_id == ANONYMOUS_ADMIN_ID && access.allowed_chats.contains(&chat_id);
        if from_id != owner_id && !anonymous {
            return Incoming::Unauthorized { update_id, from_id };
        }
        let from_id = owner_id;
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
            topic_id: cb
                .get("message")
                .and_then(|m| m.get("message_thread_id"))
                .and_then(|v| v.as_i64()),
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
    let chat = msg.get("chat");
    let chat_type = chat
        .and_then(|c| c.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("private");
    let chat_id = chat
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    // Un groupe n'est ouvert que par son identifiant : ailleurs, silence (issue #113).
    if chat_type != "private" && !access.allowed_chats.contains(&chat_id) {
        return Incoming::ForeignChat {
            update_id,
            chat_id,
            chat_type: chat_type.to_string(),
            title: chat
                .and_then(|c| c.get("title").or_else(|| c.get("username")))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            from_id,
        };
    }
    // Dans un groupe autorisé, l'administrateur anonyme parle au nom de la conversation
    // elle-même : c'est le propriétaire, et lui seul.
    let sender_chat = msg
        .get("sender_chat")
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_i64());
    let anonymous_admin =
        chat_type != "private" && from_id == ANONYMOUS_ADMIN_ID && sender_chat == Some(chat_id);
    if from_id != owner_id && !anonymous_admin {
        return Incoming::Unauthorized { update_id, from_id };
    }
    let from_id = owner_id;
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
            topic_id,
            file_ids: photos
                .iter()
                .filter_map(|p| p.get("file_id").and_then(|v| v.as_str()).map(String::from))
                .collect(),
            file_size: photos
                .last()
                .and_then(|p| p.get("file_size"))
                .and_then(|v| v.as_i64()),
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
            topic_id,
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
            mime_type: d
                .get("mime_type")
                .and_then(|v| v.as_str())
                .map(String::from),
            file_size: d.get("file_size").and_then(|v| v.as_i64()),
            caption: (!text.is_empty()).then(|| text.clone()),
        };
    }
    if let Some(v) = msg.get("voice").or_else(|| msg.get("audio")) {
        let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        return Incoming::Voice {
            update_id,
            chat_id,
            from_id,
            message_id,
            topic_id,
            file_id: str_of("file_id").unwrap_or_default(),
            file_name: str_of("file_name"),
            mime_type: str_of("mime_type"),
            file_size: v.get("file_size").and_then(|x| x.as_i64()),
            duration: v.get("duration").and_then(|x| x.as_i64()),
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

/// Un texte qui porte les paramètres `code=` et `state=` est un retour OAuth `paste_back`,
/// avec ou sans schéma (`127.0.0.1:7777/oauth/callback?code=…&state=…` après un
/// copier-coller tronqué). Il ne part jamais vers le modèle : le code d'autorisation
/// n'a rien à faire dans l'historique.
pub fn looks_like_oauth_callback(text: &str) -> bool {
    let t = text.trim();
    let param = |name: &str| {
        t.starts_with(&format!("{name}="))
            || t.contains(&format!("?{name}="))
            || t.contains(&format!("&{name}="))
    };
    param("code") && param("state")
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

    fn group(chat_id: i64) -> Access {
        Access {
            owner_id: OWNER,
            allowed_chats: vec![chat_id],
        }
    }

    fn in_supergroup(mut u: serde_json::Value, chat_id: i64, title: &str) -> serde_json::Value {
        u["message"]["chat"] =
            serde_json::json!({"id": chat_id, "type": "supergroup", "title": title});
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
}
