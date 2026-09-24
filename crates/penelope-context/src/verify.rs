//! `penelope history verify` : les caches de la conversation contre le journal (épopée
//! #208, T12 ; `design/v1/source-de-verite.md` §2.4 et §4.2).
//!
//! Chaque session est dérivée depuis son journal (préfixe hérité compris, [`crate::replay`])
//! et comparée à ses lignes : nombre et ordre des nœuds, contenu de chaque nœud (rôle,
//! blocs, appels, identifiant d'appel, artefact, jetons, épisode, drapeau `eager`),
//! drapeau `compacted` des lignes non scellées, contextes figés, bornes et texte des nœuds
//! LCM actifs, empreinte du préfixe scellé. Une ligne est appariée à son nœud par
//! `event_id` (une ligne scellée par son numéro) : après un scellement ou un fork, le
//! numéro de ligne et l'adresse du nœud divergent (`i-scellement.md`).
//!
//! Trois cas nommés plutôt que tus :
//! - l'archive d'un retour arrière n'a pas de journal : elle est comparée aux messages que
//!   le `conv.rewind` de sa mère a coupés (`archive`) ;
//! - les contextes figés qu'une fille de fork hérite ne sont pas recopiés par la V0 : ils
//!   ne sont pas attendus (voir [`crate::replay`]) ;
//! - un retour arrière qui coupe dans le préfixe scellé en retire des lignes : l'empreinte
//!   n'est plus vérifiable, la session le dit (`notes`).

