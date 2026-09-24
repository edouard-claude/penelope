//! Import de skills tierces (issue #146) : téléchargement, puis pose par
//! [`penelope_skills::install`].
//!
//! Le portage manuel de six skills officielles Anthropic a demandé un script ad hoc et
//! un socle installé à la main. Ce qui est automatisable l'est ici ; ce qui touche la
//! machine (npm, pip, binaires) est **listé, jamais installé**.

use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use penelope_skills::install::{Installed, MAX_ARCHIVE_BYTES, Source};
use serde_json::json;
use std::time::Duration;

/// Télécharge l'archive d'un dépôt et installe les skills demandées dans
/// `{data}/skills/`. Rend ce qui a été posé, avec les dépendances qui manquent.
pub async fn install(
    s: &Services,
    spec: &str,
    force: bool,
) -> anyhow::Result<(Source, Vec<Installed>, Vec<crate::skill_deps::Missing>)> {
    let (source, wanted) = Source::parse(spec).map_err(anyhow::Error::msg)?;
    let url = source.archive_url();
    let bytes = fetch(&url).await.map_err(anyhow::Error::msg)?;

    let root = s.platform.dirs.skills();
    std::fs::create_dir_all(&root)?;
    let installed = penelope_skills::install::install_from_zip(&bytes, &wanted, &root, force)
        .map_err(anyhow::Error::msg)?;

    penelope_app::services::reload_skills(s).await?;
    let missing = crate::skill_deps::missing_for(
        &installed
            .iter()
            .flat_map(|i| i.requires.clone())
            .collect::<Vec<_>>(),
    )
    .await;

    s.events
        .append(EventDraft::new(
            "skill.installed",
            json!({
                "source": source.label(),
                "skills": installed.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
                "missing_requirements": missing.len(),
            }),
        ))
        .await?;
    Ok((source, installed, missing))
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    // L'adresse n'est pas fournie par l'appelant : `Source::archive_url` la construit,
    // hôte compris, à partir d'un `proprietaire/depot` validé. Rien à filtrer de plus.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            concat!("penelope/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .map_err(|e| format!("GET {url} : {e}"))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Err(format!(
            "dépôt ou révision introuvable ({url}) : vérifier le nom et la branche"
        ));
    }
    if status >= 400 {
        return Err(format!("GET {url} : HTTP {status}"));
    }
    if resp
        .content_length()
        .is_some_and(|n| n as usize > MAX_ARCHIVE_BYTES)
    {
        return Err(format!("{url} : archive trop volumineuse"));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("GET {url} : {e}"))?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(format!("{url} : archive trop volumineuse"));
    }
    Ok(bytes.to_vec())
}

/// Ce que `penelope skill install` dit au propriétaire, en une fois.
pub fn report(
    source: &Source,
    installed: &[Installed],
    missing: &[crate::skill_deps::Missing],
) -> String {
    let mut out = format!(
        "{} skill(s) installée(s) depuis {}\n",
        installed.len(),
        source.label()
    );
    for i in installed {
        out.push_str(&format!(
            "- `{}` : {} ({} fichiers){}\n",
            i.name,
            i.description,
            i.files,
            if i.replaced { ", remplacée" } else { "" }
        ));
        if !i.allowed_tools.is_empty() {
            out.push_str(&format!("  outils : {}\n", i.allowed_tools.join(", ")));
        }
    }
    if missing.is_empty() {
        return out;
    }
    out.push_str("\nDépendances manquantes, à installer toi-même :\n");
    for m in missing {
        out.push_str(&format!("- {} → `{}`\n", m.requirement, m.how));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_skills::install::Source;

    fn installed(name: &str, requires: Vec<String>) -> Installed {
        Installed {
            name: name.into(),
            description: "Documents Word".into(),
            path: std::path::PathBuf::from("/tmp").join(name),
            files: 12,
            bytes: 1_300_000,
            allowed_tools: vec!["fs_read".into(), "shell_exec".into()],
            requires,
            replaced: false,
        }
    }

    /// #146 : le compte rendu dit ce qui est posé, les outils qui lui sont ouverts, et ce
    /// qu'il reste à installer **à la main**.
    #[test]
    fn the_report_names_what_is_installed_and_what_is_missing() {
        let (source, _) = Source::parse("anthropics/skills:docx").unwrap();
        let done = [installed("docx", vec!["pip:openpyxl".into()])];

        let clean = report(&source, &done, &[]);
        assert!(clean.contains("anthropics/skills@main"), "{clean}");
        assert!(
            clean.contains("`docx` : Documents Word (12 fichiers)"),
            "{clean}"
        );
        assert!(clean.contains("fs_read, shell_exec"), "{clean}");
        assert!(!clean.contains("Dépendances manquantes"), "{clean}");

        let missing = [crate::skill_deps::Missing {
            requirement: "pip:openpyxl".into(),
            how: crate::skill_deps::how_to_install("pip", "openpyxl"),
        }];
        let dirty = report(&source, &done, &missing);
        assert!(dirty.contains("Dépendances manquantes"), "{dirty}");
        assert!(dirty.contains("pip install openpyxl"), "{dirty}");
        assert!(
            dirty.contains("PEP 668"),
            "le piège relevé le 20/09 : {dirty}"
        );
    }
}
