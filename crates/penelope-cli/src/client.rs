//! Client RPC : JSON-RPC 2.0 en NDJSON sur la socket locale (§2.7).

use penelope_kernel::api::{RpcRequest, RpcResponse};
use serde_json::Value;
use std::path::Path;

/// Erreur de la CLI, avec code de sortie et suggestion.
#[derive(Debug)]
pub enum CliError {
    DaemonUnreachable(String),
    /// Connexion acceptée, réponse jamais venue dans le délai (issue #99).
    DaemonUnresponsive(String),
    /// Ctrl-C : le tour a été arrêté (issue #100).
    Interrupted,
    Rpc {
        code: i32,
        message: String,
    },
    Validation(String),
    Usage(String),
    Io(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::DaemonUnreachable(m) => write!(f, "daemon injoignable : {m}"),
            CliError::DaemonUnresponsive(m) => write!(f, "daemon muet : {m}"),
            CliError::Interrupted => write!(f, "interrompu : le tour est arrêté"),
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
            CliError::DaemonUnresponsive(_) => c::DAEMON_UNRESPONSIVE,
            CliError::Interrupted => c::INTERRUPTED,
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
            CliError::DaemonUnresponsive(_) => Some(
                "journaux : penelope logs ; puis penelope restart (penelope stop, puis \
                 penelope start, s'il ne répond plus du tout) ; --timeout 60 pour attendre \
                 plus, --timeout 0 pour attendre sans limite",
            ),
            CliError::Rpc { code, .. } if *code == penelope_kernel::api::METHOD_NOT_FOUND => {
                Some("cette commande n'est pas encore servie par ce daemon")
            }
            _ => None,
        }
    }
}

pub type CliResult<T> = Result<T, CliError>;

/// Délai par défaut d'une réponse du daemon (issue #99).
pub const DEFAULT_TIMEOUT_SECS: u64 = 15;

/// `--timeout` : `None` tant que la CLI ne l'a pas posé.
static TIMEOUT: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Méthodes longues par nature (un tour, une consolidation, un envoi git…) : sans
/// `--timeout` explicite, elles attendent sans limite.
const LONG: &[&str] = &[
    "chat.send",
    "chat.stream",
    "tail",
    "session.compact",
    "mem.reindex",
    "mem.dream",
    "mem.audit",
    "mem.retry_rejected",
    "mcp.test",
    "mcp.restart",
    "mcp.auth",
    "model.auth",
    "wf.run",
    "schedule.run_now",
    "onboard.answer",
    "onboard.write",
    "import.hermes",
    "export",
    "backup",
    "restore",
    "audit.verify",
    "history.verify",
    "store.rebuild",
    "eval.run",
    "upgrade",
    "vault.sync",
];

/// Pose `--timeout` (secondes ; 0 : aucune limite).
pub fn set_timeout(secs: Option<u64>) {
    if let Ok(mut g) = TIMEOUT.lock() {
        *g = secs;
    }
}

/// Délai d'une méthode : `--timeout` s'il est posé, sinon 15 s, sauf méthode longue.
pub fn timeout_for(method: &str) -> Option<std::time::Duration> {
    limit_of(TIMEOUT.lock().ok().and_then(|g| *g), method)
}

fn limit_of(explicit: Option<u64>, method: &str) -> Option<std::time::Duration> {
    let secs = match explicit {
        Some(0) => return None,
        Some(s) => s,
        None if LONG.contains(&method) => return None,
        None => DEFAULT_TIMEOUT_SECS,
    };
    Some(std::time::Duration::from_secs(secs))
}

/// Requête signée du jeton de session du daemon (issue #91).
pub fn request(socket: &Path, method: &str, params: Value) -> RpcRequest {
    RpcRequest::new(1, method, params).with_auth(penelope_platform::ipc::read_token(socket))
}

/// Appelle une méthode du daemon.
pub async fn call(socket: &Path, method: &str, params: Value) -> CliResult<Value> {
    call_limited(socket, method, params, timeout_for(method)).await
}

/// `call` avec un délai explicite (`None` : sans limite).
pub async fn call_limited(
    socket: &Path,
    method: &str,
    params: Value,
    limit: Option<std::time::Duration>,
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
    write
        .flush()
        .await
        .map_err(|e| CliError::DaemonUnreachable(e.to_string()))?;

    let mut lines = BufReader::new(read).lines();
    // Un daemon figé accepte la connexion (le noyau la met en file) et ne répond jamais :
    // sans délai, la commande pendait sans un mot (issue #99).
    let next = lines.next_line();
    let read = match limit {
        Some(limit) => tokio::time::timeout(limit, next).await.map_err(|_| {
            CliError::DaemonUnresponsive(format!(
                "le daemon accepte la connexion mais ne répond pas à `{method}` depuis {} s",
                limit.as_secs()
            ))
        })?,
        None => next.await,
    };
    let line = read
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
        assert_eq!(CliError::Interrupted.exit_code(), 130);
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

    /// #99 : 15 s par défaut, sans limite pour une méthode longue, `--timeout` l'emporte.
    #[test]
    fn each_method_has_its_limit() {
        use std::time::Duration;
        assert_eq!(limit_of(None, "status"), Some(Duration::from_secs(15)));
        assert_eq!(limit_of(None, "chat.send"), None);
        assert_eq!(limit_of(None, "session.compact"), None);
        assert_eq!(limit_of(Some(0), "status"), None);
        assert_eq!(
            limit_of(Some(60), "chat.send"),
            Some(Duration::from_secs(60))
        );
    }

    /// #99 : une socket qui accepte et ne répond jamais fait sortir la commande dans le
    /// délai, avec son propre code de sortie.
    #[tokio::test]
    async fn a_mute_daemon_is_reported_not_waited_for() {
        let dir = tempfile::Builder::new()
            .prefix("pnl")
            .tempdir_in("/tmp")
            .unwrap();
        let sock = dir.path().join("rpc.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        // Accepte et garde la connexion ouverte, sans jamais répondre.
        let hold = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });
        let t = std::time::Instant::now();
        let e = call_limited(
            &sock,
            "status",
            serde_json::json!({}),
            Some(std::time::Duration::from_secs(1)),
        )
        .await
        .unwrap_err();
        assert!(t.elapsed() < std::time::Duration::from_secs(3));
        assert!(matches!(e, CliError::DaemonUnresponsive(_)), "{e}");
        assert_eq!(e.exit_code(), c::DAEMON_UNRESPONSIVE);
        assert!(e.hint().unwrap().contains("--timeout"));
        hold.abort();
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