use crate::replay::{Expected, Lineage, ReplayError, archive_expected, archive_of};
use crate::store::{HistoryStore, deserialise_content, serialise_content};
use penelope_kernel::canonical::{canonical_json, sha256_hex};
use penelope_llm::types::{ChatMessage, Role};
use penelope_store::rusqlite::{self, Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Un écart entre le journal et les caches.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Divergence {
    pub session: String,
    /// Adresse du nœud dans la surface (§2.3), quand l'écart en vise un.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<i64>,
    /// Numéro de la ligne en cause, quand il y en a une.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<i64>,
    /// `journal`, `digest`, `missing_row`, `extra_row`, `unjournaled_row`, `order`,
    /// `content`, `compacted`, `context`, `summary`.
    pub what: String,
    pub detail: String,
}

/// Ce qu'une session a donné.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SessionCheck {
    pub session: String,
    /// `journal`, `sealed`, `fork`, `archive`, `legacy` (lignes sans journal), `empty`.
    pub kind: String,
    pub nodes: usize,
    pub divergences: Vec<Divergence>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Le rapport de `history verify`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub sessions: usize,
    pub nodes: usize,
    pub divergent: usize,
    pub divergences: Vec<Divergence>,
    /// Archives de retour arrière vérifiées contre leur mère.
    pub archives: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl VerifyReport {
    fn add(&mut self, check: SessionCheck) {
        self.sessions += 1;
        self.nodes += check.nodes;
        if !check.divergences.is_empty() {
            self.divergent += 1;
        }
        if check.kind == "archive" {
            self.archives.push(check.session.clone());
        }
        self.notes.extend(
            check
                .notes
                .into_iter()
                .map(|n| format!("{} : {n}", check.session)),
        );
        self.divergences.extend(check.divergences);
        self.ok = self.divergences.is_empty();
    }
}

/// Une ligne de `messages` telle qu'en base.
struct TableRow {
    seq: i64,
    message: ChatMessage,
    tokens: i64,
    episode: i64,
    eager: bool,
    artifact_id: Option<String>,
    compacted: bool,
    event_id: Option<i64>,
    sealed: bool,
}

fn table_rows(c: &Connection, sid: &str) -> rusqlite::Result<Vec<TableRow>> {
    let mut st = c.prepare(
        "SELECT seq, role, content, tool_call_id, tool_name, tokens_est, episode, eager,
                artifact_id, compacted, event_id, sealed
         FROM messages WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = st.query_map([sid], |r| {
        let role: String = r.get(1)?;
        let content: String = r.get(2)?;
        Ok(TableRow {
            seq: r.get(0)?,
            message: deserialise_content(
                Role::parse(&role).unwrap_or(Role::User),
                &content,
                r.get(3)?,
                r.get(4)?,
            ),
            tokens: r.get(5)?,
            episode: r.get(6)?,
            eager: r.get::<_, i64>(7)? != 0,
            artifact_id: r.get(8)?,
            compacted: r.get::<_, i64>(9)? != 0,
            event_id: r.get(10)?,
            sealed: r.get::<_, i64>(11)? != 0,
        })
    })?;
    rows.collect()
}

/// Un nœud LCM actif, tel qu'en base ou tel que le journal l'attend.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Active {
    pub from: i64,
    pub to: i64,
    pub summary: String,
    /// `None` attendu pour la copie d'un résumé hérité : son identifiant est tiré au
    /// hasard, en V0 comme à la refonte.
    pub id: Option<String>,
    pub event_id: Option<i64>,
    pub tokens_self: u64,
    pub tokens_src: u64,
    pub anchors: Value,
}

/// Les nœuds LCM actifs (même règle que `Lcm::active_nodes`).
pub(crate) fn active_nodes(c: &Connection, sid: &str) -> rusqlite::Result<Vec<Active>> {
    let mut st = c.prepare(
        "SELECT COALESCE(n.from_seq, 0), COALESCE(n.to_seq, 0), n.summary, n.id, n.event_id,
                n.tokens_self, n.tokens_src, n.anchors
         FROM lcm_nodes n
         WHERE n.session_id = ?1
           AND n.superseded_by IS NULL
           AND NOT EXISTS (
               SELECT 1 FROM lcm_edges e
               JOIN lcm_nodes p ON p.id = e.parent_id
               WHERE e.child_id = n.id AND p.superseded_by IS NULL
           )
         ORDER BY 1, 2",
    )?;
    let rows = st.query_map([sid], |r| {
        let anchors: String = r.get(7)?;
        Ok(Active {
            from: r.get(0)?,
            to: r.get(1)?,
            summary: r.get(2)?,
            id: r.get(3)?,
            event_id: r.get(4)?,
            tokens_self: r.get::<_, i64>(5)? as u64,
            tokens_src: r.get::<_, i64>(6)? as u64,
            anchors: serde_json::from_str(&anchors).unwrap_or(Value::Null),
        })
    })?;
    rows.collect()
}

/// Deux messages disent la même chose. Les objets JSON (arguments d'appel, détails de
/// raisonnement) se comparent sans l'ordre de leurs clés : le journal les range
/// (`canonical_json`), la ligne garde l'ordre du fournisseur.
fn same_message(a: &ChatMessage, b: &ChatMessage) -> bool {
    a.role == b.role
        && a.content == b.content
        && a.tool_calls == b.tool_calls
        && a.tool_call_id == b.tool_call_id
        && a.name == b.name
        && a.reasoning == b.reasoning
        && a.reasoning_details == b.reasoning_details
}

/// Empreinte courte d'un message, clés rangées, pour nommer deux contenus qui diffèrent.
fn fingerprint(m: &ChatMessage) -> String {
    let content: Value = serialise_content(m)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let text = canonical_json(&json!([m.role.as_str(), content, m.tool_call_id, m.name]));
    sha256_hex(text.as_bytes())[..12].to_string()
}

/// Les sessions à vérifier : toutes celles qu'une table ou le journal nomme.
pub(crate) fn all_sessions(c: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(
        "SELECT id FROM sessions
         UNION SELECT DISTINCT session_id FROM messages
         UNION SELECT DISTINCT session_id FROM events
               WHERE session_id IS NOT NULL AND kind LIKE 'conv.%'
         ORDER BY 1",
    )?;
    let rows = st.query_map([], |r| r.get(0))?;
    rows.collect()
}

struct Checker {
    check: SessionCheck,
}

impl Checker {
    fn diverge(&mut self, node: Option<i64>, seq: Option<i64>, what: &str, detail: String) {
        self.check.divergences.push(Divergence {
            session: self.check.session.clone(),
            node,
            seq,
            what: what.to_string(),
            detail,
        });
    }

    /// Lignes contre nœuds attendus.
    fn rows(&mut self, expected: &Expected, rows: &[TableRow]) {
        let by_event: BTreeMap<i64, usize> = expected
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.event_id.map(|id| (id, i)))
            .collect();
        let by_seq: BTreeMap<i64, usize> = expected
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.sealed)
            .map(|(i, r)| (r.seq, i))
            .collect();
        let mut matched: BTreeMap<usize, i64> = BTreeMap::new();
        for row in rows {
            let hit = match (row.event_id, row.sealed) {
                (Some(id), _) => by_event.get(&id),
                (None, true) => by_seq.get(&row.seq),
                (None, false) => {
                    self.diverge(
                        None,
                        Some(row.seq),
                        "unjournaled_row",
                        "ligne sans événement ni scellement".into(),
                    );
                    continue;
                }
            };
            let Some(&i) = hit else {
                self.diverge(
                    None,
                    Some(row.seq),
                    "extra_row",
                    format!(
                        "ligne sans nœud dans la surface (événement {:?})",
                        row.event_id
                    ),
                );
                continue;
            };
            if matched.insert(i, row.seq).is_some() {
                self.diverge(
                    Some(expected.rows[i].address),
                    Some(row.seq),
                    "extra_row",
                    "deux lignes pour le même nœud".into(),
                );
                continue;
            }
            self.compare(&expected.rows[i], row);
        }
        let mut previous: Option<i64> = None;
        for (i, want) in expected.rows.iter().enumerate() {
            let Some(&seq) = matched.get(&i) else {
                self.diverge(
                    Some(want.address),
                    None,
                    "missing_row",
                    format!(
                        "nœud {} ({}) sans ligne",
                        want.address,
                        want.message.role.as_str()
                    ),
                );
                continue;
            };
            if previous.is_some_and(|p| seq <= p) {
                self.diverge(
                    Some(want.address),
                    Some(seq),
                    "order",
                    format!(
                        "la ligne {seq} vient après la ligne {}",
                        previous.unwrap_or(0)
                    ),
                );
            }
            previous = Some(seq);
        }
    }

    fn compare(&mut self, want: &crate::replay::Row, row: &TableRow) {
        let (node, seq) = (Some(want.address), Some(row.seq));
        if !same_message(&want.message, &row.message) {
            let (a, b) = (fingerprint(&want.message), fingerprint(&row.message));
            self.diverge(
                node,
                seq,
                "content",
                format!("contenu {b}, le journal dit {a}"),
            );
        }
        let fields = [
            (
                "artifact_id",
                format!("{:?}", want.artifact_id),
                format!("{:?}", row.artifact_id),
            ),
            (
                "tokens_est",
                want.tokens.to_string(),
                row.tokens.to_string(),
            ),
            ("episode", want.episode.to_string(), row.episode.to_string()),
            ("eager", want.eager.to_string(), row.eager.to_string()),
        ];
        for (name, w, f) in fields {
            if w != f {
                self.diverge(
                    node,
                    seq,
                    "content",
                    format!("{name} {f}, le journal dit {w}"),
                );
            }
        }
        // Le drapeau d'une ligne scellée n'est pas fiable (V0) : la couverture fait foi.
        if !row.sealed && want.compacted != row.compacted {
            self.diverge(
                node,
                seq,
                "compacted",
                format!(
                    "compacted {}, le journal dit {}",
                    row.compacted, want.compacted
                ),
            );
        }
    }

    fn contexts(&mut self, expected: &Expected, found: &BTreeMap<i64, String>) {
        for (seq, block) in &expected.contexts {
            match found.get(seq) {
                None => self.diverge(None, Some(*seq), "context", "contexte figé absent".into()),
                Some(b) if b != block => self.diverge(
                    None,
                    Some(*seq),
                    "context",
                    "contexte figé différent".into(),
                ),
                Some(_) => {}
            }
        }
        for seq in found.keys().filter(|s| !expected.contexts.contains_key(s)) {
            self.diverge(
                None,
                Some(*seq),
                "context",
                "contexte figé qu'aucun conv.context ne porte".into(),
            );
        }
    }

    fn summaries(&mut self, expected: &Expected, found: &[Active]) {
        let mut want: Vec<Active> = expected
            .nodes
            .iter()
            .filter(|n| n.superseded_by.is_none())
            .map(|n| Active {
                from: n.from_seq,
                to: n.to_seq,
                summary: n.summary.clone(),
                id: n.node_id.clone(),
                event_id: n.event_id,
                tokens_self: n.tokens_self,
                tokens_src: n.tokens_src,
                anchors: serde_json::from_str(&n.anchors).unwrap_or(Value::Null),
            })
            .collect();
        want.sort_by_key(|n| (n.from, n.to));
        let bounds = |v: &[Active]| {
            v.iter()
                .map(|n| format!("{}..{}", n.from, n.to))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if want.len() != found.len()
            || want
                .iter()
                .zip(found)
                .any(|(w, f)| (w.from, w.to) != (f.from, f.to))
        {
            let detail = format!(
                "résumés actifs [{}], le journal dit [{}]",
                bounds(found),
                bounds(&want)
            );
            self.diverge(None, None, "summary", detail);
            return;
        }
        for (w, f) in want.iter().zip(found) {
            let node = f.id.clone().unwrap_or_default();
            let mut differs = Vec::new();
            if w.summary != f.summary {
                differs.push("texte".to_string());
            }
            if w.id.is_some() && w.id != f.id {
                differs.push(format!("identifiant (le journal dit {:?})", w.id));
            }
            if w.event_id != f.event_id {
                differs.push(format!(
                    "event_id {:?} (le journal dit {:?})",
                    f.event_id, w.event_id
                ));
            }
            if (w.tokens_self, w.tokens_src) != (f.tokens_self, f.tokens_src) {
                differs.push("jetons".to_string());
            }
            if w.anchors != f.anchors {
                differs.push("ancres".to_string());
            }
            if !differs.is_empty() {
                let detail = format!(
                    "résumé {node} ({}..{}) : {}",
                    f.from,
                    f.to,
                    differs.join(", ")
                );
                self.diverge(None, Some(f.from), "summary", detail);
            }
        }
    }
}

