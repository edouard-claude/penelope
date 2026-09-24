//! Wiki de concepts (issue #22) : une page par source, une page par concept, reliées.
//!
//! Le résumé d'un document ingéré nomme aussi ses concepts et entités (personne, client,
//! projet, terme métier) et les termes employés sans définition. Chaque concept crée ou
//! complète `concepts/<slug>.md` (définition, alias, sources), la fiche source reçoit
//! ses liens `[[slug]]`, les entrées de `memoire.md` et `projets.md` qui citent le
//! concept aussi, `concepts/_a-definir.md` liste les termes à définir et `index.md` sert de
//! point d'entrée du wiki. `mem_neighbors` parcourt le graphe.

use crate::runtime::Daemon;
use penelope_memory::{IndexedEntry, Level, Origin, Provenance};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

pub const DIR: &str = "concepts";
pub const TO_DEFINE: &str = "concepts/_a-definir.md";
pub const INDEX: &str = "index.md";
/// Similarité au-delà de laquelle deux noms désignent le même concept.
const SAME_CONCEPT: f32 = 0.92;

/// Concept nommé par le résumé d'un document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Concept {
    pub nom: String,
    pub definition: String,
    pub alias: Vec<String>,
}

/// Concepts et termes à définir d'une réponse de résumé.
pub fn parse(v: &Value) -> (Vec<Concept>, Vec<String>) {
    let clean = |s: &str| {
        s.replace(['\n', '\r', '[', ']', '|'], " ")
            .trim()
            .to_string()
    };
    let concepts = v["concepts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let nom = clean(c["nom"].as_str()?);
            (!nom.is_empty() && nom.chars().count() <= 80).then(|| Concept {
                definition: clean(c["definition"].as_str().unwrap_or_default())
                    .chars()
                    .take(400)
                    .collect(),
                alias: c["alias"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str().map(clean))
                    .filter(|a| !a.is_empty() && *a != nom)
                    .collect(),
                nom,
            })
        })
        .take(12)
        .collect();
    let undefined = v["a_definir"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().map(clean))
        .filter(|t| !t.is_empty() && t.chars().count() <= 80)
        .take(12)
        .collect();
    (concepts, undefined)
}

