//! Purge RGPD d'une session et rétention des traces (issue #46).
//!
//! Deux mouvements, un seul principe : **la chaîne d'audit reste, le contenu part**.
//!
//! ```text
//!  session.purge <id>
//!     messages, messages_fts, message_context, lcm_*, artifacts (fichiers compris)
//!     llm_requests, turn_queue.payload, tg_updates.payload, mem_candidates.text
//!     effects.request/result, tg_outbox, approval_requests.payload, mcp_tasks, tool_jobs,
//!     workflow_runs et workflow_step_log des runs de la session (#78)
//!     prompt_snapshots que la session seule référençait, et usage.system_hash (#205)
//!     prompt_snapshots cités par ses conv.system et par aucune autre session (T17)
//!     events : payload remplacé, hash d'origine conservé (audit.purge)
//!     sessions nées d'un fork : nommées dans le rapport, elles perdent leur préfixe
//!
//!  rétention (une fois par jour)
//!     tours terminés, requêtes au modèle, updates Telegram, clés de travail,
//!     arguments et résultats d'effets tranchés, messages envoyés, demandes décidées,
//!     tâches MCP, jobs d'outils et sorties de runs terminés : au-delà de
//!     `retention.days`, vidés ;
//!     prompts système que plus aucune ligne ne cite ;
//!     payloads des `conv.attempt` (texte partiel), hash gardé dans event_purges (T17) ;
//!     pré-images de la mémoire au-delà de `retention.memory_history_days`.
//! ```
//!
//! Un effet vidé garde sa ligne, son état et sa clé d'idempotence : un rejeu reste
//! reconnu comme tel, seul le contenu est parti.
//!
//! Ce qui n'est **jamais** touché : `events` (la ligne et son hash), la mémoire durable
//! (le vault et ses fichiers ont leurs propres outils). `usage` garde ses lignes, ses
//! jetons et ses coûts : seul son `system_hash`, qui mène au texte d'un prompt, est coupé
//! à la purge d'une session (#205).

use penelope_app::services::Services;
use penelope_context::HistoryStore;
use penelope_kernel::event::EventDraft;
use penelope_store::rusqlite::params;
use serde_json::{Value, json};

/// Clés de travail effacées par la rétention : marqueurs d'un tour, d'une session ou d'un
/// run, sans valeur une fois la trace éteinte.
const EPHEMERAL_KEYS: &[&str] = &[
    "turn.",
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

/// Clés que plus rien n'écrit ni ne lit, effacées quel que soit leur âge : le journal
/// d'événements les remplace (épopée #208, T16 : `conv.system` pour le préfixe retenu,
/// `conv.user` et sa clé de tour pour le message déjà écrit).
const RETIRED_KEYS: &[&str] = &["prompt.prefix.", "turn.recorded."];

/// Efface tout ce qu'une session a dit et fait dire, sauf la chaîne d'audit.
pub async fn session(s: &Services, session_id: &str, reason: &str) -> anyhow::Result<Value> {
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
    let forks = forks_of(s, session_id).await?;
    if !forks.is_empty() {
        tracing::warn!(session = %session_id, ?forks, "{}", forks_warning(&forks));
    }

    let (counts, files) = s
        .store
        .write(move |tx| {
            let files = session_files(tx, &sid, &media_root)?;

            // Les caches de la conversation, prompts système compris (#205) : seule
            // `penelope-context` les écrit (T16).
            let caches = HistoryStore::purge_session_in(tx, &sid)?;
            let (messages, nodes, prompts) = (caches.messages, caches.nodes, caches.prompts);
            let artifacts = tx.execute("DELETE FROM artifacts WHERE session_id = ?1", [&sid])?;
            // La ligne comptable reste, avec ses jetons et son coût : seule la clé qui
            // menait au texte est coupée.
            tx.execute(
                "UPDATE usage SET system_hash = NULL WHERE session_id = ?1",
                [&sid],
            )?;
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
            let (updates, outbox) = purge_session_channel(tx, chat_id, &since, &until_or_max)?;
            let SessionWork {
                effects,
                approvals,
                tasks,
                jobs,
                steps,
                runs,
            } = purge_session_work(tx, &sid)?;
            Ok((
                json!({
                    "messages": messages,
                    "lcm_nodes": nodes,
                    "artifacts": artifacts,
                    "llm_requests": requests,
                    "prompt_snapshots": prompts,
                    "turns": turns,
                    "candidates": candidates,
                    "tg_updates": updates,
                    "tg_outbox": outbox,
                    "effects": effects,
                    "approvals": approvals,
                    "mcp_tasks": tasks,
                    "tool_jobs": jobs,
                    "workflow_steps": steps,
                    "workflow_runs": runs,
                }),
                files,
            ))
        })
        .await?;

    let removed = remove_session_files(s, &files, &artifacts_root);

    // La chaîne d'audit garde ses lignes et ses hash : le payload seul est remplacé, et
    // `audit.purge` dit ce qui vient d'être fait.
    let events = s.events.purge_session(session_id, reason).await?;

    let mut report = json!({
        "session": session_id,
        "reason": reason,
        "events": events,
        "files": removed,
        "tables": counts,
    });
    if !forks.is_empty() {
        report["forks"] = json!(forks);
        report["avertissement"] = json!(forks_warning(&forks));
    }
    tracing::info!(session = %session_id, %reason, "session purgée");
    Ok(report)
}

/// Fichiers cités par le transcript (photos, vocaux) et artefacts externalisés.
fn session_files(
    tx: &penelope_store::rusqlite::Transaction<'_>,
    sid: &str,
    media_root: &str,
) -> penelope_store::rusqlite::Result<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    {
        let mut st = tx.prepare("SELECT content FROM messages WHERE session_id = ?1")?;
        let rows = st.query_map([sid], |r| r.get::<_, String>(0))?;
        for r in rows {
            files.extend(media_paths(&r?, media_root));
        }
    }
    {
        let mut st =
            tx.prepare("SELECT path FROM artifacts WHERE session_id = ?1 AND path IS NOT NULL")?;
        let rows = st.query_map([sid], |r| r.get::<_, String>(0))?;
        for r in rows {
            files.push(r?);
        }
    }
    Ok(files)
}

