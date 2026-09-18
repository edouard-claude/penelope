//! Purge RGPD d'une session et rétention des traces (issue #46).
//!
//! Deux mouvements, un seul principe : **la chaîne d'audit reste, le contenu part**.
//!
//! ```text
//!  session.purge <id>
//!     messages, messages_fts, message_context, lcm_*, artifacts (fichiers compris)
//!     llm_requests, turn_queue.payload, tg_updates.payload, mem_candidates.text
//!     effects.request/result, tg_outbox, approval_requests.payload, mcp_tasks,
//!     workflow_runs et workflow_step_log des runs de la session (#78)
//!     events : payload remplacé, hash d'origine conservé (audit.purge)
//!
//!  rétention (une fois par jour)
//!     tours terminés, requêtes au modèle, updates Telegram, clés de travail,
//!     arguments et résultats d'effets tranchés, messages envoyés, demandes décidées,
//!     tâches MCP et sorties de runs terminés : au-delà de `retention.days`, vidés ;
//!     pré-images de la mémoire au-delà de `retention.memory_history_days`.
//! ```
//!
//! Un effet vidé garde sa ligne, son état et sa clé d'idempotence : un rejeu reste
//! reconnu comme tel, seul le contenu est parti.
//!
//! Ce qui n'est **jamais** touché : `events` (la ligne et son hash), `usage` (comptabilité,
//! sans texte), la mémoire durable (le vault et ses fichiers ont leurs propres outils).

use crate::runtime::Daemon;
use penelope_kernel::event::EventDraft;
use penelope_store::rusqlite::params;
use serde_json::{Value, json};

/// Clés de travail effacées par la rétention : marqueurs d'un tour, d'une session ou d'un
/// run, sans valeur une fois la trace éteinte.
const EPHEMERAL_KEYS: &[&str] = &[
    "turn.",
    "prompt.prefix.",
    "session.model_last.",
    "session.tools.",
    "session.served.",
    "session.model_pin.",
    "session.title_asked.",
    "budget.alert.",
    "scheduler.watch.",
    "mcp.oauth.notified.",
    "mcp.invalid.",
    "tg.card.",
    "tg.form.",
    "tg.held.",
    "tg.await_",
    "notes.harvested.",
    "ingest.sha.",
    "memory_proposal.applied.",
    "episode.ingested.",
    "wf.",
];

