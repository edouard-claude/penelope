//! Les messages qui ne sont ni un texte, ni une photo, ni un document, ni un son (#461).
//!
//! Avant, chacun devenait un `Incoming::Text` vide : un tour sans rien à lire, payé pour
//! une réponse à rien, ou refusé par le fournisseur (`BadRequest`, 6 tours sur 27 relevés
//! le 11/11).
//!
//! ```text
//!  vidéo, note vidéo ──────────────► un fichier : il suit le chemin d'un document
//!  sticker, lieu, contact, sondage ► une ligne lisible, en tête de la légende
//!  sujet créé, épinglage, arrivée ─► message de service : pas un message entrant
//!  le reste, sans légende ─────────► « non pris en charge » dit au propriétaire, sans tour
//! ```

use serde_json::Value;

/// Ce qu'est un message sans type connu de [`crate::classify`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Other {
    /// Un fichier que le canal sait télécharger.
    File {
        file_id: String,
        file_name: String,
        mime_type: Option<String>,
        file_size: Option<i64>,
    },
    /// Un contenu sans fichier, dit en une ligne que le modèle peut lire.
    Described(String),
    /// Un message de service : Telegram raconte un événement, personne n'écrit.
    Service,
    /// Rien de reconnu ; le premier champ inconnu du message, s'il y en a un.
    Unknown(Option<String>),
}

/// Les champs d'un message de service de l'API Bot.
const SERVICE: &[&str] = &[
    "new_chat_members",
    "left_chat_member",
    "new_chat_title",
    "new_chat_photo",
    "delete_chat_photo",
    "group_chat_created",
    "supergroup_chat_created",
    "channel_chat_created",
    "message_auto_delete_timer_changed",
    "migrate_to_chat_id",
    "migrate_from_chat_id",
    "pinned_message",
    "successful_payment",
    "refunded_payment",
    "users_shared",
    "chat_shared",
    "gift",
    "unique_gift",
    "connected_website",
    "write_access_allowed",
    "passport_data",
    "proximity_alert_triggered",
    "boost_added",
    "chat_background_set",
    "checklist_tasks_done",
    "checklist_tasks_added",
    "direct_message_price_changed",
    "forum_topic_created",
    "forum_topic_edited",
    "forum_topic_closed",
    "forum_topic_reopened",
    "general_forum_topic_hidden",
    "general_forum_topic_unhidden",
    "giveaway_created",
    "giveaway",
    "giveaway_winners",
    "giveaway_completed",
    "paid_message_price_changed",
    "suggested_post_approved",
    "suggested_post_approval_failed",
    "suggested_post_declined",
    "suggested_post_paid",
    "suggested_post_refunded",
    "video_chat_scheduled",
    "video_chat_started",
    "video_chat_ended",
    "video_chat_participants_invited",
    "web_app_data",
];

/// Les champs qui entourent un contenu sans en être un : ce qui n'y figure pas, dans un
/// message que rien n'a reconnu, nomme le type à ajouter ici.
const ENVELOPE: &[&str] = &[
    "message_id",
    "message_thread_id",
    "direct_messages_topic",
    "from",
    "sender_chat",
    "sender_boost_count",
    "sender_business_bot",
    "date",
    "business_connection_id",
    "chat",
    "forward_origin",
    "forward_from",
    "forward_from_chat",
    "forward_from_message_id",
    "forward_signature",
    "forward_sender_name",
    "forward_date",
    "is_topic_message",
    "is_automatic_forward",
    "reply_to_message",
    "external_reply",
    "quote",
    "reply_to_story",
    "reply_to_checklist_task_id",
    "via_bot",
    "edit_date",
    "has_protected_content",
    "is_from_offline",
    "is_paid_post",
    "media_group_id",
    "author_signature",
    "paid_star_count",
    "text",
    "entities",
    "link_preview_options",
    "suggested_post_info",
    "effect_id",
    "caption",
    "caption_entities",
    "show_caption_above_media",
    "has_media_spoiler",
    "reply_markup",
];