/// Vérifie une session dans la connexion de l'appelant.
pub(crate) fn verify_in(c: &Connection, sid: &str) -> penelope_store::Result<SessionCheck> {
    let mut ck = Checker {
        check: SessionCheck {
            session: sid.to_string(),
            ..SessionCheck::default()
        },
    };
    let rows = table_rows(c, sid)?;
    let mut st = c.prepare("SELECT seq, context FROM message_context WHERE session_id = ?1")?;
    let contexts: BTreeMap<i64, String> = st
        .query_map([sid], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let nodes = active_nodes(c, sid)?;
    match expected_in(c, sid, &mut ck.check) {
        Ok(expected) => {
            ck.check.nodes = expected.rows.len();
            ck.rows(&expected, &rows);
            ck.contexts(&expected, &contexts);
            ck.summaries(&expected, &nodes);
        }
        Err(ReplayError::Journal(e)) => ck.diverge(None, None, "journal", e),
        Err(ReplayError::Store(e)) => return Err(e),
    }
    Ok(ck.check)
}

/// Ce que la session doit contenir, avec son genre et ses notes ; l'empreinte du préfixe
/// scellé est vérifiée au passage.
fn expected_in(
    c: &Connection,
    sid: &str,
    check: &mut SessionCheck,
) -> Result<Expected, ReplayError> {
    let lineage = Lineage::load(c, sid)?;
    if !lineage.journaled() {
        if let Some((mother, seq, after)) = archive_of(c, sid)? {
            check.kind = "archive".into();
            return archive_expected(c, &mother, seq, after);
        }
        let any: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id = ?1)",
            [sid],
            |r| r.get(0),
        )?;
        check.kind = if any { "legacy" } else { "empty" }.into();
    } else {
        check.kind = match lineage.origin {
            crate::replay::Origin::None => "journal",
            crate::replay::Origin::Fork => "fork",
            crate::replay::Origin::Import(_) => "sealed",
        }
        .into();
    }
    if let crate::replay::Origin::Import(b) = &lineage.origin {
        let (import, legacy) = b.as_ref();
        if lineage.cuts_sealed_prefix() {
            check
                .notes
                .push("retour arrière dans le préfixe scellé : empreinte non vérifiable".into());
        } else if legacy.digest() != import.digest {
            check.divergences.push(Divergence {
                session: sid.to_string(),
                node: None,
                seq: None,
                what: "digest".into(),
                detail: format!(
                    "préfixe scellé modifié : empreinte {}, le conv.import dit {}",
                    &legacy.digest()[..12],
                    &import.digest[..import.digest.len().min(12)]
                ),
            });
        }
    }
    let surface = lineage.surface()?;
    lineage.expected(c, &surface)
}

