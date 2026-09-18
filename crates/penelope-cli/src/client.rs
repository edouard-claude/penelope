//! Client RPC : JSON-RPC 2.0 en NDJSON sur la socket locale (§2.7).

use penelope_kernel::api::{RpcRequest, RpcResponse};
use serde_json::Value;
use std::path::Path;

/// Erreur de la CLI, avec code de sortie et suggestion.
#[derive(Debug)]
pub enum CliError {
    DaemonUnreachable(String),
    Rpc { code: i32, message: String },
    Validation(String),
    Usage(String),
    Io(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::DaemonUnreachable(m) => write!(f, "daemon injoignable : {m}"),
            CliError::Rpc { code, message } => write!(f, "{message} (code {code})"),
            CliError::Validation(m) => write!(f, "{m}"),
            CliError::Usage(m) => write!(f, "{m}"),
            CliError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CliError {}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        use penelope_kernel::api::exit_code as c;
        match self {
            CliError::DaemonUnreachable(_) => c::DAEMON_UNREACHABLE,
            CliError::Validation(_) => c::VALIDATION_FAILED,
            CliError::Usage(_) => c::USAGE,
            CliError::Rpc { code, .. } => match *code {
                penelope_kernel::api::NOT_FOUND => c::NOT_FOUND,
                penelope_kernel::api::DENIED => c::DENIED,
                penelope_kernel::api::INVALID_PARAMS => c::USAGE,
                _ => c::INTERNAL,
            },
            CliError::Io(_) => c::INTERNAL,
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            CliError::DaemonUnreachable(_) => {
                Some("démarrer le daemon : penelope install puis penelope start")
            }
            CliError::Rpc { code, .. } if *code == penelope_kernel::api::METHOD_NOT_FOUND => {
                Some("cette commande n'est pas encore servie par ce daemon")
            }
            _ => None,
        }
    }
}

pub type CliResult<T> = Result<T, CliError>;

/// Requête signée du jeton de session du daemon (issue #91).
pub fn request(socket: &Path, method: &str, params: Value) -> RpcRequest {
    RpcRequest::new(1, method, params).with_auth(penelope_platform::ipc::read_token(socket))
}

/// Appelle une méthode du daemon.
pub async fn call(socket: &Path, method: &str, params: Value) -> CliResult<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = penelope_platform::ipc::connect(socket)
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;
    let (read, mut write) = stream.into_split();

    let req = request(socket, method, params);
    let mut body = serde_json::to_string(&req).map_err(|e| CliError::Io(e.to_string()))?;
    body.push('\n');
    write
        .write_all(body.as_bytes())
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;
    write
        .flush()
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;

    let mut lines = BufReader::new(read).lines();
    let line = lines
        .next_line()
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?
        .ok_or_else(|| CliError::DaemonUnreachable("réponse vide".into()))?;

    let resp: RpcResponse = serde_json::from_str(&line)
        .map_err(|e| CliError::Io(format!("réponse illisible : {e}")))?;
    match (resp.result, resp.error) {
        (Some(v), None) => Ok(v),
        (_, Some(e)) => Err(CliError::Rpc {
            code: e.code,
            message: e.message,
        }),
        _ => Ok(Value::Null),
    }
}

/// Appelle une méthode en flux : chaque notification est passée à `on_event`, la
/// réponse finale est renvoyée.
pub async fn call_stream(
    socket: &Path,
    method: &str,
    params: Value,
    mut on_event: impl FnMut(&Value),
) -> CliResult<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = penelope_platform::ipc::connect(socket)
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;
    let (read, mut write) = stream.into_split();
    let req = request(socket, method, params);
    let mut body = serde_json::to_string(&req).map_err(|e| CliError::Io(e.to_string()))?;
    body.push('\n');
    write
        .write_all(body.as_bytes())
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;

    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?
    {
        let v: Value = serde_json::from_str(&line)
            .map_err(|e| CliError::Io(format!("réponse illisible : {e}")))?;
        if v.get("id").is_none() || v["id"].is_null() {
            if let Some(p) = v.get("params") {
                on_event(p);
            }
            continue;
        }
        let resp: RpcResponse = serde_json::from_value(v)
            .map_err(|e| CliError::Io(format!("réponse illisible : {e}")))?;
        return match (resp.result, resp.error) {
            (Some(v), None) => Ok(v),
            (_, Some(e)) => Err(CliError::Rpc {
                code: e.code,
                message: e.message,
            }),
            _ => Ok(Value::Null),
        };
    }
    Err(CliError::DaemonUnreachable(
        "connexion fermée avant la réponse".into(),
    ))
}

/// Résout le chemin de la socket : `--home`, `PENELOPE_HOME`, ou le backend de l'OS.
pub fn socket_path(home: Option<std::path::PathBuf>) -> CliResult<std::path::PathBuf> {
    let dirs =
        penelope_platform::resolve_directories(home).map_err(|e| CliError::Io(e.to_string()))?;
    Ok(dirs.socket_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::api::exit_code as c;

    #[test]
    fn exit_codes_follow_the_documented_table() {
        assert_eq!(
            CliError::DaemonUnreachable("x".into()).exit_code(),
            c::DAEMON_UNREACHABLE
        );
        assert_eq!(
            CliError::Validation("x".into()).exit_code(),
            c::VALIDATION_FAILED
        );
        assert_eq!(CliError::Usage("x".into()).exit_code(), c::USAGE);
        assert_eq!(
            CliError::Rpc {
                code: penelope_kernel::api::NOT_FOUND,
                message: "x".into()
            }
            .exit_code(),
            c::NOT_FOUND
        );
        assert_eq!(
            CliError::Rpc {
                code: penelope_kernel::api::INTERNAL_ERROR,
                message: "x".into()
            }
            .exit_code(),
            c::INTERNAL
        );
    }

    #[test]
    fn unreachable_daemon_suggests_how_to_start_it() {
        let h = CliError::DaemonUnreachable("pas de socket".into())
            .hint()
            .unwrap();
        assert!(h.contains("penelope start"));
    }

    #[tokio::test]
    async fn calling_an_absent_daemon_is_a_clean_error() {
        let dir = tempfile::tempdir().unwrap();
        let e = call(
            &dir.path().join("rpc.sock"),
            "status",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.exit_code(), c::DAEMON_UNREACHABLE);
    }

    #[test]
    fn socket_path_follows_home() {
        let p = socket_path(Some(std::path::PathBuf::from("/srv/pen"))).unwrap();
        assert_eq!(p, std::path::PathBuf::from("/srv/pen/state/rpc.sock"));
    }
}
