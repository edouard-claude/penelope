//! Côté enfant, le host MCP du daemon : un appel part en `mcp_call`, sa réponse revient en
//! `mcp_result` et réveille l'appel qui l'attend.

use super::FromChild;
use crate::elicitation::Destination;
use crate::ports::McpGateway;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Le canal vers le superviseur, et les appels en attente de réponse.
#[derive(Clone)]
pub struct Relay {
    out: mpsc::UnboundedSender<FromChild>,
    pending: Pending,
    next: Arc<AtomicU64>,
}

impl Relay {
    pub(super) fn new(out: mpsc::UnboundedSender<FromChild>) -> Relay {
        Relay {
            out,
            pending: Arc::default(),
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Un appel au daemon, et sa réponse.
    pub async fn call(
        &self,
        op: &str,
        qualified: &str,
        args: Value,
        session_id: Option<String>,
        origin: Option<Value>,
    ) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "relais MCP indisponible".to_string())?
            .insert(id, tx);
        self.out
            .send(FromChild::McpCall {
                id,
                op: op.into(),
                qualified: qualified.into(),
                args,
                session_id,
                origin,
            })
            .map_err(|_| "superviseur parti".to_string())?;
        rx.await.map_err(|_| "superviseur parti".to_string())?
    }

    /// Une réponse du superviseur : l'appel qui l'attend reprend.
    pub(super) fn resolve(&self, id: u64, ok: Option<Value>, error: Option<String>) {
        let waiter = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
        if let Some(tx) = waiter {
            let _ = tx.send(match error {
                Some(e) => Err(e),
                None => Ok(ok.unwrap_or(Value::Null)),
            });
        }
    }
}

/// Le host MCP vu de l'enfant : chaque appel traverse le protocole jusqu'au daemon.
pub struct ChildMcp(pub Relay);

fn origin_json(dest: &Destination) -> (Option<String>, Option<Value>) {
    (
        dest.session_id.clone(),
        dest.origin
            .as_ref()
            .and_then(|o| serde_json::to_value(o).ok()),
    )
}

#[async_trait::async_trait]
impl McpGateway for ChildMcp {
    async fn call_tool(
        &self,
        qualified: &str,
        args: &Value,
        from: Destination,
    ) -> Result<Value, String> {
        let (session, origin) = origin_json(&from);
        self.0
            .call("call_tool", qualified, args.clone(), session, origin)
            .await
    }

    async fn server_lines(&self) -> Vec<String> {
        Vec::new()
    }

    async fn tool_policy(&self, qualified: &str) -> Option<String> {
        self.0
            .call("tool_policy", qualified, Value::Null, None, None)
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
    }

    async fn read_resource(&self, server: &str, uri: &str) -> Result<Value, String> {
        self.0
            .call("read_resource", server, json!({"uri": uri}), None, None)
            .await
    }
}
