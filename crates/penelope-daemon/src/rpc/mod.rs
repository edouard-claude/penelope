//! Serveur RPC local (§2.7, §15) : JSON-RPC 2.0 en NDJSON sur socket de domaine.

use crate::bus::Origin;
use crate::runtime::{Daemon, Services};
use penelope_kernel::api::*;
use serde_json::{Value, json};
use std::sync::Arc;

mod methods;
mod server;
mod stream;

pub use server::{serve, serve_on};
pub use stream::outcome_json;

/// Routeur : une méthode, des paramètres, une valeur.
pub struct Rpc {
    pub daemon: Arc<Daemon>,
}

impl Rpc {
    pub fn new(daemon: Arc<Daemon>) -> Self {
        Rpc { daemon }
    }

    fn services(&self) -> &Services {
        &self.daemon.services
    }

    /// Traite une requête. Une méthode inconnue renvoie `-32601`, jamais une panique.
    pub async fn handle(&self, req: RpcRequest) -> RpcResponse {
        let id = req.id.clone();
        let params = req.params.clone().unwrap_or(json!({}));
        match Box::pin(self.dispatch(&req.method, &params)).await {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => {
                let code = classify(&e);
                RpcResponse::err(id, code, e.to_string())
            }
        }
    }

    /// Appel en processus, pour les canaux qui vivent dans le daemon (Telegram).
    pub async fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        Box::pin(self.dispatch(method, &params)).await
    }
}

impl Rpc {
    /// Session visée : paramètre `session`, sinon la session courante de la CLI.
    async fn session_param(&self, p: &Value) -> anyhow::Result<String> {
        match p.get("session").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => {
                self.daemon.services.sessions.require(s).await?;
                Ok(s.to_string())
            }
            _ => self.daemon.chat_session_for(&Origin::Cli).await,
        }
    }
}

fn required_str(p: &Value, key: &str) -> anyhow::Result<String> {
    p.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("paramètre `{key}` manquant"))
}

fn classify(e: &anyhow::Error) -> i32 {
    let m = e.to_string();
    if m.contains("méthode inconnue") {
        METHOD_NOT_FOUND
    } else if m.contains("manquant") {
        INVALID_PARAMS
    } else if m.contains("introuvable") {
        NOT_FOUND
    } else if m.contains("déjà tranchée") {
        CONFLICT
    } else {
        INTERNAL_ERROR
    }
}

/// Avertissements de cohérence qui touchent un réglage, après son écriture.
pub(crate) fn config_warnings(daemon: &Daemon, path: &str) -> Vec<String> {
    penelope_kernel::coherence::contradictions(&daemon.services.config.config())
        .into_iter()
        .filter(|c| c.concerns(path))
        .map(|c| c.message)
        .collect()
}

#[cfg(test)]
mod tests;