impl HistoryStore {
    /// Vérifie une session contre son journal.
    pub async fn verify_session(&self, session_id: &str) -> penelope_store::Result<SessionCheck> {
        let sid = session_id.to_string();
        self.store().read(move |c| verify_in(c, &sid)).await
    }

    /// Vérifie `session`, ou toutes les sessions (`None`). `since` (RFC 3339) restreint
    /// aux sessions mises à jour depuis : c'est la borne de `doctor`.
    pub async fn verify(
        &self,
        session: Option<&str>,
        since: Option<&str>,
    ) -> penelope_store::Result<VerifyReport> {
        let (only, since) = (session.map(String::from), since.map(String::from));
        self.store()
            .read(move |c| {
                let ids = match (&only, &since) {
                    (Some(s), _) => vec![s.clone()],
                    (None, Some(t)) => {
                        let mut st = c.prepare(
                            "SELECT id FROM sessions WHERE updated_at >= ?1 ORDER BY id",
                        )?;
                        let rows = st.query_map(params![t], |r| r.get(0))?;
                        rows.collect::<Result<_, _>>()?
                    }
                    (None, None) => all_sessions(c)?,
                };
                let mut report = VerifyReport {
                    ok: true,
                    ..VerifyReport::default()
                };
                for sid in ids {
                    report.add(verify_in(c, &sid)?);
                }
                Ok(report)
            })
            .await
    }
}

#[cfg(test)]
mod tests;
