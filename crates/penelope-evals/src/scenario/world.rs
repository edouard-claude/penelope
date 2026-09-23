//! Relevé du monde après un scénario : ce que la base, le journal et l'espace de travail
//! contiennent, en lignes JSON (une par objet, `type` en tête), avant normalisation.
//!
//! Ordre des lignes : sessions (création), puis pour chaque session ses messages et ses
//! nœuds de résumé ; événements du journal (ordre d'écriture) ; tours ; effets ; requêtes
//! LLM ; approbations ; artefacts ; usage ; `tg_outbox` ; fichiers du workspace ;
//! vérification de la chaîne d'audit.

use penelope_context::transcript::Entry;
use penelope_daemon::Services;
use penelope_store::rusqlite;
use serde_json::{Value, json};
use std::path::Path;

/// Contenu d'un fichier gardé en clair au-delà duquel seule la tête est relevée.
const FILE_HEAD_CHARS: usize = 2_000;

pub async fn dump(s: &Services, workspace: &Path) -> anyhow::Result<Vec<Value>> {
    let mut lines = Vec::new();

    let sessions = rows(
        s,
        "SELECT id, kind, title, state, parent_id, model_alias, episode_seq
         FROM sessions ORDER BY created_at, rowid",
        |r| {
            Ok(json!({
                "type": "session",
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "title": r.get::<_, Option<String>>(2)?,
                "state": r.get::<_, String>(3)?,
                "parent": r.get::<_, Option<String>>(4)?,
                "model_alias": r.get::<_, Option<String>>(5)?,
                "episode": r.get::<_, i64>(6)?,
            }))
        },
    )
    .await?;
    let ids: Vec<String> = sessions
        .iter()
        .filter_map(|v| v["id"].as_str().map(String::from))
        .collect();
    lines.extend(sessions);

    for sid in &ids {
        for e in s.context.history.load(sid, 0).await? {
            lines.push(message_line(sid, &e));
        }
        lines.extend(summaries(s, sid).await?);
    }

    let mut events: Vec<Value> = s
        .events
        .range(0, i64::MAX)
        .await?
        .into_iter()
        .map(|ev| {
            json!({
                "type": "event",
                "session": ev.session_id,
                "seq": ev.seq,
                "kind": ev.kind,
                "payload": ev.payload,
            })
        })
        .collect();
    canonicalise_parallel_runs(&mut events);
    lines.extend(events);

    lines.extend(
        rows(
            s,
            "SELECT id, session_id, kind, state, attempts, last_error
             FROM turn_queue ORDER BY enqueued_at, rowid",
            |r| {
                Ok(json!({
                    "type": "turn",
                    "id": r.get::<_, String>(0)?,
                    "session": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "state": r.get::<_, String>(3)?,
                    "attempts": r.get::<_, i64>(4)?,
                    "error": r.get::<_, Option<String>>(5)?,
                }))
            },
        )
        .await?,
    );

    // Les lectures parallèles (#85) planifient leurs effets en même temps : l'ordre
    // d'insertion n'est pas reproductible, celui des appels l'est.
    lines.extend(
        rows(
            s,
            "SELECT id, session_id, step_id, kind, tool, state, attempts, idempotent, request
             FROM effects ORDER BY session_id, step_id, tool, rowid",
            |r| {
                let request: String = r.get(8)?;
                Ok(json!({
                    "type": "effect",
                    "id": r.get::<_, String>(0)?,
                    "session": r.get::<_, Option<String>>(1)?,
                    "call": r.get::<_, Option<String>>(2)?,
                    "kind": r.get::<_, String>(3)?,
                    "tool": r.get::<_, Option<String>>(4)?,
                    "state": r.get::<_, String>(5)?,
                    "attempts": r.get::<_, i64>(6)?,
                    "idempotent": r.get::<_, i64>(7)? == 1,
                    "request": parsed(&request),
                }))
            },
        )
        .await?,
    );

    lines.extend(
        rows(
            s,
            "SELECT id, session_id, model, provider, state
             FROM llm_requests ORDER BY created_at, rowid",
            |r| {
                Ok(json!({
                    "type": "llm",
                    "id": r.get::<_, String>(0)?,
                    "session": r.get::<_, Option<String>>(1)?,
                    "model": r.get::<_, String>(2)?,
                    "provider": r.get::<_, String>(3)?,
                    "state": r.get::<_, String>(4)?,
                }))
            },
        )
        .await?,
    );

    lines.extend(
        rows(
            s,
            "SELECT id, kind, subject, risk, state, session_id, decision
             FROM approval_requests ORDER BY created_at, rowid",
            |r| {
                Ok(json!({
                    "type": "approval",
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "subject": r.get::<_, String>(2)?,
                    "risk": r.get::<_, String>(3)?,
                    "state": r.get::<_, String>(4)?,
                    "session": r.get::<_, Option<String>>(5)?,
                    "decision": r.get::<_, Option<String>>(6)?,
                }))
            },
        )
        .await?,
    );

    lines.extend(
        rows(
            s,
            "SELECT id, session_id, kind, bytes FROM artifacts ORDER BY created_at, rowid",
            |r| {
                Ok(json!({
                    "type": "artifact",
                    "id": r.get::<_, String>(0)?,
                    "session": r.get::<_, Option<String>>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "bytes": r.get::<_, i64>(3)?,
                }))
            },
        )
        .await?,
    );

    lines.extend(
        rows(
            s,
            "SELECT session_id, model, provider, role, prompt, completion, cached, reasoning
             FROM usage ORDER BY id",
            |r| {
                Ok(json!({
                    "type": "usage",
                    "session": r.get::<_, Option<String>>(0)?,
                    "model": r.get::<_, String>(1)?,
                    "provider": r.get::<_, String>(2)?,
                    "role": r.get::<_, Option<String>>(3)?,
                    "prompt": r.get::<_, i64>(4)?,
                    "completion": r.get::<_, i64>(5)?,
                    "cached": r.get::<_, i64>(6)?,
                    "reasoning": r.get::<_, i64>(7)?,
                }))
            },
        )
        .await?,
    );

    lines.extend(
        rows(
            s,
            "SELECT chat_id, method, state, payload FROM tg_outbox ORDER BY created_at, rowid",
            |r| {
                let payload: String = r.get(3)?;
                Ok(json!({
                    "type": "outbox",
                    "chat_id": r.get::<_, i64>(0)?,
                    "method": r.get::<_, String>(1)?,
                    "state": r.get::<_, String>(2)?,
                    "payload": parsed(&payload),
                }))
            },
        )
        .await?,
    );

    lines.extend(files(workspace, workspace)?);

    let audit = s.events.verify().await?;
    lines.push(json!({
        "type": "audit",
        "ok": audit.ok,
        "checked": audit.checked,
        "purged": audit.purged,
    }));
    Ok(lines)
}

