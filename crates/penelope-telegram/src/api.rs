//! Client Bot API maison (§14.1).
//!
//! teloxide annonce une couverture jusqu'à Bot API 9.2, insuffisante pour les drafts (9.3)
//! et les rich messages (10.1+). Ce client couvre les méthodes utilisées, avec file
//! d'envoi par chat, respect de `retry_after` et backoff sur 5xx.

use crate::error::{TgError, TgResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Réponse générique de l'API.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub error_code: Option<i32>,
    #[serde(default)]
    pub parameters: Option<ResponseParameters>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseParameters {
    #[serde(default)]
    pub retry_after: Option<u64>,
    #[serde(default)]
    pub migrate_to_chat_id: Option<i64>,
}

/// Transport abstrait : le mock de la suite `telegram` l'implémente sans réseau.
#[async_trait::async_trait]
pub trait BotTransport: Send + Sync {
    async fn call(&self, method: &str, body: Value) -> TgResult<ApiResponse>;

    /// Envoi `multipart/form-data` d'un fichier local (`sendDocument`, `sendPhoto`).
    async fn upload(
        &self,
        method: &str,
        fields: Vec<(String, String)>,
        file_field: &str,
        path: &std::path::Path,
    ) -> TgResult<ApiResponse> {
        let _ = (method, fields, file_field, path);
        Err(TgError::Transport(
            "ce transport ne sait pas envoyer de fichier".into(),
        ))
    }

    /// Télécharge un fichier reçu, à partir du `file_path` rendu par `getFile`.
    async fn download(&self, file_path: &str) -> TgResult<Vec<u8>> {
        let _ = file_path;
        Err(TgError::Transport(
            "ce transport ne sait pas télécharger de fichier".into(),
        ))
    }
}

/// Taille maximale d'un fichier téléchargeable par un bot (Bot API : 20 Mo).
pub const DOWNLOAD_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Erreur `reqwest` sans son URL : celle de la Bot API contient le jeton (issue #26).
fn transport_error(e: reqwest::Error) -> TgError {
    TgError::Transport(e.without_url().to_string())
}

/// Transport HTTP réel.
pub struct HttpTransport {
    client: reqwest::Client,
    base: String,
    /// `{api}/file/bot<jeton>` : racine des téléchargements.
    file_base: String,
}

impl HttpTransport {
    pub fn new(api_base: &str, token: &str) -> TgResult<Self> {
        penelope_observe::register_secret(token);
        Ok(HttpTransport {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .map_err(|e| TgError::Transport(e.to_string()))?,
            base: format!("{}/bot{token}", api_base.trim_end_matches('/')),
            file_base: format!("{}/file/bot{token}", api_base.trim_end_matches('/')),
        })
    }
}

#[async_trait::async_trait]
impl BotTransport for HttpTransport {
    async fn download(&self, file_path: &str) -> TgResult<Vec<u8>> {
        use futures::StreamExt;
        let resp = self
            .client
            .get(format!(
                "{}/{}",
                self.file_base,
                file_path.trim_start_matches('/')
            ))
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        if status >= 400 {
            return Err(TgError::Transport(format!(
                "téléchargement refusé ({status})"
            )));
        }
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            if out.len() + chunk.len() > DOWNLOAD_MAX_BYTES {
                return Err(TgError::Transport(
                    "fichier trop gros : un bot ne télécharge pas plus de 20 Mo".into(),
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    async fn call(&self, method: &str, body: Value) -> TgResult<ApiResponse> {
        let resp = self
            .client
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        let parsed: ApiResponse = resp.json().await.map_err(|e| {
            TgError::Transport(format!(
                "réponse illisible ({status}) : {}",
                e.without_url()
            ))
        })?;
        Ok(parsed)
    }

    async fn upload(
        &self,
        method: &str,
        fields: Vec<(String, String)>,
        file_field: &str,
        path: &std::path::Path,
    ) -> TgResult<ApiResponse> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| TgError::Transport(format!("{} : {e}", path.display())))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "fichier".into());
        let mut form = reqwest::multipart::Form::new();
        for (k, v) in fields {
            form = form.text(k, v);
        }
        form = form.part(
            file_field.to_string(),
            reqwest::multipart::Part::bytes(bytes).file_name(name),
        );
        let resp = self
            .client
            .post(format!("{}/{method}", self.base))
            .multipart(form)
            .send()
            .await
            .map_err(transport_error)?;
        let status = resp.status().as_u16();
        resp.json().await.map_err(|e| {
            TgError::Transport(format!(
                "réponse illisible ({status}) : {}",
                e.without_url()
            ))
        })
    }
}

/// Limiteur de débit par chat : 1 message/s par défaut (§14.1).
#[derive(Clone)]
pub struct RateLimiter {
    per_chat: Arc<Mutex<BTreeMap<i64, i64>>>,
    min_interval_ms: i64,
}

impl RateLimiter {
    pub fn new(per_second: f64) -> Self {
        let ms = if per_second > 0.0 {
            (1000.0 / per_second) as i64
        } else {
            1000
        };
        RateLimiter {
            per_chat: Arc::new(Mutex::new(BTreeMap::new())),
            min_interval_ms: ms,
        }
    }

    /// Renvoie le délai à attendre avant d'envoyer dans ce chat.
    pub async fn delay_for(&self, chat_id: i64, now_ms: i64) -> i64 {
        let mut g = self.per_chat.lock().await;
        let next = g.get(&chat_id).copied().unwrap_or(0);
        let wait = (next - now_ms).max(0);
        g.insert(chat_id, now_ms.max(next) + self.min_interval_ms);
        wait
    }

    /// Impose un `retry_after` renvoyé par l'API (429).
    pub async fn apply_retry_after(&self, chat_id: i64, now_ms: i64, seconds: u64) {
        let mut g = self.per_chat.lock().await;
        g.insert(chat_id, now_ms + (seconds as i64) * 1000);
    }
}

/// Backoff sur 5xx.
pub fn backoff_ms(attempt: u32) -> u64 {
    (500u64 << attempt.min(6)).min(60_000)
}

/// Client typé.
pub struct Bot {
    transport: Arc<dyn BotTransport>,
    limiter: RateLimiter,
    clock: penelope_kernel::clock::SharedClock,
}

/// Méthodes couvertes (§14.1, Bot API 10.3 pour ce qui est utilisé).
pub mod method {
    pub const GET_ME: &str = "getMe";
    pub const GET_UPDATES: &str = "getUpdates";
    pub const SET_WEBHOOK: &str = "setWebhook";
    pub const DELETE_WEBHOOK: &str = "deleteWebhook";
    pub const SEND_MESSAGE: &str = "sendMessage";
    pub const SEND_RICH_MESSAGE: &str = "sendRichMessage";
    pub const SEND_MESSAGE_DRAFT: &str = "sendMessageDraft";
    pub const SEND_RICH_MESSAGE_DRAFT: &str = "sendRichMessageDraft";
    pub const EDIT_MESSAGE_TEXT: &str = "editMessageText";
    pub const EDIT_RICH_MESSAGE: &str = "editRichMessage";
    pub const EDIT_MESSAGE_REPLY_MARKUP: &str = "editMessageReplyMarkup";
    pub const DELETE_MESSAGE: &str = "deleteMessage";
    pub const SEND_DOCUMENT: &str = "sendDocument";
    pub const SEND_PHOTO: &str = "sendPhoto";
    pub const SEND_CHAT_ACTION: &str = "sendChatAction";
    pub const SET_MESSAGE_REACTION: &str = "setMessageReaction";
    pub const ANSWER_CALLBACK_QUERY: &str = "answerCallbackQuery";
    pub const SET_MY_COMMANDS: &str = "setMyCommands";
    pub const CREATE_FORUM_TOPIC: &str = "createForumTopic";
    pub const EDIT_FORUM_TOPIC: &str = "editForumTopic";
    pub const CLOSE_FORUM_TOPIC: &str = "closeForumTopic";
    pub const GET_FILE: &str = "getFile";
}

/// Réactions d'état sur le message utilisateur (§14.2).
pub mod reaction {
    pub const RECEIVED: &str = "👀";
    pub const WORKING: &str = "⚙️";
    pub const DONE: &str = "✅";
    pub const WAITING_APPROVAL: &str = "⚠️";
    pub const ERROR: &str = "❌";
}

impl Bot {
    pub fn new(
        transport: Arc<dyn BotTransport>,
        rate_per_second: f64,
        clock: penelope_kernel::clock::SharedClock,
    ) -> Self {
        Bot {
            transport,
            limiter: RateLimiter::new(rate_per_second),
            clock,
        }
    }

    /// Appel brut, avec gestion de `retry_after` et backoff.
    pub async fn call(&self, method: &str, chat_id: Option<i64>, body: Value) -> TgResult<Value> {
        let mut attempt = 0u32;
        loop {
            if let Some(c) = chat_id {
                let wait = self.limiter.delay_for(c, self.clock.now_ms()).await;
                if wait > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
                }
            }
            let resp = self.transport.call(method, body.clone()).await?;
            if resp.ok {
                return Ok(resp.result.unwrap_or(Value::Null));
            }

            let code = resp.error_code.unwrap_or(0);
            let description = resp.description.clone().unwrap_or_default();

            if code == 429 {
                let secs = resp
                    .parameters
                    .as_ref()
                    .and_then(|p| p.retry_after)
                    .unwrap_or(1);
                if let Some(c) = chat_id {
                    self.limiter
                        .apply_retry_after(c, self.clock.now_ms(), secs)
                        .await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                attempt += 1;
                if attempt > 8 {
                    return Err(TgError::RateLimited(secs));
                }
                continue;
            }
            if code >= 500 {
                attempt += 1;
                if attempt > 6 {
                    return Err(TgError::Api { code, description });
                }
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms(attempt))).await;
                continue;
            }
            return Err(TgError::Api { code, description });
        }
    }

    pub async fn get_me(&self) -> TgResult<Value> {
        self.call(method::GET_ME, None, json!({})).await
    }

    /// Long polling (§14.1) : `timeout = 50`, `allowed_updates` explicite.
    pub async fn get_updates(&self, offset: i64, timeout_s: u64) -> TgResult<Vec<Value>> {
        let v = self
            .call(
                method::GET_UPDATES,
                None,
                json!({
                    "offset": offset,
                    "timeout": timeout_s,
                    "allowed_updates": allowed_updates(),
                }),
            )
            .await?;
        Ok(v.as_array().cloned().unwrap_or_default())
    }

    pub async fn send_text(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        html: &str,
        markup: Option<Value>,
        reply_to: Option<i64>,
    ) -> TgResult<Value> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
        });
        decorate(&mut body, topic_id, markup, reply_to);
        self.call(method::SEND_MESSAGE, Some(chat_id), body).await
    }

    /// `sendRichMessage` (Bot API ≥ 10.1).
    ///
    /// `InputRichMessage` accepte `blocks`, `html` **ou** `markdown`. On envoie le Markdown
    /// tel quel : c'est Telegram qui le met en blocs, ce qui évite de maintenir une
    /// correspondance fragile avec la trentaine de types `InputRichBlock*`.
    pub async fn send_rich(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        markdown: &str,
        markup: Option<Value>,
        reply_to: Option<i64>,
    ) -> TgResult<Value> {
        let mut body = json!({
            "chat_id": chat_id,
            "rich_message": {"markdown": markdown},
        });
        decorate(&mut body, topic_id, markup, reply_to);
        self.call(method::SEND_RICH_MESSAGE, Some(chat_id), body)
            .await
    }

    /// Aperçu de frappe (§14.2) : `draft_id` stable par réponse, `can_stop`.
    ///
    /// Dans l'API réelle, `draft_id` est un **entier non nul**, et le brouillon n'est
    /// qu'un aperçu de 30 s : la réponse finale doit être envoyée par `sendMessage`.
    pub async fn send_draft(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        draft_id: i64,
        text: &str,
    ) -> TgResult<Value> {
        let mut body = json!({
            "chat_id": chat_id,
            "draft_id": draft_id,
            "text": text,
            "can_stop": true,
        });
        decorate(&mut body, topic_id, None, None);
        self.call(method::SEND_MESSAGE_DRAFT, Some(chat_id), body)
            .await
    }

    pub async fn set_reaction(
        &self,
        chat_id: i64,
        message_id: i64,
        emoji: &str,
    ) -> TgResult<Value> {
        self.call(
            method::SET_MESSAGE_REACTION,
            Some(chat_id),
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "reaction": [{"type":"emoji","emoji":emoji}],
                "is_big": false,
            }),
        )
        .await
    }

    /// `answerCallbackQuery` doit répondre **sous 1 s**, toujours (§14.4).
    pub async fn answer_callback(
        &self,
        callback_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> TgResult<Value> {
        self.call(
            method::ANSWER_CALLBACK_QUERY,
            None,
            json!({
                "callback_query_id": callback_id,
                "text": text,
                "show_alert": show_alert,
            }),
        )
        .await
    }

    pub async fn edit_markup(
        &self,
        chat_id: i64,
        message_id: i64,
        markup: Option<Value>,
    ) -> TgResult<Value> {
        self.call(
            method::EDIT_MESSAGE_REPLY_MARKUP,
            Some(chat_id),
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "reply_markup": markup,
            }),
        )
        .await
    }

    pub async fn edit_text(
        &self,
        chat_id: i64,
        message_id: i64,
        html: &str,
        markup: Option<Value>,
    ) -> TgResult<Value> {
        self.call(
            method::EDIT_MESSAGE_TEXT,
            Some(chat_id),
            json!({
                "chat_id": chat_id,
                "message_id": message_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": markup,
            }),
        )
        .await
    }

    /// Envoie un fichier local en document (≤ 50 Mo côté Bot API).
    pub async fn send_document(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        path: &std::path::Path,
        caption: Option<&str>,
    ) -> TgResult<Value> {
        let mut fields = vec![("chat_id".to_string(), chat_id.to_string())];
        if let Some(t) = topic_id {
            fields.push(("message_thread_id".into(), t.to_string()));
        }
        if let Some(c) = caption {
            fields.push(("caption".into(), c.chars().take(1024).collect()));
        }
        let wait = self.limiter.delay_for(chat_id, self.clock.now_ms()).await;
        if wait > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
        }
        let resp = self
            .transport
            .upload(method::SEND_DOCUMENT, fields, "document", path)
            .await?;
        if resp.ok {
            Ok(resp.result.unwrap_or(Value::Null))
        } else {
            Err(TgError::Api {
                code: resp.error_code.unwrap_or(0),
                description: resp.description.unwrap_or_default(),
            })
        }
    }

    /// Télécharge un fichier reçu (`getFile` puis URL de fichier).
    /// Récupère le contenu d'un fichier reçu. Renvoie aussi son `file_path`, dont
    /// l'extension dit le format.
    pub async fn download_file(&self, file_id: &str) -> TgResult<(Vec<u8>, String)> {
        let path = self.file_path(file_id).await?;
        let bytes = self.transport.download(&path).await?;
        Ok((bytes, path))
    }

    pub async fn file_path(&self, file_id: &str) -> TgResult<String> {
        let v = self
            .call(method::GET_FILE, None, json!({"file_id": file_id}))
            .await?;
        v.get("file_path")
            .and_then(|p| p.as_str())
            .map(String::from)
            .ok_or_else(|| TgError::Transport("getFile sans file_path".into()))
    }

    pub async fn create_topic(&self, chat_id: i64, name: &str) -> TgResult<Value> {
        self.call(
            method::CREATE_FORUM_TOPIC,
            Some(chat_id),
            json!({"chat_id": chat_id, "name": name}),
        )
        .await
    }

    pub async fn set_commands(&self, commands: Value) -> TgResult<Value> {
        self.call(method::SET_MY_COMMANDS, None, json!({"commands": commands}))
            .await
    }
}

