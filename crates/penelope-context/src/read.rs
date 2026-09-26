//! Lecture de la conversation d'une session : les entrées que la requête projette et la
//! queue de l'historique (épopée #208, T14 ; `design/v1/source-de-verite.md` §4.3 et §4.4).
//!
//! La conversation se relit toujours dans le journal (T16 : la clé `history.source` et
//! la lecture des tables sont retirées) : le journal de la session plié par
//! [`crate::derive`] après son préfixe hérité ([`crate::replay::Lineage`]), les entrées
//! numérotées comme leurs lignes (le niveau 0 cite `seq:N`), repris sur les seuls
//! événements nouveaux ([`cache`]).
//!
//! Une session que le journal ne sait pas redonner (l'archive d'un retour arrière, qui
//! n'a pas de journal à elle) se lit dans ses caches, projetés depuis le journal de sa
//! mère ; un journal qui ne se plie pas aussi, avec une erreur dans les journaux du
//! daemon (`penelope history verify` la nomme, `doctor` aussi).

use crate::engine::ContextEngine;
use crate::replay::{Lineage, ReplayError};
use crate::store::HistoryStore;
use crate::transcript::Entry;
use cache::Folded;
pub(crate) use cache::SharedReadCache;
use penelope_llm::types::{ChatMessage, Role};
use penelope_store::rusqlite::Connection;

/// La conversation d'une session pliée depuis son journal.
#[derive(Debug, Clone, Default)]
pub struct JournalRead {
    /// Les nœuds visibles, dans l'ordre de la requête (résumés en messages système).
    pub projected: Vec<Entry>,
    /// Tous les messages non coupés, masqués compris, comme les lignes de `messages`.
    pub entries: Vec<Entry>,
}

impl HistoryStore {
    /// La conversation de la session pliée depuis son journal, numérotée comme ses
    /// lignes. `Ok(None)` : le journal ne sait pas la redonner, les caches font foi.
    pub async fn read_journal(
        &self,
        session_id: &str,
    ) -> penelope_store::Result<Result<Option<JournalRead>, String>> {
        let sid = session_id.to_string();
        let reads = self.reads.clone();
        self.store()
            .read(move |c| Ok(read_journal_in(c, &sid, &reads)))
            .await
    }
}

