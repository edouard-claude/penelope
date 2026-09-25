//! Ledger des passes : phases, statistiques, rapports, historique, restauration.

use super::*;

// ------------------------------------------------------------------ ledger des passes

pub(super) async fn last_finished_start(s: &Services) -> anyhow::Result<Option<String>> {
    Ok(s.store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT started_at FROM dream_runs WHERE phase = 'done'
                 ORDER BY started_at DESC LIMIT 1",
            )?;
            let mut rows = st.query([])?;
            Ok(match rows.next()? {
                Some(r) => Some(r.get::<_, String>(0)?),
                None => None,
            })
        })
        .await?)
}

pub(super) async fn record_run(
    s: &Services,
    id: &str,
    started: &str,
    phase: &str,
    since: Option<&str>,
) -> anyhow::Result<()> {
    let (id, started, phase, since) = (
        id.to_string(),
        started.to_string(),
        phase.to_string(),
        since.map(String::from),
    );
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO dream_runs(id, started_at, phase, since) VALUES(?1,?2,?3,?4)",
                params![id, started, phase, since],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

pub(super) async fn set_phase(s: &Services, id: &str, phase: &str) -> anyhow::Result<()> {
    let (id, phase) = (id.to_string(), phase.to_string());
    s.store
        .write(move |tx| {
            tx.execute(
                "UPDATE dream_runs SET phase = ?2 WHERE id = ?1",
                params![id, phase],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Passe restée ouverte (processus tué, machine endormie) : close en `interrupted` avec
/// ce qu'elle avait écrit, pour que la nuit suivante sache d'où elle repart (issue #152).
/// Rend la passe close, ses lots écrits et ses entrées gardées.
pub(super) async fn close_interrupted(
    s: &Services,
    current: &str,
) -> anyhow::Result<Option<(String, u32, u32)>> {
    let current = current.to_string();
    let found: Option<(String, String)> = s
        .store
        .read(move |c| {
            use penelope_store::rusqlite::OptionalExtension;
            Ok(c.query_row(
                "SELECT id, COALESCE(stats, '{}') FROM dream_runs
                 WHERE finished_at IS NULL AND id <> ?1
                 ORDER BY started_at DESC LIMIT 1",
                [current],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .await?;
    let Some((id, stats)) = found else {
        return Ok(None);
    };
    let report: DreamReport = serde_json::from_str(&stats).unwrap_or_default();
    let (lots, promoted) = (report.lots, report.promoted);
    let ts = s.clock.now_rfc3339();
    let closed = id.clone();
    s.store
        .write(move |tx| {
            tx.execute(
                "UPDATE dream_runs SET phase = 'interrupted', error = ?2, finished_at = ?3
                 WHERE id = ?1",
                params![
                    closed,
                    format!("passe interrompue après {lots} lot(s) écrit(s)"),
                    ts
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(Some((id, lots, promoted)))
}

/// État de la passe après un lot écrit : `stats` avance au fil de l'eau, pas à la fin
/// (issue #152). Une passe tuée en cours laisse ainsi le compte de ce qu'elle a fait.
pub(super) async fn save_stats(s: &Services, id: &str, report: &DreamReport) -> anyhow::Result<()> {
    let (id, stats) = (
        id.to_string(),
        serde_json::to_string(report).unwrap_or_default(),
    );
    s.store
        .write(move |tx| {
            tx.execute(
                "UPDATE dream_runs SET stats = ?2 WHERE id = ?1",
                params![id, stats],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

pub(super) async fn finish_run(
    s: &Services,
    id: &str,
    phase: &str,
    report: &DreamReport,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let (id, phase, stats, error, ts) = (
        id.to_string(),
        phase.to_string(),
        serde_json::to_string(report).unwrap_or_default(),
        error.map(String::from),
        s.clock.now_rfc3339(),
    );
    s.store
        .write(move |tx| {
            tx.execute(
                "UPDATE dream_runs SET phase = ?2, stats = ?3, error = ?4, finished_at = ?5
                 WHERE id = ?1",
                params![id, phase, stats, error, ts],
            )?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// Dernière passe terminée : identifiant, fin, rapport.
pub async fn last_report(s: &Services) -> anyhow::Result<Option<(String, String, DreamReport)>> {
    Ok(s.store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT id, COALESCE(finished_at, started_at), stats FROM dream_runs
                 WHERE phase = 'done' ORDER BY started_at DESC LIMIT 1",
            )?;
            let mut rows = st.query([])?;
            Ok(match rows.next()? {
                Some(r) => {
                    let stats: String = r.get(2)?;
                    Some((
                        r.get(0)?,
                        r.get(1)?,
                        serde_json::from_str(&stats).unwrap_or_default(),
                    ))
                }
                None => None,
            })
        })
        .await?)
}

// ------------------------------------------------------------------ historique

/// Pré-images d'une entrée ou d'un fichier, les plus récentes d'abord.
pub async fn history(s: &Services, uid: Option<&str>, file: Option<&str>) -> anyhow::Result<Value> {
    let (uid, file) = (uid.map(String::from), file.map(String::from));
    let rows = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT id, uid, file, op, ts, dream_run, length(before), length(after)
                 FROM mem_history
                 WHERE (?1 IS NULL OR uid = ?1) AND (?2 IS NULL OR file = ?2)
                 ORDER BY id DESC LIMIT 50",
            )?;
            let rows = st.query_map(params![uid, file], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "uid": r.get::<_, Option<String>>(1)?,
                    "file": r.get::<_, String>(2)?,
                    "op": r.get::<_, String>(3)?,
                    "ts": r.get::<_, String>(4)?,
                    "dream_run": r.get::<_, Option<String>>(5)?,
                    "before_bytes": r.get::<_, Option<i64>>(6)?,
                    "after_bytes": r.get::<_, Option<i64>>(7)?,
                }))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            Ok(v)
        })
        .await?;
    Ok(json!(rows))
}

/// Remet un fichier dans l'état d'une pré-image, puis réindexe le vault.
pub async fn restore(s: &Services, history_id: i64) -> anyhow::Result<Value> {
    let row: Option<(String, Option<String>, Option<String>)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare("SELECT file, before, after FROM mem_history WHERE id = ?1")?;
            let mut rows = st.query([history_id])?;
            Ok(match rows.next()? {
                Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?)),
                None => None,
            })
        })
        .await?;
    let Some((file, before, after)) = row else {
        anyhow::bail!("historique {history_id} introuvable");
    };
    if file.contains("..") {
        anyhow::bail!("chemin refusé : {file}");
    }
    let vault = crate::helpers::vault_dir(s);
    let path = vault.join(&file);
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let before = before.unwrap_or_default();
    penelope_kernel::config::atomic_write(&path, before.as_bytes())?;
    let (f, ts) = (file.clone(), s.clock.now_rfc3339());
    let cur = current.clone();
    let prev = before.clone();
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO mem_history(uid, file, op, before, after, ts, dream_run)
                 VALUES(NULL, ?1, 'restore', ?2, ?3, ?4, NULL)",
                params![f, cur, prev, ts],
            )?;
            Ok(())
        })
        .await?;
    let reindexed = crate::vault_ops::reindex(s, &vault)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(json!({
        "file": file,
        "restored": history_id,
        "edited_since": after.as_deref() != Some(current.as_str()),
        "reindexed": reindexed,
    }))
}

/// Apprentissages des derniers jours : entrées ajoutées ou modifiées par la consolidation.
pub async fn learned(s: &Services, days: i64) -> anyhow::Result<Vec<Value>> {
    let cutoff = (s.clock.now_utc() - chrono::Duration::days(days.max(1)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let rows: Vec<(Option<String>, String, String, String)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT uid, file, op, ts FROM mem_history
                 WHERE ts >= ?1 AND op IN ('add_entry','replace_entry','add_exception',
                    'record_ecart','update_exception','create_entity')
                 ORDER BY id DESC LIMIT 100",
            )?;
            let rows = st.query_map([cutoff], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            Ok(v)
        })
        .await?;
    let mut out = Vec::new();
    for (uid, file, op, ts) in rows {
        let text = match &uid {
            Some(u) => s.memory.get(u).await?.map(|e| e.text),
            None => None,
        };
        out.push(json!({"uid": uid, "file": file, "op": op, "ts": ts, "text": text}));
    }
    Ok(out)
}
