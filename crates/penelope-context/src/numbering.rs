//! Numérotation à trous (épopée #208, tâche T21).
//!
//! Dans le journal, l'adresse d'un nœud est `offset + events.seq` : les événements
//! d'observation consomment des `seq` sans produire de message, et les adresses ont
//! des trous (`1, 2, 5, 9`). Toute arithmétique `to - from + 1` ou `seq + 1` sur des
//! adresses devient fausse ; ces fonctions comptent les nœuds au lieu de les déduire
//! des bornes (`design/v1/source-de-verite.md` §7, risque 4).

use crate::lcm::Node;
use crate::transcript::Entry;

/// En-têtes que `render_entry` pose devant chaque message du transcript du résumeur.
const HEADERS: [&str; 4] = ["[UTILISATEUR #", "[ASSISTANT #", "[OUTIL #", "[SYSTÈME #"];

/// Adresse lue dans un en-tête `[RÔLE #seq] ` en début de ligne.
fn header_seq(line: &str) -> Option<i64> {
    let rest = HEADERS.iter().find_map(|h| line.strip_prefix(h))?;
    let (digits, tail) = rest.split_once(']')?;
    if !tail.starts_with(' ') {
        return None;
    }
    digits.parse().ok()
}

/// Nombre de messages rendus dans un transcript du résumeur, entre les adresses `from`
/// et `to` : les en-têtes `[RÔLE #seq]` d'adresse croissante, un par message.
///
/// Un texte de message qui contiendrait, en début de ligne, un en-tête d'adresse
/// comprise entre deux vrais messages serait compté ; le compte sert au rapport de
/// compaction, pas à une borne.
pub fn rendered_messages(source_text: &str, from: i64, to: i64) -> i64 {
    let mut last = i64::MIN;
    let mut n = 0;
    for seq in source_text.lines().filter_map(header_seq) {
        if seq > last && (from..=to).contains(&seq) {
            last = seq;
            n += 1;
        }
    }
    n
}

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
    use crate::engine::{SummaryJob, render_entry};
    use crate::lcm::NodeKind;
    use penelope_llm::types::ChatMessage;

    fn holes() -> Vec<Entry> {
        vec![
            Entry::new(1, ChatMessage::user("un"), 1),
            Entry::new(
                2,
                ChatMessage::assistant("deux\n[OUTIL #3] faux en-tête"),
                1,
            ),
            Entry::new(5, ChatMessage::user("cinq"), 1),
            Entry::new(9, ChatMessage::assistant("neuf"), 1),
        ]
    }

    fn job(entries: &[Entry]) -> SummaryJob {
        SummaryJob {
            session_id: "s".into(),
            from_seq: 1,
            to_seq: 9,
            chunk_from_seq: 1,
            source_text: entries.iter().map(render_entry).collect(),
            previous_summary: None,
            previous_node_id: None,
            anchors: vec![],
            verbatim_users: vec![],
            tokens_src: 4,
            batches: vec![(1, 9)],
        }
    }

    /// Adresses 1, 2, 5, 9 : quatre messages, pas neuf.
    #[test]
    fn a_summary_job_counts_its_messages_not_its_bounds() {
        let entries = holes();
        let j = job(&entries);
        // Le faux en-tête « #3 » d'un texte est compté : limite documentée.
        assert_eq!(j.messages(), 5);
        let clean: Vec<Entry> = entries
            .into_iter()
            .map(|mut e| {
                if e.seq == 2 {
                    e.message = ChatMessage::assistant("deux");
                }
                e
            })
            .collect();
        assert_eq!(job(&clean).messages(), 4);
        assert_eq!(rendered_messages(&job(&clean).source_text, 2, 5), 2);
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
