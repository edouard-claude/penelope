//! Mock de la Bot API (§14, CA 14) : toute la suite `telegram` tourne sans réseau.

use crate::api::{ApiResponse, BotTransport, ResponseParameters};
use crate::error::{TgError, TgResult};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

struct Scripted {
    code: i32,
    description: String,
    retry_after: Option<u64>,
}

#[derive(Default)]
struct State {
    calls: Vec<(String, Value)>,
    failures: VecDeque<Scripted>,
    always_fail: Option<Scripted>,
    replies: std::collections::BTreeMap<String, VecDeque<Value>>,
    next_message_id: i64,
    updates: VecDeque<Value>,
}

/// Transport simulé.
pub struct MockTransport {
    state: Mutex<State>,
}

impl MockTransport {
    pub fn new() -> Arc<MockTransport> {
        Arc::new(MockTransport {
            state: Mutex::new(State {
                next_message_id: 1000,
                ..Default::default()
            }),
        })
    }

    /// Programme une réponse pour une méthode (consommée dans l'ordre).
    pub async fn reply(&self, method: &str, result: Value) {
        self.state
            .lock()
            .await
            .replies
            .entry(method.to_string())
            .or_default()
            .push_back(result);
    }

    /// Programme un échec unique.
    pub async fn fail_once(&self, code: i32, description: &str, retry_after: Option<u64>) {
        self.state.lock().await.failures.push_back(Scripted {
            code,
            description: description.to_string(),
            retry_after,
        });
    }

    /// Échoue systématiquement.
    pub async fn always_fail(&self, code: i32, description: &str) {
        self.state.lock().await.always_fail = Some(Scripted {
            code,
            description: description.to_string(),
            retry_after: None,
        });
    }

    /// Ajoute un update à renvoyer par `getUpdates`.
    pub async fn push_update(&self, update: Value) {
        self.state.lock().await.updates.push_back(update);
    }

    pub async fn calls(&self) -> Vec<(String, Value)> {
        self.state.lock().await.calls.clone()
    }

    pub async fn calls_to(&self, method: &str) -> Vec<Value> {
        self.state
            .lock()
            .await
            .calls
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, v)| v.clone())
            .collect()
    }

    pub async fn clear(&self) {
        let mut g = self.state.lock().await;
        g.calls.clear();
    }
}

#[async_trait::async_trait]
impl BotTransport for MockTransport {
    async fn upload(
        &self,
        method: &str,
        fields: Vec<(String, String)>,
        file_field: &str,
        path: &std::path::Path,
    ) -> TgResult<ApiResponse> {
        let mut body = serde_json::Map::new();
        for (k, v) in fields {
            body.insert(k, json!(v));
        }
        body.insert(
            file_field.to_string(),
            json!(path.file_name().map(|n| n.to_string_lossy().to_string())),
        );
        self.call(method, Value::Object(body)).await
    }

    async fn call(&self, method: &str, body: Value) -> TgResult<ApiResponse> {
        let mut g = self.state.lock().await;
        g.calls.push((method.to_string(), body.clone()));

        if let Some(f) = &g.always_fail {
            return Ok(ApiResponse {
                ok: false,
                result: None,
                description: Some(f.description.clone()),
                error_code: Some(f.code),
                parameters: f.retry_after.map(|s| ResponseParameters {
                    retry_after: Some(s),
                    migrate_to_chat_id: None,
                }),
            });
        }
        if let Some(f) = g.failures.pop_front() {
            return Ok(ApiResponse {
                ok: false,
                result: None,
                description: Some(f.description),
                error_code: Some(f.code),
                parameters: f.retry_after.map(|s| ResponseParameters {
                    retry_after: Some(s),
                    migrate_to_chat_id: None,
                }),
            });
        }
        if let Some(q) = g.replies.get_mut(method) {
            if let Some(v) = q.pop_front() {
                return Ok(ok(v));
            }
        }

        // Réponses par défaut, réalistes.
        let result = match method {
            crate::api::method::GET_ME => {
                json!({"id": 1, "is_bot": true, "username": "penelope_test_bot"})
            }
            crate::api::method::GET_UPDATES => {
                let v: Vec<Value> = g.updates.drain(..).collect();
                Value::Array(v)
            }
            crate::api::method::SEND_MESSAGE
            | crate::api::method::SEND_RICH_MESSAGE
            | crate::api::method::SEND_DOCUMENT
            | crate::api::method::SEND_PHOTO => {
                g.next_message_id += 1;
                json!({
                    "message_id": g.next_message_id,
                    "chat": {"id": body.get("chat_id").cloned().unwrap_or(json!(0))},
                })
            }
            crate::api::method::SEND_MESSAGE_DRAFT
            | crate::api::method::SEND_RICH_MESSAGE_DRAFT => {
                // L'API réelle renvoie `True`.
                json!(true)
            }
            crate::api::method::CREATE_FORUM_TOPIC => {
                g.next_message_id += 1;
                json!({
                    "message_thread_id": g.next_message_id,
                    "name": body.get("name").cloned().unwrap_or(json!("topic")),
                })
            }
            _ => json!(true),
        };
        Ok(ok(result))
    }
}

fn ok(result: Value) -> ApiResponse {
    ApiResponse {
        ok: true,
        result: Some(result),
        description: None,
        error_code: None,
        parameters: None,
    }
}

/// Fabrique d'updates réalistes, pour les tests d'ingestion.
pub mod updates {
    use serde_json::{Value, json};

