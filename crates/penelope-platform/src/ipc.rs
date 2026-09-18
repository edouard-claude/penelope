//! IPC locale entre la CLI et le daemon (§2.7).
//!
//! Unix : socket de domaine `{state}/rpc.sock`, permissions `0600`.
//! Windows : named pipe `\\.\pipe\penelope-<user-sid>` (conception de référence).
//!
//! Protocole identique sur les deux : JSON-RPC 2.0, une enveloppe par ligne (NDJSON).
//! Décision d'architecture : `tokio::net::UnixListener` plutôt que le crate
//! `interprocess`, pour éviter une dépendance dont l'API bouge et qui n'apporte rien tant
//! que seul macOS est livré (`docs/decisions/0004-ipc-tokio-unix-socket.md`).

use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};

/// Écoute locale.
pub struct IpcListener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    path: PathBuf,
    /// Jeton de session, tiré une fois la socket prise : un second daemon lancé par
    /// erreur n'écrase pas celui du premier (issue #91).
    token: String,
}

/// Connexion acceptée ou établie.
#[cfg(unix)]
pub type IpcStream = tokio::net::UnixStream;

impl IpcListener {
    /// Lie la socket. Si une socket morte traîne (daemon tué), elle est remplacée ; si un
    /// daemon vivant écoute déjà, l'erreur est explicite.
    pub async fn bind(path: &Path) -> Result<IpcListener> {
        #[cfg(unix)]
        {
            if path.exists() {
                match tokio::net::UnixStream::connect(path).await {
                    Ok(_) => {
                        return Err(PlatformError::Ipc(format!(
                            "un daemon écoute déjà sur {} — utiliser `penelope restart`",
                            path.display()
                        )));
                    }
                    Err(_) => {
                        // Socket orpheline : on la retire.
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p)?;
            }
            // Aucun daemon vivant ici : le jeton est tiré avant la socket, si bien qu'une
            // socket visible a toujours son jeton.
            let token = issue_token(path)?;
            let inner = tokio::net::UnixListener::bind(path)
                .map_err(|e| PlatformError::Ipc(format!("bind {} : {e}", path.display())))?;
            crate::secrets::restrict_permissions(path)?;
            Ok(IpcListener {
                inner,
                path: path.to_path_buf(),
                token,
            })
        }
        #[cfg(not(unix))]
        {
            Err(PlatformError::Unsupported(format!(
                "IPC non implémentée sur cette plateforme (cible : named pipe \
                 \\\\.\\pipe\\penelope-<user-sid>) — chemin demandé : {}",
                path.display()
            )))
        }
    }

    #[cfg(unix)]
    pub async fn accept(&self) -> Result<IpcStream> {
        let (s, _) = self
            .inner
            .accept()
            .await
            .map_err(|e| PlatformError::Ipc(e.to_string()))?;
        Ok(s)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Jeton que chaque requête doit porter.
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(token_path(&self.path));
    }
}

/// Se connecte au daemon.
#[cfg(unix)]
pub async fn connect(path: &Path) -> Result<IpcStream> {
    tokio::net::UnixStream::connect(path).await.map_err(|e| {
        PlatformError::Ipc(format!(
            "daemon injoignable sur {} : {e} — est-il démarré ? (`penelope status`)",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
pub async fn connect(path: &Path) -> Result<()> {
    Err(PlatformError::Unsupported(format!(
        "IPC non implémentée sur cette plateforme : {}",
        path.display()
    )))
}

// ------------------------------------------------------------------ jeton (#91)

/// Fichier du jeton de session, à côté de la socket (`{state}/rpc.token`, `0600`).
///
/// Le mode `0600` de la socket ne distingue pas deux processus du même utilisateur : un
/// serveur MCP stdio ou une commande confinée pouvait s'y connecter et appeler
/// `config.set`, `secret.set` ou `approve` au nom du propriétaire. Chaque requête porte
/// désormais un jeton que seul le propriétaire lit : `{state}` est refusé en lecture aux
/// processus confinés (`sandbox.deny_read`).
pub fn token_path(socket: &Path) -> PathBuf {
    socket.with_file_name("rpc.token")
}

/// Tire un nouveau jeton et l'écrit (`0600`, écriture atomique). Appelé à chaque
/// démarrage du daemon : un jeton volé ne survit pas au redémarrage.
pub fn issue_token(socket: &Path) -> Result<String> {
    use std::io::Write;
    let mut raw = [0u8; 32];
    getrandom::getrandom(&mut raw)
        .map_err(|e| PlatformError::Ipc(format!("entropie indisponible : {e}")))?;
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let path = token_path(socket);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(token.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(token)
}

/// Jeton courant du daemon, lu par la CLI. `None` : daemon jamais démarré ici.
pub fn read_token(socket: &Path) -> Option<String> {
    std::fs::read_to_string(token_path(socket))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Comparaison à temps constant : la durée ne dit pas combien de caractères concordent.
pub fn tokens_match(given: Option<&str>, expected: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Nom du named pipe Windows, pour la conception de référence.
pub fn windows_pipe_name(user_sid: &str) -> String {
    format!(r"\\.\pipe\penelope-{user_sid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #91 : un jeton par démarrage, en `0600`, relu à l'identique ; comparaison stricte.
    #[cfg(unix)]
    #[test]
    fn a_token_is_issued_privately_and_compared_strictly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("rpc.sock");
        let a = issue_token(&sock).unwrap();
        assert_eq!(a.len(), 64);
        let mode = std::fs::metadata(token_path(&sock))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_token(&sock).as_deref(), Some(a.as_str()));
        let b = issue_token(&sock).unwrap();
        assert_ne!(a, b, "nouveau jeton à chaque démarrage");
        assert!(tokens_match(Some(&b), &b));
        assert!(!tokens_match(Some(&a), &b));
        assert!(!tokens_match(None, &b));
        assert!(!tokens_match(Some(""), &b));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bind_accept_and_roundtrip() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");
        let listener = IpcListener::bind(&path).await.unwrap();

        let server = tokio::spawn(async move {
            let s = listener.accept().await.unwrap();
            let mut reader = BufReader::new(s);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let mut s = reader.into_inner();
            s.write_all(format!("echo:{line}").as_bytes())
                .await
                .unwrap();
            s.flush().await.unwrap();
        });

        let mut client = connect(&path).await.unwrap();
        client.write_all(b"bonjour\n").await.unwrap();
        client.flush().await.unwrap();
        let mut reader = BufReader::new(client);
        let mut resp = String::new();
        reader.read_line(&mut resp).await.unwrap();
        assert_eq!(resp, "echo:bonjour\n");
        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");
        std::fs::write(&path, b"").unwrap();
        // Un fichier ordinaire n'accepte pas de connexion : il doit être remplacé.
        let l = IpcListener::bind(&path).await.unwrap();
        assert_eq!(l.path(), path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn second_bind_while_alive_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");
        let _l = IpcListener::bind(&path).await.unwrap();
        let e = IpcListener::bind(&path)
            .await
            .err()
            .expect("second bind refusé")
            .to_string();
        assert!(e.contains("écoute déjà"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_permissions_are_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rpc.sock");
        let _l = IpcListener::bind(&path).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn pipe_name_format() {
        assert_eq!(
            windows_pipe_name("S-1-5-21-1"),
            r"\\.\pipe\penelope-S-1-5-21-1"
        );
    }
}