/// Le côté canal de la session, sur la fenêtre `[since, until[` de son chat : updates
/// reçus et messages envoyés. Rien sans chat.
fn purge_session_channel(
    tx: &penelope_store::rusqlite::Transaction<'_>,
    chat: Option<i64>,
    since: &str,
    until: &str,
) -> penelope_store::rusqlite::Result<(usize, usize)> {
    // Updates Telegram reçus pendant la session sur ce chat : leur payload porte le
    // texte intégral.
    let updates = match chat {
        Some(chat) => tx.execute(
            "UPDATE tg_updates SET payload = '{}'
             WHERE received_at >= ?1 AND received_at < ?3 AND payload <> '{}'
               AND ?2 IN (
                json_extract(payload, '$.message.chat.id'),
                json_extract(payload, '$.edited_message.chat.id'),
                json_extract(payload, '$.callback_query.message.chat.id')
             )",
            params![since, chat, until],
        )?,
        None => 0,
    };
    // Ce que l'assistante a dit sur Telegram pendant la session (#78).
    let outbox = match chat {
        Some(chat) => tx.execute(
            "DELETE FROM tg_outbox
             WHERE chat_id = ?1 AND created_at >= ?2 AND created_at < ?3",
            params![chat, since, until],
        )?,
        None => 0,
    };
    Ok((updates, outbox))
}

/// Lignes vidées par [`purge_session_work`], table par table.
struct SessionWork {
    effects: usize,
    approvals: usize,
    tasks: usize,
    jobs: usize,
    steps: usize,
    runs: usize,
}

fn purge_session_work(
    tx: &penelope_store::rusqlite::Transaction<'_>,
    sid: &str,
) -> penelope_store::rusqlite::Result<SessionWork> {
    // Ce que l'agent a fait (#78) : arguments et résultats d'outils, demandes
    // d'approbation, tâches MCP, sorties de workflows. Les lignes, leurs états et
    // les clés d'idempotence restent : un rejeu est toujours reconnu.
    const RUNS: &str = "SELECT id FROM workflow_runs WHERE session_id = ?1";
    let effects = tx.execute(
        &format!(
            "UPDATE effects SET request = '{{}}', result = NULL, error = NULL
             WHERE session_id = ?1 OR run_id IN ({RUNS})"
        ),
        [sid],
    )?;
    let approvals = tx.execute(
        &format!(
            "UPDATE approval_requests SET subject = '(purgé)', payload = '{{}}',
                reason = NULL,
                state = CASE state WHEN 'pending' THEN 'cancelled' ELSE state END
             WHERE session_id = ?1 OR run_id IN ({RUNS})"
        ),
        [sid],
    )?;
    let tasks = tx.execute(
        &format!(
            "UPDATE mcp_tasks SET request = '{{}}', result = NULL
             WHERE session_id = ?1 OR run_id IN ({RUNS})"
        ),
        [sid],
    )?;
    // #204 : un job d'outil porte la commande demandée et sa sortie. La ligne, son
    // état et son effet restent : la chaîne d'audit tient, le contenu part. Un job
    // encore en cours est déclaré annulé — la session n'existe plus pour l'accueillir.
    let jobs = tx.execute(
        &format!(
            "UPDATE tool_jobs SET request = '{{}}', result = NULL,
                state = CASE WHEN state IN ('working','input_required')
                             THEN 'cancelled' ELSE state END,
                delivered_at = COALESCE(delivered_at, updated_at)
             WHERE session_id = ?1 OR run_id IN ({RUNS})"
        ),
        [sid],
    )?;
    let steps = tx.execute(
        &format!(
            "UPDATE workflow_step_log SET output = NULL, error = NULL
             WHERE run_id IN ({RUNS})"
        ),
        [sid],
    )?;
    let runs = tx.execute(
        "UPDATE workflow_runs SET params = '{}', step_outputs = '{}', result = NULL,
            error = NULL,
            state = CASE WHEN state IN ('running','paused','blocked')
                         THEN 'cancelled' ELSE state END,
            finished_at = COALESCE(finished_at, updated_at)
         WHERE session_id = ?1",
        [sid],
    )?;
    Ok(SessionWork {
        effects,
        approvals,
        tasks,
        jobs,
        steps,
        runs,
    })
}

