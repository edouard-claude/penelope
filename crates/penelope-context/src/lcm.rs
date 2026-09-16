//! LCM — Lossless Context Management (§5.5).
//!
//! DAG de résumés : nœuds `leaf` (intervalle contigu de messages) et `condensed`
//! (couvrent des enfants), profondeur illimitée, **provenance obligatoire**.
//!
//! Le contexte actif est l'ensemble des nœuds de plus haut niveau assurant une couverture
//! **complète et sans trou**, plus la queue verbatim.

use penelope_kernel::clock::SharedClock;
use penelope_kernel::ids::NodeId;
use penelope_store::Store;
use penelope_store::rusqlite::params;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    Leaf,
    Condensed,
}

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::Leaf => "leaf",
            NodeKind::Condensed => "condensed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub session_id: String,
    pub kind: NodeKind,
    pub level: i64,
    pub from_seq: Option<i64>,
    pub to_seq: Option<i64>,
    pub summary: String,
    pub anchors: Vec<crate::anchors::Anchor>,
    pub tokens_src: u64,
    pub tokens_self: u64,
    pub tokens_subtree: u64,
    pub superseded_by: Option<String>,
}

/// Manifeste renvoyé par `history_describe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub kind: NodeKind,
    pub level: i64,
    pub covers: Option<(i64, i64)>,
    pub tokens_src: u64,
    pub tokens_self: u64,
    pub tokens_subtree: u64,
    pub children: Vec<String>,
    pub anchors: Vec<crate::anchors::Anchor>,
}

#[derive(Clone)]
pub struct Lcm {
    store: Store,
    clock: SharedClock,
}

impl Lcm {
    pub fn new(store: Store, clock: SharedClock) -> Self {
        Lcm { store, clock }
    }