/// Une ligne ne dépasse pas cette longueur : un sondage a dix choix de cent caractères.
const LINE_MAX: usize = 600;

/// Classe un message que ni `text`, ni `photo`, ni `document`, ni `voice` ou `audio` n'a
/// fait reconnaître.
pub(crate) fn of(msg: &Value) -> Other {
    if let Some(v) = msg.get("video") {
        return file(v, "video.mp4");
    }
    if let Some(v) = msg.get("video_note") {
        return file(v, "note-video.mp4");
    }
    if let Some(line) = describe(msg) {
        let mut line: String = line.chars().take(LINE_MAX).collect();
        if !line.ends_with(']') {
            line.push_str("…]");
        }
        return Other::Described(line);
    }
    if SERVICE.iter().any(|k| msg.get(*k).is_some()) {
        return Other::Service;
    }
    let unknown = msg
        .as_object()
        .and_then(|o| o.keys().find(|k| !ENVELOPE.contains(&k.as_str())))
        .cloned();
    Other::Unknown(unknown)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    let s = v.get(key)?.as_str()?;
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!s.is_empty()).then_some(s)
}

fn file(v: &Value, default_name: &str) -> Other {
    Other::File {
        file_id: str_of(v, "file_id").unwrap_or_default(),
        file_name: str_of(v, "file_name").unwrap_or_else(|| default_name.to_string()),
        mime_type: str_of(v, "mime_type").or_else(|| Some("video/mp4".to_string())),
        file_size: v.get("file_size").and_then(|x| x.as_i64()),
    }
}

/// Latitude et longitude d'un `Location`, telles que Telegram les donne.
fn coordinates(location: Option<&Value>) -> Option<String> {
    let l = location?;
    let (lat, lon) = (l.get("latitude")?.as_f64()?, l.get("longitude")?.as_f64()?);
    Some(format!("{lat}, {lon}"))
}

