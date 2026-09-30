//! Inventaire du vault (issue #15) : ce qui est présent confronté à ce qui est indexé.
//!
//! Un rappel vide ne décrit que le périmètre interrogé. Tout contenu présent mais hors de
//! l'index produit un avertissement nommé (`penelope vault check`, `doctor`, journal), et
//! une recherche sans résultat le rappelle au modèle : « rien dans ce qui est indexé »
//! n'est pas « cela n'existe pas ».
//!
//! | Emplacement | Indexé | Pourquoi |
//! |---|---|---|
//! | `profil.md`, `memoire.md`, `projets.md`, `notes.md`, `AGENTS.md`, `journal/`, `pratiques/`, `concepts/`, tout autre `.md` avec des entrées `- …` | oui, entrée par entrée | mémoire |
//! | `sources/*.md` | oui, par passages | documents ingérés |
//! | `inbox/` | non | en attente d'ingestion |
//! | `accueil/`, `audits/` | non | comptes rendus : l'accueil écrit dans le profil |
//! | `log.md`, `DREAMS.md` | non | journaux techniques en ajout seul : opérations, rapport des rêves (#282) |
//! | `index.md`, `concepts/_a-definir.md`, `archive/`, fichiers cachés | non | pages générées ou archivées |
//! | autre format (`.pdf`, `.docx`, `.txt`…) hors `sources/` | non | à envoyer pour ingestion |

use penelope_app::services::Services;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// Écart entre le vault et l'index.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gap {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Inventory {
    pub files: usize,
    pub indexed_files: usize,
    pub entries: i64,
    /// Contenu présent mais hors de l'index : à signaler.
    pub not_indexed: Vec<Gap>,
    /// Exclu à dessein (en attente, comptes rendus, pages générées).
    pub excluded: Vec<Gap>,
}

/// Exclusion documentée d'un chemin, s'il y en a une.
pub fn excluded(rel: &str) -> Option<&'static str> {
    let first = rel.split('/').next().unwrap_or_default();
    match first {
        "inbox" => Some("en attente d'ingestion"),
        "accueil" | "audits" => Some("compte rendu, repris dans le profil ou le digest"),
        "archive" => Some("archivé"),
        crate::session_notes::DIR => Some("notes de travail, injectées dans leur session"),
        penelope_memory::wiki::ATTACHMENTS_DIR => {
            Some("original immuable, indexé par sa fiche source")
        }
        _ if rel == penelope_memory::wiki::LOG_FILE => {
            Some("journal des opérations, en ajout seul")
        }
        _ if rel == penelope_memory::wiki::DREAMS_FILE => {
            Some("compte rendu des rêves, en ajout seul")
        }
        _ if rel == crate::concepts::INDEX || rel == crate::concepts::TO_DEFINE => {
            Some("page générée")
        }
        _ if rel.split('/').any(|p| p.starts_with('.')) => Some("fichier caché"),
        _ => None,
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        if p.is_dir() {
            walk(root, &p, out);
        } else if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// Inventaire complet du vault.
pub async fn inventory(s: &Services) -> anyhow::Result<Inventory> {
    let vault = penelope_app::helpers::vault_dir(s);
    let mut paths = Vec::new();
    walk(&vault, &vault, &mut paths);
    paths.sort();
    let counts: BTreeMap<String, i64> = s
        .store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT file, COUNT(*) FROM mem_entries WHERE statut != 'retiree' GROUP BY file",
            )?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await?;
    let mut inv = Inventory {
        files: paths.len(),
        entries: counts.values().sum(),
        ..Default::default()
    };
    for rel in paths {
        if let Some(why) = excluded(&rel) {
            inv.excluded.push(Gap {
                path: rel,
                reason: why.into(),
            });
            continue;
        }
        let indexed = counts.get(&rel).copied().unwrap_or(0);
        if indexed > 0 {
            inv.indexed_files += 1;
        }
        if !rel.ends_with(".md") {
            let ext = rel.rsplit('.').next().unwrap_or_default();
            inv.not_indexed.push(Gap {
                reason: if penelope_memory::ingest::is_ingestible(&rel) {
                    format!("format .{ext} non indexé dans le vault : l'envoyer à Pénélope pour l'ingérer")
                } else {
                    format!("format .{ext} non pris en charge par l'index")
                },
                path: rel,
            });
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(vault.join(&rel)) else {
            continue;
        };
        if rel.starts_with("sources/") {
            if indexed == 0 && penelope_memory::ingest::parse_source(&raw).is_some() {
                inv.not_indexed.push(Gap {
                    path: rel,
                    reason: "fiche source sans passage indexé : `penelope mem reindex`".into(),
                });
            }
            continue;
        }
        let (entries, rewritten) = penelope_memory::vault::parse_entries(&raw);
        let pending = entries.len() as i64 - indexed;
        if rewritten.is_some() || pending > 0 {
            inv.not_indexed.push(Gap {
                path: rel,
                reason: format!(
                    "{} entrée(s) présente(s), {indexed} indexée(s) : `penelope mem reindex`",
                    entries.len()
                ),
            });
        } else if entries.is_empty() && has_prose(&raw) {
            inv.not_indexed.push(Gap {
                path: rel,
                reason: "texte sans entrées `- …` : invisible à la recherche, le découper en \
                         entrées ou l'envoyer comme document"
                    .into(),
            });
        }
    }
    Ok(inv)
}

/// Du texte hors titres et frontmatter.
fn has_prose(raw: &str) -> bool {
    let body = match raw.strip_prefix("---") {
        Some(rest) => rest.split_once("\n---").map(|(_, b)| b).unwrap_or(rest),
        None => raw,
    };
    body.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("<!--"))
}