    /// Crée un nœud feuille couvrant `[from_seq, to_seq]`.
    // Chaque paramètre est une colonne du ledger : les regrouper dans une structure
    // ne ferait que déplacer la liste.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_leaf(
        &self,
        session_id: &str,
        from_seq: i64,
        to_seq: i64,
        summary: &str,
        anchors: &[crate::anchors::Anchor],
        tokens_src: u64,
        tokens_self: u64,
    ) -> penelope_store::Result<String> {
        let id = NodeId::new().0;
        let (sid, sum, anc, ts) = (
            session_id.to_string(),
            summary.to_string(),
            serde_json::to_string(anchors).unwrap_or_else(|_| "[]".into()),
            self.clock.now_rfc3339(),
        );
        let node_id = id.clone();
        self.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary,
                        anchors, tokens_src, tokens_self, tokens_subtree, created_at)
                     VALUES(?1,?2,'leaf',0,?3,?4,?5,?6,?7,?8,?8,?9)",
                    params![
                        node_id,
                        sid,
                        from_seq,
                        to_seq,
                        sum,
                        anc,
                        tokens_src as i64,
                        tokens_self as i64,
                        ts
                    ],
                )?;
                Ok(())
            })
            .await?;
        Ok(id)
    }

    /// Crée un nœud condensé au-dessus d'enfants existants.
    pub async fn insert_condensed(
        &self,
        session_id: &str,
        children: &[String],
        summary: &str,
        anchors: &[crate::anchors::Anchor],
        tokens_self: u64,
    ) -> penelope_store::Result<String> {
        let id = NodeId::new().0;
        let (sid, sum, anc, ts, kids) = (
            session_id.to_string(),
            summary.to_string(),
            serde_json::to_string(anchors).unwrap_or_else(|_| "[]".into()),
            self.clock.now_rfc3339(),
            children.to_vec(),
        );
        let node_id = id.clone();
        self.store
            .write(move |tx| {
                // Provenance obligatoire : intervalle couvert et totaux du sous-arbre.
                let mut from = i64::MAX;
                let mut to = i64::MIN;
                let mut tokens_src = 0i64;
                let mut subtree = tokens_self as i64;
                for c in &kids {
                    let (f, t, src, sub, level): (Option<i64>, Option<i64>, i64, i64, i64) = tx
                        .query_row(
                            "SELECT from_seq, to_seq, tokens_src, tokens_subtree, level
                             FROM lcm_nodes WHERE id = ?1",
                            [c],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                        )?;
                    let _ = level;
                    if let Some(f) = f {
                        from = from.min(f);
                    }
                    if let Some(t) = t {
                        to = to.max(t);
                    }
                    tokens_src += src;
                    subtree += sub;
                }
                let max_child_level: i64 = {
                    let mut m = 0;
                    for c in &kids {
                        let l: i64 =
                            tx.query_row("SELECT level FROM lcm_nodes WHERE id = ?1", [c], |r| {
                                r.get(0)
                            })?;
                        m = m.max(l);
                    }
                    m
                };

                tx.execute(
                    "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary,
                        anchors, tokens_src, tokens_self, tokens_subtree, created_at)
                     VALUES(?1,?2,'condensed',?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        node_id,
                        sid,
                        max_child_level + 1,
                        (from != i64::MAX).then_some(from),
                        (to != i64::MIN).then_some(to),
                        sum,
                        anc,
                        tokens_src,
                        tokens_self as i64,
                        subtree,
                        ts
                    ],
                )?;
                for c in &kids {
                    tx.execute(
                        "INSERT OR IGNORE INTO lcm_edges(parent_id, child_id) VALUES(?1,?2)",
                        params![node_id, c],
                    )?;
                }
                Ok(())
            })
            .await?;
        Ok(id)
    }

    /// Remplace un nœud par une version mise à jour (la re-compaction **met à jour** le
    /// résumé précédent au lieu de repartir de zéro, §5.4).
    pub async fn supersede(
        &self,
        old_id: &str,
        new_summary: &str,
        anchors: &[crate::anchors::Anchor],
        tokens_self: u64,
    ) -> penelope_store::Result<String> {
        self.replace(old_id, None, new_summary, anchors, tokens_self)
            .await
    }

    /// Met à jour un nœud **et** prolonge sa couverture jusqu'à `to_seq` : le résumé
    /// précédent absorbe les messages qui le suivent. La provenance suit : l'intervalle
    /// s'étend et les tokens source s'additionnent.
    pub async fn extend(
        &self,
        old_id: &str,
        to_seq: i64,
        added_tokens_src: u64,
        new_summary: &str,
        anchors: &[crate::anchors::Anchor],
        tokens_self: u64,
    ) -> penelope_store::Result<String> {
        self.replace(
            old_id,
            Some((to_seq, added_tokens_src)),
            new_summary,
            anchors,
            tokens_self,
        )
        .await
    }

    async fn replace(
        &self,
        old_id: &str,
        extension: Option<(i64, u64)>,
        new_summary: &str,
        anchors: &[crate::anchors::Anchor],
        tokens_self: u64,
    ) -> penelope_store::Result<String> {
        let old = old_id.to_string();
        let (sum, anc, ts) = (
            new_summary.to_string(),
            serde_json::to_string(anchors).unwrap_or_else(|_| "[]".into()),
            self.clock.now_rfc3339(),
        );
        let new_id = NodeId::new().0;
        let nid = new_id.clone();
        self.store
            .write(move |tx| {
                let (sid, kind, level, from, to, src, superseded): (
                    String,
                    String,
                    i64,
                    Option<i64>,
                    Option<i64>,
                    i64,
                    Option<String>,
                ) = tx.query_row(
                    "SELECT session_id, kind, level, from_seq, to_seq, tokens_src, superseded_by
                     FROM lcm_nodes WHERE id = ?1",
                    [&old],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )?;
                // Deux mises à jour concurrentes du même nœud : la seconde est périmée.
                if let Some(by) = superseded {
                    return Err(penelope_store::StoreError::other(format!(
                        "le nœud {old} a déjà été remplacé par {by}"
                    )));
                }
                let (to, src) = match extension {
                    Some((until, added)) => {
                        (Some(to.map_or(until, |t| t.max(until))), src + added as i64)
                    }
                    None => (to, src),
                };
                tx.execute(
                    "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary,
                        anchors, tokens_src, tokens_self, tokens_subtree, created_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,?11)",
                    params![
                        nid,
                        sid,
                        kind,
                        level,
                        from,
                        to,
                        sum,
                        anc,
                        src,
                        tokens_self as i64,
                        ts
                    ],
                )?;
                // Les enfants du nœud remplacé sont rattachés au nouveau.
                tx.execute(
                    "INSERT OR IGNORE INTO lcm_edges(parent_id, child_id)
                     SELECT ?1, child_id FROM lcm_edges WHERE parent_id = ?2",
                    params![nid, old],
                )?;
                tx.execute(
                    "UPDATE lcm_nodes SET superseded_by = ?2 WHERE id = ?1",
                    params![old, nid],
                )?;
                Ok(())
            })
            .await?;
        Ok(new_id)
    }

    /// Nœuds de plus haut niveau, vivants, ordonnés par intervalle couvert.
    pub async fn active_nodes(&self, session_id: &str) -> penelope_store::Result<Vec<Node>> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT n.id, n.session_id, n.kind, n.level, n.from_seq, n.to_seq, n.summary,
                            n.anchors, n.tokens_src, n.tokens_self, n.tokens_subtree, n.superseded_by
                     FROM lcm_nodes n
                     WHERE n.session_id = ?1
                       AND n.superseded_by IS NULL
                       AND NOT EXISTS (
                           SELECT 1 FROM lcm_edges e
                           JOIN lcm_nodes p ON p.id = e.parent_id
                           WHERE e.child_id = n.id AND p.superseded_by IS NULL
                       )
                     ORDER BY COALESCE(n.from_seq, 0), n.level",
                )?;
                let rows = st.query_map([sid], row_to_node)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })
            .await
    }

    /// Vérifie qu'un ensemble de nœuds couvre `[1, up_to]` sans trou.
    pub fn coverage_gaps(nodes: &[Node], up_to: i64) -> Vec<(i64, i64)> {
        let mut ranges: Vec<(i64, i64)> = nodes
            .iter()
            .filter_map(|n| Some((n.from_seq?, n.to_seq?)))
            .collect();
        ranges.sort();
        let mut gaps = Vec::new();
        let mut cursor = 1i64;
        for (from, to) in ranges {
            if from > cursor {
                gaps.push((cursor, from - 1));
            }
            cursor = cursor.max(to + 1);
        }
        if cursor <= up_to {
            gaps.push((cursor, up_to));
        }
        gaps
    }

    pub async fn describe(&self, node_id: &str) -> penelope_store::Result<Option<Manifest>> {
        let id = node_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, session_id, kind, level, from_seq, to_seq, summary, anchors,
                            tokens_src, tokens_self, tokens_subtree, superseded_by
                     FROM lcm_nodes WHERE id = ?1",
                )?;
                let mut rows = st.query([&id])?;
                let Some(r) = rows.next()? else {
                    return Ok(None);
                };
                let n = row_to_node(r)?;
                drop(rows);
                let mut st = c.prepare("SELECT child_id FROM lcm_edges WHERE parent_id = ?1")?;
                let kids = st.query_map([&id], |r| r.get::<_, String>(0))?;
                let mut children = Vec::new();
                for k in kids {
                    children.push(k?);
                }
                Ok(Some(Manifest {
                    id: n.id,
                    kind: n.kind,
                    level: n.level,
                    covers: match (n.from_seq, n.to_seq) {
                        (Some(a), Some(b)) => Some((a, b)),
                        _ => None,
                    },
                    tokens_src: n.tokens_src,
                    tokens_self: n.tokens_self,
                    tokens_subtree: n.tokens_subtree,
                    children,
                    anchors: n.anchors,
                }))
            })
            .await
    }

    pub async fn get(&self, node_id: &str) -> penelope_store::Result<Option<Node>> {
        let id = node_id.to_string();
        self.store
            .read(move |c| {
                let mut st = c.prepare(
                    "SELECT id, session_id, kind, level, from_seq, to_seq, summary, anchors,
                            tokens_src, tokens_self, tokens_subtree, superseded_by
                     FROM lcm_nodes WHERE id = ?1",
                )?;
                let mut rows = st.query([&id])?;
                match rows.next()? {
                    Some(r) => Ok(Some(row_to_node(r)?)),
                    None => Ok(None),
                }
            })
            .await
    }

    /// Compte les nœuds vivants d'une session.
    pub async fn count(&self, session_id: &str) -> penelope_store::Result<i64> {
        let sid = session_id.to_string();
        self.store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM lcm_nodes WHERE session_id = ?1 AND superseded_by IS NULL",
                    [sid],
                    |r| r.get(0),
                )?)
            })
            .await
    }
}

