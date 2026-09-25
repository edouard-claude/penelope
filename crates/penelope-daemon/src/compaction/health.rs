//! Une session qui ne se résume plus, vue du propriétaire (issue #131), sortie de
//! `compaction.rs`.

use super::*;

/// Dit au propriétaire qu'une session s'est compactée sans modèle, avec son coût par tour.
pub(super) async fn tell_mechanical(
    d: &Context,
    session_id: &str,
    model: &str,
    error: &str,
    report: &Report,
) {
    let s = &d.services;
    let _ = s
        .events
        .append(
            EventDraft::new(
                "context.compaction_mechanical",
                json!({"model": model, "error": error, "messages": report.messages}),
            )
            .session(session_id),
        )
        .await;
    let title = s
        .sessions
        .get(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|x| x.title)
        .unwrap_or_else(|| session_id.to_string());
    let per_turn = cost_per_turn(s, session_id).await;
    if let Some(m) = d.compaction.messenger.get() {
        let _ = m
            .send_text(
                &crate::helpers::owner_origin_of(&d.services),
                &format!(
                    "⚠️ La session « {title} » ne se résumait plus : le résumeur (`{model}`) a \
                     échoué {MECHANICAL_AFTER} fois ({error}). {} messages ont été compactés \
                     sans modèle : tes derniers messages de ce passage et les ancres sont \
                     gardés tels quels, le reste se relit à la demande.{}",
                    report.messages,
                    per_turn
                        .map(|c| format!(" Coût moyen des derniers tours : {c:.3} $."))
                        .unwrap_or_default()
                ),
            )
            .await;
    }
}

/// Sessions dont la compaction a échoué ces dernières 24 h : titre, échecs, coût moyen
/// des derniers tours ; pour le digest (issue #131).
pub async fn struggling_sessions(s: &Services) -> Vec<(String, u32, Option<f64>)> {
    let since = chrono::DateTime::from_timestamp_millis(s.clock.now_ms() - 86_400_000)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(String, u32)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT session_id, COUNT(*) FROM events
                 WHERE kind = 'context.compaction_failed' AND session_id IS NOT NULL
                   AND ts >= ?1
                 GROUP BY session_id ORDER BY COUNT(*) DESC LIMIT 5",
            )?;
            let rows = st.query_map([&since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default();
    let mut out = Vec::new();
    for (sid, n) in rows {
        let title = s
            .sessions
            .get(&sid)
            .await
            .ok()
            .flatten()
            .and_then(|x| x.title)
            .unwrap_or_else(|| sid.clone());
        out.push((title, n, cost_per_turn(s, &sid).await));
    }
    out
}

/// Coût moyen des cinq derniers tours d'une session.
pub(super) async fn cost_per_turn(s: &Services, session_id: &str) -> Option<f64> {
    let sid = session_id.to_string();
    s.store
        .read(move |c| {
            let v: Option<f64> = c
                .query_row(
                    "SELECT AVG(cost) FROM (SELECT SUM(cost_usd) AS cost FROM usage
                     WHERE session_id = ?1 AND turn_id IS NOT NULL
                     GROUP BY turn_id ORDER BY MAX(ts) DESC LIMIT 5)",
                    [&sid],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            Ok(v)
        })
        .await
        .ok()
        .flatten()
}