/// Efface tout ce qu'une session a dit et fait dire, sauf la chaîne d'audit.
pub async fn session(d: &Daemon, session_id: &str, reason: &str) -> anyhow::Result<Value> {
    let s = &d.services;
    let sess = s.sessions.require(session_id).await?;
    let sid = session_id.to_string();
    let chat_id = sess.tg_chat_id;
    let since = sess.created_at.clone();
    // Sur un chat partagé par plusieurs sessions successives, la fenêtre s'arrête à
    // l'ouverture de la suivante.
    let until: Option<String> = match chat_id {
        Some(chat) => {
            let since = since.clone();
            s.store
                .read(move |c| {
                    Ok(c.query_row(
                        "SELECT min(created_at) FROM sessions
                         WHERE tg_chat_id = ?1 AND created_at > ?2",
                        params![chat, since],
                        |r| r.get::<_, Option<String>>(0),
                    )?)
                })
                .await?
        }
        None => None,
    };
    let until_or_max = until.unwrap_or_else(|| "9999".into());
    let media_root = s
        .platform
        .dirs
        .data()
        .join("media")
        .to_string_lossy()
        .to_string();
    let artifacts_root = s.platform.dirs.artifacts();

    let (counts, files) = s
        .store
        .write(move |tx| {
            // Fichiers cités par le transcript (photos, vocaux) et artefacts externalisés.
            let mut files: Vec<String> = Vec::new();
            {
                let mut st = tx.prepare("SELECT content FROM messages WHERE session_id = ?1")?;
                let rows = st.query_map([&sid], |r| r.get::<_, String>(0))?;
                for r in rows {
                    files.extend(media_paths(&r?, &media_root));
                }
            }
            {
                let mut st = tx.prepare(
                    "SELECT path FROM artifacts WHERE session_id = ?1 AND path IS NOT NULL",
                )?;
                let rows = st.query_map([&sid], |r| r.get::<_, String>(0))?;
                for r in rows {
                    files.push(r?);
                }
            }

            let messages = tx.execute(
                "DELETE FROM messages_fts WHERE msg_id IN
                   (SELECT id FROM messages WHERE session_id = ?1)",
                [&sid],
            )?;
            tx.execute("DELETE FROM messages WHERE session_id = ?1", [&sid])?;
            tx.execute("DELETE FROM message_context WHERE session_id = ?1", [&sid])?;
            tx.execute(
                "DELETE FROM lcm_edges WHERE parent_id IN
                   (SELECT id FROM lcm_nodes WHERE session_id = ?1)
                 OR child_id IN (SELECT id FROM lcm_nodes WHERE session_id = ?1)",
                [&sid],
            )?;
            let nodes = tx.execute("DELETE FROM lcm_nodes WHERE session_id = ?1", [&sid])?;
            let artifacts = tx.execute("DELETE FROM artifacts WHERE session_id = ?1", [&sid])?;
            let requests = tx.execute("DELETE FROM llm_requests WHERE session_id = ?1", [&sid])?;
            let turns = tx.execute(
                "UPDATE turn_queue SET payload = '{\"purged\":true}', last_error = NULL
                 WHERE session_id = ?1",
                [&sid],
            )?;
            let candidates = tx.execute(
                "UPDATE mem_candidates SET text = '(purgé)', source_ref = NULL
                 WHERE session_id = ?1",
                [&sid],
            )?;
            tx.execute(
                "UPDATE mem_provenance SET source_ref = NULL WHERE session_id = ?1",
                [&sid],
            )?;
            tx.execute(
                "UPDATE sessions SET title = '(session purgée)' WHERE id = ?1",
                [&sid],
            )?;
            // Updates Telegram reçus pendant la session sur ce chat : leur payload porte le
            // texte intégral.
            let updates = match chat_id {
                Some(chat) => tx.execute(
                    "UPDATE tg_updates SET payload = '{}'
                     WHERE received_at >= ?1 AND received_at < ?3 AND payload <> '{}'
                       AND ?2 IN (
                        json_extract(payload, '$.message.chat.id'),
                        json_extract(payload, '$.edited_message.chat.id'),
                        json_extract(payload, '$.callback_query.message.chat.id')
                     )",
                    params![since, chat, until_or_max],
                )?,
                None => 0,
            };
            // Ce que l'assistante a dit sur Telegram pendant la session (#78).
            let outbox = match chat_id {
                Some(chat) => tx.execute(
                    "DELETE FROM tg_outbox
                     WHERE chat_id = ?1 AND created_at >= ?2 AND created_at < ?3",
                    params![chat, since, until_or_max],
                )?,
                None => 0,
            };
            // Ce que l'agent a fait (#78) : arguments et résultats d'outils, demandes
            // d'approbation, tâches MCP, sorties de workflows. Les lignes, leurs états et
            // les clés d'idempotence restent : un rejeu est toujours reconnu.
            const RUNS: &str = "SELECT id FROM workflow_runs WHERE session_id = ?1";
            let effects = tx.execute(
                &format!(
                    "UPDATE effects SET request = '{{}}', result = NULL, error = NULL
                     WHERE session_id = ?1 OR run_id IN ({RUNS})"
                ),
                [&sid],
            )?;
            let approvals = tx.execute(
                &format!(
                    "UPDATE approval_requests SET subject = '(purgé)', payload = '{{}}',
                        reason = NULL,
                        state = CASE state WHEN 'pending' THEN 'cancelled' ELSE state END
                     WHERE session_id = ?1 OR run_id IN ({RUNS})"
                ),
                [&sid],
            )?;
            let tasks = tx.execute(
                &format!(
                    "UPDATE mcp_tasks SET request = '{{}}', result = NULL
                     WHERE session_id = ?1 OR run_id IN ({RUNS})"
                ),
                [&sid],
            )?;
            let steps = tx.execute(
                &format!(
                    "UPDATE workflow_step_log SET output = NULL, error = NULL
                     WHERE run_id IN ({RUNS})"
                ),
                [&sid],
            )?;
            let runs = tx.execute(
                "UPDATE workflow_runs SET params = '{}', step_outputs = '{}', result = NULL,
                    error = NULL,
                    state = CASE WHEN state IN ('running','paused','blocked')
                                 THEN 'cancelled' ELSE state END,
                    finished_at = COALESCE(finished_at, updated_at)
                 WHERE session_id = ?1",
                [&sid],
            )?;
            Ok((
                json!({
                    "messages": messages,
                    "lcm_nodes": nodes,
                    "artifacts": artifacts,
                    "llm_requests": requests,
                    "turns": turns,
                    "candidates": candidates,
                    "tg_updates": updates,
                    "tg_outbox": outbox,
                    "effects": effects,
                    "approvals": approvals,
                    "mcp_tasks": tasks,
                    "workflow_steps": steps,
                    "workflow_runs": runs,
                }),
                files,
            ))
        })
        .await?;

    let mut removed = 0usize;
    for f in &files {
        let path = std::path::Path::new(f);
        let under_media = f.starts_with(
            &s.platform
                .dirs
                .data()
                .join("media")
                .to_string_lossy()
                .to_string(),
        );
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            artifacts_root.join(path)
        };
        if (under_media || path.starts_with(&artifacts_root)) && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }

    // La chaîne d'audit garde ses lignes et ses hash : le payload seul est remplacé, et
    // `audit.purge` dit ce qui vient d'être fait.
    let events = s.events.purge_session(session_id, reason).await?;

    let report = json!({
        "session": session_id,
        "reason": reason,
        "events": events,
        "files": removed,
        "tables": counts,
    });
    tracing::info!(session = %session_id, %reason, "session purgée");
    Ok(report)
}