/// Efface du disque les fichiers de la session, s'ils vivent sous les médias ou les
/// artefacts : un chemin cité ailleurs n'est jamais suivi.
fn remove_session_files(s: &Services, files: &[String], artifacts_root: &std::path::Path) -> usize {
    let mut removed = 0usize;
    for f in files {
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
        if (under_media || path.starts_with(artifacts_root)) && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Les sessions nées d'un fork de celle-ci : elles lisent leur préfixe dans son journal
/// (fork par référence, §2.6) et le perdent à sa purge.
async fn forks_of(s: &Services, session_id: &str) -> anyhow::Result<Vec<String>> {
    let sid = session_id.to_string();
    Ok(s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT DISTINCT session_id FROM events
                 WHERE kind = 'conv.fork' AND json_extract(payload, '$.parent') = ?1
                 ORDER BY session_id",
            )?;
            let rows = st.query_map([&sid], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}

/// L'avertissement du propriétaire (arbitrage 3, design/v1/README.md §10) : « cette
/// session a deux forks, ils perdront leur début », suivi des sessions nommées.
pub fn forks_warning(forks: &[String]) -> String {
    const WORDS: [&str; 10] = [
        "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix",
    ];
    let n = forks.len();
    let count = WORDS
        .get(n.wrapping_sub(1))
        .map_or_else(|| n.to_string(), |w| (*w).to_string());
    let (noun, tail) = if n == 1 {
        ("fork", "il perdra son début")
    } else {
        ("forks", "ils perdront leur début")
    };
    format!(
        "Cette session a {count} {noun}, {tail} : {}.",
        forks.join(", ")
    )
}

/// Ce que la purge d'une session emporterait chez ses forks, lu sans rien effacer :
/// `penelope session purge` et `/purge` le citent avant de demander confirmation.
pub async fn preview(s: &Services, session_id: &str) -> anyhow::Result<Value> {
    let sess = s.sessions.require(session_id).await?;
    let mut forks = Vec::new();
    let mut named = Vec::new();
    for id in forks_of(s, session_id).await? {
        let title = s
            .sessions
            .get(&id)
            .await?
            .map(|f| crate::helpers::session_label(&f));
        named.push(match &title {
            Some(t) => format!("{id} « {t} »"),
            None => id.clone(),
        });
        forks.push(json!({"id": id, "title": title}));
    }
    let mut out = json!({
        "session": session_id,
        "title": crate::helpers::session_label(&sess),
        "forks": forks,
    });
    if !named.is_empty() {
        out["avertissement"] = json!(forks_warning(&named));
    }
    Ok(out)
}

/// Rétention des tentatives (T17) : le texte partiel d'un `conv.attempt` ne survit pas à
/// la comptabilité qu'il accompagne. Le payload est remplacé, le hash d'origine gardé dans
/// `event_purges`, comme à la purge d'une session : `audit verify` compte ces lignes
/// `purged` et la chaîne tient.
fn purge_attempts(
    tx: &penelope_store::rusqlite::Transaction<'_>,
    cutoff: &str,
    now: &str,
) -> penelope_store::rusqlite::Result<usize> {
    const OLD: &str = "SELECT id FROM events
         WHERE kind = 'conv.attempt' AND ts < ?1 AND payload <> '{\"purged\":true}'";
    tx.execute(
        &format!(
            "INSERT OR IGNORE INTO event_purges(event_id, purged_at, original_hash, reason)
             SELECT id, ?2, hash, 'retention' FROM events WHERE id IN ({OLD})"
        ),
        params![cutoff, now],
    )?;
    tx.execute(
        &format!("UPDATE events SET payload = '{{\"purged\":true}}' WHERE id IN ({OLD})"),
        [cutoff],
    )
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
pub async fn retention(s: &Services) -> anyhow::Result<Value> {
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
    let stamp = s.clock.now_rfc3339();
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
            let mut jobs = 0;
            let mut prompts = 0;
            let mut attempts = 0;
            if let Some(c) = &general {
                attempts = purge_attempts(tx, c, &stamp)?;
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
                for prefix in RETIRED_KEYS {
                    keys += tx.execute("DELETE FROM kv WHERE k LIKE ?1", [format!("{prefix}%")])?;
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
                // #204 : même règle pour les jobs d'outils que pour les tâches MCP — un job
                // encore en cours n'est jamais ramassé.
                jobs = tx.execute(
                    "DELETE FROM tool_jobs
                     WHERE state IN ('completed','failed','cancelled') AND updated_at < ?1",
                    [c],
                )?;
                // #205 : un instantané de prompt suit la ligne d'`usage` qui le cite ; il
                // ne part qu'une fois que plus personne ne le désigne.
                prompts = HistoryStore::retire_prompts_in(tx, c)?;
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
                "tool_jobs": jobs,
                "prompt_snapshots": prompts,
                "workflow_steps": steps,
                "workflow_runs": runs,
                "conv_attempts": attempts,
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
/// Repasse le rédacteur sur les messages déjà en file (issue #148).
///
/// Le 20/09, cinq lignes de `tg_outbox` ont gardé un `Grant` Codex en hexadécimal :
/// `redact` ne reconnaissait pas cette forme, et la rétention les aurait laissées quatre-
/// vingt-dix jours. Les règles ont changé ; les lignes déjà écrites, non. Une passe, une
/// fois, les réécrit avec les règles du jour.
pub async fn reredact_outbox(s: &Services) -> anyhow::Result<usize> {
    const FLAG: &str = "outbox.reredacted.v1";
    if s.kv_get(FLAG).await?.is_some() {
        return Ok(0);
    }
    // La rédaction se fait **hors** transaction, et hors du thread écrivain (issue
    // #153) : elle traversait toute la file dans une seule écriture, et le premier texte
    // qui la faisait boucler emportait le processus au démarrage. Ici, une lecture, un
    // calcul dans la tâche courante, puis des écritures par paquets.
    let rows: Vec<(String, String)> = s
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT id, payload FROM tg_outbox")?;
            let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(r.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let changed: Vec<(String, String)> = rows
        .into_iter()
        .filter_map(|(id, payload)| {
            let red = penelope_observe::redact(&payload);
            (red != payload).then_some((id, red))
        })
        .collect();
    let mut fixed = 0usize;
    // Par paquets : une transaction par centaine de lignes, pour qu'un redémarrage au
    // milieu ne perde que le paquet en cours. La passe est idempotente — rédiger une
    // ligne déjà rédigée ne la change plus.
    for lot in changed.chunks(100) {
        let lot: Vec<(String, String)> = lot.to_vec();
        let n = lot.len();
        match s
            .store
            .write(move |tx| {
                for (id, red) in &lot {
                    tx.execute(
                        "UPDATE tg_outbox SET payload = ?2 WHERE id = ?1",
                        params![id, red],
                    )?;
                }
                Ok(())
            })
            .await
        {
            Ok(()) => fixed += n,
            // Un paquet qui échoue est dit, il n'arrête pas la passe : le drapeau n'est
            // posé qu'à la fin, la prochaine reprendra ce qui reste.
            Err(e) => tracing::error!(erreur = %e, lignes = n, "paquet non réécrit"),
        }
    }
    s.kv_set(FLAG, &fixed.to_string()).await?;
    if fixed > 0 {
        tracing::warn!(
            lignes = fixed,
            "messages déjà en file réécrits par le rédacteur : les considérer comme exposés"
        );
    }
    Ok(fixed)
}

pub async fn retention_tick(s: &Services) -> anyhow::Result<()> {
    let now = s.clock.now_ms();
    let last = s
        .kv_get("retention.last")
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < 24 * 3_600_000 {
        return Ok(());
    }
    s.kv_set("retention.last", &now.to_string()).await?;
    retention(s).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
