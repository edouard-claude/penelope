//! Une base de test pour `verify` et le projecteur : un moteur journalisé et des sessions
//! qui passent par tous les chemins d'écriture (message, appel et résultat d'outil,
//! contexte figé, niveau 1, résumé puis prolongation, préfixe V0 scellé, retour arrière
//! avec archive).

use crate::compaction::CompactionParams;
use crate::engine::ContextEngine;
use crate::lcm::Lcm;
use crate::store::HistoryStore;
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_llm::catalog::Catalog;
use penelope_llm::tokens::TokenEstimator;
use penelope_llm::types::{ChatMessage, ToolCall};
use penelope_store::Store;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) struct World {
    pub engine: ContextEngine,
    pub log: EventLog,
    pub store: Store,
}

impl World {
    pub fn history(&self) -> &HistoryStore {
        &self.engine.history
    }

    /// Exécute une requête d'écriture brute (altération, effacement).
    pub async fn sql(&self, sql: &'static str) {
        self.store
            .write(move |tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Une valeur entière lue en base.
    pub async fn int(&self, sql: &'static str) -> i64 {
        self.store
            .read(move |c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .await
            .unwrap()
    }
}

pub(crate) async fn world() -> World {
    let store = Store::open_memory().unwrap();
    store
        .write(|tx| {
            for id in ["s1", "s2", "s3"] {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES(?1, 'chat', 't', 't')",
                    [id],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let log = EventLog::new(store.clone(), clock.clone());
    let engine = ContextEngine::new(
        HistoryStore::new(store.clone(), clock.clone(), log.clone()),
        Lcm::new(store.clone(), clock.clone()),
        TokenEstimator::new(),
        Catalog::new(),
        clock.clone(),
    );
    World { engine, log, store }
}

pub(crate) fn params() -> CompactionParams {
    CompactionParams {
        window: 40_000,
        threshold: 0.70,
        tail_ratio: 0.025,
        tail_min_tokens: 1_000,
        tail_max_tokens: 25_000,
        min_tail_user_messages: 2,
        max_tool_result_share: 0.25,
        large_payload_tokens: 25_000,
        background_margin: 0.10,
        max_prompt_tokens: 0,
    }
}

/// Des échanges entrecoupés d'événements d'observation, un contexte figé sur chaque
/// message utilisateur.
pub(crate) async fn exchanges(w: &World, sid: &str, range: std::ops::Range<usize>) {
    for i in range {
        w.log
            .append(EventDraft::new("turn.started", json!({})).session(sid))
            .await
            .unwrap();
        let h = w.history();
        let seq = h
            .append(
                sid,
                &ChatMessage::user(format!("demande {i}")),
                500,
                0,
                false,
                None,
            )
            .await
            .unwrap();
        h.freeze_context(sid, seq, &format!("<contexte>{i}</contexte>\n"))
            .await
            .unwrap();
        h.append(
            sid,
            &ChatMessage::assistant(format!("réponse {i}")),
            20,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    }
}

/// Un appel d'outil et son résultat, externalisé (niveau 1).
pub(crate) async fn tool_round(w: &World, sid: &str) {
    let h = w.history();
    let call = ToolCall {
        id: "c1".into(),
        name: "fs_read".into(),
        // Clés dans l'ordre du fournisseur : le journal les range.
        arguments: json!({"path": "a.rs", "lines": 10}),
    };
    h.append(
        sid,
        &ChatMessage::assistant("").with_tool_calls(vec![call]),
        10,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    let body = "ligne\n".repeat(400);
    let seq = h
        .append(
            sid,
            &ChatMessage::tool_result("c1", "fs_read", &body),
            800,
            0,
            true,
            None,
        )
        .await
        .unwrap();
    let artifact = h
        .put_artifact(Some(sid), None, "text", None, &body)
        .await
        .unwrap();
    h.externalise_as(sid, seq, "[externalisé]", &artifact, 5, 800)
        .await
        .unwrap();
}

/// Un résumé publié (ou sa prolongation).
pub(crate) async fn compact(w: &World, sid: &str) -> String {
    let e = &w.engine;
    let job = e
        .prepare_summary(sid, &params(), 128_000, "m", false)
        .await
        .unwrap()
        .expect("un travail de résumé");
    e.apply_summary(&job, &json!({"objectif": "suivre"}), "m")
        .await
        .unwrap()
}

/// `s1` : tous les chemins d'écriture du journal.
pub(crate) async fn rich(w: &World) {
    exchanges(w, "s1", 0..20).await;
    tool_round(w, "s1").await;
    exchanges(w, "s1", 20..30).await;
    compact(w, "s1").await;
    exchanges(w, "s1", 30..60).await;
    compact(w, "s1").await;
    exchanges(w, "s1", 60..62).await;
}

/// `s3` : un historique V0 (écrit sans journal) scellé, puis deux échanges journalisés.
pub(crate) async fn sealed(w: &World) {
    let bare = w.history();
    for i in 0..3 {
        let seq = bare
            .append_legacy("s3", &ChatMessage::user(format!("ancien {i}")), 3, 0)
            .await
            .unwrap();
        bare.freeze_legacy("s3", seq, "<ancien/>").await.unwrap();
        bare.append_legacy("s3", &ChatMessage::assistant(format!("vieux {i}")), 3, 0)
            .await
            .unwrap();
    }
    let report = w.history().seal_legacy().await.unwrap();
    assert_eq!(report.sealed, vec![("s3".to_string(), 6)]);
    exchanges(w, "s3", 0..2).await;
}

/// `s1` perd son dernier échange par un retour arrière, archivé dans `s2`.
pub(crate) async fn rewound(w: &World) {
    let h = w.history();
    let cutoff = h
        .load("s1", 0)
        .await
        .unwrap()
        .iter()
        .rev()
        .find(|e| e.message.role == penelope_llm::types::Role::User)
        .unwrap()
        .seq;
    h.rewind_from("s1", cutoff, 1, Some("s2")).await.unwrap();
}

/// `s3` en V0 : huit messages, un contexte figé par demande, un résumé actif sur 1..4 qui
/// porte ses ancres et un `tokens_src` distinct de `tokens_self` (ce que la 0.17 écrit),
/// puis le scellement.
pub(crate) async fn sealed_with_summary(w: &World) {
    use crate::anchors::{Anchor, AnchorKind};
    let bare = w.history();
    for i in 0..4 {
        let seq = bare
            .append_legacy("s3", &ChatMessage::user(format!("ancien {i}")), 300, 0)
            .await
            .unwrap();
        bare.freeze_legacy("s3", seq, &format!("<ancien>{i}</ancien>"))
            .await
            .unwrap();
        bare.append_legacy("s3", &ChatMessage::assistant(format!("vieux {i}")), 40, 0)
            .await
            .unwrap();
    }
    let anchors = [
        Anchor {
            kind: AnchorKind::Path,
            value: "src/main.rs".into(),
        },
        Anchor {
            kind: AnchorKind::Ticket,
            value: "#147".into(),
        },
    ];
    w.engine
        .lcm
        .insert_leaf("s3", 1, 4, "résumé ancien", &anchors, 680, 25)
        .await
        .unwrap();
    bare.mark_compacted("s3", 1, 4).await.unwrap();
    let report = w.history().seal_legacy().await.unwrap();
    assert_eq!(report.sealed, vec![("s3".to_string(), 8)]);
}

/// Ce qu'une refonte doit redonner : lignes (contenu relu comme du JSON, le journal range
/// les clés des arguments d'appel), contextes, nœuds LCM hors identifiants tirés au hasard.
pub(crate) async fn caches(w: &World) -> Vec<String> {
    w.store
        .read(|c| {
            let mut out = Vec::new();
            let mut st = c.prepare(
                "SELECT session_id, seq, role, content, tool_call_id, tool_name, tokens_est,
                        episode, eager, artifact_id, compacted, event_id, sealed, source_turn_id
                 FROM messages ORDER BY session_id, seq",
            )?;
            let rows = st.query_map([], |r| {
                let content: String = r.get(3)?;
                let content: Value = serde_json::from_str(&content).unwrap_or(Value::Null);
                Ok(format!(
                    "{:?} {:?} {:?} {} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    penelope_kernel::canonical::canonical_json(&content),
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, i64>(10)?,
                    r.get::<_, Option<i64>>(11)?,
                    r.get::<_, i64>(12)?,
                    r.get::<_, Option<String>>(13)?,
                ))
            })?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
            let mut st = c.prepare(
                "SELECT session_id, seq, context FROM message_context ORDER BY session_id, seq",
            )?;
            let rows = st.query_map([], |r| {
                Ok(format!(
                    "ctx {:?} {:?} {:?}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?
                ))
            })?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
            let mut st = c.prepare(
                "SELECT session_id, from_seq, to_seq, summary, anchors, tokens_src, tokens_self,
                        superseded_by IS NULL, event_id, id
                 FROM lcm_nodes ORDER BY session_id, from_seq, to_seq, summary",
            )?;
            let rows = st.query_map([], |r| {
                Ok(format!(
                    "node {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<i64>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, bool>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, String>(9)?,
                ))
            })?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
            let mut st = c.prepare(
                "SELECT f.session_id, m.seq, f.content FROM messages_fts f
                 JOIN messages m ON m.id = f.msg_id ORDER BY f.session_id, m.seq",
            )?;
            let rows = st.query_map([], |r| {
                Ok(format!(
                    "fts {:?} {:?} {:?}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?
                ))
            })?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
            Ok(out)
        })
        .await
        .unwrap()
}
