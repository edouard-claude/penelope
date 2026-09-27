//! Port des tentatives (`design/v1/boucle-et-outils.md` §3.7, issue #206, épopée #208,
//! lot K, tâche T15) : ce qu'un appel au modèle a coûté sans donner de réponse.
//!
//! La boucle décide de ce qui est une tentative (flux coupé, erreur d'avant flux, repli,
//! réponse vide relancée), la rédige, la plafonne et la trace ; le port ne fait que la
//! garder. L'implémentation de la composition écrit un `conv.attempt` dans le journal
//! ([`crate::journal::JournalAttempts`]), hors de la surface : aucune tentative ne
//! repart dans un prompt.

use std::sync::Mutex;

/// Une tentative, dans la forme où le journal la garde.
pub use penelope_kernel::journal::{AttemptCause, AttemptPayload as Attempt, TokenUsage};

/// Où vont les tentatives d'un tour.
#[async_trait::async_trait]
pub trait AttemptSink: Send + Sync {
    /// Garde une tentative de la session. Une erreur ne change pas l'issue de l'appel :
    /// la boucle la trace et continue.
    async fn record(&self, session_id: &str, attempt: &Attempt) -> anyhow::Result<()>;
}

/// Tentatives gardées en mémoire, dans l'ordre : l'implémentation des tests.
#[derive(Default)]
pub struct MemoryAttempts(Mutex<Vec<(String, Attempt)>>);

impl MemoryAttempts {
    /// Les tentatives d'une session, dans l'ordre où elles ont été gardées.
    pub fn of_session(&self, session_id: &str) -> Vec<Attempt> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|(sid, _)| sid == session_id)
            .map(|(_, a)| a.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl AttemptSink for MemoryAttempts {
    async fn record(&self, session_id: &str, attempt: &Attempt) -> anyhow::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((session_id.to_string(), attempt.clone()));
        Ok(())
    }
}
