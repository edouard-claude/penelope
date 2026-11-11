//! `penelope-telegram` : client Bot API typé, rendu, templates, CTA, formulaires (§14).

#![forbid(unsafe_code)]

pub mod actions;
pub mod api;
pub mod commands;
pub mod error;
pub mod forms;
pub mod html;
pub mod mock;
mod other;
pub mod render;
pub mod templates;
pub mod topics;

pub use actions::{Action, ActionStore, ClickOutcome};
pub use api::{Bot, BotTransport, RenderMode};
pub use error::{TgError, TgResult};
pub use html::{html_to_plain, markdown_to_html};
pub use render::{Block, ButtonSpec, split_message, to_blocks, to_html};
pub use templates::{Template, TemplateRegistry};

use other::Other;
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
        /// Le message cité par « Répondre » (#381), sauf la réponse implicite d'un sujet.
        replied: Option<Replied>,
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
        sizes: Vec<PhotoSize>,
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
    /// URL collée contenant `code=` et `state=` : peut-être un retour OAuth `paste_back`
    /// (§8.5). La forme seule ne suffit pas : une URL d'un outil tiers lui ressemble. La
    /// passerelle vérifie que le `state` est attendu, sinon le message redevient un
    /// [`Incoming::Text`] ([`Incoming::into_text`]) (issue #320).
    OAuthCallback {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        /// Sujet d'où vient l'adresse : la réponse y part, pas dans Général.
        topic_id: Option<i64>,
        /// Le message tel qu'il a été collé.
        text: String,
        reply_to: Option<i64>,
        replied: Option<Replied>,
        forwarded: bool,
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
    /// Message du propriétaire que le canal ne sait pas lire, et sans légende (#461) :
    /// dit au propriétaire, jamais mis en file. `what` nomme le champ inconnu.
    Unsupported {
        update_id: i64,
        chat_id: i64,
        from_id: i64,
        message_id: i64,
        topic_id: Option<i64>,
        what: Option<String>,
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
            | Incoming::Ignored { update_id, .. }
            | Incoming::Unsupported { update_id, .. } => *update_id,
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
            | Incoming::Unsupported { from_id, .. }
            | Incoming::Unauthorized { from_id, .. } => Some(*from_id),
            _ => None,
        }
    }

    /// Un retour OAuth que rien n'attend redevient le message texte qu'il était ; les
    /// autres variants sont rendus tels quels.
    pub fn into_text(self) -> Incoming {
        match self {
            Incoming::OAuthCallback {
                update_id,
                chat_id,
                from_id,
                message_id,
                topic_id,
                text,
                reply_to,
                replied,
                forwarded,
            } => Incoming::Text {
                update_id,
                chat_id,
                from_id,
                message_id,
                topic_id,
                text,
                reply_to,
                replied,
                forwarded,
            },
            other => other,
        }
    }
}

/// Une taille d'une photo (`PhotoSize`) : Telegram en propose plusieurs, réduites de
/// son côté.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub width: u32,
    pub height: u32,
    /// Poids annoncé, s'il l'est.
    pub file_size: Option<i64>,
}

impl PhotoSize {
    fn parse(p: &Value) -> Option<PhotoSize> {
        let side = |k: &str| p.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        Some(PhotoSize {
            file_id: p.get("file_id")?.as_str()?.to_string(),
            width: side("width"),
            height: side("height"),
            file_size: p.get("file_size").and_then(|v| v.as_i64()),
        })
    }
}

/// La plus grande taille dont le poids annoncé tient sous `max_bytes` (issue #242) : une
/// photo lourde n'est pas perdue quand Telegram en propose une version plus légère. Une
/// taille sans poids annoncé passe ; `None` si aucune ne tient.
pub fn largest_photo_within(sizes: &[PhotoSize], max_bytes: usize) -> Option<&PhotoSize> {
    sizes
        .iter()
        .filter(|p| p.file_size.unwrap_or(0) as usize <= max_bytes)
        .max_by_key(|p| (p.width.max(p.height), p.file_size.unwrap_or(0)))
}

/// `from.id` d'un message écrit en administrateur anonyme : le bot `GroupAnonymousBot`
/// de Telegram, la conversation étant dans `sender_chat`.
pub const ANONYMOUS_ADMIN_ID: i64 = 1_087_968_824;

/// Le message auquel un message répond (#381), tel que Telegram le donne : son texte (ou
/// sa légende), son auteur et le passage surligné par celui qui répond.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Replied {
    pub message_id: i64,
    pub text: String,
    pub from_id: Option<i64>,
    pub from_bot: bool,
    /// Prénom ou nom d'utilisateur de l'auteur, pour le dire au modèle.
    pub author: String,
    pub quote: Option<String>,
    /// La bulle citée est elle-même un message transféré : sa parole est celle d'un
    /// tiers, quel que soit qui l'a transférée.
    pub forwarded: bool,
}

