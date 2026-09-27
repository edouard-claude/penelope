//! Opérations sur les sessions et le stockage (§4.4, §15) : fork, retour en arrière,
//! export, reconstruction des index dérivés.

use crate::bus::Bus;
use crate::ports::ProviderSource;
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use penelope_kernel::session::SessionKind;
use penelope_llm::types::Role;
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

/// Duplique une session : transcript, métadonnées et résumés actifs. La nouvelle session
/// part du même point et diverge ensuite.
pub async fn fork(s: &Services, session_id: &str, title: Option<String>) -> anyhow::Result<Value> {
    let source = s
        .sessions
        .get(session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session {session_id} introuvable"))?;
    let title = title.or_else(|| {
        Some(format!(
            "Fork de {}",
            source
                .title
                .clone()
                .unwrap_or_else(|| session_id.to_string())
        ))
    });
    let fork = s
        .sessions
        .create_with(
            source.kind,
            title,
            Some(session_id.to_string()),
            source.workspace.clone(),
        )
        .await?;
    let fork_id = fork.id.as_str().to_string();
    // Le fork par référence entre au journal ; les messages et les résumés actifs de la
    // fille en sont projetés, sous les mêmes numéros (T10, T16).
    let copied = s.context.history.fork(&fork_id, session_id).await?;
    if let Some(obj) = source.metadata.as_object() {
        // Le fichier de notes n'est pas partagé : le fork reçoit sa propre copie.
        for (k, v) in obj
            .iter()
            .filter(|(k, _)| k.as_str() != crate::session_notes::META_KEY)
        {
            s.sessions
                .metadata(
                    &fork_id,
                    penelope_kernel::session::MetadataOp::Set,
                    k,
                    v.clone(),
                )
                .await?;
        }
    }
    crate::session_notes::copy(s, session_id, &fork_id).await?;
    if let Some(budget) = source.budget_usd {
        s.sessions.set_budget(&fork_id, Some(budget)).await?;
    }
    s.events
        .append(
            EventDraft::new(
                "session.forked",
                json!({"from": session_id, "messages": copied}),
            )
            .session(&fork_id),
        )
        .await?;
    Ok(json!({"session": fork_id, "from": session_id, "messages": copied}))
}

/// Revient `turns` échanges en arrière : les messages retirés ne sont pas perdus, ils
/// partent dans une session d'archive (fermée, rattachée à la session d'origine).
/// Arrête ce que fait une session sans la fermer : tour en cours interrompu, file vidée.
/// Sert quand une session perd son chat (`/fork`, `/switch`) ou se ferme (issue #10).
pub async fn silence(
    s: &Services,
    bus: &Bus,
    session_id: &str,
    reason: &str,
) -> anyhow::Result<usize> {
    bus.cancel_session(session_id);
    let cancelled = s.turns.cancel_pending(session_id, reason).await?;
    if cancelled > 0 {
        tracing::info!(session = %session_id, cancelled, %reason, "tours annulés");
    }
    Ok(cancelled)
}

/// Ferme une session : tour en cours arrêté, file vidée, chat détaché, épisode relu.
pub async fn close(
    s: &Arc<Services>,
    providers: Arc<dyn ProviderSource>,
    bus: &Bus,
    session_id: &str,
) -> anyhow::Result<Value> {
    let sess = s.sessions.require(session_id).await?;
    let cancelled = silence(s, bus, session_id, "session fermée").await?;
    s.sessions.set_state(session_id, "closed").await?;
    s.sessions.unbind_telegram(session_id).await?;
    crate::episodes::spawn_ingest(
        s.clone(),
        providers,
        session_id.to_string(),
        sess.episode_seq,
        crate::episodes::Boundary::NewSession,
    );
    let _ = s
        .events
        .append(
            EventDraft::new("session.closed", json!({"cancelled": cancelled})).session(session_id),
        )
        .await;
    Ok(json!({
        "session": session_id,
        "title": sess.title,
        "cancelled": cancelled,
    }))
}

/// Retrouve une session par identifiant, préfixe unique d'identifiant ou titre exact.
pub async fn resolve(
    s: &Services,
    query: &str,
) -> Result<penelope_kernel::session::Session, String> {
    let q = query.trim();
    if q.is_empty() {
        return Err("session non précisée".into());
    }
    if let Ok(Some(sess)) = s.sessions.get(q).await {
        return Ok(sess);
    }
    let all = s
        .sessions
        .list(Some(SessionKind::Chat), 500)
        .await
        .map_err(|e| e.to_string())?;
    let by_prefix: Vec<_> = all
        .iter()
        .filter(|x| x.id.as_str().starts_with(q))
        .collect();
    let by_title: Vec<_> = all
        .iter()
        .filter(|x| {
            x.title
                .as_deref()
                .is_some_and(|t| t.trim().eq_ignore_ascii_case(q))
        })
        .collect();
    match (by_prefix.as_slice(), by_title.as_slice()) {
        ([one], _) | ([], [one]) => Ok((*one).clone()),
        ([], []) => Err(format!("aucune session ne correspond à « {q} »")),
        (many, _) if many.len() > 1 => Err(format!(
            "« {q} » désigne {} sessions : préciser l'identifiant",
            many.len()
        )),
        (_, many) => Err(format!(
            "{} sessions s'appellent « {q} » : préciser l'identifiant",
            many.len()
        )),
    }
}

pub async fn rewind(
    s: &Services,
    bus: &Bus,
    session_id: &str,
    turns: usize,
) -> anyhow::Result<Value> {
    if turns == 0 {
        anyhow::bail!("nombre de tours à défaire : au moins 1");
    }
    if bus.is_active(session_id) {
        anyhow::bail!("un tour est en cours dans cette session : `/stop` d'abord");
    }
    let entries = s.context.history.load(session_id, 0).await?;
    let users: Vec<i64> = entries
        .iter()
        .filter(|e| e.message.role == Role::User)
        .map(|e| e.seq)
        .collect();
    if users.is_empty() {
        anyhow::bail!("rien à défaire : la session est vide");
    }
    let cutoff = users[users.len().saturating_sub(turns)];
    let covered = s
        .context
        .lcm
        .active_nodes(session_id)
        .await?
        .iter()
        .filter_map(|n| n.to_seq)
        .max()
        .unwrap_or(0);
    if cutoff <= covered {
        anyhow::bail!(
            "impossible de revenir avant le dernier résumé (message #{covered}) : `/fork` \
             garde une copie si besoin"
        );
    }
    let archive = s
        .sessions
        .create_with(
            SessionKind::Chat,
            Some(format!("Rewind de {session_id}")),
            Some(session_id.to_string()),
            None,
        )
        .await?;
    let archive_id = archive.id.as_str().to_string();
    s.sessions.set_state(&archive_id, "closed").await?;
    // `conv.rewind` d'abord ; l'archive projetée depuis le journal, puis la coupe, dans la
    // même écriture (T10, T16).
    let done = s
        .context
        .history
        .rewind_from(session_id, cutoff, turns as u64, Some(&archive_id))
        .await?;
    let (removed, archived) = (done.removed, done.archived);
    // L'ancre d'usage décrivait un transcript qui n'existe plus.
    s.sessions
        .set_usage_anchor(session_id, &Value::Null)
        .await?;
    s.events
        .append(
            EventDraft::new(
                "session.rewound",
                json!({"turns": turns, "from_seq": cutoff, "removed": removed, "archive": archive_id}),
            )
            .session(session_id),
        )
        .await?;
    Ok(json!({
        "session": session_id,
        "turns": turns,
        "removed": removed,
        "archived": archived,
        "archive": archive_id,
    }))
}

/// Répertoire des exports.
fn exports_dir(s: &Services) -> PathBuf {
    s.platform.dirs.data().join("exports")
}

/// Exporte en JSONL : `session <id>` (messages et événements), `run <id>` (trajectoire
/// du run : état, étapes, messages, événements), `all` (toutes les sessions).
pub async fn export(s: &Services, what: &str, id: Option<&str>) -> anyhow::Result<Value> {
    let stamp = s.clock.now_rfc3339().replace([':', '.'], "-");
    let dir = exports_dir(s);
    std::fs::create_dir_all(&dir)?;
    let (path, lines) = match what {
        "session" => {
            let query = id.ok_or_else(|| anyhow::anyhow!("identifiant de session attendu"))?;
            // Session inconnue : on le dit, au lieu d'écrire un fichier vide (issue #72).
            // Un préfixe ou un titre est accepté, comme pour `/switch` et `/close`.
            let sess = resolve(s, query).await.map_err(anyhow::Error::msg)?;
            let id = sess.id.to_string();
            let lines = session_lines(s, &id).await?;
            (dir.join(format!("session-{id}-{stamp}.jsonl")), lines)
        }
        "run" => {
            let id = id.ok_or_else(|| anyhow::anyhow!("identifiant de run attendu"))?;
            let run = s
                .runs
                .get(id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("run {id} introuvable"))?;
            let mut lines = vec![json!({"type": "run", "run": run})];
            for step in s.runs.trace(id).await? {
                lines.push(json!({"type": "step", "step": step}));
            }
            lines.extend(session_lines(s, &run.session_id).await?);
            (dir.join(format!("run-{id}-{stamp}.jsonl")), lines)
        }
        "all" => {
            let mut lines = Vec::new();
            for sess in s.sessions.list(None, 10_000).await? {
                lines.push(json!({"type": "session", "session": sess}));
                lines.extend(session_lines(s, sess.id.as_str()).await?);
            }
            (dir.join(format!("all-{stamp}.jsonl")), lines)
        }
        other => anyhow::bail!("export inconnu `{other}` : session, run ou all"),
    };
    let mut file = std::fs::File::create(&path)?;
    for l in &lines {
        // Les secrets connus sont masqués, comme dans les journaux (§13.1).
        writeln!(file, "{}", penelope_observe::redact_json(l))?;
    }
    Ok(json!({"path": path, "lines": lines.len()}))
}

/// Les lignes d'une session : ses messages (le préfixe scellé marqué `sealed`), son
/// journal, et la surface qu'en dérive le pliage, ce que le modèle voit (T15).
async fn session_lines(s: &Services, session_id: &str) -> anyhow::Result<Vec<Value>> {
    let mut lines = Vec::new();
    let sid = session_id.to_string();
    let sealed: std::collections::HashSet<i64> = s
        .store
        .read(move |c| {
            let mut st =
                c.prepare("SELECT seq FROM messages WHERE session_id = ?1 AND sealed = 1")?;
            let rows = st.query_map([&sid], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await?;
    for e in s.context.history.load(session_id, 0).await? {
        lines.push(json!({
            "type": "message",
            "session": session_id,
            "seq": e.seq,
            "compacted": e.compacted,
            "sealed": sealed.contains(&e.seq),
            "message": e.message,
        }));
    }
    for ev in s.events.session_events(session_id, 0).await? {
        lines.push(json!({
            "type": "event",
            "session": session_id,
            "seq": ev.seq,
            "ts": ev.ts,
            "kind": ev.kind,
            "payload": ev.payload,
        }));
    }
    // Un journal qui ne se plie pas n'empêche pas l'export : `history verify` le nomme.
    if let Ok(Some(read)) = s.context.history.read_journal(session_id).await? {
        for e in read.projected {
            lines.push(json!({
                "type": "surface",
                "session": session_id,
                "seq": e.seq,
                "message": e.message,
            }));
        }
    }
    Ok(lines)
}

/// Reconstruit ce qui se reconstruit (§4.4) : index plein texte des messages, index de la
/// mémoire depuis le vault ; vérifie la chaîne d'audit au passage.
pub async fn rebuild(s: &Services) -> anyhow::Result<Value> {
    let messages = s.context.history.rebuild_fts().await?;
    let vault = crate::helpers::vault_dir(s);
    let memory = crate::vault_ops::reindex(s, &vault)
        .await
        .map_err(anyhow::Error::msg)?;
    let audit = s.events.verify().await?;
    s.events
        .append(EventDraft::new(
            "store.rebuilt",
            json!({"messages_fts": messages, "memory_entries": memory, "audit_ok": audit.ok}),
        ))
        .await?;
    Ok(json!({
        "messages_fts": messages,
        "memory_entries": memory,
        "audit": audit,
    }))
}

#[cfg(test)]
mod more_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::types::ChatMessage;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        (dir, s)
    }

    /// Une session de conversation, comme celle qu'ouvre la CLI.
    async fn chat(s: &Services) -> String {
        let sess = s
            .sessions
            .create(SessionKind::Chat, Some("CLI".into()))
            .await
            .unwrap();
        sess.id.to_string()
    }

    async fn say(s: &Services, sid: &str, user: &str, answer: &str) {
        let h = &s.context.history;
        h.append(sid, &ChatMessage::user(user), 5, 0, false, None)
            .await
            .unwrap();
        h.append(sid, &ChatMessage::assistant(answer), 5, 0, false, None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn fork_copies_then_diverges() {
        let (_dir, s) = services().await;
        let sid = chat(&s).await;
        say(&s, &sid, "on parle du devis ACME", "d'accord").await;
        let v = fork(&s, &sid, None).await.unwrap();
        let fork_id = v["session"].as_str().unwrap().to_string();
        assert_eq!(v["messages"], 2);
        say(&s, &fork_id, "variante sans remise", "noté").await;
        let h = &s.context.history;
        assert_eq!(
            h.load(&sid, 0).await.unwrap().len(),
            2,
            "l'original ne bouge pas"
        );
        assert_eq!(h.load(&fork_id, 0).await.unwrap().len(), 4);
        let hits = h.grep("ACME", Some(&fork_id), 10).await.unwrap();
        assert!(!hits.is_empty(), "la recherche plein texte suit le fork");
    }

    #[tokio::test]
    async fn rewind_archives_what_it_removes() {
        let (_dir, s) = services().await;
        let sid = chat(&s).await;
        say(&s, &sid, "un", "1").await;
        say(&s, &sid, "deux", "2").await;
        say(&s, &sid, "trois", "3").await;
        let v = rewind(&s, &Bus::new(), &sid, 2).await.unwrap();
        assert_eq!(v["removed"], 4);
        let h = &s.context.history;
        let left: Vec<String> = h
            .load(&sid, 0)
            .await
            .unwrap()
            .iter()
            .map(|e| e.message.text())
            .collect();
        assert_eq!(left, vec!["un", "1"]);
        let archive = v["archive"].as_str().unwrap();
        assert_eq!(h.load(archive, 0).await.unwrap().len(), 4);
        // Le numéro suivant repart juste après ce qui reste.
        say(&s, &sid, "reprise", "ok").await;
        assert_eq!(h.load(&sid, 0).await.unwrap()[2].seq, 3);
        assert!(rewind(&s, &Bus::new(), &sid, 0).await.is_err());
    }

    /// T10 : le fork et le retour arrière entrent au journal, avec des bornes que le
    /// pliage accepte (elles désignent des nœuds présents).
    #[tokio::test]
    async fn fork_and_rewind_are_journaled_with_bounds_that_derive_accepts() {
        use penelope_context::derive::{Sealed, derive};
        let (_dir, s) = services().await;
        let sid = chat(&s).await;
        say(&s, &sid, "un", "1").await;
        say(&s, &sid, "deux", "2").await;
        let fork_id = fork(&s, &sid, None).await.unwrap()["session"]
            .as_str()
            .unwrap()
            .to_string();
        say(&s, &fork_id, "trois", "3").await;
        say(&s, &fork_id, "quatre", "4").await;
        rewind(&s, &Bus::new(), &fork_id, 1).await.unwrap();
        say(&s, &fork_id, "cinq", "5").await;

        let parent = s.events.session_events(&sid, 0).await.unwrap();
        let child = s.events.session_events(&fork_id, 0).await.unwrap();
        let conv = |kind: &str| {
            child
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| e.payload.clone())
                .collect::<Vec<_>>()
        };
        let forks = conv("conv.fork");
        assert_eq!(forks.len(), 1);
        let first_conv = child.iter().find(|e| e.kind.starts_with("conv.")).unwrap();
        assert_eq!(first_conv.kind, "conv.fork", "premier conv.* de la fille");
        let up_to = forks[0]["up_to"].as_i64().unwrap();
        assert_eq!(forks[0]["offset"].as_i64(), Some(up_to));
        let rewinds = conv("conv.rewind");
        assert_eq!(rewinds.len(), 1);
        assert_eq!(rewinds[0]["turns"], 1);

        let prefix = Sealed::fork(&sid, &Sealed::none(), &parent, up_to).unwrap();
        let surface = derive(&prefix, &child).expect("bornes valides");
        let texts: Vec<String> = surface
            .nodes
            .iter()
            .map(|slot| match slot {
                penelope_context::derive::Slot::Message(a) => surface.messages[a].message.text(),
                penelope_context::derive::Slot::Summary(_) => String::new(),
            })
            .collect();
        let rows: Vec<String> = s
            .context
            .history
            .load(&fork_id, 0)
            .await
            .unwrap()
            .iter()
            .map(|e| e.message.text())
            .collect();
        assert_eq!(
            texts, rows,
            "la surface dérivée redonne le canonique de la fille"
        );
    }

    #[tokio::test]
    async fn export_writes_jsonl_and_rebuild_restores_search() {
        let (_dir, s) = services().await;
        let sid = chat(&s).await;
        say(&s, &sid, "le mot secret est framboise", "compris").await;
        let v = export(&s, "session", Some(&sid)).await.unwrap();
        let raw = std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap();
        assert!(raw.lines().count() >= 2);
        let lines: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // T15 : le journal et la surface qu'il dérive accompagnent les messages.
        let of = |t: &str| lines.iter().filter(|l| l["type"] == t).count();
        assert_eq!(of("message"), 2);
        assert_eq!(of("surface"), 2, "{raw}");
        assert!(
            lines
                .iter()
                .any(|l| l["type"] == "event" && l["kind"] == "conv.user")
        );
        assert!(
            lines
                .iter()
                .all(|l| l["type"] != "message" || l["sealed"] == false)
        );
        assert!(export(&s, "inconnu", None).await.is_err());

        s.store
            .write(|tx| {
                tx.execute("DELETE FROM messages_fts", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            s.context
                .history
                .grep("framboise", Some(&sid), 5)
                .await
                .unwrap()
                .is_empty()
        );
        let r = rebuild(&s).await.unwrap();
        assert_eq!(r["messages_fts"], 2);
        assert_eq!(r["audit"]["ok"], true);
        assert!(
            !s.context
                .history
                .grep("framboise", Some(&sid), 5)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