fn decorate(body: &mut Value, topic_id: Option<i64>, markup: Option<Value>, reply_to: Option<i64>) {
    let Some(o) = body.as_object_mut() else {
        return;
    };
    if let Some(t) = topic_id {
        o.insert("message_thread_id".into(), json!(t));
    }
    if let Some(m) = markup {
        o.insert("reply_markup".into(), m);
    }
    if let Some(r) = reply_to {
        o.insert(
            "reply_parameters".into(),
            json!({"message_id": r, "allow_sending_without_reply": true}),
        );
    }
}

/// `allowed_updates` explicite (§14.1) : on ne reçoit que ce qu'on traite.
pub fn allowed_updates() -> Vec<&'static str> {
    vec![
        "message",
        "edited_message",
        "callback_query",
        "message_reaction",
        "my_chat_member",
        "stopped_message_generation",
    ]
}

/// Construit un `inline_keyboard` à partir de boutons rendus.
pub fn inline_keyboard(rows: &[Vec<crate::render::ButtonSpec>]) -> Value {
    json!({
        "inline_keyboard": rows.iter().map(|r| {
            r.iter().map(|b| b.to_json()).collect::<Vec<_>>()
        }).collect::<Vec<_>>()
    })
}

/// Mode de rendu mémorisé par chat (§14.2 : repli HTML mémorisé 24 h).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    Rich,
    Html,
}

