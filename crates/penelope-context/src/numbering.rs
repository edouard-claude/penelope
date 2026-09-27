//! Numérotation à trous (épopée #208, tâche T21).
//!
//! Dans le journal, l'adresse d'un nœud est `offset + events.seq` : les événements
//! d'observation consomment des `seq` sans produire de message, et les adresses ont
//! des trous (`1, 2, 5, 9`). Toute arithmétique `to - from + 1` ou `seq + 1` sur des
//! adresses devient fausse ; ces fonctions comptent les nœuds au lieu de les déduire
//! des bornes (`design/v1/source-de-verite.md` §7, risque 4). `SummaryJob` porte de même
//! le nombre d'entrées de son lot et la fin du résumé qu'il prolonge.

use crate::lcm::Node;
use crate::transcript::Entry;

/// Plages d'adresses de `addresses` (triées) qu'aucun nœud ne couvre, chacune donnée par
/// sa première et sa dernière adresse existante. Un trou de numérotation n'est pas une
/// lacune de couverture, contrairement à [`crate::Lcm::coverage_gaps`], qui suppose des
/// adresses contiguës.
pub fn uncovered(nodes: &[Node], addresses: &[i64]) -> Vec<(i64, i64)> {
    let covered = |a: i64| {
        nodes.iter().any(|n| match (n.from_seq, n.to_seq) {
            (Some(f), Some(t)) => f <= a && a <= t,
            _ => false,
        })
    };
    let mut gaps: Vec<(i64, i64)> = Vec::new();
    let mut open = false;
    for &a in addresses {
        if covered(a) {
            open = false;
        } else if open {
            if let Some(g) = gaps.last_mut() {
                g.1 = a;
            }
        } else {
            gaps.push((a, a));
            open = true;
        }
    }
    gaps
}