/// La ligne qui dit un contenu sans fichier. Un lieu porte aussi `location` : il passe
/// avant.
fn describe(msg: &Value) -> Option<String> {
    if let Some(s) = msg.get("sticker") {
        return Some(match str_of(s, "emoji") {
            Some(emoji) => format!("[sticker {emoji}]"),
            None => "[sticker]".to_string(),
        });
    }
    if let Some(v) = msg.get("venue") {
        let name = [str_of(v, "title"), str_of(v, "address")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        return Some(match coordinates(v.get("location")) {
            Some(at) => format!("[lieu : {name} ({at})]"),
            None => format!("[lieu : {name}]"),
        });
    }
    if let Some(l) = msg.get("location") {
        return Some(match coordinates(Some(l)) {
            Some(at) => format!("[localisation {at}]"),
            None => "[localisation]".to_string(),
        });
    }
    if let Some(c) = msg.get("contact") {
        let who = [
            str_of(c, "first_name"),
            str_of(c, "last_name"),
            str_of(c, "phone_number"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        return Some(format!("[contact : {who}]"));
    }
    if let Some(p) = msg.get("poll") {
        let question = str_of(p, "question").unwrap_or_default();
        return Some(match texts_of(p.get("options")) {
            Some(options) => format!("[sondage : {question} ; choix : {options}]"),
            None => format!("[sondage : {question}]"),
        });
    }
    if let Some(c) = msg.get("checklist") {
        let title = str_of(c, "title").unwrap_or_default();
        return Some(match texts_of(c.get("tasks")) {
            Some(tasks) => format!("[liste : {title} ; tâches : {tasks}]"),
            None => format!("[liste : {title}]"),
        });
    }
    if let Some(d) = msg.get("dice") {
        let emoji = str_of(d, "emoji").unwrap_or_else(|| "🎲".to_string());
        let value = d.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
        return Some(format!("[dé {emoji} : {value}]"));
    }
    if let Some(g) = msg.get("game") {
        let title = str_of(g, "title").unwrap_or_default();
        return Some(format!("[jeu : {title}]"));
    }
    if let Some(i) = msg.get("invoice") {
        let title = str_of(i, "title").unwrap_or_default();
        return Some(format!("[facture : {title}]"));
    }
    if msg.get("story").is_some() {
        return Some("[story partagée, contenu non lu]".to_string());
    }
    if msg.get("paid_media").is_some() {
        return Some("[média payant, contenu non lu]".to_string());
    }
    None
}

/// Les `text` d'une liste d'objets (choix d'un sondage, tâches d'une liste), en une ligne.
fn texts_of(list: Option<&Value>) -> Option<String> {
    let texts: Vec<String> = list?
        .as_array()?
        .iter()
        .filter_map(|o| str_of(o, "text"))
        .collect();
    (!texts.is_empty()).then(|| texts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, Incoming, classify};
    use serde_json::json;

    const OWNER: i64 = 42;

    /// Un message du propriétaire, en privé, avec ce contenu.
    fn update(content: Value) -> Value {
        let mut msg = json!({
            "message_id": 70,
            "date": 1789516800,
            "chat": {"id": OWNER, "type": "private"},
            "from": {"id": OWNER, "is_bot": false, "first_name": "Edouard"},
        });
        for (k, v) in content.as_object().unwrap() {
            msg[k] = v.clone();
        }
        json!({"update_id": 7, "message": msg})
    }

    fn classified(content: Value) -> Incoming {
        classify(&update(content), &Access::owner_only(OWNER))
    }

    fn text_of(content: Value) -> String {
        match classified(content) {
            Incoming::Text { text, .. } => text,
            other => panic!("attendu un texte, obtenu {other:?}"),
        }
    }

    /// Les messages de service ne sont pas des messages : aucun ne devient un texte vide.
    #[test]
    fn a_service_message_is_not_an_incoming_message() {
        let services = [
            json!({"forum_topic_created": {"name": "Projet", "icon_color": 7322096},
                   "message_thread_id": 12, "is_topic_message": true}),
            json!({"forum_topic_edited": {"name": "Projet bis"}, "message_thread_id": 12}),
            json!({"forum_topic_closed": {}, "message_thread_id": 12}),
            json!({"pinned_message": {"message_id": 3, "text": "à garder"}}),
            json!({"new_chat_title": "Nouveau nom"}),
            json!({"new_chat_members": [{"id": 9, "is_bot": true}]}),
        ];
        for content in services {
            match classified(content.clone()) {
                Incoming::Ignored { reason, .. } => assert_eq!(reason, "message de service"),
                other => panic!("{content} : {other:?}"),
            }
        }
    }

    #[test]
    fn a_content_without_a_file_becomes_a_line_the_model_can_read() {
        let cases = [
            (
                json!({"sticker": {"file_id": "s1", "emoji": "😀"}}),
                "[sticker 😀]",
            ),
            (json!({"sticker": {"file_id": "s1"}}), "[sticker]"),
            (
                json!({"location": {"latitude": 48.85, "longitude": 2.35}}),
                "[localisation 48.85, 2.35]",
            ),
            (
                json!({"venue": {"title": "Gare de Lyon", "address": "Place Louis-Armand",
                                 "location": {"latitude": 48.8443, "longitude": 2.3743}},
                       "location": {"latitude": 48.8443, "longitude": 2.3743}}),
                "[lieu : Gare de Lyon, Place Louis-Armand (48.8443, 2.3743)]",
            ),
            (
                json!({"contact": {"first_name": "Ada", "last_name": "Lovelace",
                                   "phone_number": "+33100000000"}}),
                "[contact : Ada Lovelace +33100000000]",
            ),
            (
                json!({"poll": {"question": "Quel jour ?",
                                "options": [{"text": "lundi"}, {"text": "mardi"}]}}),
                "[sondage : Quel jour ? ; choix : lundi, mardi]",
            ),
            (json!({"dice": {"emoji": "🎯", "value": 6}}), "[dé 🎯 : 6]"),
            (
                json!({"story": {"id": 4}}),
                "[story partagée, contenu non lu]",
            ),
        ];
        for (content, expected) in cases {
            assert_eq!(text_of(content), expected);
        }
    }

    /// La légende reste la demande : la ligne la précède, et n'en fait ni une commande ni
    /// un texte vide.
    #[test]
    fn the_line_comes_before_the_caption() {
        let text = text_of(json!({
            "location": {"latitude": 48.85, "longitude": 2.35},
            "caption": "c'est loin de la gare ?",
        }));
        assert_eq!(text, "[localisation 48.85, 2.35]\nc'est loin de la gare ?");
    }

    #[test]
    fn a_long_line_is_cut_and_closed() {
        let options: Vec<Value> = (0..12).map(|_| json!({"text": "x".repeat(100)})).collect();
        let text = text_of(json!({"poll": {"question": "Lequel ?", "options": options}}));
        assert_eq!(text.chars().count(), LINE_MAX + 2);
        assert!(text.ends_with("…]"), "{text}");
    }

    /// Une vidéo et une note vidéo sont des fichiers : elles suivent le chemin d'un
    /// document, légende comprise.
    #[test]
    fn a_video_takes_the_path_of_a_document() {
        let video = classified(json!({
            "video": {"file_id": "v9", "file_name": "démo.mov", "mime_type": "video/quicktime",
                      "file_size": 4096, "duration": 3},
            "caption": "extrais le son",
        }));
        match video {
            Incoming::Document {
                file_id,
                file_name,
                mime_type,
                file_size,
                caption,
                ..
            } => {
                assert_eq!(file_id, "v9");
                assert_eq!(file_name, "démo.mov");
                assert_eq!(mime_type.as_deref(), Some("video/quicktime"));
                assert_eq!(file_size, Some(4096));
                assert_eq!(caption.as_deref(), Some("extrais le son"));
            }
            other => panic!("{other:?}"),
        }
        match classified(json!({"video_note": {"file_id": "n1", "length": 240}})) {
            Incoming::Document {
                file_name,
                mime_type,
                caption,
                ..
            } => {
                assert_eq!(file_name, "note-video.mp4");
                assert_eq!(mime_type.as_deref(), Some("video/mp4"));
                assert_eq!(caption, None);
            }
            other => panic!("{other:?}"),
        }
    }

    /// Un type que rien ne reconnaît n'est jamais un texte vide : il est nommé, pour être
    /// dit au propriétaire et ajouté ici. Avec une légende, la légende passe.
    #[test]
    fn an_unknown_type_is_named_and_never_an_empty_text() {
        match classified(json!({"hologram": {"id": 1}})) {
            Incoming::Unsupported {
                what, message_id, ..
            } => {
                assert_eq!(what.as_deref(), Some("hologram"));
                assert_eq!(message_id, 70);
            }
            other => panic!("{other:?}"),
        }
        match classified(json!({})) {
            Incoming::Unsupported { what, .. } => assert_eq!(what, None),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            text_of(json!({"hologram": {"id": 1}, "caption": "regarde"})),
            "regarde"
        );
    }

    /// Ce qui marchait continue : un texte, une commande, une photo sans légende.
    #[test]
    fn known_types_are_left_alone() {
        assert_eq!(text_of(json!({"text": "bonjour"})), "bonjour");
        assert!(matches!(
            classified(json!({"text": "/status"})),
            Incoming::Command { .. }
        ));
        assert!(matches!(
            classified(json!({"photo": [{"file_id": "p", "width": 1, "height": 1}]})),
            Incoming::Photo { caption: None, .. }
        ));
        assert!(matches!(
            classified(json!({"animation": {"file_id": "a"},
                              "document": {"file_id": "a", "file_name": "chat.gif"}})),
            Incoming::Document { .. }
        ));
    }
}
