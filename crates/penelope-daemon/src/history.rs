//! L'historique comme journal (épopée #208, `design/v1/source-de-verite.md`) : étapes de
//! démarrage et, à venir, `penelope history verify` et `reindex` (T12, T13).

use crate::runtime::Services;
use penelope_context::store::seal::SealReport;

/// Scelle l'historique V0 (T11, §4.5) : un `conv.import` par session écrite avant la
/// double écriture, qui porte l'empreinte de son préfixe. Étape de démarrage idempotente :
/// le second démarrage ne trouve plus rien à sceller.
pub async fn seal_legacy(s: &Services) -> anyhow::Result<SealReport> {
    let report = s.context.history.seal_legacy().await?;
    if !report.sealed.is_empty() {
        let messages: i64 = report.sealed.iter().map(|(_, n)| n).sum();
        tracing::info!(
            sessions = report.sealed.len(),
            messages,
            "historique V0 scellé"
        );
    }
    if !report.skipped.is_empty() {
        tracing::warn!(
            sessions = ?report.skipped,
            "lignes V0 non scellées : le journal de la session a déjà des conv.*"
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
