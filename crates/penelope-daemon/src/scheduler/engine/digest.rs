//! Ce que le digest du matin lit au-dessus du rêve, calculé ici et transmis en données
//! (T26, puis T27) : planifications en échec, sessions dont le résumé échoue, départs du
//! jour.

use super::*;
use penelope_dream::{DigestInputs, DigestSource};

/// Les entrées du digest, lues dans les services.
pub async fn digest_inputs(s: &Services) -> DigestInputs {
    // Planifications dont la dernière exécution a échoué, ou n'a rien livré (#39, #120).
    let mut failing = Vec::new();
    for sched in s.schedules.list().await.unwrap_or_default() {
        if sched.state == "active"
            && let Some(err) = &sched.last_error
        {
            failing.push(format!("- {} : {err}", label(s, &sched).await));
        }
    }
    DigestInputs {
        failing_schedules: failing,
        struggling_sessions: crate::compaction::struggling_sessions(s).await,
        due_today: due_today(s).await,
    }
}

/// Source des entrées du digest pour les crons système (`system_crons`).
pub struct DigestFeed(pub Arc<Services>);

#[async_trait::async_trait]
impl DigestSource for DigestFeed {
    async fn digest_inputs(&self) -> DigestInputs {
        digest_inputs(&self.0).await
    }
}
