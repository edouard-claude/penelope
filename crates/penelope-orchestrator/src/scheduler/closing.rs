//! Fermeture des sessions de tir et de run finies (#459).
//!
//! Chaque tir de planification et chaque run ouvre sa session, et rien ne la fermait : le
//! 11/11, une instance comptait 445 sessions de tir et 54 de run « actives » pour 28
//! conversations. Les listes plafonnées ne voyaient plus qu'elles, et leurs clés de
//! travail restaient (6,7 Mo de listes d'outils gelées).
//!
//! Une telle session est finie quand plus rien ne peut y écrire :
//!
//! ```text
//! aucun message depuis GRACE                           sessions.updated_at
//! aucun tour en file ni en cours                       turn_queue
//! aucune carte en attente, ni tranchée depuis GRACE    approval_requests
//! aucun job vivant, à livrer, ni livré depuis GRACE    tool_jobs
//! aucun run ouvert, ni fini depuis GRACE               workflow_runs
//! ```
//!
//! La session d'une étape à contexte neuf (#191) n'a pas de run à elle : elle suit celui
//! de sa session parente, et reste ouverte tant qu'il vit.
//!
//! Le délai couvre les écritures en deux temps : une carte tranchée puis son tour de
//! reprise (#379, #399), un résultat marqué livré puis son tour de réveil, une session
//! créée puis son tour ou son run.
//!
//! Deux sessions restent à leur propriétaire, qui les ferme lui-même : celle où il a
//! écrit (un tour `message` dans sa file) et celle qu'il a rouverte (`closed_at` déjà
//! posé). Une mission lancée par un run vit dans une session de conversation : elle n'est
//! pas concernée.

use super::*;

/// Sans activité depuis ce délai, une session de tir ou de run peut être fermée.
pub const GRACE_MS: i64 = 5 * 60_000;

/// Sessions fermées par passage : le rattrapage d'une instance ancienne s'étale sur
/// quelques passages au lieu d'en retarder un.
const BATCH: i64 = 100;

/// Les sessions d'un run qui vit encore, ou fini depuis moins que le délai.
const LIVE_RUNS: &str = "SELECT session_id FROM workflow_runs
    WHERE state NOT IN ('done', 'failed', 'cancelled') OR finished_at >= ?1";

fn finished_sql() -> String {
    format!(
        "SELECT id FROM sessions
          WHERE kind IN ('scheduled', 'workflow_run') AND state = 'active'
            AND closed_at IS NULL AND updated_at < ?1
            AND id NOT IN (SELECT session_id FROM turn_queue
                           WHERE state IN ('pending', 'leased') OR kind = 'message')
            AND id NOT IN (SELECT session_id FROM approval_requests
                           WHERE session_id IS NOT NULL
                             AND (state = 'pending' OR decided_at >= ?1))
            AND id NOT IN (SELECT session_id FROM tool_jobs
                           WHERE delivered_at IS NULL OR delivered_at >= ?1)
            AND id NOT IN ({LIVE_RUNS})
            AND (parent_id IS NULL OR parent_id NOT IN ({LIVE_RUNS}))
          ORDER BY updated_at LIMIT ?2"
    )
}

/// Ferme les sessions de tir et de run finies, et efface leurs clés de travail. Le
/// premier passage après le démarrage fait le rattrapage. Rend le nombre de sessions
/// fermées.
pub(super) async fn close_finished(d: &Context) -> anyhow::Result<usize> {
    let s = &d.services;
    let cutoff = chrono::DateTime::from_timestamp_millis(s.clock.now_ms() - GRACE_MS)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let ids: Vec<String> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(&finished_sql())?;
            let rows = st.query_map(penelope_store::rusqlite::params![cutoff, BATCH], |r| {
                r.get(0)
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let (mut closed, mut keys) = (0, 0);
    for id in &ids {
        if s.sessions.close_finished(id).await? {
            closed += 1;
            keys += penelope_app::session_keys::closed(s, id, true).await;
        }
    }
    if closed > 0 {
        tracing::info!(
            sessions = closed,
            keys,
            "sessions de tir et de run finies : fermées"
        );
    }
    Ok(closed)
}
