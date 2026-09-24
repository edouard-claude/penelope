//! L'historique comme journal (épopée #208, `design/v1/source-de-verite.md`) : étape de
//! démarrage (scellement, T11), `penelope history verify` et sa ligne `doctor` (T12).
//! Le cœur est dans `penelope_context::verify` ; ce module ne fait que l'exposer.

use crate::runtime::Services;
use penelope_context::store::seal::SealReport;
use penelope_kernel::api::DoctorCheck;
use serde_json::Value;

/// Fenêtre de la ligne `doctor` : les sessions mises à jour dans la semaine. Vérifier
/// toute la base est `penelope history verify`, à la demande.
const DOCTOR_DAYS: i64 = 7;

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

/// `history.verify` (T12, §4.2) : dérive chaque session (ou `session`) depuis le journal
/// et la compare à ses caches. Rapport JSON ; `ok` faux à la première divergence.
pub async fn verify(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let session = p.get("session").and_then(Value::as_str);
    let report = s.context.history.verify(session, None).await?;
    Ok(serde_json::to_value(report)?)
}

/// Ligne `doctor` : les sessions de la semaine vérifiées contre le journal.
pub async fn doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "history.journal";
    const LABEL: &str = "Historique et journal";
    let since = (s.clock.now_utc() - chrono::Duration::days(DOCTOR_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let report = match s.context.history.verify(None, Some(&since)).await {
        Ok(r) => r,
        Err(e) => return DoctorCheck::fail(ID, LABEL, e.to_string(), None),
    };
    if report.ok {
        return DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "{} session(s) de la semaine, {} nœud(s), aucune divergence",
                report.sessions, report.nodes
            ),
        );
    }
    let first = &report.divergences[0];
    DoctorCheck::fail(
        ID,
        LABEL,
        format!(
            "{} session(s) sur {} divergent du journal ({} écart(s)) ; d'abord {} : {}",
            report.divergent,
            report.sessions,
            report.divergences.len(),
            first.session,
            first.detail
        ),
        Some("penelope history verify".into()),
    )
}

#[cfg(test)]
mod tests;
