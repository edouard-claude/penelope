//! Registre des jobs d'outils qui tournent dans ce processus (issue #204).
//!
//! Descendu de `penelope-daemon/src/tool_jobs.rs` (épopée #208, T21) : `Services` le
//! porte, le cycle de vie des jobs reste au daemon.

use penelope_llm::CancelToken;
use std::collections::HashMap;
use std::sync::Mutex;

/// Jobs qui tournent **dans ce processus**, avec leur jeton d'annulation.
///
/// La table dit ce qui existe, ce registre dit ce qu'on peut encore interrompre : après
/// un redémarrage, la table garde des jobs `working` que plus aucun jeton ne couvre, et
/// c'est exactement ce que `JobStore::recover_on_boot` (daemon) tranche.
#[derive(Default)]
pub struct Running {
    inner: Mutex<HashMap<String, (String, CancelToken)>>,
    /// Réveille la boucle de livraison : un job qui vient de finir n'attend pas le
    /// prochain battement pour revenir dans sa conversation.
    wake: tokio::sync::Notify,
}

impl Running {
    fn with<T>(&self, f: impl FnOnce(&mut HashMap<String, (String, CancelToken)>) -> T) -> T {
        match self.inner.lock() {
            Ok(mut g) => f(&mut g),
            Err(p) => f(&mut p.into_inner()),
        }
    }

    /// Inscrit un job qui démarre dans ce processus, avec son jeton d'annulation.
    pub fn insert(&self, job: &str, session: &str, cancel: CancelToken) {
        self.with(|m| m.insert(job.to_string(), (session.to_string(), cancel)));
    }

    /// Oublie un job qui a conclu : plus rien à interrompre.
    pub fn forget(&self, job: &str) {
        self.with(|m| m.remove(job));
    }

    /// Interrompt un job nommé. Faux : il ne tourne pas ici.
    pub fn cancel(&self, job: &str) -> bool {
        self.with(|m| match m.remove(job) {
            Some((_, c)) => {
                c.cancel();
                true
            }
            None => false,
        })
    }

    /// Interrompt les jobs d'une session (`/stop`, issue #57).
    pub fn cancel_session(&self, session: &str) -> usize {
        self.with(|m| {
            let mine: Vec<String> = m
                .iter()
                .filter(|(_, (s, _))| s == session)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &mine {
                if let Some((_, c)) = m.remove(id) {
                    c.cancel();
                }
            }
            mine.len()
        })
    }

    /// Interrompt tous les jobs du daemon (`/stop tout`, issue #155).
    pub fn cancel_all(&self) -> usize {
        self.with(|m| {
            let n = m.len();
            for (_, (_, c)) in m.drain() {
                c.cancel();
            }
            n
        })
    }

    /// Jobs de cette session qui tournent ici.
    pub fn of_session(&self, session: &str) -> usize {
        self.with(|m| m.values().filter(|(s, _)| s == session).count())
    }

    pub fn len(&self) -> usize {
        self.with(|m| m.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Un job vient de conclure : la livraison peut partir.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Attend qu'un job conclue (réveil de la boucle de livraison).
    pub async fn woken(&self) {
        self.wake.notified().await;
    }
}