/// Remarque jointe à une recherche mémoire vide : le périmètre, et ce qui en sort.
pub async fn empty_search_note(s: &Services) -> Value {
    let inv = match inventory(s).await {
        Ok(i) => i,
        Err(e) => {
            return json!({"remarque": format!(
                "Aucun résultat dans l'index ; inventaire du vault impossible ({e}) : ce n'est pas \
                 une preuve d'absence."
            )});
        }
    };
    let mut remark = format!(
        "Aucun résultat dans le périmètre indexé ({} entrée(s), {} fichier(s)). Ce n'est pas une \
         preuve d'absence : dire « je ne trouve rien dans ce que j'ai indexé », pas « cela \
         n'existe pas ».",
        inv.entries, inv.indexed_files
    );
    if !inv.not_indexed.is_empty() {
        remark.push_str(&format!(
            " {} fichier(s) du vault sont présents mais hors index.",
            inv.not_indexed.len()
        ));
    }
    json!({
        "résultats": [],
        "remarque": remark,
        "hors_index": inv.not_indexed.iter().take(10).collect::<Vec<_>>(),
    })
}

/// Signale une fois tout nouvel écart (journal et événement) ; rend les écarts.
pub async fn report_gaps(s: &penelope_app::services::Services) -> anyhow::Result<Vec<Gap>> {
    let inv = inventory(s).await?;
    let fingerprint =
        penelope_kernel::canonical::sha256_hex(serde_json::to_string(&inv.not_indexed)?.as_bytes());
    const KEY: &str = "vault.gaps";
    if s.kv_get(KEY).await?.as_deref() != Some(fingerprint.as_str()) {
        s.kv_set(KEY, &fingerprint).await?;
        for g in &inv.not_indexed {
            tracing::warn!(fichier = %g.path, raison = %g.reason, "contenu du vault hors index");
        }
        if !inv.not_indexed.is_empty() {
            s.events
                .append(penelope_kernel::event::EventDraft::new(
                    "memory.index_gap",
                    json!({"gaps": inv.not_indexed}),
                ))
                .await?;
        }
    }
    Ok(inv.not_indexed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    /// Issue #15 : un fichier présent mais hors index est nommé, une recherche vide le
    /// rappelle, et la réindexation résorbe ce qui peut l'être.
    #[tokio::test]
    async fn content_outside_the_index_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let vault = penelope_app::helpers::vault_dir(&s);
        std::fs::create_dir_all(vault.join("clients/acme")).unwrap();
        std::fs::write(
            vault.join("clients/acme/contrat.md"),
            "# Contrat ACME\n\n- Renouvellement tacite le 1er mars\n",
        )
        .unwrap();
        std::fs::write(vault.join("clients/acme/avenant.pdf"), b"%PDF-1.4").unwrap();
        std::fs::write(
            vault.join("clients/acme/notes.md"),
            "# Notes\n\nAppel du 3 : prix revu.\n",
        )
        .unwrap();
        std::fs::create_dir_all(vault.join("inbox")).unwrap();
        std::fs::write(vault.join("inbox/a-lire.pdf"), b"%PDF-1.4").unwrap();

        let inv = inventory(&s).await.unwrap();
        let paths: Vec<&str> = inv.not_indexed.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "clients/acme/avenant.pdf",
                "clients/acme/contrat.md",
                "clients/acme/notes.md"
            ],
            "{inv:?}"
        );
        assert!(inv.excluded.iter().any(|g| g.path == "inbox/a-lire.pdf"));

        let note = empty_search_note(&s).await;
        let remark = note["remarque"].as_str().unwrap();
        assert!(remark.contains("pas une preuve d'absence"), "{remark}");
        assert!(note["hors_index"].to_string().contains("contrat.md"));

        crate::vault_ops::reindex(&s, &vault).await.unwrap();
        let inv = inventory(&s).await.unwrap();
        let paths: Vec<&str> = inv.not_indexed.iter().map(|g| g.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["clients/acme/avenant.pdf", "clients/acme/notes.md"],
            "{inv:?}"
        );
        assert!(inv.not_indexed[1].reason.contains("texte sans entrées"));
    }

    /// Issue #282 : `DREAMS.md` est un compte rendu en ajout seul, pas une connaissance.
    /// Sur l'instance, il pesait 1 375 entrées sur 4 257 et le rappel ramenait des
    /// verdicts de tri et des candidats écartés. Il est exclu avec son motif, ses entrées
    /// déjà indexées sortent à la réindexation, et la note voisine reste indexée.
    #[tokio::test]
    async fn the_dream_report_is_excluded_and_its_entries_leave_on_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let vault = penelope_app::helpers::vault_dir(&s);
        std::fs::create_dir_all(&vault).unwrap();
        let dreams = "# Revue\n\n## Rêve du 2026-09-29 (`d_1`)\n\n### Tri\n\
                      - ⏭ ignoré « le café était chaud » : trivial\n\
                      - 📓 journal « appel avec Martin » : épisodique\n";
        std::fs::write(vault.join("DREAMS.md"), dreams).unwrap();
        std::fs::write(
            vault.join("notes.md"),
            "# Notes\n\n- Le client Martin est basé à Lyon\n",
        )
        .unwrap();
        // Une entrée du rapport indexée par une version antérieure.
        let mut stale = penelope_memory::index::simple_entry(
            "01JSTALE",
            "⏭ ignoré « le café était chaud » : trivial",
            penelope_memory::Level::Revue,
            "2026-09-29",
        );
        stale.file = "DREAMS.md".into();
        let prov = penelope_memory::Provenance::owner("reindex", "maintenance", "2026-09-29");
        s.memory.upsert(&stale, &prov).await.unwrap();

        crate::vault_ops::reindex(&s, &vault).await.unwrap();

        let inv = inventory(&s).await.unwrap();
        assert_eq!(inv.entries, 1, "{inv:?}");
        assert_eq!(inv.indexed_files, 1, "{inv:?}");
        assert!(inv.not_indexed.is_empty(), "{inv:?}");
        let gap = inv
            .excluded
            .iter()
            .find(|g| g.path == "DREAMS.md")
            .expect("exclu, avec son motif");
        assert_eq!(gap.reason, "compte rendu des rêves, en ajout seul");
        let stale = s.memory.get("01JSTALE").await.unwrap().unwrap();
        assert_eq!(stale.statut, "retiree");
        assert!(
            s.memory
                .by_level(penelope_memory::Level::Revue)
                .await
                .unwrap()
                .is_empty()
        );
        // Le rapport n'est pas réécrit : pas de `^uid` posé sur ses lignes de tri.
        assert_eq!(
            std::fs::read_to_string(vault.join("DREAMS.md")).unwrap(),
            dreams
        );
        // Une note voisine est indexée et tenue par son uid.
        let raw = std::fs::read_to_string(vault.join("notes.md")).unwrap();
        assert!(raw.contains("Lyon ^"), "{raw}");
    }
}
