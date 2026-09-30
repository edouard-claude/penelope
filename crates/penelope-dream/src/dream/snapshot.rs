//! État du vault au début de la phase Deep.

use super::*;

// ------------------------------------------------------------------ vault

/// État du vault au début de la phase Deep : uids, empreintes des fichiers, pratiques.
pub(super) struct VaultSnapshot {
    pub(super) uids: BTreeSet<String>,
    pub(super) entries_per_file: BTreeMap<String, usize>,
    pub(super) uid_file: BTreeMap<String, String>,
    hashes: BTreeMap<String, String>,
    pub(super) practices: BTreeSet<String>,
    pub(super) excerpts: Vec<(String, String)>,
    pub(super) practice_lines: Vec<String>,
    /// Textes normalisés des entrées des fichiers de mémoire, contre les doublons.
    pub(super) texts: BTreeSet<String>,
    /// Texte de chaque entrée des fichiers de mémoire, par uid : ce qu'une réécriture
    /// remplacerait, montré au propriétaire avant (issue #224).
    pub(super) uid_text: BTreeMap<String, String>,
    /// Entrées écrites par le propriétaire lui-même (provenance `owner`) : comme le
    /// profil, elles ne se réécrivent pas sans lui (issue #224).
    pub(super) owner_uids: BTreeSet<String>,
}

impl VaultSnapshot {
    pub(super) async fn read(s: &Services, vault: &Path) -> anyhow::Result<VaultSnapshot> {
        let mut snap = VaultSnapshot {
            uids: BTreeSet::new(),
            entries_per_file: BTreeMap::new(),
            uid_file: BTreeMap::new(),
            hashes: BTreeMap::new(),
            practices: BTreeSet::new(),
            excerpts: Vec::new(),
            practice_lines: Vec::new(),
            texts: BTreeSet::new(),
            uid_text: BTreeMap::new(),
            owner_uids: BTreeSet::new(),
        };
        for rel in markdown_files(vault) {
            if rel.starts_with("sources/")
                || rel.starts_with("journal/")
                || rel.starts_with("inbox/")
                || rel == DREAMS_FILE
            {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(vault.join(&rel)) else {
                continue;
            };
            snap.hashes.insert(
                rel.clone(),
                penelope_kernel::canonical::sha256_hex(raw.as_bytes()),
            );
            let (entries, _) = penelope_memory::vault::parse_entries(&raw);
            snap.entries_per_file.insert(rel.clone(), entries.len());
            for e in &entries {
                snap.uids.insert(e.uid.clone());
                snap.uid_file.insert(e.uid.clone(), rel.clone());
            }
            if let Some(stem) = rel
                .strip_prefix("pratiques/")
                .and_then(|f| f.strip_suffix(".md"))
            {
                if let Ok(p) = Practice::parse(&raw, stem) {
                    let default = p
                        .default_entry
                        .as_ref()
                        .map(|e| e.text.clone())
                        .unwrap_or_default();
                    snap.practice_lines
                        .push(format!("- {} : défaut « {} »", p.id, default));
                    snap.practices.insert(p.id);
                }
                continue;
            }
            if WRITABLE_FILES.contains(&rel.as_str()) {
                snap.texts.extend(
                    entries
                        .iter()
                        .map(|e| penelope_memory::grid::normalized(&e.text)),
                );
                for e in &entries {
                    snap.uid_text.insert(e.uid.clone(), e.text.clone());
                    if s.memory.origin_of(&e.uid).await.ok().flatten() == Some(Origin::Owner) {
                        snap.owner_uids.insert(e.uid.clone());
                    }
                }
                snap.excerpts
                    .push((rel.clone(), raw.chars().take(FILE_EXCERPT_CHARS).collect()));
            }
        }
        Ok(snap)
    }

    /// L'entrée `uid` est au profil du propriétaire.
    pub(super) fn in_profile(&self, uid: &str) -> bool {
        self.uid_file.get(uid).is_some_and(|f| f == "profil.md")
    }

    /// uids des fichiers modifiés depuis la lecture : leurs opérations sont reportées.
    pub(super) fn changed_since_read(&self, vault: &Path) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (rel, hash) in &self.hashes {
            let now = std::fs::read_to_string(vault.join(rel))
                .map(|raw| penelope_kernel::canonical::sha256_hex(raw.as_bytes()))
                .unwrap_or_default();
            if &now != hash {
                out.extend(
                    self.uid_file
                        .iter()
                        .filter(|(_, f)| *f == rel)
                        .map(|(u, _)| u.clone()),
                );
            }
        }
        out
    }
}

/// Fichiers Markdown du vault, relatifs, hors répertoires cachés et archives.
pub(super) fn markdown_files(vault: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "archive" {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else if name.ends_with(".md")
                && let Ok(rel) = p.strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(vault, vault, &mut out);
    out.sort();
    out
}