    pub fn text_message(update_id: i64, chat_id: i64, from_id: i64, text: &str) -> Value {
        json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "date": 1789516800,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": from_id, "is_bot": false, "first_name": "Edouard"},
                "text": text,
            }
        })
    }

    pub fn in_topic(mut m: Value, topic_id: i64) -> Value {
        if let Some(msg) = m.get_mut("message").and_then(|x| x.as_object_mut()) {
            msg.insert("message_thread_id".into(), json!(topic_id));
        }
        m
    }

    pub fn callback(update_id: i64, from_id: i64, data: &str, message_id: i64) -> Value {
        json!({
            "update_id": update_id,
            "callback_query": {
                "id": format!("cb{update_id}"),
                "from": {"id": from_id, "is_bot": false},
                "data": data,
                "message": {"message_id": message_id, "chat": {"id": from_id}},
            }
        })
    }

    pub fn photo(update_id: i64, chat_id: i64, from_id: i64, group: Option<&str>) -> Value {
        let mut v = json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "date": 1789516800,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": from_id, "is_bot": false},
                "photo": [{"file_id": "f1", "width": 100, "height": 100, "file_size": 1000}],
            }
        });
        if let Some(g) = group {
            v["message"]["media_group_id"] = json!(g);
        }
        v
    }

    pub fn document(update_id: i64, chat_id: i64, from_id: i64, name: &str) -> Value {
        json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "date": 1789516800,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": from_id, "is_bot": false},
                "document": {"file_id": "d1", "file_name": name, "file_size": 2048},
            }
        })
    }

    pub fn voice(update_id: i64, chat_id: i64, from_id: i64) -> Value {
        json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "date": 1789516800,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": from_id, "is_bot": false},
                "voice": {"file_id": "v1", "duration": 5},
            }
        })
    }

    pub fn forwarded(update_id: i64, chat_id: i64, from_id: i64, text: &str) -> Value {
        let mut v = text_message(update_id, chat_id, from_id, text);
        v["message"]["forward_origin"] = json!({
            "type": "user",
            "sender_user": {"id": 999, "is_bot": false, "first_name": "Quelqu'un"}
        });
        v
    }

    pub fn edited(update_id: i64, chat_id: i64, from_id: i64, text: &str) -> Value {
        json!({
            "update_id": update_id,
            "edited_message": {
                "message_id": update_id * 10,
                "date": 1789516800,
                "edit_date": 1789516900,
                "chat": {"id": chat_id, "type": "private"},
                "from": {"id": from_id, "is_bot": false},
                "text": text,
            }
        })
    }

    pub fn stopped_generation(update_id: i64, chat_id: i64, draft_id: i64) -> Value {
        json!({
            "update_id": update_id,
            "stopped_message_generation": {
                "chat": {"id": chat_id, "type": "private"},
                "draft_id": draft_id,
            }
        })
    }
}

/// Erreur de transport simulée, pour les tests de résilience.
pub fn transport_error(msg: &str) -> TgError {
    TgError::Transport(msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Bot, method};
    use penelope_kernel::clock::TestClock;

    #[tokio::test]
    async fn mock_answers_with_realistic_shapes() {
        let m = MockTransport::new();
        let b = Bot::new(m.clone(), 1000.0, Arc::new(TestClock::default()));
        let me = b.get_me().await.unwrap();
        assert_eq!(me["is_bot"], true);

        let sent = b.send_text(42, None, "bonjour", None, None).await.unwrap();
        assert!(sent["message_id"].as_i64().unwrap() > 1000);
        assert_eq!(sent["chat"]["id"], 42);
    }

    #[tokio::test]
    async fn updates_are_drained_once() {
        let m = MockTransport::new();
        m.push_update(updates::text_message(1, 42, 42, "salut"))
            .await;
        let b = Bot::new(m.clone(), 1000.0, Arc::new(TestClock::default()));
        assert_eq!(b.get_updates(0, 50).await.unwrap().len(), 1);
        assert!(b.get_updates(2, 50).await.unwrap().is_empty());
    }

    #[test]
    fn update_factories_produce_valid_shapes() {
        let m = updates::text_message(1, 42, 42, "salut");
        assert_eq!(m["message"]["text"], "salut");
        let t = updates::in_topic(m, 7);
        assert_eq!(t["message"]["message_thread_id"], 7);

        let c = updates::callback(2, 42, "a:abc", 100);
        assert_eq!(c["callback_query"]["data"], "a:abc");

        let p = updates::photo(3, 42, 42, Some("g1"));
        assert_eq!(p["message"]["media_group_id"], "g1");

        let f = updates::forwarded(4, 42, 42, "contenu transféré");
        assert!(f["message"]["forward_origin"].is_object());

        let e = updates::edited(5, 42, 42, "corrigé");
        assert!(e["edited_message"].is_object());

        let s = updates::stopped_generation(6, 42, 71);
        assert_eq!(s["stopped_message_generation"]["draft_id"], 71);
    }

    #[tokio::test]
    async fn scripted_replies_take_precedence() {
        let m = MockTransport::new();
        m.reply(method::SEND_MESSAGE, json!({"message_id": 7}))
            .await;
        let b = Bot::new(m.clone(), 1000.0, Arc::new(TestClock::default()));
        assert_eq!(
            b.send_text(1, None, "x", None, None).await.unwrap()["message_id"],
            7
        );
        // Une fois consommée, la réponse par défaut reprend.
        assert!(
            b.send_text(1, None, "x", None, None).await.unwrap()["message_id"]
                .as_i64()
                .unwrap()
                > 1000
        );
    }
}
