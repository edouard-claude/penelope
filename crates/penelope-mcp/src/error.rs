//! Erreurs du client MCP.

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport : {0}")]
    Transport(String),

    #[error("http {status} : {body}")]
    Http { status: u16, body: String },

    #[error("autorisation requise (HTTP {status})")]
    Unauthorized {
        status: u16,
        www_authenticate: String,
    },

    #[error("erreur JSON-RPC {code} : {message}")]
    Rpc {
        code: i32,
        message: String,
        data: Option<Value>,
    },

    #[error("délai dépassé sur `{method}` après {ms} ms")]
    Timeout { method: String, ms: u64 },

    #[error("aucune version de protocole commune : le serveur annonce {server:?}")]
    NoCommonVersion { server: Vec<String> },

    #[error("json : {0}")]
    Json(#[from] serde_json::Error),

    #[error("stockage : {0}")]
    Store(#[from] penelope_store::StoreError),

    #[error("plateforme : {0}")]
    Platform(#[from] penelope_platform::PlatformError),

    #[error("configuration du serveur `{server}` : {reason}")]
    Config { server: String, reason: String },

    #[error("serveur `{0}` introuvable")]
    UnknownServer(String),

    #[error("outil `{0}` introuvable")]
    UnknownTool(String),

    #[error("arguments invalides pour `{tool}` : {reason}")]
    InvalidArguments { tool: String, reason: String },

    #[error("oauth : {0}")]
    OAuth(String),

    #[error("refusé par la politique : {0}")]
    Denied(String),

    #[error("annulé")]
    Cancelled,
}

impl McpError {
    /// Code JSON-RPC à renvoyer quand Pénélope répond à une requête du serveur.
    pub fn rpc_code(&self) -> i32 {
        match self {
            McpError::Rpc { code, .. } => *code,
            McpError::InvalidArguments { .. } => crate::protocol::INVALID_PARAMS,
            McpError::UnknownTool(_) | McpError::UnknownServer(_) => {
                crate::protocol::METHOD_NOT_FOUND
            }
            McpError::NoCommonVersion { .. } => crate::protocol::UNSUPPORTED_PROTOCOL_VERSION,
            _ => crate::protocol::INTERNAL_ERROR,
        }
    }

    /// Vrai si l'erreur justifie une nouvelle tentative avec backoff.
    pub fn is_retryable(&self) -> bool {
        match self {
            McpError::Transport(_) | McpError::Timeout { .. } => true,
            McpError::Http { status, .. } => *status >= 500 || *status == 429,
            _ => false,
        }
    }

    /// Vrai si l'erreur signale qu'il faut (re)faire le flux d'autorisation.
    pub fn needs_auth(&self) -> bool {
        matches!(self, McpError::Unauthorized { .. })
    }

    /// Scopes manquants extraits de `WWW-Authenticate` (consentement incrémental, §8.5).
    pub fn missing_scopes(&self) -> Vec<String> {
        let McpError::Unauthorized {
            www_authenticate, ..
        } = self
        else {
            return Vec::new();
        };
        crate::oauth::parse_www_authenticate(www_authenticate)
            .scope
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default()
    }
}

pub type Result<T, E = McpError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        assert!(McpError::Transport("coupure".into()).is_retryable());
        assert!(
            McpError::Timeout {
                method: "tools/list".into(),
                ms: 30_000
            }
            .is_retryable()
        );
        assert!(
            McpError::Http {
                status: 503,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(
            !McpError::Http {
                status: 400,
                body: String::new()
            }
            .is_retryable()
        );
        assert!(!McpError::UnknownTool("x".into()).is_retryable());
    }

    #[test]
    fn unauthorized_exposes_missing_scopes() {
        let e = McpError::Unauthorized {
            status: 403,
            www_authenticate: r#"Bearer realm="x", scope="repo:write issues:read""#.into(),
        };
        assert!(e.needs_auth());
        assert_eq!(e.missing_scopes(), vec!["repo:write", "issues:read"]);
    }

    #[test]
    fn rpc_codes_are_mapped() {
        assert_eq!(
            McpError::NoCommonVersion { server: vec![] }.rpc_code(),
            -32022
        );
        assert_eq!(
            McpError::InvalidArguments {
                tool: "t".into(),
                reason: "r".into()
            }
            .rpc_code(),
            -32602
        );
    }
}