impl RenderMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RenderMode::Rich => "rich",
            RenderMode::Html => "html",
        }
    }
}

/// Vrai si l'erreur signifie « ce chat ne sait pas faire de rich message » : on bascule en
/// HTML pour 24 h (§14.2).
pub fn should_fall_back_to_html(e: &TgError) -> bool {
    match e {
        TgError::Api { code, description } => {
            let d = description.to_lowercase();
            // On ne bascule que sur un refus de **capacité**, jamais sur une erreur de
            // cible : « chat not found » doit remonter telle quelle.
            *code == 400
                && (d.contains("rich")
                    || d.contains("block")
                    || d.contains("unsupported")
                    || d.contains("unknown method"))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;
    use penelope_kernel::clock::TestClock;

    fn bot(mock: Arc<MockTransport>) -> Bot {
        Bot::new(mock, 1000.0, Arc::new(TestClock::default()))
    }

    #[tokio::test]
    async fn send_text_builds_the_expected_body() {
        let m = MockTransport::new();
        let b = bot(m.clone());
        b.send_text(42, Some(7), "<b>bonjour</b>", None, Some(99))
            .await
            .unwrap();
        let calls = m.calls().await;
        assert_eq!(calls[0].0, method::SEND_MESSAGE);
        let body = &calls[0].1;
        assert_eq!(body["chat_id"], 42);
        assert_eq!(body["parse_mode"], "HTML");
        assert_eq!(body["message_thread_id"], 7);
        assert_eq!(body["reply_parameters"]["message_id"], 99);
        assert_eq!(body["link_preview_options"]["is_disabled"], true);
    }

    #[tokio::test]
    async fn get_updates_declares_allowed_updates() {
        let m = MockTransport::new();
        m.reply(method::GET_UPDATES, json!([])).await;
        let b = bot(m.clone());
        b.get_updates(100, 50).await.unwrap();
        let body = &m.calls().await[0].1;
        assert_eq!(body["timeout"], 50);
        assert_eq!(body["offset"], 100);
        assert!(body["allowed_updates"].as_array().unwrap().len() >= 4);
    }

    /// CA 14 : 429 simulé, aucune perte, respect de `retry_after`.
    #[tokio::test]
    async fn ca_14_3_rate_limit_is_respected_without_loss() {
        let m = MockTransport::new();
        m.fail_once(429, "Too Many Requests: retry after 1", Some(1))
            .await;
        m.reply(method::SEND_MESSAGE, json!({"message_id": 5}))
            .await;
        let b = bot(m.clone());

        let started = std::time::Instant::now();
        let r = b.send_text(42, None, "texte", None, None).await.unwrap();
        assert_eq!(r["message_id"], 5, "le message finit par partir");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(900),
            "le délai imposé doit être respecté"
        );
        assert_eq!(m.calls().await.len(), 2, "une seule reprise");
    }

    #[tokio::test]
    async fn server_errors_are_retried_with_backoff() {
        let m = MockTransport::new();
        m.fail_once(500, "Internal Server Error", None).await;
        m.reply(method::SEND_MESSAGE, json!({"message_id": 1}))
            .await;
        let b = bot(m.clone());
        assert!(b.send_text(1, None, "x", None, None).await.is_ok());
        assert_eq!(m.calls().await.len(), 2);
    }

    #[tokio::test]
    async fn client_errors_are_not_retried() {
        let m = MockTransport::new();
        m.always_fail(400, "Bad Request: chat not found").await;
        let b = bot(m.clone());
        let e = b.send_text(1, None, "x", None, None).await.unwrap_err();
        assert!(matches!(e, TgError::Api { code: 400, .. }));
        assert_eq!(
            m.calls().await.len(),
            1,
            "aucune reprise sur une erreur 4xx"
        );
    }

    #[tokio::test]
    async fn rate_limiter_spaces_messages_per_chat() {
        let l = RateLimiter::new(1.0);
        assert_eq!(l.delay_for(1, 0).await, 0);
        assert_eq!(
            l.delay_for(1, 0).await,
            1000,
            "second message : 1 s d'attente"
        );
        assert_eq!(
            l.delay_for(2, 0).await,
            0,
            "un autre chat n'est pas pénalisé"
        );
    }

    /// Issue #26 : une panne réseau ne met pas l'URL, donc le jeton, dans l'erreur.
    #[tokio::test]
    async fn transport_errors_never_carry_the_token() {
        let token = "5123456789:AAHno_token_in_errors_abcdefghijklmnop";
        let t = HttpTransport::new("http://127.0.0.1:9", token).unwrap();
        let err = t
            .call("getUpdates", json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("AAHno_token"), "{err}");
        assert!(!err.contains("127.0.0.1:9/bot"), "{err}");
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(1), 1000);
        assert_eq!(backoff_ms(3), 4000);
        assert_eq!(backoff_ms(20), 32_000);
    }

    #[test]
    fn html_fallback_is_triggered_by_capability_errors() {
        assert!(should_fall_back_to_html(&TgError::Api {
            code: 400,
            description: "Bad Request: unknown method sendRichMessage".into()
        }));
        assert!(should_fall_back_to_html(&TgError::Api {
            code: 400,
            description: "Bad Request: RICH_BLOCK_INVALID".into()
        }));
        assert!(!should_fall_back_to_html(&TgError::Api {
            code: 400,
            description: "Bad Request: chat not found".into()
        }));
        assert!(!should_fall_back_to_html(&TgError::Transport(
            "coupure".into()
        )));
    }

    #[test]
    fn inline_keyboard_shape() {
        let rows = vec![vec![
            crate::render::ButtonSpec::callback("Oui", "a:1", "success"),
            crate::render::ButtonSpec::url("Doc", "https://x"),
        ]];
        let k = inline_keyboard(&rows);
        assert_eq!(k["inline_keyboard"][0][0]["callback_data"], "a:1");
        assert_eq!(k["inline_keyboard"][0][1]["url"], "https://x");
    }

    #[tokio::test]
    async fn reactions_use_the_prd_emojis() {
        let m = MockTransport::new();
        let b = bot(m.clone());
        b.set_reaction(1, 2, reaction::WORKING).await.unwrap();
        let body = &m.calls().await[0].1;
        assert_eq!(body["reaction"][0]["emoji"], "⚙️");
    }
}
