//! Envoi vers un sujet supprimé (#463).
//!
//! Telegram ne prévient pas quand un sujet disparaît : on l'apprend au refus d'un envoi,
//! 400 « message thread not found ». Avant, la ligne passait `failed` au premier refus, et
//! la note d'échec disait qu'un message manquait sans dire lequel. Le 12/10, quatre
//! messages de planifications et de runs se sont perdus ainsi.
//!
//! ```text
//!  envoi (groupe, sujet 21) ── 400 « message thread not found »
//!        │
//!        ├─ le même envoi, sans sujet : il arrive dans le fil général, précédé d'une ligne
//!        │  qui dit d'où il vient ; `failed` seulement si ce second essai échoue
//!        └─ les planifications et les runs qui livraient dans ce sujet sont rattachés
//!           au fil général : leurs envois suivants ne refont pas le détour
//! ```
//!
//! La session du sujet, elle, garde sa liaison : la rattacher au fil général ferait
//! continuer là une conversation que le propriétaire a supprimée, ou délogerait la session
//! qui s'y trouve. Ses sorties tardives (résultat d'un outil en arrière-plan) prennent le
//! détour, ligne comprise.

use super::*;
use penelope_telegram::render::escape_html;

/// Limites de Telegram pour un texte et pour une légende, en unités UTF-16.
const TEXT_MAX: usize = 4_096;
const CAPTION_MAX: usize = 1_024;

/// Le sujet que Telegram dit introuvable, si `error` est ce refus-là pour `body`.
/// « message thread not found » est ce que disent les journaux du 12/10 ; `TOPIC_DELETED`
/// est le nom du même refus quand il remonte tel quel de MTProto.
pub(super) fn lost_topic(error: &TgError, body: &Value) -> Option<i64> {
    let TgError::Api {
        code: 400,
        description,
    } = error
    else {
        return None;
    };
    let said = description.to_lowercase();
    if !(said.contains("message thread not found") || said.contains("topic_deleted")) {
        return None;
    }
    body.get("message_thread_id").and_then(Value::as_i64)
}

/// Telegram refuse le HTML de `body` : le même contenu part en texte brut.
fn refuses_html(error: &TgError, body: &Value) -> bool {
    matches!(error, TgError::Api { code: 400, description }
        if description.to_lowercase().contains("parse"))
        && body.get("parse_mode").is_some()
}

fn plain(mut body: Value) -> Value {
    if let Some(o) = body.as_object_mut() {
        o.remove("parse_mode");
        let text = o
            .get("text")
            .and_then(Value::as_str)
            .map(html_to_plain)
            .unwrap_or_default();
        o.insert("text".into(), json!(text));
    }
    body
}

/// `body` sans son sujet ni sa réponse à un message parti avec lui, précédé de `line`
/// quand elle tient dans le texte ou la légende. Faux : elle n'y tient pas (ou l'envoi
/// n'a ni texte ni légende), à l'appelant de la faire partir seule, juste avant.
fn to_general(body: &mut Value, line: &str) -> bool {
    let html = body.get("parse_mode").and_then(Value::as_str) == Some("HTML");
    let Some(o) = body.as_object_mut() else {
        return false;
    };
    for key in [
        "message_thread_id",
        "reply_parameters",
        "reply_to_message_id",
    ] {
        o.remove(key);
    }
    let line = if html {
        escape_html(line)
    } else {
        line.to_string()
    };
    for (key, max) in [("text", TEXT_MAX), ("caption", CAPTION_MAX)] {
        let Some(old) = o.get(key).and_then(Value::as_str) else {
            continue;
        };
        if old.encode_utf16().count() + line.encode_utf16().count() + 2 > max {
            return false;
        }
        let new = format!("{line}\n\n{old}");
        o.insert(key.into(), json!(new));
        return true;
    }
    false
}

impl TelegramGateway {
    /// Envoie une ligne de la file, avec ses seconds essais : sans sujet quand celui-ci
    /// n'existe plus (#463), en texte brut quand Telegram refuse le HTML. Chaque second
    /// essai retire ce qui l'a provoqué : trois appels au plus.
    pub(super) async fn send_row(
        &self,
        id: &str,
        method: &str,
        chat_id: i64,
        mut body: Value,
    ) -> Result<Value, TgError> {
        loop {
            let error = match self.bot.call(method, Some(chat_id), body.clone()).await {
                Ok(sent) => return Ok(sent),
                Err(e) => e,
            };
            if let Some(topic_id) = lost_topic(&error, &body) {
                body = self.rerouted(chat_id, topic_id, body).await;
                self.keep_rerouted(id, &body).await;
            } else if refuses_html(&error, &body) {
                body = plain(body);
            } else {
                return Err(error);
            }
        }
    }

    /// Le même envoi pour le fil général, et les destinations mémorisées corrigées.
    async fn rerouted(&self, chat_id: i64, topic_id: i64, mut body: Value) -> Value {
        let s = &self.daemon.services;
        tracing::warn!(
            chat_id,
            topic_id,
            "envoi vers un sujet supprimé : il part dans le fil général"
        );
        let name = s.kv_get(&topic_name_key(chat_id, topic_id)).await;
        let line = match name.ok().flatten() {
            Some(name) => format!("↪️ Sujet « {name} » supprimé : ce message lui était destiné."),
            None => "↪️ Sujet supprimé : ce message lui était destiné.".to_string(),
        };
        if !to_general(&mut body, &line) {
            let alone = json!({"chat_id": chat_id, "text": line});
            if let Err(e) = self
                .bot
                .call(
                    penelope_telegram::api::method::SEND_MESSAGE,
                    Some(chat_id),
                    alone,
                )
                .await
            {
                tracing::warn!(error = %e, chat_id, "ligne d'un envoi rerouté non partie");
            }
        }
        self.topic_lost(chat_id, topic_id).await;
        body
    }

    /// La ligne de la file garde son envoi rerouté : une nouvelle tentative (panne de
    /// transport au second essai) ne refait ni le détour ni la ligne partie seule.
    async fn keep_rerouted(&self, id: &str, body: &Value) {
        let (id, payload) = (id.to_string(), body.to_string());
        let kept = self.daemon.services.store.write(move |tx| {
            tx.execute(
                "UPDATE tg_outbox SET topic_id = NULL, payload = ?2 WHERE id = ?1",
                params![id, payload],
            )?;
            Ok(())
        });
        if let Err(e) = kept.await {
            tracing::warn!(error = %e, "envoi rerouté non gardé dans la file");
        }
    }

    /// Rattache au fil général ce que le cœur avait mémorisé pour ce sujet. Le cœur ne
    /// sait pas ce qu'est un sujet : il présente chaque destination, le canal répond.
    async fn topic_lost(&self, chat_id: i64, topic_id: i64) {
        let general = Origin::Telegram {
            chat_id,
            topic_id: None,
            message_id: None,
        };
        let moved = move |origin: &Origin| {
            (origin.telegram_chat() == Some((chat_id, Some(topic_id)))).then(|| general.clone())
        };
        let s = &self.daemon.services;
        match penelope_orchestrator::destinations::relocate(s, &moved).await {
            Ok(done) if done.is_empty() => {}
            Ok(done) => tracing::info!(
                chat_id,
                topic_id,
                schedules = ?done.schedules,
                runs = ?done.runs,
                "sujet supprimé : destinations rattachées au fil général"
            ),
            Err(e) => {
                tracing::warn!(error = %e, chat_id, topic_id, "sujet supprimé : destinations non corrigées")
            }
        }
    }
}