fn row_to_node(r: &penelope_store::rusqlite::Row<'_>) -> penelope_store::rusqlite::Result<Node> {
    let kind: String = r.get(2)?;
    let anchors: String = r.get(7)?;
    Ok(Node {
        id: r.get(0)?,
        session_id: r.get(1)?,
        kind: if kind == "condensed" {
            NodeKind::Condensed
        } else {
            NodeKind::Leaf
        },
        level: r.get(3)?,
        from_seq: r.get(4)?,
        to_seq: r.get(5)?,
        summary: r.get(6)?,
        anchors: serde_json::from_str(&anchors).unwrap_or_default(),
        tokens_src: r.get::<_, i64>(8)? as u64,
        tokens_self: r.get::<_, i64>(9)? as u64,
        tokens_subtree: r.get::<_, i64>(10)? as u64,
        superseded_by: r.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    fn lcm() -> Lcm {
        Lcm::new(
            Store::open_memory().unwrap(),
            Arc::new(TestClock::default()),
        )
    }

    #[tokio::test]
    async fn leaf_then_condensed_builds_a_dag() {
        let l = lcm();
        let a = l
            .insert_leaf("s1", 1, 10, "tours 1-10", &[], 5000, 300)
            .await
            .unwrap();
        let b = l
            .insert_leaf("s1", 11, 20, "tours 11-20", &[], 6000, 320)
            .await
            .unwrap();
        let c = l
            .insert_condensed("s1", &[a.clone(), b.clone()], "tours 1-20", &[], 400)
            .await
            .unwrap();

        let m = l.describe(&c).await.unwrap().unwrap();
        assert_eq!(m.kind, NodeKind::Condensed);
        assert_eq!(m.level, 1);
        assert_eq!(m.covers, Some((1, 20)), "provenance : intervalle source");
        assert_eq!(m.tokens_src, 11_000, "provenance : tokens source");
        assert_eq!(m.tokens_subtree, 400 + 300 + 320);
        assert_eq!(m.children.len(), 2);

        // Le contexte actif ne retient que le nœud de plus haut niveau.
        let active = l.active_nodes("s1").await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, c);
    }

    #[tokio::test]
    async fn coverage_gaps_are_detected() {
        let l = lcm();
        l.insert_leaf("s1", 1, 10, "a", &[], 100, 10).await.unwrap();
        l.insert_leaf("s1", 15, 20, "b", &[], 100, 10)
            .await
            .unwrap();
        let nodes = l.active_nodes("s1").await.unwrap();
        let gaps = Lcm::coverage_gaps(&nodes, 25);
        assert_eq!(gaps, vec![(11, 14), (21, 25)]);
    }

    #[tokio::test]
    async fn full_coverage_has_no_gap() {
        let l = lcm();
        l.insert_leaf("s1", 1, 10, "a", &[], 100, 10).await.unwrap();
        l.insert_leaf("s1", 11, 20, "b", &[], 100, 10)
            .await
            .unwrap();
        let nodes = l.active_nodes("s1").await.unwrap();
        assert!(Lcm::coverage_gaps(&nodes, 20).is_empty());
    }

    #[tokio::test]
    async fn recompaction_updates_instead_of_restarting() {
        let l = lcm();
        let a = l
            .insert_leaf("s1", 1, 10, "version 1", &[], 5000, 300)
            .await
            .unwrap();
        let b = l
            .supersede(&a, "version 2, enrichie", &[], 350)
            .await
            .unwrap();

        let old = l.get(&a).await.unwrap().unwrap();
        assert_eq!(old.superseded_by.as_deref(), Some(b.as_str()));

        let active = l.active_nodes("s1").await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, b);
        assert_eq!(active[0].summary, "version 2, enrichie");
        assert_eq!(
            active[0].from_seq,
            Some(1),
            "l'intervalle couvert est conservé"
        );
        assert_eq!(active[0].tokens_src, 5000, "la provenance est conservée");
    }

    #[tokio::test]
    async fn extension_absorbs_the_following_messages() {
        let l = lcm();
        let a = l
            .insert_leaf("s1", 1, 10, "tours 1-10", &[], 5000, 300)
            .await
            .unwrap();
        let b = l
            .extend(&a, 24, 2500, "tours 1-24", &[], 380)
            .await
            .unwrap();

        let active = l.active_nodes("s1").await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, b);
        assert_eq!((active[0].from_seq, active[0].to_seq), (Some(1), Some(24)));
        assert_eq!(
            active[0].tokens_src, 7500,
            "les tokens source s'additionnent"
        );
        assert!(Lcm::coverage_gaps(&active, 24).is_empty());

        // Le nœud remplacé ne se met plus à jour : une seconde écriture est périmée.
        assert!(l.extend(&a, 30, 100, "doublon", &[], 10).await.is_err());
        assert_eq!(l.active_nodes("s1").await.unwrap()[0].id, b);
    }

    #[tokio::test]
    async fn anchors_survive_in_the_node() {
        let l = lcm();
        let anchors = crate::anchors::extract("le bug est dans src/facture.rs, ticket PROJ-42");
        let id = l
            .insert_leaf("s1", 1, 5, "résumé", &anchors, 100, 20)
            .await
            .unwrap();
        let m = l.describe(&id).await.unwrap().unwrap();
        assert!(m.anchors.iter().any(|a| a.value == "src/facture.rs"));
        assert!(m.anchors.iter().any(|a| a.value == "PROJ-42"));
    }

    #[tokio::test]
    async fn depth_is_unlimited() {
        let l = lcm();
        let mut current = l
            .insert_leaf("s1", 1, 10, "niveau 0", &[], 1000, 100)
            .await
            .unwrap();
        for level in 1..6 {
            current = l
                .insert_condensed("s1", &[current], &format!("niveau {level}"), &[], 50)
                .await
                .unwrap();
        }
        let m = l.describe(&current).await.unwrap().unwrap();
        assert_eq!(m.level, 5);
        assert_eq!(l.active_nodes("s1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn describe_unknown_node_is_none() {
        assert!(lcm().describe("n_inconnu").await.unwrap().is_none());
    }
}