/// Le message cité, sauf la réponse implicite d'un sujet de forum : dans un sujet,
/// Telegram fait « répondre » chaque bulle au message de création du sujet.
fn replied_of(msg: &Value) -> Option<Replied> {
    let r = msg.get("reply_to_message")?;
    if r.get("forum_topic_created").is_some()
        || (msg.get("is_topic_message").is_some()
            && r.get("message_id") == msg.get("message_thread_id"))
    {
        return None;
    }
    let text = r
        .get("text")
        .or_else(|| r.get("caption"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let quote = msg
        .get("quote")
        .and_then(|q| q.get("text"))
        .and_then(|v| v.as_str())
        .map(String::from);
    if text.is_empty() && quote.is_none() {
        return None;
    }
    let from = r.get("from");
    let str_of = |k: &str| from.and_then(|f| f.get(k)).and_then(|v| v.as_str());
    Some(Replied {
        message_id: r.get("message_id").and_then(|v| v.as_i64()).unwrap_or(0),
        text,
        from_id: from.and_then(|f| f.get("id")).and_then(|v| v.as_i64()),
        from_bot: from
            .and_then(|f| f.get("is_bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        author: str_of("first_name")
            .or_else(|| str_of("username"))
            .unwrap_or("un membre")
            .to_string(),
        quote,
        forwarded: [
            "forward_origin",
            "forward_from",
            "forward_from_chat",
            "forward_sender_name",
        ]
        .iter()
        .any(|k| r.get(*k).is_some()),
    })
}

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

/// Un clic sur un bouton : du propriétaire, ou de l'administrateur anonyme d'un groupe
/// autorisé.
fn classify_callback(cb: &Value, update_id: i64, access: &Access) -> Incoming {
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
    if from_id != access.owner_id && !anonymous {
        return Incoming::Unauthorized { update_id, from_id };
    }
    let from_id = access.owner_id;
    Incoming::Callback {
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
    }
}

/// Photo, document, vocal ou audio : les médias que le canal sait lire.
fn media_of(
    msg: &Value,
    update_id: i64,
    chat_id: i64,
    from_id: i64,
    message_id: i64,
    topic_id: Option<i64>,
    text: &str,
) -> Option<Incoming> {
    if let Some(photos) = msg.get("photo").and_then(|v| v.as_array()) {
        return Some(Incoming::Photo {
            update_id,
            chat_id,
            from_id,
            message_id,
            topic_id,
            sizes: photos.iter().filter_map(PhotoSize::parse).collect(),
            media_group: msg
                .get("media_group_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            caption: (!text.is_empty()).then(|| text.to_string()),
        });
    }
    if let Some(d) = msg.get("document") {
        return Some(Incoming::Document {
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
            caption: (!text.is_empty()).then(|| text.to_string()),
        });
    }
    if let Some(v) = msg.get("voice").or_else(|| msg.get("audio")) {
        let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        return Some(Incoming::Voice {
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
        });
    }
    None
}

/// Classe un update brut. Le propriétaire est la **liste blanche d'un seul élément**
/// (§13.4) ; un groupe n'est ouvert que s'il figure dans `access.allowed_chats`, et y
/// parler en administrateur anonyme vaut propriétaire (issue #113).
pub fn classify(update: &Value, access: &Access) -> Incoming {
    let owner_id = access.owner_id;
    let update_id = update
        .get("update_id")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if let Some(cb) = update.get("callback_query") {
        return classify_callback(cb, update_id, access);
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

    if let Some(media) = media_of(
        msg, update_id, chat_id, from_id, message_id, topic_id, &text,
    ) {
        return media;
    }

    // Ni texte, ni photo, ni document, ni son (#461) : un fichier, une ligne lisible, un
    // message de service, ou rien que le canal sache lire. Jamais un texte vide.
    let text = match other::of(msg) {
        _ if msg.get("text").is_some() && !text.is_empty() => text,
        Other::File {
            file_id,
            file_name,
            mime_type,
            file_size,
        } => {
            return Incoming::Document {
                update_id,
                chat_id,
                from_id,
                message_id,
                topic_id,
                file_id,
                file_name,
                mime_type,
                file_size,
                caption: (!text.is_empty()).then_some(text),
            };
        }
        Other::Described(line) if text.is_empty() => line,
        Other::Described(line) => format!("{line}\n{text}"),
        Other::Service => {
            return Incoming::Ignored {
                update_id,
                reason: "message de service".into(),
            };
        }
        Other::Unknown(what) if text.is_empty() => {
            return Incoming::Unsupported {
                update_id,
                chat_id,
                from_id,
                message_id,
                topic_id,
                what,
            };
        }
        Other::Unknown(_) => text,
    };

    let reply_to = msg
        .get("reply_to_message")
        .and_then(|r| r.get("message_id"))
        .and_then(|v| v.as_i64());
    let forwarded = msg.get("forward_origin").is_some() || msg.get("forward_from").is_some();
    let replied = replied_of(msg);

    if looks_like_oauth_callback(&text) {
        return Incoming::OAuthCallback {
            update_id,
            chat_id,
            from_id,
            message_id,
            topic_id,
            text,
            reply_to,
            replied,
            forwarded,
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
        reply_to,
        replied,
        forwarded,
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
mod tests;
