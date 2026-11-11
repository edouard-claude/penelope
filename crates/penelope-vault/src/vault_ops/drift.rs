//! Ce que l'index porte encore et que le vault ne porte plus (#449).
//!
//! `reindex` ne faisait que des `upsert` : une ligne effacée à la main, une note
//! supprimée restaient actives dans l'index, rappelées et injectées. Le propriétaire qui
//! corrigeait sa mémoire dans son éditeur n'était pas obéi.
//!
//! La comparaison porte sur toute la passe, pas fichier par fichier : une ligne déplacée
//! d'une note à l'autre garde son identifiant, donc son entrée. Seul ce qui a été relu
//! compte : une note illisible ou restée dans le nuage ne fait rien retirer. Et une passe
//! qui retirerait une grande part de l'index ne retire rien sans confirmation
//! ([`Drift::held`]) : un vault démonté ressemble à un vault vidé.

use penelope_app::services::Services;
use penelope_memory::vault;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// En dessous, une passe retire sans confirmation, quelle que soit la part.
const HELD_FROM: usize = 5;
/// Part de l'index (un quart) au-delà de laquelle une passe ne retire rien d'elle-même.
const HELD_SHARE: usize = 4;

/// Ce qu'une passe a relu du vault.
#[derive(Debug, Default)]
pub struct Scan {
    /// Identifiants portés par une ligne d'une note relue.
    uids: HashSet<String>,
    /// Notes relues ligne à ligne.
    notes: HashSet<String>,
    /// Fiches de documents ingérés : leurs passages ne se comparent pas ligne à ligne,
    /// ils ne partent qu'avec la fiche.
    sources: HashSet<String>,
}

impl Scan {
    /// Une note relue, telle qu'elle est sur le disque après la passe.
    pub fn note(&mut self, rel: &str, raw: &str) {
        self.notes.insert(rel.to_string());
        // Un identifiant dans un bloc de code ne porte pas d'entrée (#453) : celle qui en
        // était née à tort quitte l'index comme une ligne effacée.
        let mut fences = penelope_memory::fence::Fences::default();
        for line in raw.lines().filter(|l| !fences.covers(l)) {
            self.uids.extend(vault::line_uid(line));
            self.uids.extend(vault::block_id(line));
            self.uids.extend(vault::standalone_block_id(line));
        }
    }

    /// Une fiche de document relue.
    pub fn source(&mut self, rel: &str) {
        self.sources.insert(rel.to_string());
    }

    /// Le vault relu sans rien écrire, hors d'une réindexation (`mem audit`, `doctor`).
    pub fn of(vault: &Path) -> Scan {
        let mut files = Vec::new();
        super::collect_markdown(vault, vault, &mut files);
        let mut scan = Scan::default();
        for rel in files {
            let Ok(raw) = std::fs::read_to_string(vault.join(&rel)) else {
                continue;
            };
            if rel.starts_with(&format!("{}/", penelope_memory::ingest::SOURCES_DIR)) {
                scan.source(&rel);
            } else if crate::vault_inventory::excluded(&rel).is_none() {
                scan.note(&rel, &raw);
            }
        }
        scan
    }

    /// Vrai si une entrée de `file` dont l'identifiant n'a été relu nulle part est perdue.
    /// Relue, la note fait foi. Sinon, seule une note qui n'existe plus fait retirer ses
    /// entrées : illisible, cachée, archivée ou exclue, rien ne bouge.
    fn lost(&self, vault: &Path, file: &str) -> bool {
        if file.is_empty()
            || self.sources.contains(file)
            || crate::vault_inventory::excluded(file).is_some()
        {
            return false;
        }
        self.notes.contains(file) || absent(vault, file)
    }
}

/// Vrai si le fichier n'est plus dans le vault. Un fichier que la synchronisation n'a pas
/// encore descendu (`.nom.icloud` à sa place) n'est pas absent.
fn absent(vault: &Path, file: &str) -> bool {
    let path = vault.join(file);
    let missing = matches!(
        std::fs::symlink_metadata(&path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    let placeholder = path
        .file_name()
        .map(|n| path.with_file_name(format!(".{}.icloud", n.to_string_lossy())));
    missing && !placeholder.is_some_and(|p| p.exists())
}

/// Entrée vivante de l'index que le vault ne porte plus.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Orphan {
    pub uid: String,
    pub file: String,
}

/// Écart de l'index au vault.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Drift {
    /// Entrées vivantes de l'index au moment de la comparaison.
    pub live: usize,
    /// Fichiers relus : zéro, le vault est vide, démonté ou illisible.
    pub read: usize,
    pub gone: Vec<Orphan>,
}

impl Drift {
    /// Garde contre la catastrophe : rien n'a été relu, ou la passe retirerait plus du
    /// quart de l'index. Un vault démonté, un dossier pas encore synchronisé ressemblent à
    /// des notes supprimées : la passe ne retire rien, et le dit.
    pub fn held(&self) -> bool {
        !self.gone.is_empty()
            && (self.read == 0
                || (self.gone.len() > HELD_FROM && self.gone.len() * HELD_SHARE > self.live))
    }
}

/// Nombre d'entrées par fichier, dans l'ordre des noms.
pub fn by_file(gone: &[Orphan]) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for o in gone {
        *out.entry(o.file.clone()).or_insert(0) += 1;
    }
    out
}

/// Confronte l'index à ce qu'une passe a relu.
pub async fn drift(s: &Services, vault: &Path, scan: &Scan) -> Result<Drift, String> {
    let live = s.memory.live().await.map_err(|e| e.to_string())?;
    let mut drift = Drift {
        live: live.len(),
        read: scan.notes.len() + scan.sources.len(),
        gone: Vec::new(),
    };
    for (uid, file) in live {
        if !scan.uids.contains(&uid) && scan.lost(vault, &file) {
            drift.gone.push(Orphan { uid, file });
        }
    }
    Ok(drift)
}

#[cfg(test)]
mod tests;
