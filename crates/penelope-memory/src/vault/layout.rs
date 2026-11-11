//! Corps d'une pratique tel qu'il est écrit dans le vault.
//!
//! `Practice::render` reconstruisait le fichier depuis ses trois listes : un paragraphe
//! d'explication, une section « Notes », un exemple ajoutés à la main disparaissaient à
//! la première réécriture (#453). Le corps est gardé ligne à ligne ; le rendu ne touche
//! que les lignes des entrées qui ont changé, ajoute les nouvelles et retire celles qui
//! ne sont plus dans la pratique.

use super::practice::section_matches;
use super::{Fences, VaultEntry, standalone_block_id};
use std::collections::BTreeMap;

/// Sections du gabarit, dans l'ordre de `Layout::read` et `Layout::render`.
pub(super) const SECTIONS: [&str; 3] = ["Défaut", "Exceptions", "Écarts observés"];

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Layout {
    /// Lignes du corps, frontmatter exclu.
    lines: Vec<String>,
    /// Entrées tenues par la pratique, avec leur état à la lecture.
    slots: Vec<Slot>,
    /// Titres `## ` hors bloc de code : ligne, et section du gabarit s'il en est une.
    headings: Vec<(usize, Option<usize>)>,
    title: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Slot {
    section: usize,
    /// Lignes occupées : une seule pour une entrée de liste, l'encadré et son identifiant
    /// de bloc pour un encadré.
    first: usize,
    last: usize,
    was: VaultEntry,
}

/// Une entrée du gabarit s'écrit en entrée de liste.
pub(super) fn entry_line(e: &VaultEntry) -> String {
    format!("- {} {}", e.text, e.annotations.render())
}

impl Layout {
    /// `raw` est le fichier entier, `offset` le nombre de lignes du frontmatter ; `held`
    /// donne les entrées de chaque section du gabarit, numéros de ligne compris.
    pub(super) fn read(raw: &str, offset: usize, title: &str, held: [&[VaultEntry]; 3]) -> Layout {
        let lines: Vec<String> = raw
            .replace("\r\n", "\n")
            .split('\n')
            .skip(offset)
            .map(String::from)
            .collect();
        let mut fences = Fences::default();
        let headings = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| !fences.covers(l))
            .filter_map(|(i, l)| {
                let h = l.trim().strip_prefix("## ")?.trim();
                Some((i, SECTIONS.iter().position(|s| section_matches(h, s))))
            })
            .collect();
        let mut slots = Vec::new();
        for (section, entries) in held.iter().enumerate() {
            for e in entries.iter() {
                let Some(last) = e.line.checked_sub(offset + 1).filter(|l| *l < lines.len()) else {
                    continue;
                };
                slots.push(Slot {
                    section,
                    first: callout_start(&lines, last),
                    last,
                    was: e.clone(),
                });
            }
        }
        Layout {
            lines,
            slots,
            headings,
            title: title.to_string(),
        }
    }

    /// Corps à écrire, ou `None` pour une pratique qui ne vient pas d'un fichier.
    pub(super) fn render(&self, title: &str, held: [Vec<&VaultEntry>; 3]) -> Option<String> {
        if self.lines.is_empty() {
            return None;
        }
        // Lignes remplacées (première ligne → dernière, nouveau texte ou retrait), lignes
        // insérées avant un rang, et sections que le fichier n'avait pas.
        let mut swap: BTreeMap<usize, (usize, Option<String>)> = BTreeMap::new();
        let mut add: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut tail: Vec<String> = Vec::new();
        for (section, entries) in held.iter().enumerate() {
            let slots: Vec<&Slot> = self.slots.iter().filter(|s| s.section == section).collect();
            for slot in &slots {
                match entries.iter().find(|e| e.uid == slot.was.uid) {
                    Some(e) if e.text == slot.was.text && e.annotations == slot.was.annotations => {
                    }
                    Some(e) => {
                        swap.insert(slot.first, (slot.last, Some(entry_line(e))));
                    }
                    None => {
                        swap.insert(slot.first, (slot.last, None));
                    }
                }
            }
            let fresh: Vec<String> = entries
                .iter()
                .filter(|e| !slots.iter().any(|s| s.was.uid == e.uid))
                .map(|e| entry_line(e))
                .collect();
            if fresh.is_empty() {
                continue;
            }
            // Après la dernière entrée de la section ; sans entrée, à la fin de la
            // section ; sans section, à la fin du fichier.
            let after_last = slots.iter().map(|s| s.last + 1).max();
            match after_last.or_else(|| self.end_of(section)) {
                Some(at) => add.entry(at).or_default().extend(fresh),
                None => {
                    tail.push(String::new());
                    tail.push(format!("## {}", SECTIONS[section]));
                    tail.extend(fresh);
                }
            }
        }
        let retitled = (title != self.title)
            .then(|| self.lines.iter().position(|l| l.starts_with("# ")))
            .flatten();

        let mut out: Vec<String> = Vec::with_capacity(self.lines.len() + 4);
        let mut i = 0;
        while i <= self.lines.len() {
            if let Some(new) = add.get(&i) {
                out.extend(new.iter().cloned());
            }
            if i == self.lines.len() {
                break;
            }
            if let Some((last, with)) = swap.get(&i) {
                out.extend(with.clone());
                i = last + 1;
                // Une entrée retirée entre deux lignes vides n'en laisse qu'une.
                let blank = |l: Option<&String>| l.is_some_and(|l| l.trim().is_empty());
                if with.is_none()
                    && blank(out.last())
                    && blank(self.lines.get(i))
                    && !add.contains_key(&i)
                {
                    i += 1;
                }
            } else {
                out.push(if retitled == Some(i) {
                    format!("# {title}")
                } else {
                    self.lines[i].clone()
                });
                i += 1;
            }
        }
        if !tail.is_empty() {
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            out.extend(tail);
            out.push(String::new());
        }
        Some(out.join("\n"))
    }

    /// Rang où ajouter une entrée à une section sans entrée : après sa dernière ligne non
    /// vide, donc avant le blanc qui la sépare de la suivante.
    fn end_of(&self, section: usize) -> Option<usize> {
        let at = self
            .headings
            .iter()
            .position(|(_, s)| *s == Some(section))?;
        let start = self.headings[at].0;
        let end = self
            .headings
            .get(at + 1)
            .map_or(self.lines.len(), |(next, _)| *next);
        (start..end)
            .rev()
            .find(|i| !self.lines[*i].trim().is_empty())
            .map(|i| i + 1)
    }
}

/// Première ligne d'une entrée qui finit à `last` : `last` pour une entrée de liste, le
/// début de l'encadré quand `last` n'est qu'un identifiant de bloc.
fn callout_start(lines: &[String], last: usize) -> usize {
    if standalone_block_id(&lines[last]).is_none() {
        return last;
    }
    let mut first = last;
    for i in (0..last).rev() {
        let t = lines[i].trim();
        if t.starts_with('>') {
            first = i;
        } else if !t.is_empty() {
            break;
        }
    }
    first
}