/// Forme de comparaison : minuscules, sans accents ni pluriel simple.
pub fn normalize(s: &str) -> String {
    let base: String = s
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            '-' | '_' | '’' | '\'' => ' ',
            c => c,
        })
        .collect();
    base.split_whitespace()
        .map(|w| {
            if w.chars().count() > 4 && (w.ends_with('s') || w.ends_with('x')) {
                &w[..w.len() - 1]
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Page de concept relue.
#[derive(Debug, Clone, Default, PartialEq)]
struct Page {
    slug: String,
    nom: String,
    alias: Vec<String>,
    definition: (String, String),
    sources: Vec<(String, String, String)>,
    /// Date de création conservée d'une réécriture à l'autre.
    created: String,
}

fn uid() -> String {
    penelope_kernel::ids::Ulid::new().to_string()
}

/// Texte et uid d'une entrée de liste (identifiant de bloc ou ancien commentaire).
fn line_uid(line: &str) -> Option<(String, String)> {
    let item = line.trim_start().strip_prefix("- ")?;
    let id = penelope_memory::vault::line_uid(item)?;
    Some((penelope_memory::vault::strip_annotations(item), id))
}

fn read_page(vault: &Path, slug: &str) -> Option<Page> {
    let raw = std::fs::read_to_string(vault.join(DIR).join(format!("{slug}.md"))).ok()?;
    let fm = penelope_kernel::frontmatter::parse(&raw).ok()?;
    let mut page = Page {
        slug: slug.to_string(),
        nom: fm.string("nom"),
        alias: penelope_memory::wiki::aliases_of(&fm),
        created: fm.string("created"),
        ..Default::default()
    };
    let mut section = "";
    for line in fm.body.lines() {
        if line.starts_with("## ") {
            section = if line.contains("Sources") {
                "sources"
            } else {
                ""
            };
        } else if let Some((text, id)) = line_uid(line) {
            if section == "sources" {
                let slug = penelope_memory::vault::links(&text)
                    .first()
                    .cloned()
                    .unwrap_or_default();
                page.sources.push((slug, text, id));
            } else if page.definition.0.is_empty() {
                page.definition = (text, id);
            }
        }
    }
    (!page.nom.is_empty()).then_some(page)
}

fn entry(text: &str, id: &str) -> String {
    penelope_memory::edit::entry_line(
        text,
        &penelope_memory::vault::Annotations {
            uid: Some(id.to_string()),
            ..Default::default()
        },
    )
}

/// Page de concept : propriétés `aliases` et `tags` en listes, entrées à identifiant de
/// bloc (issue #29).
fn render_page(p: &Page) -> String {
    use penelope_kernel::frontmatter::FmValue;
    let mut fields = BTreeMap::new();
    fields.insert("type".to_string(), FmValue::Str("concept".into()));
    fields.insert("nom".to_string(), FmValue::Str(p.nom.clone()));
    fields.insert("aliases".to_string(), FmValue::List(p.alias.clone()));
    fields.insert("tags".to_string(), FmValue::List(vec!["concepts".into()]));
    if !p.created.is_empty() {
        fields.insert("created".to_string(), FmValue::Str(p.created.clone()));
    }
    let mut body = format!("\n# {}\n\n", p.nom);
    if !p.definition.0.is_empty() {
        body.push_str(&entry(&p.definition.0, &p.definition.1));
        body.push('\n');
    }
    body.push_str("\n## Sources\n\n");
    for (_, text, id) in &p.sources {
        body.push_str(&entry(text, id));
        body.push('\n');
    }
    penelope_kernel::frontmatter::render(&fields, &body)
}

/// Réécrit les pages de concept au format courant (`aliases` en liste, identifiants de
/// bloc) ; rend le nombre de pages réécrites.
pub fn migrate_pages(vault: &Path, day: &str) -> Result<usize, String> {
    let mut n = 0;
    for mut page in pages(vault) {
        let rel = format!("{DIR}/{}.md", page.slug);
        let current = std::fs::read_to_string(vault.join(&rel)).unwrap_or_default();
        if page.created.is_empty() {
            page.created = day.to_string();
        }
        let rendered = render_page(&page);
        // Une page déjà au format garde son `updated`.
        if penelope_memory::wiki::body_of(&current) == penelope_memory::wiki::body_of(&rendered)
            && !current.contains("\nalias:")
        {
            continue;
        }
        crate::vault_ops::save_note(vault, &rel, &rendered, day)?;
        n += 1;
    }
    Ok(n)
}

/// Sources ingérées pendant une session, les plus récentes d'abord.
pub async fn session_sources(s: &crate::runtime::Services, session_id: &str) -> Vec<String> {
    let sid = session_id.to_string();
    s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT e.slug FROM mem_entries e JOIN mem_provenance p ON p.uid = e.uid
                 WHERE p.session_id = ?1 AND e.file LIKE 'sources/%' AND e.slug IS NOT NULL
                 GROUP BY e.slug ORDER BY MAX(p.observed_at) DESC LIMIT 5",
            )?;
            let rows = st.query_map([&sid], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap_or_default()
}

/// Complète un texte des wikilinks des concepts qu'il cite (journal, mémoire).
pub fn link_known_concepts(vault: &Path, text: &str) -> String {
    let words = format!(" {} ", normalize(text));
    let mut out = text.to_string();
    for p in pages(vault) {
        let cited = std::iter::once(&p.nom)
            .chain(&p.alias)
            .any(|name| words.contains(&format!(" {} ", normalize(name))));
        if cited && !out.contains(&format!("[[{}]]", p.slug)) {
            out.push_str(&format!(" [[{}]]", p.slug));
        }
    }
    out
}

fn pages(vault: &Path) -> Vec<Page> {
    let mut out: Vec<Page> = std::fs::read_dir(vault.join(DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let slug = name.strip_suffix(".md")?;
            (!slug.starts_with('_'))
                .then(|| read_page(vault, slug))
                .flatten()
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Slug d'un nouveau concept : un nom libre dans tout le vault, pour que `[[slug]]` reste
/// univoque.
fn concept_slug(vault: &Path, nom: &str) -> String {
    let base = penelope_memory::ingest::slugify(nom);
    penelope_memory::wiki::Resolver::scan(vault).unique_name(&base, "concept")
}

/// Indexe les entrées d'une page de concept sous son slug.
async fn index_page(d: &Daemon, page: &Page, prov: &Provenance) -> anyhow::Result<()> {
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let rel = format!("{DIR}/{}.md", page.slug);
    let raw = std::fs::read_to_string(vault.join(&rel))?;
    let day: String = s.clock.now_rfc3339().chars().take(10).collect();
    let (entries, _) = penelope_memory::vault::parse_entries(&raw);
    for e in entries {
        let ie = IndexedEntry::from_vault(&e, &rel, Level::Cure, "entite", Some(&page.slug), &day);
        s.memory.upsert(&ie, prov).await?;
    }
    Ok(())
}

/// Relie une source à ses concepts ; rend les slugs des concepts touchés.
pub async fn apply(
    d: &Daemon,
    source_slug: &str,
    source_title: &str,
    origin: Origin,
    concepts: &[Concept],
    undefined: &[String],
) -> anyhow::Result<Vec<String>> {
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(vault.join(DIR))?;
    let prov = Provenance {
        origin,
        session_kind: "ingestion".into(),
        observed_at: s.clock.now_rfc3339(),
        supersedes_uid: None,
        source_ref: Some(format!("sources/{source_slug}.md")),
        session_id: None,
    };
    let mut existing = pages(&vault);
    let mut touched: Vec<String> = Vec::new();
    let day = crate::vault_ops::day(s);

    for c in concepts {
        let names: Vec<String> = std::iter::once(&c.nom)
            .chain(&c.alias)
            .map(|n| normalize(n))
            .collect();
        let mut found = existing.iter().position(|p| {
            std::iter::once(&p.nom)
                .chain(&p.alias)
                .any(|n| names.contains(&normalize(n)))
        });
        // Sans correspondance par les mots : par le sens, si les embeddings répondent.
        if found.is_none() && !existing.is_empty() {
            found = same_by_meaning(d, &c.nom, &existing).await;
        }
        let page = match found {
            Some(i) => &mut existing[i],
            None => {
                existing.push(Page {
                    slug: concept_slug(&vault, &c.nom),
                    nom: c.nom.clone(),
                    created: day.clone(),
                    ..Default::default()
                });
                existing.last_mut().expect("page ajoutée")
            }
        };
        for alias in std::iter::once(&c.nom).chain(&c.alias) {
            let n = normalize(alias);
            if normalize(&page.nom) != n && !page.alias.iter().any(|a| normalize(a) == n) {
                page.alias.push(alias.clone());
            }
        }
        if page.definition.0.is_empty() && !c.definition.is_empty() {
            page.definition = (c.definition.clone(), uid());
        }
        if !page.sources.iter().any(|(slug, _, _)| slug == source_slug) {
            let target = penelope_memory::wiki::Resolver::scan(&vault).link_target(&format!(
                "{}/{source_slug}.md",
                penelope_memory::ingest::SOURCES_DIR
            ));
            page.sources.push((
                source_slug.to_string(),
                format!("[[{target}]] · {source_title}"),
                uid(),
            ));
        }
        let rel = format!("{DIR}/{}.md", page.slug);
        crate::vault_ops::save_note(&vault, &rel, &render_page(page), &day)
            .map_err(anyhow::Error::msg)?;
        index_page(d, page, &prov).await?;
        if !touched.contains(&page.slug) {
            touched.push(page.slug.clone());
        }
    }

    if !touched.is_empty() {
        link_source(d, source_slug, &touched, &prov).await?;
        link_memory(d, &existing).await?;
    }
    update_to_define(&vault, source_slug, undefined, &existing, &day)?;
    write_index(d, &existing, &day).await?;
    Ok(touched)
}

async fn same_by_meaning(d: &Daemon, nom: &str, pages: &[Page]) -> Option<usize> {
    let mut texts = vec![nom.to_string()];
    texts.extend(pages.iter().map(|p| p.nom.clone()));
    let call = crate::embeddings::embed_texts(d, &texts);
    let (_, vectors) = tokio::time::timeout(std::time::Duration::from_secs(5), call)
        .await
        .ok()?
        .ok()?;
    let (query, others) = vectors.split_first()?;
    others
        .iter()
        .enumerate()
        .map(|(i, v)| (i, penelope_store::cosine_similarity(query, v)))
        .filter(|(_, sim)| *sim >= SAME_CONCEPT)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

/// Réindexe la ligne de concepts d'une fiche source (`penelope mem reindex`).
pub async fn reindex_source_links(
    s: &crate::runtime::Services,
    source_slug: &str,
    raw: &str,
    origin: Origin,
) -> anyhow::Result<()> {
    let id = format!("concepts-{source_slug}");
    let Some(line) = raw
        .lines()
        .find(|l| penelope_memory::vault::line_uid(l).as_deref() == Some(id.as_str()))
    else {
        return Ok(());
    };
    let Some((text, _)) = line_uid(line) else {
        return Ok(());
    };
    let prov = Provenance {
        origin,
        session_kind: "maintenance".into(),
        observed_at: s.clock.now_rfc3339(),
        supersedes_uid: None,
        source_ref: Some(format!("sources/{source_slug}.md")),
        session_id: None,
    };
    s.memory
        .upsert(&source_links_entry(s, source_slug, text), &prov)
        .await?;
    Ok(())
}

fn source_links_entry(
    s: &crate::runtime::Services,
    source_slug: &str,
    text: String,
) -> IndexedEntry {
    IndexedEntry {
        uid: format!("concepts-{source_slug}"),
        file: format!("{}/{source_slug}.md", penelope_memory::ingest::SOURCES_DIR),
        anchor: Some("Concepts".into()),
        level: Level::Cure,
        etype: "note".into(),
        slug: Some(source_slug.to_string()),
        content_hash: penelope_kernel::canonical::sha256_hex(text.as_bytes()),
        text,
        quand: None,
        importance: None,
        projet: None,
        confiance: None,
        statut: "active".into(),
        depuis: None,
        maj: s.clock.now_rfc3339().chars().take(10).collect(),
        pinned: false,
        declencheurs: Vec::new(),
        retired_at: None,
    }
}

/// Section `## Concepts` de la fiche source, indexée pour que la source pointe vers ses
/// concepts.
async fn link_source(
    d: &Daemon,
    source_slug: &str,
    concepts: &[String],
    prov: &Provenance,
) -> anyhow::Result<()> {
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let rel = format!("{}/{source_slug}.md", penelope_memory::ingest::SOURCES_DIR);
    let path = vault.join(&rel);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let id = format!("concepts-{source_slug}");
    let mut all: Vec<String> = raw
        .lines()
        .find(|l| penelope_memory::vault::line_uid(l).as_deref() == Some(id.as_str()))
        .map(penelope_memory::vault::links)
        .unwrap_or_default();
    for c in concepts {
        if !all.contains(c) {
            all.push(c.clone());
        }
    }
    let text = format!(
        "Concepts : {}",
        all.iter()
            .map(|c| format!("[[{c}]]"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let line = entry(&text, &id);
    let header = penelope_memory::ingest::CONTENT_HEADER;
    crate::vault_ops::update_note(&vault, &rel, None, &crate::vault_ops::day(s), |raw| {
        // La section se place avant le contenu extrait, ou avant l'original embarqué.
        let anchor = [penelope_memory::ingest::ORIGINAL_HEADER, header]
            .iter()
            .filter_map(|h| raw.find(h))
            .min();
        Ok(match raw.find("## Concepts") {
            Some(start) => {
                let end = raw[start + 1..]
                    .find("\n## ")
                    .map(|e| start + 2 + e)
                    .unwrap_or(raw.len());
                format!("{}## Concepts\n\n{line}\n\n{}", &raw[..start], &raw[end..])
            }
            None => match anchor {
                Some(i) => format!("{}## Concepts\n\n{line}\n\n{}", &raw[..i], &raw[i..]),
                None => format!("{raw}\n## Concepts\n\n{line}\n"),
            },
        })
    })
    .map_err(anyhow::Error::msg)?;
    s.memory
        .upsert(&source_links_entry(s, source_slug, text), prov)
        .await?;
    Ok(())
}

/// Entrées de `memoire.md` et `projets.md` qui citent un concept : le lien `[[slug]]`
/// s'ajoute à la ligne, qui garde son uid et sa provenance.
async fn link_memory(d: &Daemon, pages: &[Page]) -> anyhow::Result<usize> {
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let mut n = 0;
    for level in [Level::Coeur, Level::Projet] {
        for mut e in s.memory.by_level(level).await? {
            let words = format!(" {} ", normalize(&e.text));
            let mut text = e.text.clone();
            for p in pages {
                let cited = std::iter::once(&p.nom)
                    .chain(&p.alias)
                    .any(|name| words.contains(&format!(" {} ", normalize(name))));
                if cited && !text.contains(&format!("[[{}]]", p.slug)) {
                    text.push_str(&format!(" [[{}]]", p.slug));
                }
            }
            if text == e.text {
                continue;
            }
            if !vault.join(&e.file).exists() {
                continue;
            }
            let written = crate::vault_ops::update_note(
                &vault,
                &e.file,
                Some(&e.uid),
                &crate::vault_ops::day(s),
                |raw| {
                    penelope_memory::edit::replace_entry_text(raw, &e.uid, &text)
                        .ok_or_else(|| "uid absent".to_string())
                },
            );
            if written.is_err() {
                continue;
            }
            e.content_hash = penelope_kernel::canonical::sha256_hex(text.as_bytes());
            e.text = text;
            let prov = Provenance::owner("concepts", "maintenance", &s.clock.now_rfc3339());
            s.memory.upsert(&e, &prov).await?;
            n += 1;
        }
    }
    Ok(n)
}

/// Termes employés sans définition : ajoutés s'ils n'ont pas de page, retirés dès qu'ils
/// en ont une.
fn update_to_define(
    vault: &Path,
    source_slug: &str,
    undefined: &[String],
    pages: &[Page],
    day: &str,
) -> anyhow::Result<()> {
    let path = vault.join(TO_DEFINE);
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let body_raw = penelope_memory::wiki::body_of(&raw);
    let defined = |term: &str| {
        let n = normalize(term);
        pages.iter().any(|p| {
            !p.definition.0.is_empty()
                && std::iter::once(&p.nom)
                    .chain(&p.alias)
                    .any(|x| normalize(x) == n)
        })
    };
    let mut terms: BTreeMap<String, String> = body_raw
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .map(|l| {
            let (term, rest) = l.split_once(" · ").unwrap_or((l, ""));
            (term.trim().to_string(), rest.trim().to_string())
        })
        .collect();
    for t in undefined {
        if !terms.keys().any(|k| normalize(k) == normalize(t)) {
            terms.insert(t.clone(), format!("vu dans [[{source_slug}]]"));
        }
    }
    terms.retain(|t, _| !defined(t));
    if terms.is_empty() && raw.is_empty() {
        return Ok(());
    }
    let mut body = String::from(
        "# Concepts à définir\n\nTermes employés dans les sources sans définition ; le digest du matin \
         propose de les compléter.\n\n",
    );
    for (t, where_) in &terms {
        body.push_str(&format!("- {t} · {where_}\n"));
    }
    crate::vault_ops::save_note(
        vault,
        TO_DEFINE,
        &penelope_memory::wiki::replace_body(&raw, &body),
        day,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

/// Termes à définir, pour le digest.
pub fn to_define(vault: &Path) -> Vec<String> {
    penelope_memory::wiki::body_of(
        &std::fs::read_to_string(vault.join(TO_DEFINE)).unwrap_or_default(),
    )
    .lines()
    .filter_map(|l| l.strip_prefix("- "))
    .map(|l| l.split(" · ").next().unwrap_or(l).trim().to_string())
    .collect()
}

/// `index.md` : concepts les plus liés, sources récentes, projets.
async fn write_index(d: &Daemon, pages: &[Page], day: &str) -> anyhow::Result<()> {
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let resolver = penelope_memory::wiki::Resolver::scan(&vault);
    let mut concepts: Vec<&Page> = pages.iter().collect();
    concepts.sort_by(|a, b| {
        b.sources
            .len()
            .cmp(&a.sources.len())
            .then(a.nom.cmp(&b.nom))
    });
    let mut t = String::from(
        "# Index\n\nPoint d'entrée du vault, régénéré à chaque document reçu.\n\n## Concepts les plus liés\n\n",
    );
    for p in concepts.iter().take(20) {
        t.push_str(&format!(
            "- [[{}]] · {} ({} source(s))\n",
            resolver.link_target(&format!("{DIR}/{}.md", p.slug)),
            p.nom,
            p.sources.len()
        ));
    }
    let mut sources: Vec<(String, String, String)> =
        std::fs::read_dir(vault.join(penelope_memory::ingest::SOURCES_DIR))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let slug = name.strip_suffix(".md")?.to_string();
                let raw = std::fs::read_to_string(e.path()).ok()?;
                let recu = penelope_kernel::frontmatter::parse(&raw)
                    .map(|fm| fm.string("recu"))
                    .unwrap_or_default();
                let titre = penelope_memory::ingest::parse_source(&raw)
                    .map(|p| p.titre)
                    .unwrap_or_else(|| slug.clone());
                Some((recu, slug, titre))
            })
            .collect();
    sources.sort_by(|a, b| b.0.cmp(&a.0));
    t.push_str("\n## Sources récentes\n\n");
    for (recu, slug, titre) in sources.iter().take(10) {
        let received: String = recu.chars().take(10).collect();
        let target = resolver.link_target(&format!(
            "{}/{slug}.md",
            penelope_memory::ingest::SOURCES_DIR
        ));
        t.push_str(&format!("- [[{target}]] · {titre} ({received})\n"));
    }
    t.push_str("\n## Projets\n\n");
    let mut projects: Vec<String> = s
        .memory
        .by_level(Level::Projet)
        .await?
        .into_iter()
        .map(|e| e.text)
        .collect();
    projects.extend(
        s.memory
            .by_level(Level::Coeur)
            .await?
            .into_iter()
            .map(|e| e.text)
            .filter(|t| t.starts_with("Projet en cours")),
    );
    for p in projects.iter().take(20) {
        t.push_str(&format!("- {p}\n"));
    }
    let raw = std::fs::read_to_string(vault.join(INDEX)).unwrap_or_default();
    crate::vault_ops::save_note(
        &vault,
        INDEX,
        &penelope_memory::wiki::replace_body(&raw, &t),
        day,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(())
}

/// `mem_neighbors` : voisins d'une note par liens sortants et entrants.
pub async fn neighbors(s: &crate::runtime::Services, slug: &str) -> anyhow::Result<Value> {
    let (a, b) = (slug.to_string(), slug.to_string());
    /// Liens sortants, puis entrants (slug, fichier, texte).
    type Links = (Vec<String>, Vec<(Option<String>, String, String)>);
    let rows: Links = s
        .store
        .read(move |c| {
            let mut out = c.prepare(
                "SELECT DISTINCT l.to_slug FROM mem_links l JOIN mem_entries e ON e.uid = l.from_uid
                 WHERE e.slug = ?1 AND e.statut != 'retiree' ORDER BY l.to_slug",
            )?;
            let outgoing = out
                .query_map([&a], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            let mut inc = c.prepare(
                "SELECT e.slug, e.file, e.text FROM mem_links l JOIN mem_entries e ON e.uid = l.from_uid
                 WHERE l.to_slug = ?1 AND e.statut != 'retiree' ORDER BY e.file",
            )?;
            let incoming = inc
                .query_map([&b], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((outgoing, incoming))
        })
        .await?;
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for to in rows.0 {
        if to != slug && !seen.contains(&to) {
            out.push(json!({"slug": to, "direction": "sortant"}));
            seen.push(to);
        }
    }
    for (from, file, text) in rows.1 {
        match from {
            Some(f) if f != slug && !seen.contains(&f) => {
                out.push(json!({"slug": f, "file": file, "direction": "entrant"}));
                seen.push(f);
            }
            Some(_) => {}
            None => out.push(json!({"file": file, "text": text, "direction": "entrant"})),
        }
    }
    Ok(json!({"slug": slug, "voisins": out}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Origin as Channel;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::MockProvider;
    use std::sync::Arc;

    /// Issue #22 : deux documents qui partagent un terme créent une page de concept liée
    /// aux deux sources ; `mem_neighbors` la retrouve depuis chacune, l'entrée de mémoire
    /// qui la cite est reliée, l'index et les termes à définir suivent.
    #[tokio::test]
    async fn two_sources_sharing_a_term_meet_on_a_concept_page() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = d.chat_session_for(&Channel::Cli).await.unwrap();
        let vault = crate::helpers::vault_dir(&s);
        crate::vault_ops::remember(
            &s,
            &vault,
            Level::Coeur,
            "Projet en cours du propriétaire : passage à Factur-X",
            &sid,
        )
        .await
        .unwrap();

        p.reply(
            r#"{"resume": "Guide de la facturation.", "faits": [],
                "concepts": [{"nom": "Factur-X", "definition": "Format de facture électronique hybride.", "alias": []}],
                "a_definir": ["PDP"]}"#,
        );
        let a = crate::ingest::ingest(
            &d,
            "guide-facturation.md",
            b"# Guide\n\nFactur-X et PDP.".to_vec(),
            "cli",
            Origin::Owner,
            Some(&sid),
            &penelope_llm::CancelToken::new(),
        )
        .await
        .unwrap();
        p.reply(
            r#"{"resume": "Compte rendu.", "faits": [],
                "concepts": [{"nom": "factur x", "definition": "", "alias": ["format hybride"]}],
                "a_definir": []}"#,
        );
        let b = crate::ingest::ingest(
            &d,
            "reunion-comptable.md",
            b"# Reunion\n\nOn passe au factur x.".to_vec(),
            "cli",
            Origin::Owner,
            Some(&sid),
            &penelope_llm::CancelToken::new(),
        )
        .await
        .unwrap();

        let pages = pages(&vault);
        assert_eq!(pages.len(), 1, "{pages:?}");
        let concept = &pages[0];
        assert_eq!(concept.slug, "factur-x");
        assert_eq!(concept.sources.len(), 2);
        assert_eq!(
            concept.alias,
            vec!["format hybride"],
            "une graphie ne fait pas un alias"
        );

        let from_concept = neighbors(&s, "factur-x").await.unwrap().to_string();
        assert!(
            from_concept.contains(&a.slug) && from_concept.contains(&b.slug),
            "{from_concept}"
        );
        for source in [&a.slug, &b.slug] {
            let n = neighbors(&s, source).await.unwrap();
            assert!(n.to_string().contains("factur-x"), "{n}");
        }
        let fiche = std::fs::read_to_string(vault.join(&a.file)).unwrap();
        assert!(
            fiche.contains("## Concepts\n\n- Concepts : [[factur-x]]"),
            "{fiche}"
        );
        assert!(
            std::fs::read_to_string(vault.join("memoire.md"))
                .unwrap()
                .contains("passage à Factur-X [[factur-x]]")
        );
        assert!(
            from_concept.contains("memoire.md"),
            "l'entrée de mémoire est une voisine"
        );
        assert!(
            std::fs::read_to_string(vault.join(INDEX))
                .unwrap()
                .contains("[[factur-x]] · Factur-X (2 source(s))")
        );
        assert_eq!(to_define(&vault), vec!["PDP"]);

        // La réindexation garde le graphe.
        crate::vault_ops::reindex(&s, &vault).await.unwrap();
        assert!(
            neighbors(&s, &a.slug)
                .await
                .unwrap()
                .to_string()
                .contains("factur-x")
        );
    }

    #[test]
    fn names_compare_without_accents_or_plurals() {
        assert_eq!(
            normalize("Factures électroniques"),
            normalize("facture electronique")
        );
        assert_ne!(normalize("Pénélope"), normalize("Hermès"));
        let (c, u) = parse(&json!({
            "concepts": [{"nom": "Factur-X", "definition": "Format de facture.", "alias": ["factur x"]}],
            "a_definir": ["PDP"]
        }));
        assert_eq!(c[0].alias, vec!["factur x"]);
        assert_eq!(u, vec!["PDP"]);
    }
}
