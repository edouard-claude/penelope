//! Lecture incrémentale du journal (épopée #208, lot I, suite de T14).
//!
//! Plier tout le journal d'une session, et celui de ses ancêtres, à chaque requête coûte
//! un temps linéaire en longueur de session. Le pliage est un état qu'on reprend
//! ([`Folding`]) : chaque session lue garde ici sa surface pliée, son préfixe hérité, les
//! événements de ses adresses et le `seq` du dernier événement plié ; la lecture suivante
//! ne relit et ne plie que les événements de `seq` supérieur.
//!
//! Remplacements, coupes et héritages n'invalident rien : ce sont des événements de la
//! session, pliés à leur tour, et reprendre le pliage donne la même surface que le refaire
//! (`Folding`). Ce qui change le journal **en place** est la purge (le payload remplacé,
//! `event_purges` gagne une ligne) : toute nouvelle ligne d'`event_purges` jette le cache.
//! Le préfixe d'une fille de fork s'arrête à `up_to` : ce que sa mère écrit ensuite ne le
//! touche pas. Un pliage qui échoue n'est pas gardé.

use crate::derive::{Folding, Sealed};
use crate::replay::{Lineage, Owner, own_owner, row_seqs, session_events_after};
use penelope_store::rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// Sessions gardées au plus : au-delà, la moins récemment lue est oubliée.
const CAPACITY: usize = 64;

pub(crate) type SharedReadCache = Arc<Mutex<ReadCache>>;

/// Les surfaces pliées, par session.
#[derive(Default)]
pub(crate) struct ReadCache {
    sessions: HashMap<String, Folded>,
    tick: u64,
    /// Événements pliés par la dernière lecture (tests du coût).
    pub folded: usize,
}

/// Une session pliée jusqu'à `last_seq`.
pub(crate) struct Folded {
    /// Dernière ligne d'`event_purges` au moment du pliage.
    purges: i64,
    last_seq: i64,
    prefix: Sealed,
    offset: i64,
    owners: BTreeMap<i64, Owner>,
    folding: Folding,
    used: u64,
}

/// Dernière ligne d'`event_purges` : une purge remplace des payloads en place.
pub(crate) fn purge_mark(c: &Connection) -> Result<i64, String> {
    c.query_row(
        "SELECT COALESCE(MAX(rowid), 0) FROM event_purges",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

impl Folded {
    /// Plie la lignée chargée en entier ; `None` sans aucun événement.
    pub(crate) fn load(lineage: Lineage, purges: i64) -> Result<Option<Folded>, String> {
        let Some(last_seq) = lineage.events.last().map(|e| e.seq) else {
            return Ok(None);
        };
        let mut folding = Folding::default();
        folding
            .advance(&lineage.prefix, &lineage.events)
            .map_err(|e| e.to_string())?;
        Ok(Some(Folded {
            purges,
            last_seq,
            prefix: lineage.prefix,
            offset: lineage.offset,
            owners: lineage.owners,
            folding,
            used: 0,
        }))
    }

    /// Plie ce que le journal a reçu depuis ; rend le nombre d'événements pliés.
    pub(crate) fn catch_up(&mut self, c: &Connection, sid: &str) -> Result<usize, String> {
        let events = session_events_after(c, sid, self.last_seq).map_err(|e| e.to_string())?;
        let Some(last) = events.last() else {
            return Ok(0);
        };
        self.last_seq = last.seq;
        for e in &events {
            if let Some(owner) = own_owner(e) {
                self.owners.insert(self.offset + e.seq, owner);
            }
        }
        self.folding
            .advance(&self.prefix, &events)
            .map_err(|e| e.to_string())?;
        Ok(events.len())
    }

    pub(crate) fn surface(&self) -> &crate::derive::Surface {
        self.folding.surface()
    }

    /// Numéro de ligne de chaque adresse de message (`Lineage::row_seqs`).
    pub(crate) fn row_seqs(&self) -> BTreeMap<i64, i64> {
        row_seqs(&self.owners, self.surface())
    }
}

impl ReadCache {
    /// Retire la session pliée, si ce qu'elle a plié vaut encore (aucune purge depuis).
    pub(crate) fn take(&mut self, sid: &str, purges: i64) -> Option<Folded> {
        self.sessions.remove(sid).filter(|f| f.purges == purges)
    }

    /// Garde la session pliée.
    pub(crate) fn put(&mut self, sid: &str, mut folded: Folded, count: usize) {
        self.tick += 1;
        self.folded = count;
        folded.used = self.tick;
        if self.sessions.len() >= CAPACITY
            && !self.sessions.contains_key(sid)
            && let Some(oldest) = self
                .sessions
                .iter()
                .min_by_key(|(_, f)| f.used)
                .map(|(k, _)| k.clone())
        {
            self.sessions.remove(&oldest);
        }
        self.sessions.insert(sid.to_string(), folded);
    }

    /// Oublie tout : la lecture suivante replie depuis le début.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.sessions.clear();
    }
}