/// Les lectures pures d'un même lot partent ensemble (#85) : leurs événements
/// `runtime.tool` s'écrivent dans l'ordre d'achèvement, qui n'est pas reproductible. Un
/// lot consécutif est relu dans l'ordre (outil, arguments), les numéros de séquence
/// restant à leur place : le monde dit ce qui a été fait, pas qui a fini le premier.
fn canonicalise_parallel_runs(events: &mut [Value]) {
    let mut i = 0;
    while i < events.len() {
        if events[i]["kind"] != "runtime.tool" {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < events.len()
            && events[j]["kind"] == "runtime.tool"
            && events[j]["session"] == events[i]["session"]
        {
            j += 1;
        }
        if j - i > 1 {
            let seqs: Vec<Value> = events[i..j].iter().map(|e| e["seq"].clone()).collect();
            let mut payloads: Vec<Value> =
                events[i..j].iter().map(|e| e["payload"].clone()).collect();
            payloads.sort_by_key(|p| {
                serde_json::to_string(&json!([p["tool"], p["args"]])).unwrap_or_default()
            });
            for (k, (seq, payload)) in seqs.into_iter().zip(payloads).enumerate() {
                events[i + k]["seq"] = seq;
                events[i + k]["payload"] = payload;
            }
        }
        i = j;
    }
}

/// Tous les nœuds de résumé d'une session, par création : un nœud prolongé apparaît
/// deux fois, l'ancien marqué `superseded_by`.
async fn summaries(s: &Services, sid: &str) -> anyhow::Result<Vec<Value>> {
    let session = sid.to_string();
    Ok(s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT id, from_seq, to_seq, summary, superseded_by
                 FROM lcm_nodes WHERE session_id = ?1 ORDER BY created_at, rowid",
            )?;
            let rows = st.query_map([&session], |r| {
                Ok(json!({
                    "type": "summary",
                    "session": session,
                    "node": r.get::<_, String>(0)?,
                    "from": r.get::<_, Option<i64>>(1)?,
                    "to": r.get::<_, Option<i64>>(2)?,
                    "superseded_by": r.get::<_, Option<String>>(4)?,
                    "summary": r.get::<_, String>(3)?,
                }))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await?)
}

fn message_line(sid: &str, e: &Entry) -> Value {
    let m = &e.message;
    let mut line = json!({
        "type": "message",
        "session": sid,
        "seq": e.seq,
        "role": m.role.as_str(),
        "text": m.text(),
    });
    let images = m.content.iter().filter(|c| c.as_text().is_none()).count();
    if images > 0 {
        line["images"] = json!(images);
    }
    if !m.tool_calls.is_empty() {
        line["tool_calls"] = m
            .tool_calls
            .iter()
            .map(|c| json!({"id": c.id, "name": c.name, "arguments": c.arguments}))
            .collect();
    }
    if let Some(id) = &m.tool_call_id {
        line["tool_call_id"] = json!(id);
    }
    if let Some(name) = &m.name {
        line["name"] = json!(name);
    }
    if e.compacted {
        line["compacted"] = json!(true);
    }
    if e.eager {
        line["eager"] = json!(true);
    }
    if let Some(a) = &e.artifact_id {
        line["artifact"] = json!(a);
    }
    if e.episode != 0 {
        line["episode"] = json!(e.episode);
    }
    line
}

/// Lignes de fichiers, par chemin trié, sous `root`.
fn files(root: &Path, dir: &Path) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(out);
    };
    let mut entries: Vec<std::path::PathBuf> =
        rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            out.extend(files(root, &path)?);
            continue;
        }
        let bytes = std::fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        let head: String = text.chars().take(FILE_HEAD_CHARS).collect();
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        let mut line = json!({"type": "file", "path": rel, "bytes": bytes.len()});
        if head.chars().count() < text.chars().count() {
            line["head"] = json!(head);
        } else {
            line["content"] = json!(head);
        }
        out.push(line);
    }
    Ok(out)
}

fn parsed(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

async fn rows<F>(s: &Services, sql: &'static str, map: F) -> anyhow::Result<Vec<Value>>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value> + Send + 'static,
{
    Ok(s.store
        .read(move |c| {
            let mut st = c.prepare(sql)?;
            let rows = st.query_map([], |r| map(r))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })
        .await?)
}
