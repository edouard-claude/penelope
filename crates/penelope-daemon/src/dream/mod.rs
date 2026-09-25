//! Consolidation nocturne, digest du matin et entretien du vault : descendus dans
//! `penelope-dream` (T26), réexportés ici sous leur ancien chemin jusqu'à T30. Reste au
//! daemon ce que le digest lit au-dessus du rêve : l'ordonnanceur et le compactage.

pub use penelope_dream::dream::*;

use crate::ports::McpAdmin;
use penelope_dream::{DigestInputs, DigestSource};
use std::sync::Arc;

// ------------------------------------------------------------------ daemon

/// Ce que le digest lit au-dessus du rêve, calculé par le daemon et transmis en
/// données (T26) : planifications en échec, sessions dont le résumé échoue, départs du
/// jour.
pub async fn digest_inputs(d: &crate::runtime::Daemon) -> DigestInputs {
    let s = &d.services;
    // Planifications dont la dernière exécution a échoué, ou n'a rien livré (#39, #120).
    let mut failing = Vec::new();
    for sched in s.schedules.list().await.unwrap_or_default() {
        if sched.state == "active"
            && let Some(err) = &sched.last_error
        {
            failing.push(format!(
                "- {} : {err}",
                crate::scheduler::label(d, &sched).await
            ));
        }
    }
    DigestInputs {
        failing_schedules: failing,
        struggling_sessions: crate::compaction::struggling_sessions(s).await,
        due_today: crate::scheduler::due_today(d).await,
    }
}

/// Digest du matin vu du daemon : ses entrées calculées ici, le corps dans le rêve.
pub async fn digest_text(
    d: &crate::runtime::Daemon,
    mcp: Option<Arc<dyn McpAdmin>>,
) -> anyhow::Result<String> {
    penelope_dream::digest_text(&d.dream(), digest_inputs(d).await, mcp).await
}

/// Source des entrées du digest pour les crons système (`system_crons`).
pub struct DigestFeed(pub Arc<crate::runtime::Daemon>);

#[async_trait::async_trait]
impl DigestSource for DigestFeed {
    async fn digest_inputs(&self) -> DigestInputs {
        digest_inputs(&self.0).await
    }
}

#[cfg(test)]
mod tests;
