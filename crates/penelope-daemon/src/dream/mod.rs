//! Consolidation nocturne, digest du matin et entretien du vault : descendus dans
//! `penelope-dream` (T26), réexportés ici sous leur ancien chemin jusqu'à T30. Ce que le
//! digest lit au-dessus du rêve est calculé par l'ordonnanceur (`scheduler::digest_inputs`,
//! T27) ; restent ici ses entrées en `&Daemon`.

pub use penelope_dream::dream::*;

use crate::ports::McpAdmin;
use penelope_dream::DigestInputs;
use std::sync::Arc;

// ------------------------------------------------------------------ daemon

/// Ce que le digest lit au-dessus du rêve : calculé par l'ordonnanceur (T27).
pub async fn digest_inputs(d: &crate::runtime::Daemon) -> DigestInputs {
    crate::scheduler::digest_inputs(&d.services).await
}

/// Digest du matin vu du daemon : ses entrées calculées ici, le corps dans le rêve.
pub async fn digest_text(
    d: &crate::runtime::Daemon,
    mcp: Option<Arc<dyn McpAdmin>>,
) -> anyhow::Result<String> {
    penelope_dream::digest_text(&d.dream(), digest_inputs(d).await, mcp).await
}

#[cfg(test)]
mod tests;