/// Page `page` (de `per_page` nœuds) des entrées couvertes par `[from, to]` :
/// `history_expand` pagine par nœud, jamais par intervalle d'adresses.
pub fn node_page(entries: &[Entry], from: i64, to: i64, page: usize, per_page: usize) -> &[Entry] {
    let start = entries.partition_point(|e| e.seq < from);
    let end = entries.partition_point(|e| e.seq <= to);
    let covered = &entries[start..end.max(start)];
    let first = page.saturating_mul(per_page).min(covered.len());
    let last = first.saturating_add(per_page).min(covered.len());
    &covered[first..last]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::CompactionParams;
    use crate::engine::{ContextEngine, SummaryJob, render_entry};
    use crate::lcm::{Lcm, NodeKind};
    use crate::store::HistoryStore;
    use penelope_kernel::clock::{SharedClock, TestClock};
    use penelope_llm::catalog::Catalog;
    use penelope_llm::tokens::TokenEstimator;
    use penelope_llm::types::ChatMessage;
    use penelope_store::Store;
    use std::sync::Arc;

    fn holes() -> Vec<Entry> {
        vec![
            Entry::new(1, ChatMessage::user("un"), 1),
            Entry::new(2, ChatMessage::assistant("deux"), 1),
            Entry::new(5, ChatMessage::user("cinq"), 1),
            Entry::new(9, ChatMessage::assistant("neuf"), 1),
        ]
    }

    async fn engine() -> ContextEngine {
        let store = Store::open_memory().unwrap();
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id, kind, created_at, updated_at)
                     VALUES('s1','chat','t','t')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let clock: SharedClock = Arc::new(TestClock::default());
        ContextEngine::new(
            HistoryStore::new(
                store.clone(),
                clock.clone(),
                penelope_kernel::event::EventLog::new(store.clone(), clock.clone()),
            ),
            Lcm::new(store, clock.clone()),
            TokenEstimator::new(),
            Catalog::new(),
            clock,
        )
    }

    fn params() -> CompactionParams {
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

    /// Un historique aux adresses trouées (un retour arrière V0, ou le journal) : le lot
    /// compte ses entrées, pas l'écart de ses bornes, et le résumé prolongé est borné par
    /// sa vraie dernière adresse.
    #[tokio::test]
    async fn a_summary_job_counts_its_entries_not_its_bounds() {
        let e = engine().await;
        for i in 0..24 {
            let m = if i % 2 == 0 {
                ChatMessage::user(format!("question {i}"))
            } else {
                ChatMessage::assistant("réponse")
            };
            e.history
                .append("s1", &m, 500, 0, false, None)
                .await
                .unwrap();
        }
        // Trous : 3, 4, 7, 8 disparaissent.
        e.history
            .store()
            .write(|tx| {
                tx.execute("DELETE FROM messages WHERE seq IN (3, 4, 7, 8)", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let job = e
            .prepare_summary("s1", &params(), 128_000, "m", true)
            .await
            .unwrap()
            .expect("un travail de résumé");
        let entries = e.history.load("s1", 0).await.unwrap();
        let in_chunk = entries
            .iter()
            .filter(|x| x.seq >= job.chunk_from_seq && x.seq <= job.to_seq)
            .count() as i64;
        assert!(job.to_seq > 8, "le lot enjambe les trous : {job:?}");
        assert_eq!(job.messages(), in_chunk);
        assert!(job.messages() < job.to_seq - job.chunk_from_seq + 1);
        assert_eq!(job.previous_to_seq, None);
    }

    fn job(entries: &[Entry]) -> SummaryJob {
        SummaryJob {
            session_id: "s".into(),
            from_seq: 1,
            to_seq: 9,
            chunk_from_seq: 5,
            source_text: entries.iter().map(render_entry).collect(),
            previous_summary: Some("{\"objectif\": \"x\"}".into()),
            previous_node_id: Some("n_1".into()),
            anchors: vec![],
            verbatim_users: vec![],
            tokens_src: 4,
            batches: vec![(5, 9)],
            chunk_messages: 2,
            previous_to_seq: Some(2),
        }
    }

    /// Adresses 1, 2, 5, 9 : le résumé précédent finit à 2, pas à 4.
    #[test]
    fn the_summarizer_names_real_bounds() {
        let entries = holes();
        let j = job(&entries[2..]);
        assert_eq!(j.messages(), 2);
        let prompt = j.summarizer_messages()[1].text();
        assert!(prompt.contains("(messages #1 à #2)"), "{prompt}");
        assert!(prompt.contains("(messages #5 à #9)"), "{prompt}");
        // Un travail préparé avant T21 (sans compte) garde l'ancienne arithmétique.
        let old = SummaryJob {
            chunk_messages: 0,
            previous_to_seq: None,
            ..j
        };
        assert_eq!(old.messages(), 5);
        assert!(
            old.summarizer_messages()[1]
                .text()
                .contains("(messages #1 à #4)")
        );
    }

    #[test]
    fn a_hole_is_not_a_coverage_gap() {
        let node = |from, to| Node {
            id: format!("n_{from}"),
            session_id: "s".into(),
            kind: NodeKind::Leaf,
            level: 0,
            from_seq: Some(from),
            to_seq: Some(to),
            summary: String::new(),
            anchors: vec![],
            tokens_src: 0,
            tokens_self: 0,
            tokens_subtree: 0,
            superseded_by: None,
        };
        let addresses = [1, 2, 5, 9, 12];
        assert_eq!(uncovered(&[node(1, 2), node(5, 12)], &addresses), []);
        assert_eq!(uncovered(&[node(1, 2)], &addresses), [(5, 12)]);
        assert_eq!(uncovered(&[node(2, 5)], &addresses), [(1, 1), (9, 12)]);
    }

    #[test]
    fn pages_count_nodes() {
        let entries = holes();
        let seqs = |p: &[Entry]| p.iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(seqs(node_page(&entries, 2, 9, 0, 2)), [2, 5]);
        assert_eq!(seqs(node_page(&entries, 2, 9, 1, 2)), [9]);
        assert!(node_page(&entries, 2, 9, 2, 2).is_empty());
        assert_eq!(seqs(node_page(&entries, 3, 4, 0, 2)), Vec::<i64>::new());
    }
}
