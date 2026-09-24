//! L'historique comme journal (épopée #208, `design/v1/source-de-verite.md`) : étape de
//! démarrage (scellement, T11), `penelope history verify` et sa ligne `doctor` (T12),
//! `penelope history reindex` et le rattrapage à l'ouverture d'une session (T13). Le cœur
//! est dans `penelope_context::verify` et `penelope_context::projector` ; ce module ne
//! fait que l'exposer.

use crate::runtime::Services;
use penelope_context::projector::CatchUp;
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

/// `history.reindex` (T13, §2.4) : refond les caches de `session`, ou de toutes les
/// sessions, depuis le journal. Une session que le journal ne sait pas refaire est
/// laissée intacte et nommée (`refused`).
pub async fn reindex(s: &Services, p: &Value) -> anyhow::Result<Value> {
    let session = p.get("session").and_then(Value::as_str);
    let report = s.context.history.reindex(session).await?;
    Ok(serde_json::to_value(report)?)
}

/// Rattrape les caches d'une session depuis son filigrane, à l'ouverture d'un tour (T13) :
/// ce qu'une seconde transaction n'a pas écrit (arrêt, écriture en erreur) est refait
/// depuis le journal. Une erreur ne fait pas échouer le tour : elle est journalisée, le
/// filigrane porte `dirty` et `doctor` la signale.
pub async fn catch_up(s: &Services, session_id: &str) {
    match s.context.history.catch_up(session_id).await {
        Ok(CatchUp::Rebuilt) => {
            tracing::info!(session = session_id, "caches de la session refondus");
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(session = session_id, error = %e, "rattrapage des caches en échec");
        }
    }
}

/// Ligne `doctor` : les sessions de la semaine vérifiées contre le journal, et les
/// rattrapages en échec.
pub async fn doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "history.journal";
    const LABEL: &str = "Historique et journal";
    let since = (s.clock.now_utc() - chrono::Duration::days(DOCTOR_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let report = match s.context.history.verify(None, Some(&since)).await {
        Ok(r) => r,
        Err(e) => return DoctorCheck::fail(ID, LABEL, e.to_string(), None),
    };
    let dirty = s
        .context
        .history
        .dirty_projections()
        .await
        .unwrap_or_default();
    if let Some((session, error)) = dirty.first() {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!(
                "{} session(s) au rattrapage en échec ; d'abord {session} : {error}",
                dirty.len()
            ),
            Some(format!("penelope history reindex --session {session}")),
        );
    }
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
