//! Le clavier d'un job `tty` (#397, point 3) : un tube nommé dont les octets vont sur
//! l'entrée standard de la commande. `penelope job attach <id> --control` y écrit ce que
//! le propriétaire tape après avoir confirmé, depuis SSH ou un terminal local.
//!
//! Le tube est ouvert en lecture **et** écriture : un `attach` qui se détache n'envoie
//! pas de fin de fichier à la commande, un autre peut reprendre la main. À la fin de la
//! commande, [`Keyboard`] réveille le fil de lecture (un octet poussé dans le tube) puis
//! le laisse finir : aucun fil ne reste bloqué sur un tube que plus personne n'ouvre.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Crée le tube `path`, accès réservé au propriétaire. `false` si `mkfifo` échoue.
pub fn create(path: &Path) -> bool {
    let _ = std::fs::remove_file(path);
    std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

/// Le relais en cours : arrêté et le tube retiré quand il tombe.
pub struct Keyboard {
    path: PathBuf,
    done: Arc<AtomicBool>,
    /// Une seconde ouverture en lecture-écriture, qui ne bloque jamais : c'est par elle
    /// que le fil est réveillé, même s'il s'est déjà arrêté.
    poke: std::fs::File,
}

impl Keyboard {
    /// Relaie le tube `path` vers `stdin`. `None` si le tube ne s'ouvre pas.
    pub fn start(path: &Path, mut stdin: tokio::process::ChildStdin) -> Option<Keyboard> {
        let mut fifo = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()?;
        let poke = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()?;
        let done = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let stop = done.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                let n = match fifo.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                if stop.load(Ordering::SeqCst) || tx.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Some(bytes) = rx.recv().await {
                if stdin.write_all(&bytes).await.is_err() || stdin.flush().await.is_err() {
                    return;
                }
            }
        });
        Some(Keyboard {
            path: path.to_path_buf(),
            done,
            poke,
        })
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        // Le fil attend un octet : on lui en donne un, qu'il ne relaiera pas.
        let _ = self.poke.write_all(b"\0");
        let _ = std::fs::remove_file(&self.path);
    }
}