/// Chemins de médias cités dans un message (`"/…/media/voice/01J.ogg"`).
fn media_paths(content: &str, root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(i) = rest.find(root) {
        let tail = &rest[i..];
        let end = tail
            .find(['"', '\\', ' ', ')', '\''])
            .unwrap_or(tail.len().min(512));
        let path = &tail[..end];
        if !path.is_empty() {
            out.push(path.to_string());
        }
        rest = &tail[end.max(1)..];
    }
    out
}

/// Rétention : efface ce qui a passé l'âge. Renvoie le détail par table.
pub async fn retention(d: &Daemon) -> anyhow::Result<Value> {
    let s = &d.services;
    let cfg = s.config.config();
    let now = s.clock.now_ms();
    let cutoff = |days: u32| {
        chrono::DateTime::from_timestamp_millis(now - days as i64 * 86_400_000)
            .unwrap_or_default()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let days = cfg.retention.days;
    let history_days = cfg.retention.memory_history_days;
    let general = (days > 0).then(|| cutoff(days));
    let history = (history_days > 0).then(|| cutoff(history_days));

    let report = s
        .store
        .write(move |tx| {
            let mut turns = 0;
            let mut requests = 0;
            let mut updates = 0;
            let mut keys = 0;
            let (mut effects, mut outbox, mut approvals) = (0, 0, 0);
            let (mut tasks, mut steps, mut runs) = (0, 0, 0);
            if let Some(c) = &general {
                turns = tx.execute(
                    "DELETE FROM turn_queue
                     WHERE state IN ('done','failed','cancelled')
                       AND finished_at IS NOT NULL AND finished_at < ?1",
                    [c],
                )?;
                requests = tx.execute(
                    "DELETE FROM llm_requests
                     WHERE state IN ('completed','failed') AND updated_at < ?1",
                    [c],
                )?;
                updates = tx.execute(
                    "UPDATE tg_updates SET payload = '{}'
                     WHERE processed = 1 AND received_at < ?1 AND payload <> '{}'",
                    [c],
                )?;
                for prefix in EPHEMERAL_KEYS {
                    keys += tx.execute(
                        "DELETE FROM kv WHERE ts IS NOT NULL AND ts < ?1 AND k LIKE ?2",
                        params![c, format!("{prefix}%")],
                    )?;
                }
                // #78 : le contenu de ce qui est tranché. Un effet `unknown` attend une
                // décision : il garde tout. La ligne d'un effet reste pour l'idempotence.
                effects = tx.execute(
                    "UPDATE effects SET request = '{}', result = NULL, error = NULL
                     WHERE state IN ('completed','failed') AND updated_at < ?1
                       AND (request <> '{}' OR result IS NOT NULL OR error IS NOT NULL)",
                    [c],
                )?;
                outbox = tx.execute(
                    "DELETE FROM tg_outbox
                     WHERE state IN ('sent','failed') AND created_at < ?1",
                    [c],
                )?;
                approvals = tx.execute(
                    "UPDATE approval_requests SET payload = '{}', reason = NULL
                     WHERE state <> 'pending' AND payload <> '{}'
                       AND COALESCE(decided_at, expires_at) < ?1",
                    [c],
                )?;
                tasks = tx.execute(
                    "DELETE FROM mcp_tasks
                     WHERE state IN ('completed','failed','cancelled') AND updated_at < ?1",
                    [c],
                )?;
                steps = tx.execute(
                    "UPDATE workflow_step_log SET output = NULL, error = NULL
                     WHERE (output IS NOT NULL OR error IS NOT NULL) AND run_id IN (
                        SELECT id FROM workflow_runs
                        WHERE state IN ('done','failed','cancelled') AND finished_at < ?1)",
                    [c],
                )?;
                runs = tx.execute(
                    "UPDATE workflow_runs SET step_outputs = '{}'
                     WHERE state IN ('done','failed','cancelled') AND finished_at < ?1
                       AND step_outputs <> '{}'",
                    [c],
                )?;
            }
            let mut history_rows = 0;
            if let Some(c) = &history {
                history_rows = tx.execute("DELETE FROM mem_history WHERE ts < ?1", [c])?;
            }
            Ok(json!({
                "turns": turns,
                "llm_requests": requests,
                "tg_updates": updates,
                "kv": keys,
                "mem_history": history_rows,
                "effects": effects,
                "tg_outbox": outbox,
                "approvals": approvals,
                "mcp_tasks": tasks,
                "workflow_steps": steps,
                "workflow_runs": runs,
            }))
        })
        .await?;

    let total: i64 = report
        .as_object()
        .map(|o| o.values().filter_map(|v| v.as_i64()).sum())
        .unwrap_or(0);
    if total > 0 {
        tracing::info!(detail = %report, "rétention appliquée");
        let _ = s
            .events
            .append(EventDraft::new(
                "store.retention",
                json!({"days": days, "memory_history_days": history_days, "rows": report}),
            ))
            .await;
    }
    Ok(report)
}

/// Une passe de rétention par jour, appelée par le superviseur.
pub async fn retention_tick(d: &Daemon) -> anyhow::Result<()> {
    let now = d.services.clock.now_ms();
    let last = d
        .kv_get("retention.last")
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < 24 * 3_600_000 {
        return Ok(());
    }
    d.kv_set("retention.last", &now.to_string()).await?;
    retention(d).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Origin;
    use penelope_llm::types::ChatMessage;
    use std::sync::Arc;

    async fn daemon() -> (
        tempfile::TempDir,
        Arc<Daemon>,
        penelope_kernel::clock::TestClock,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let test_clock = penelope_kernel::clock::TestClock::default();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(test_clock.clone());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        (dir, Arc::new(Daemon::from_services(s)), test_clock)
    }

    /// Le mot du transcript ne doit plus exister nulle part : neuf tables, l'index plein
    /// texte et les fichiers.
    async fn word_is_gone(d: &Daemon, word: &str) -> bool {
        let w = format!("%{word}%");
        d.services
            .store
            .read(move |c| {
                let tables = [
                    ("messages", "content"),
                    ("message_context", "context"),
                    ("lcm_nodes", "summary"),
                    ("artifacts", "head"),
                    ("llm_requests", "request"),
                    ("turn_queue", "payload"),
                    ("tg_updates", "payload"),
                    ("mem_candidates", "text"),
                    ("sessions", "title"),
                    ("events", "payload"),
                    // #78 : ce que l'agent a fait et dit.
                    ("effects", "request"),
                    ("effects", "result"),
                    ("tg_outbox", "payload"),
                    ("approval_requests", "payload"),
                    ("mcp_tasks", "request"),
                    ("mcp_tasks", "result"),
                    ("workflow_step_log", "output"),
                    ("workflow_runs", "params"),
                    ("workflow_runs", "step_outputs"),
                ];
                for (table, column) in tables {
                    let n: i64 = c.query_row(
                        &format!("SELECT count(*) FROM {table} WHERE {column} LIKE ?1"),
                        [&w],
                        |r| r.get(0),
                    )?;
                    if n > 0 {
                        return Ok(false);
                    }
                }
                let fts: i64 = c.query_row(
                    "SELECT count(*) FROM messages_fts WHERE content LIKE ?1",
                    [&w],
                    |r| r.get(0),
                )?;
                Ok(fts == 0)
            })
            .await
            .unwrap()
    }

    /// #46 : après la purge, plus un mot du transcript dans la base, les fichiers sont
    /// partis, la chaîne d'audit tient toujours.
    #[tokio::test]
    async fn purging_a_session_leaves_the_audit_chain_and_nothing_else() {
        let (dir, d, _clock) = daemon().await;
        let s = d.services.clone();
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        s.sessions.bind_telegram(&sid, 4242, None).await.unwrap();
        const SECRET: &str = "Zéphyrine";

        // Un transcript, son contexte figé, un résumé, un artefact avec son fichier.
        let h = &s.context.history;
        h.append(
            &sid,
            &ChatMessage::user(format!("ma carte est au nom de {SECRET}")),
            10,
            0,
            false,
            None,
        )
        .await
        .unwrap();
        h.append(&sid, &ChatMessage::assistant("noté"), 5, 0, false, None)
            .await
            .unwrap();
        h.freeze_context(&sid, 1, SECRET).await.unwrap();
        let art = h
            .put_artifact(Some(&sid), None, "text", None, &format!("dossier {SECRET}"))
            .await
            .unwrap();
        let media = s.platform.dirs.data().join("media").join("voice");
        std::fs::create_dir_all(&media).unwrap();
        let vocal = media.join("01J.ogg");
        std::fs::write(&vocal, b"OggS").unwrap();
        h.append(
            &sid,
            &ChatMessage::user(format!("{} (vocal)", vocal.display())),
            5,
            0,
            false,
            None,
        )
        .await
        .unwrap();

        // Une trace dans chacune des autres tables.
        let sid2 = sid.clone();
        let (secret_owned, path_owned) = (SECRET.to_string(), art.id.clone());
        s.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO lcm_nodes(id, session_id, kind, level, summary, created_at)
                     VALUES('n1',?1,'condensed',1,?2,'2026-01-01T00:00:00Z')",
                    penelope_store::rusqlite::params![sid2, format!("résumé {secret_owned}")],
                )?;
                tx.execute(
                    "INSERT INTO llm_requests(id, session_id, model, provider, state, body_hash,
                        request, created_at, updated_at)
                     VALUES('r1',?1,'m','p','completed','h',?2,'2026-01-01T00:00:00Z',
                        '2026-01-01T00:00:00Z')",
                    penelope_store::rusqlite::params![sid2, format!("prompt {secret_owned}")],
                )?;
                tx.execute(
                    "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(77,'2026-06-01T00:00:00Z',1,?1)",
                    [format!(
                        r#"{{"update_id":77,"message":{{"chat":{{"id":4242}},"text":"{secret_owned}"}}}}"#
                    )],
                )?;
                tx.execute(
                    "INSERT INTO mem_candidates(id, ctype, text, importance, origin, session_id,
                        session_kind, observed_at, day, state)
                     VALUES('c1','fait',?2,5,'owner',?1,'chat','2026-06-01T00:00:00Z',
                        '2026-06-01','new')",
                    penelope_store::rusqlite::params![sid2, format!("{secret_owned} paie comptant")],
                )?;
                tx.execute(
                    "UPDATE artifacts SET path = ?2 WHERE id = ?1",
                    penelope_store::rusqlite::params![path_owned, "art.txt"],
                )?;
                // #78 : réponse partie sur Telegram, demande d'approbation, tâche MCP,
                // run de workflow et sa journalisation d'étape.
                let dit = format!(r#"{{"text":"{secret_owned}"}}"#);
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES('o1',4242,'sendMessage',?1,'sent','2026-06-01T00:00:00Z')",
                    [&dit],
                )?;
                tx.execute(
                    "INSERT INTO approval_requests(id, kind, subject, risk, payload, session_id,
                        created_at, expires_at, state)
                     VALUES('a1','tool_call','fs_write','write',?2,?1,'2026-06-01T00:00:00Z',
                        '2026-06-02T00:00:00Z','pending')",
                    penelope_store::rusqlite::params![sid2, dit],
                )?;
                tx.execute(
                    "INSERT INTO workflow_runs(id, workflow_id, session_id, params, state,
                        step_outputs, started_at, updated_at, finished_at)
                     VALUES('wr1','demo',?1,?2,'done',?2,'2026-06-01T00:00:00Z',
                        '2026-06-01T00:00:00Z','2026-06-01T00:00:00Z')",
                    penelope_store::rusqlite::params![sid2, dit],
                )?;
                tx.execute(
                    "INSERT INTO workflow_step_log(run_id, step_id, started_at, output)
                     VALUES('wr1','un','2026-06-01T00:00:00Z',?1)",
                    [&dit],
                )?;
                tx.execute(
                    "INSERT INTO mcp_tasks(id, server, task_ref, run_id, request, state, result,
                        created_at, updated_at)
                     VALUES('mt1','forge','t','wr1',?1,'completed',?1,'2026-06-01T00:00:00Z',
                        '2026-06-01T00:00:00Z')",
                    [&dit],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        // Un effet mené à terme dans la session : arguments et résultat portent le mot.
        let effet = || {
            penelope_kernel::effects::EffectSpec::new(
                penelope_kernel::effects::EffectKind::Tool,
                "fs_write",
                serde_json::json!({"path": "notes.md", "content": SECRET}),
            )
            .session(&sid)
        };
        let eid = match s.effects.plan(effet()).await.unwrap() {
            penelope_kernel::effects::Planned::Fresh(id) => id,
            o => panic!("{o:?}"),
        };
        s.effects.dispatching(&eid).await.unwrap();
        s.effects
            .complete(&eid, serde_json::json!({"written": SECRET}))
            .await
            .unwrap();
        let art_file = s.platform.dirs.artifacts().join("art.txt");
        std::fs::create_dir_all(s.platform.dirs.artifacts()).unwrap();
        std::fs::write(&art_file, format!("dossier {SECRET}")).unwrap();
        s.turns
            .enqueue(
                &sid,
                penelope_kernel::turn::TurnKind::Message,
                serde_json::json!({"text": format!("ma carte est au nom de {SECRET}")}),
                None,
                0,
            )
            .await
            .unwrap();
        s.events
            .append(
                penelope_kernel::event::EventDraft::new(
                    "message.received",
                    serde_json::json!({"text": SECRET}),
                )
                .session(&sid),
            )
            .await
            .unwrap();

        assert!(!word_is_gone(&d, SECRET).await, "le mot doit être là avant");
        assert!(!h.grep(SECRET, None, 10).await.unwrap().is_empty());

        let report = session(&d, &sid, "essai").await.unwrap();
        assert!(report["events"].as_u64().unwrap() > 0);
        assert_eq!(report["files"], 2, "vocal et artefact effacés : {report}");

        assert!(
            word_is_gone(&d, SECRET).await,
            "purge incomplète : {report}"
        );
        assert!(h.grep(SECRET, None, 10).await.unwrap().is_empty());
        // L'idempotence survit : l'effet purgé est rejoué, jamais ré-exécuté.
        assert!(matches!(
            s.effects.plan(effet()).await.unwrap(),
            penelope_kernel::effects::Planned::Replayed(_)
        ));
        // La demande en attente de la session ne peut plus être décidée.
        assert!(s.approvals.pending(10).await.unwrap().is_empty());
        assert!(!vocal.exists());
        assert!(!art_file.exists());
        // La chaîne d'audit tient : les lignes sont là, leurs hash d'origine aussi.
        let verified = s.events.verify().await.unwrap();
        assert!(verified.ok, "chaîne rompue : {verified:?}");
        drop(dir);
    }

    /// #46 : le payload d'un update traité est vidé, mais son `update_id` reste : rejoué,
    /// il n'est pas traité deux fois.
    #[tokio::test]
    async fn a_replayed_update_is_still_deduplicated_without_its_payload() {
        let (_dir, d, _clock) = daemon().await;
        let s = &d.services;
        s.store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(12,'2026-06-01T00:00:00Z',1,'{}')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let already: bool = s
            .store
            .write(|tx| {
                tx.execute(
                    "INSERT OR IGNORE INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(12,'2026-06-02T00:00:00Z',0,'{\"update_id\":12}')",
                    [],
                )?;
                let processed: i64 = tx.query_row(
                    "SELECT processed FROM tg_updates WHERE update_id = 12",
                    [],
                    |r| r.get(0),
                )?;
                Ok(processed == 1)
            })
            .await
            .unwrap();
        assert!(already, "un update rejoué reste dédoublonné sans payload");
    }

    /// #46 : la rétention efface ce qui a passé l'âge, et rien d'autre.
    #[tokio::test]
    async fn retention_removes_what_is_past_its_age_only() {
        let (_dir, d, clock) = daemon().await;
        let s = &d.services;
        // L'horloge de test démarre au 1er janvier 2026 : on avance de 91 jours et tout ce
        // qui date du jour 1 a dépassé les 90 jours de rétention.
        let vieux = "2026-01-01T00:00:00Z";
        clock.advance_ms(91 * 86_400_000);
        s.store
            .write(move |tx| {
                let p = penelope_store::rusqlite::params![vieux];
                tx.execute(
                    "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at,
                        finished_at) VALUES('t_vieux','s','message','{}','done',?1,?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO turn_queue(id, session_id, kind, payload, state, enqueued_at)
                     VALUES('t_attente','s','message','{}','pending',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO llm_requests(id, session_id, model, provider, state, body_hash,
                        request, created_at, updated_at)
                     VALUES('r_vieux','s','m','p','completed','h','prompt',?1,?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO tg_updates(update_id, received_at, processed, payload)
                     VALUES(1,?1,1,'{\"text\":\"salut\"}')",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO mem_history(uid, file, op, before, after, ts)
                     VALUES('u1','memoire.md','update','avant','apres',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO kv(k, v, ts) VALUES('turn.recorded.x','1',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO kv(k, v, ts) VALUES('upgrade.state','{}',?1)",
                    p,
                )?;
                // #78 : un effet tranché perd son contenu, un effet incertain le garde.
                tx.execute(
                    "INSERT INTO effects(id, session_id, idem_key, kind, tool, request, state,
                        result, created_at, updated_at)
                     VALUES('e_fait','s','k1','tool','fs_write','{\"c\":1}','completed',
                        '{\"ok\":true}',?1,?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO effects(id, session_id, idem_key, kind, tool, request, state,
                        created_at, updated_at)
                     VALUES('e_doute','s','k2','tool','git_push','{\"b\":1}','unknown',?1,?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES('o_vieux',1,'sendMessage','{}','sent',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES('o_attente',1,'sendMessage','{}','pending',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO approval_requests(id, kind, subject, risk, payload, created_at,
                        expires_at, state, decided_at)
                     VALUES('a_vieille','tool_call','x','write','{\"a\":1}',?1,?1,'approved',?1)",
                    p,
                )?;
                tx.execute(
                    "INSERT INTO mcp_tasks(id, server, task_ref, request, state, created_at,
                        updated_at) VALUES('mt_vieille','f','t','{}','completed',?1,?1)",
                    p,
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let report = retention(&d).await.unwrap();
        assert_eq!(report["effects"], 1, "{report}");
        assert_eq!(report["tg_outbox"], 1);
        assert_eq!(report["approvals"], 1);
        assert_eq!(report["mcp_tasks"], 1);
        let (fait, doute, attente): (Option<String>, String, i64) = s
            .store
            .read(|c| {
                Ok((
                    c.query_row("SELECT result FROM effects WHERE id='e_fait'", [], |r| {
                        r.get(0)
                    })?,
                    c.query_row("SELECT request FROM effects WHERE id='e_doute'", [], |r| {
                        r.get(0)
                    })?,
                    c.query_row(
                        "SELECT count(*) FROM tg_outbox WHERE id='o_attente'",
                        [],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(fait, None, "le résultat d'un effet tranché part");
        assert_eq!(doute, r#"{"b":1}"#, "un effet incertain garde tout");
        assert_eq!(attente, 1, "un message à envoyer reste");
        assert_eq!(report["turns"], 1, "{report}");
        assert_eq!(report["llm_requests"], 1);
        assert_eq!(report["tg_updates"], 1);
        assert_eq!(report["mem_history"], 1);
        assert_eq!(report["kv"], 1);

        let (turns, keys): (i64, i64) = s
            .store
            .read(|c| {
                Ok((
                    c.query_row("SELECT count(*) FROM turn_queue", [], |r| r.get(0))?,
                    c.query_row("SELECT count(*) FROM kv WHERE k='upgrade.state'", [], |r| {
                        r.get(0)
                    })?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(turns, 1, "le tour en attente reste");
        assert_eq!(keys, 1, "une clé durable n'est pas balayée");
    }

    #[test]
    fn media_paths_are_read_out_of_a_message() {
        let root = "/data/media";
        let content = r#"[{"type":"text","text":"voici"},
            {"type":"image","path":"/data/media/photos/01J.jpg"},
            {"type":"audio","path":"/data/media/voice/01K.ogg"}]"#;
        assert_eq!(
            media_paths(content, root),
            vec!["/data/media/photos/01J.jpg", "/data/media/voice/01K.ogg"]
        );
        assert!(media_paths(r#"{"text":"rien"}"#, root).is_empty());
    }
}
