//! L'atlas du vault (#428) : le graphe des notes et de leurs liens, recalculé à la
//! demande depuis les fichiers, pour `penelope vault atlas`.
//!
//! Le périmètre et la résolution sont ceux du wiki ([`crate::wiki`]) : les notes
//! Markdown de `vault_files` (ni dossiers cachés, ni `archive/`, ni les journaux en ajout
//! seul `log.md` et `DREAMS.md`, dont les liens recopiés ne sont pas des liens, #413), les
//! wikilinks hors blocs de code, résolus par [`Resolver`] (nom, chemin, puis alias). Une
//! cible introuvable devient une « référence » non résolue. Aucun appel au modèle, aucune
//! écriture : l'atlas ne fait que lire.
//!
//! [`fingerprint`] résume le vault (chemins, tailles, dates de modification) sans lire les
//! notes : le serveur ne recalcule que s'il a changé.

use crate::wiki::{Resolver, vault_files, wiki_links};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Node {
    pub id: usize,
    pub title: String,
    /// Chemin relatif d'une note ; cible telle qu'écrite pour une référence.
    pub path: String,
    /// Dossier de premier niveau, `racine` à la racine, `références` pour une cible
    /// introuvable.
    pub cat: String,
    /// `note` ou `ref`.
    pub kind: &'static str,
    pub degree: usize,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Link {
    pub a: usize,
    pub b: usize,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Stats {
    pub notes: usize,
    pub references: usize,
    pub liens: usize,
    pub occurrences: usize,
    pub par_dossier: BTreeMap<String, usize>,
    #[serde(rename = "isolées")]
    pub isolees: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Atlas {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    pub cats: Vec<String>,
    pub stats: Stats,
}

/// Les notes de l'atlas : Markdown, hors pièces jointes et hors tout chemin qui contient
/// « secret » (même le nom d'une telle note ne sort pas).
fn notes_of(vault: &Path) -> Vec<String> {
    vault_files(vault)
        .into_iter()
        .filter(|rel| {
            rel.ends_with(".md")
                && !rel.starts_with("attachments/")
                && !rel.to_lowercase().contains("secret")
        })
        .collect()
}

/// Empreinte du vault : change dès qu'une note est créée, modifiée, renommée ou
/// supprimée. Ne lit que les métadonnées.
pub fn fingerprint(vault: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for rel in vault_files(vault) {
        rel.hash(&mut h);
        if let Ok(m) = std::fs::metadata(vault.join(&rel)) {
            m.len().hash(&mut h);
            if let Ok(t) = m.modified()
                && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
            {
                d.as_nanos().hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

fn title_of(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

fn cat_of(rel: &str) -> String {
    match rel.split_once('/') {
        Some((top, _)) => top.to_string(),
        None => "racine".into(),
    }
}

/// Construit l'atlas. `Err` : le vault ne se lit pas (dossier absent, droits).
pub fn build(vault: &Path) -> std::io::Result<Atlas> {
    std::fs::read_dir(vault)?;
    let resolver = Resolver::scan(vault);
    let notes = notes_of(vault);
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut nodes: Vec<Node> = notes
        .iter()
        .enumerate()
        .map(|(id, rel)| {
            index.insert(rel.clone(), id);
            Node {
                id,
                title: title_of(rel),
                path: rel.clone(),
                cat: cat_of(rel),
                kind: "note",
                degree: 0,
                x: 0.0,
                y: 0.0,
            }
        })
        .collect();
    let mut refs: BTreeMap<String, usize> = BTreeMap::new();
    let mut edges: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut occurrences = 0;
    for (from, rel) in notes.iter().enumerate() {
        let Ok(raw) = std::fs::read_to_string(vault.join(rel)) else {
            continue;
        };
        for link in wiki_links(&raw) {
            if link.target.is_empty() {
                continue;
            }
            occurrences += 1;
            let to = match resolver
                .resolve(&link.target)
                .or_else(|| resolver.resolve_alias(&link.target))
            {
                // Une pièce jointe ou un fichier hors atlas n'est pas un nœud.
                Some(path) => match index.get(&path) {
                    Some(to) => *to,
                    None => continue,
                },
                None => {
                    let key = link.target.to_lowercase();
                    *refs.entry(key).or_insert_with(|| {
                        let id = nodes.len();
                        nodes.push(Node {
                            id,
                            title: link.target.clone(),
                            path: link.target.clone(),
                            cat: "références".into(),
                            kind: "ref",
                            degree: 0,
                            x: 0.0,
                            y: 0.0,
                        });
                        id
                    })
                }
            };
            if to != from {
                edges.insert((from.min(to), from.max(to)));
            }
        }
    }
    for (a, b) in &edges {
        nodes[*a].degree += 1;
        nodes[*b].degree += 1;
    }
    let links: Vec<Link> = edges.iter().map(|(a, b)| Link { a: *a, b: *b }).collect();
    layout(&mut nodes, &links);
    let mut stats = Stats {
        notes: notes.len(),
        references: refs.len(),
        liens: links.len(),
        occurrences,
        ..Default::default()
    };
    for n in nodes.iter().filter(|n| n.kind == "note") {
        *stats.par_dossier.entry(n.cat.clone()).or_default() += 1;
        if n.degree == 0 {
            stats.isolees += 1;
        }
    }
    let cats: Vec<String> = nodes
        .iter()
        .map(|n| n.cat.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(Atlas {
        nodes,
        links,
        cats,
        stats,
    })
}

/// Placement déterministe (Fruchterman-Reingold) : même vault, même dessin. Départ sur
/// une spirale par rang, sans hasard.
fn layout(nodes: &mut [Node], links: &[Link]) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let area = 1200.0 * 1200.0 * (n as f64 / 100.0).max(1.0);
    let k = (area / n as f64).sqrt();
    for (i, node) in nodes.iter_mut().enumerate() {
        let angle = i as f64 * 2.399_963; // angle d'or
        let r = k * (i as f64).sqrt();
        node.x = r * angle.cos();
        node.y = r * angle.sin();
    }
    let iterations = if n > 2000 { 60 } else { 200 };
    let mut temp = k * 3.0;
    let mut disp = vec![(0.0f64, 0.0f64); n];
    for _ in 0..iterations {
        disp.iter_mut().for_each(|d| *d = (0.0, 0.0));
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (nodes[i].x - nodes[j].x, nodes[i].y - nodes[j].y);
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = k * k / d;
                let (fx, fy) = (dx / d * f, dy / d * f);
                disp[i].0 += fx;
                disp[i].1 += fy;
                disp[j].0 -= fx;
                disp[j].1 -= fy;
            }
        }
        for l in links {
            let (dx, dy) = (nodes[l.a].x - nodes[l.b].x, nodes[l.a].y - nodes[l.b].y);
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            let f = d * d / k;
            let (fx, fy) = (dx / d * f, dy / d * f);
            disp[l.a].0 -= fx;
            disp[l.a].1 -= fy;
            disp[l.b].0 += fx;
            disp[l.b].1 += fy;
        }
        for (node, (dx, dy)) in nodes.iter_mut().zip(&disp) {
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            node.x += dx / d * d.min(temp);
            node.y += dy / d * d.min(temp);
            // Une note isolée reste à portée du centre.
            node.x *= 0.995;
            node.y *= 0.995;
        }
        temp = (temp * 0.97).max(k * 0.05);
    }
    for node in nodes.iter_mut() {
        node.x = (node.x * 10.0).round() / 10.0;
        node.y = (node.y * 10.0).round() / 10.0;
    }
}

#[cfg(test)]
mod tests;
