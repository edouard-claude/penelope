//! Le protocole MCP côté serveur, sur stdio : une ligne JSON-RPC par message, dans les
//! deux sens. `initialize` négocie la version (2025-06-18 au plus : sortie structurée,
//! sans découverte sans état), `tools/list` et `tools/call` servent l'agenda, `ping`
//! répond ; `server/discover` est inconnu, ce qui ramène le client au `initialize`
//! historique (§8.2 de la négociation côté Pénélope).

#[cfg(test)]
mod tests;

use crate::tools::Agenda;
use penelope_mcp::protocol::{
    INVALID_PARAMS, Incoming, METHOD_NOT_FOUND, PARSE_ERROR, ProtocolVersion, Request, Response,
    RpcError, decode,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// La version la plus récente que ce serveur parle.
pub const SERVED_VERSION: ProtocolVersion = ProtocolVersion::V20250618;

pub struct Server {
    agenda: Arc<Agenda>,
}

impl Server {
    pub fn new(agenda: Agenda) -> Server {
        Server {
            agenda: Arc::new(agenda),
        }
    }

    /// Sert stdin jusqu'à sa fermeture.
    pub async fn serve_stdio(&self) -> Result<(), String> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut out = tokio::io::stdout();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| format!("lecture de l'entrée : {e}"))?
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(resp) = self.handle(&line).await {
                let mut payload = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
                payload.push('\n');
                out.write_all(payload.as_bytes())
                    .await
                    .map_err(|e| format!("écriture de la sortie : {e}"))?;
                out.flush()
                    .await
                    .map_err(|e| format!("écriture de la sortie : {e}"))?;
            }
        }
        Ok(())
    }

    /// Une ligne reçue, et la réponse à écrire s'il en faut une : une notification n'en a
    /// pas, une réponse du client (à une requête que ce serveur n'émet jamais) non plus.
    pub async fn handle(&self, line: &str) -> Option<Value> {
        match decode(line) {
            None => Some(error(
                Value::Null,
                RpcError {
                    code: PARSE_ERROR,
                    message: "message JSON-RPC illisible".into(),
                    data: None,
                },
            )),
            Some(Incoming::Notification(n)) => {
                tracing::debug!(method = %n.method, "notification reçue");
                None
            }
            Some(Incoming::Response(_)) => None,
            Some(Incoming::ServerRequest(req)) => Some(self.request(req).await),
        }
    }

    async fn request(&self, req: Request) -> Value {
        let params = req.params.unwrap_or(Value::Null);
        let result = match req.method.as_str() {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": Agenda::descriptors()})),
            "tools/call" => self.call(&params).await,
            other => Err(RpcError {
                code: METHOD_NOT_FOUND,
                message: format!("méthode inconnue : {other}"),
                data: None,
            }),
        };
        match result {
            Ok(v) => ok(req.id, v),
            Err(e) => error(req.id, e),
        }
    }

    async fn call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError {
                code: INVALID_PARAMS,
                message: "`name` manquant dans tools/call".into(),
                data: None,
            })?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.agenda.call(name, &args).await
    }
}

/// La réponse à `initialize` : la version demandée si ce serveur la parle, la sienne sinon.
fn initialize(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .and_then(ProtocolVersion::parse)
        .unwrap_or(SERVED_VERSION);
    json!({
        "protocolVersion": asked.min(SERVED_VERSION).as_str(),
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {
            "name": "penelope-agenda-mcp",
            "title": "Agenda CalDAV",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Agenda du propriétaire, en lecture seule (CalDAV). Les heures sont \
                         rendues dans le fuseau demandé ; un événement sur la journée porte \
                         des dates, pas des heures. Un calendrier partagé peut contenir des \
                         rendez-vous de tiers : les citer, ne pas les retenir."
    })
}

fn ok(id: Value, result: Value) -> Value {
    serde_json::to_value(Response {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    })
    .unwrap_or(Value::Null)
}

fn error(id: Value, e: RpcError) -> Value {
    serde_json::to_value(Response {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(e),
    })
    .unwrap_or(Value::Null)
}
