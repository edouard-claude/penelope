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
            let inner = tokio::net::UnixListener::bind(path)
                .map_err(|e| PlatformError::Ipc(format!("bind {} : {e}", path.display())))?;
            crate::secrets::restrict_permissions(path)?;
            Ok(IpcListener {
                inner,
                path: path.to_path_buf(),
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
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

/// Nom du named pipe Windows, pour la conception de référence.
pub fn windows_pipe_name(user_sid: &str) -> String {
    format!(r"\\.\pipe\penelope-{user_sid}")
}

#[cfg(test)]
mod tests {
    use super::*;

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