fn read_journal_in(
    c: &Connection,
    sid: &str,
    reads: &SharedReadCache,
) -> Result<Option<JournalRead>, String> {
    let count = |sql: &str| -> Result<i64, String> {
        c.query_row(sql, [sid], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    // Lignes que la double écriture n'a pas couvertes : le journal les ignore.
    let unjournaled = count(
        "SELECT COUNT(*) FROM messages
         WHERE session_id = ?1 AND event_id IS NULL AND sealed = 0",
    )?;
    if unjournaled > 0 {
        return Ok(None);
    }
    let purges = cache::purge_mark(c)?;
    // Le verrou n'est pas tenu pendant le pliage : la session est retirée, puis remise.
    let cached = reads.lock().ok().and_then(|mut r| r.take(sid, purges));
    let (folded, count) = match cached {
        Some(mut folded) => {
            let n = folded.catch_up(c, sid)?;
            (folded, n)
        }
        None => {
            let lineage = Lineage::load(c, sid).map_err(|e: ReplayError| e.to_string())?;
            if !lineage.journaled() {
                // Sans `conv.*`, une session n'a de conversation que si elle n'a pas de
                // lignes : sinon c'est une archive de retour arrière, copie V0 sans journal.
                let rows = count("SELECT COUNT(*) FROM messages WHERE session_id = ?1")?;
                return Ok((rows == 0).then(JournalRead::default));
            }
            let n = lineage.events.len();
            match Folded::load(lineage, purges)? {
                Some(folded) => (folded, n),
                None => return Ok(Some(JournalRead::default())),
            }
        }
    };
    let read = journal_read(&folded);
    if let Ok(mut r) = reads.lock() {
        r.put(sid, folded, count);
    }
    Ok(Some(read))
}

/// Les entrées de la surface pliée, numérotées comme les lignes.
fn journal_read(folded: &Folded) -> JournalRead {
    let surface = folded.surface();
    let seqs = folded.row_seqs();
    let renumber = |mut e: Entry| {
        if e.seq != 0 {
            e.seq = seqs.get(&e.seq).copied().unwrap_or(e.seq);
        }
        e
    };
    JournalRead {
        projected: surface
            .projected_entries()
            .into_iter()
            .map(renumber)
            .collect(),
        entries: surface.entries().into_iter().map(renumber).collect(),
    }
}

impl ContextEngine {
    /// La conversation pliée depuis le journal ; `None` si le journal ne sait pas la
    /// redonner ou ne se plie pas : les caches font foi.
    async fn journal_side(&self, session_id: &str) -> penelope_store::Result<Option<JournalRead>> {
        match self.history.read_journal(session_id).await? {
            Ok(read) => Ok(read),
            Err(reason) => {
                tracing::error!(
                    session = session_id,
                    "le journal ne se plie pas, lecture dans les caches : {reason}"
                );
                Ok(None)
            }
        }
    }

    /// Entrées à projeter dans la requête.
    pub async fn projected_entries(&self, session_id: &str) -> penelope_store::Result<Vec<Entry>> {
        match self.journal_side(session_id).await? {
            Some(read) => Ok(read.projected),
            None => self.projected_from_tables(session_id).await,
        }
    }

    /// Les `limit` dernières entrées de l'historique (masquées comprises) : de quoi
    /// retrouver les appels d'outils en attente.
    pub async fn tail(&self, session_id: &str, limit: usize) -> penelope_store::Result<Vec<Entry>> {
        match self.journal_side(session_id).await? {
            Some(read) => {
                let mut entries = read.entries;
                entries.drain(..entries.len().saturating_sub(limit));
                Ok(entries)
            }
            None => self.history.tail(session_id, limit).await,
        }
    }

    /// Entrées à projeter, lues dans les caches : résumés LCM actifs, puis tout ce qu'ils
    /// ne couvrent pas. Le repli de [`projected_entries`](Self::projected_entries).
    pub async fn projected_from_tables(
        &self,
        session_id: &str,
    ) -> penelope_store::Result<Vec<Entry>> {
        // Les résumés actifs d'abord : ce qu'ils couvrent n'a pas à être relu ni
        // désérialisé pour être aussitôt jeté (issue #55).
        let nodes = self.lcm.active_nodes(session_id).await?;
        let covered_to = nodes.iter().filter_map(|n| n.to_seq).max().unwrap_or(0);
        let from_seq = if nodes.is_empty() { 0 } else { covered_to + 1 };
        let mut entries = self.history.load(session_id, from_seq).await?;
        // Contexte volatil figé avec chaque message utilisateur (issue #17).
        let contexts = self.history.contexts_from(session_id, from_seq).await?;
        for e in entries.iter_mut() {
            if let Some(block) = contexts.get(&e.seq)
                && e.message.role == Role::User
            {
                let m = &mut e.message;
                match m.content.iter_mut().find_map(|c| match c {
                    penelope_llm::types::Content::Text { text } => Some(text),
                    _ => None,
                }) {
                    Some(text) => text.insert_str(0, block),
                    None => m
                        .content
                        .insert(0, penelope_llm::types::Content::text(block.clone())),
                }
            }
        }
        if nodes.is_empty() {
            return Ok(entries);
        }
        let mut out: Vec<Entry> = nodes
            .iter()
            .map(|n| {
                Entry::new(
                    0,
                    ChatMessage::system(format!(
                        "Résumé de la conversation antérieure (nœud {}) :\n{}",
                        n.id, n.summary
                    )),
                    n.tokens_self,
                )
            })
            .collect();
        out.extend(entries.into_iter().filter(|e| !e.compacted));
        Ok(out)
    }
}

pub(crate) mod at;
mod cache;
#[cfg(test)]
mod tests;
