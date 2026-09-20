//! Import de skills tierces depuis un dépôt GitHub (issue #146).
//!
//! Le format `agentskills.io` est déjà celui de Pénélope : un `SKILL.md` amont se charge
//! tel quel. Ce qui manquait, c'est le chemin entre « le dépôt existe » et « la skill
//! tourne ici » : copier le dossier **entier** (les documentaires embarquent scripts,
//! schémas XSD, thèmes), compléter le frontmatter, et dire au modèle comment lire un
//! corps écrit pour un autre agent.
//!
//! Rien n'est installé sans que le propriétaire l'ait demandé, et **aucune dépendance
//! n'est installée** : les manques sont listés, jamais comblés en silence.

use crate::{Scope, Skill, parse_skill};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Correspondance des outils entre Claude Code (le vocabulaire des skills amont) et
/// Pénélope. Elle est écrite **une fois** ici : sans elle, chaque agent la redécouvre et
/// l'écrit à la main en tête de corps, où la mise à jour suivante l'efface.
pub const TOOL_MAP: &[(&str, &str)] = &[
    ("Read", "fs_read"),
    ("Write", "fs_write"),
    ("Edit", "fs_edit"),
    ("MultiEdit", "fs_edit"),
    ("NotebookEdit", "fs_edit"),
    ("Bash", "shell_exec"),
    ("BashOutput", "shell_exec"),
    ("Glob", "fs_search"),
    ("Grep", "fs_search"),
    ("WebFetch", "http_fetch"),
    ("WebSearch", "http_fetch"),
];

/// Taille maximale d'un fichier tiré d'une archive, et de l'ensemble d'une skill : un
/// documentaire tient en quelques mégaoctets (un schéma XSD en fait 1,2), une archive
/// piégée non.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_SKILL_BYTES: u64 = 32 * 1024 * 1024;
/// Taille maximale de l'archive téléchargée.
pub const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;

/// Dépôt d'origine et révision. Seul GitHub est servi : l'archive se télécharge en HTTPS,
/// sans `git` ni sous-processus, et l'adresse ne peut pas désigner autre chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub owner: String,
    pub repo: String,
    /// Branche, étiquette ou empreinte. `main` par défaut.
    pub git_ref: String,
}

impl Source {
    /// `owner/repo`, `owner/repo@ref`, suivis d'un `:skill,skill` facultatif.
    /// Rend la source et les skills demandées (vide = toutes celles du dépôt).
    pub fn parse(spec: &str) -> Result<(Source, Vec<String>), String> {
        let (repo_part, wanted) = match spec.split_once(':') {
            Some((r, list)) => (
                r,
                list.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            ),
            None => (spec, Vec::new()),
        };
        let (repo_part, git_ref) = match repo_part.split_once('@') {
            Some((r, rf)) if !rf.trim().is_empty() => (r, rf.trim().to_string()),
            _ => (repo_part, "main".to_string()),
        };
        let Some((owner, repo)) = repo_part.trim().split_once('/') else {
            return Err(format!(
                "source `{spec}` : attendu `proprietaire/depot[@revision][:skill,skill]` \
                 (par exemple `anthropics/skills:docx,pdf`)"
            ));
        };
        for (what, v) in [("propriétaire", owner), ("dépôt", repo)] {
            if v.is_empty() || !v.chars().all(is_repo_char) {
                return Err(format!("{what} `{v}` invalide dans la source `{spec}`"));
            }
        }
        if !git_ref.chars().all(is_ref_char) {
            return Err(format!("révision `{git_ref}` invalide"));
        }
        for name in &wanted {
            penelope_platform::validate_slug(name)
                .map_err(|e| format!("skill demandée `{name}` : {e}"))?;
        }
        Ok((
            Source {
                owner: owner.to_string(),
                repo: repo.to_string(),
                git_ref,
            },
            wanted,
        ))
    }

    /// Archive ZIP de la révision. `codeload` sert le même contenu que `git clone
    /// --depth 1`, sans dépôt local ni sous-processus, et le format ZIP est déjà lu par
    /// le crate mémoire.
    pub fn archive_url(&self) -> String {
        format!(
            "https://codeload.github.com/{}/{}/zip/{}",
            self.owner, self.repo, self.git_ref
        )
    }

    pub fn label(&self) -> String {
        format!("{}/{}@{}", self.owner, self.repo, self.git_ref)
    }
}

fn is_repo_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn is_ref_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')
}

/// Une skill installée, et ce qu'il faut savoir avant de s'en servir.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Installed {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    /// Fichiers copiés, corps compris : les documentaires embarquent des scripts.
    pub files: usize,
    pub bytes: u64,
    /// Outils Pénélope écrits dans le frontmatter, déduits du corps.
    pub allowed_tools: Vec<String>,
    /// Dépendances déclarées (`requires`), à vérifier par `penelope doctor`.
    pub requires: Vec<String>,
    /// Vrai si une skill du même nom a été remplacée.
    pub replaced: bool,
}

/// Skills présentes dans une archive, qu'elles soient demandées ou non.
pub fn list_in_zip(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("archive : {e}"))?;
    let mut names = BTreeSet::new();
    for i in 0..archive.len() {
        let Ok(f) = archive.by_index(i) else { continue };
        if let Some(name) = skill_name_of(f.name()) {
            names.insert(name);
        }
    }
    Ok(names.into_iter().collect())
}

/// Installe les skills d'une archive dans `dest_root`. `wanted` vide = toutes.
///
/// Refuse d'écraser une skill existante sans `force`, et refuse d'installer un
/// `SKILL.md` qui ne se charge pas : une skill invalide ne doit jamais atteindre le
/// registre.
pub fn install_from_zip(
    bytes: &[u8],
    wanted: &[String],
    dest_root: &Path,
    force: bool,
) -> Result<Vec<Installed>, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("archive : {e}"))?;

    // Dossiers de skills présents, par nom.
    let mut dirs: BTreeSet<(String, String)> = BTreeSet::new();
    for i in 0..archive.len() {
        let Ok(f) = archive.by_index(i) else { continue };
        let name = f.name().to_string();
        if let Some(skill) = skill_name_of(&name) {
            let dir = name.trim_end_matches("SKILL.md").to_string();
            dirs.insert((skill, dir));
        }
    }
    if dirs.is_empty() {
        return Err("aucun `SKILL.md` dans cette archive".into());
    }
    let selected: Vec<(String, String)> = if wanted.is_empty() {
        dirs.into_iter().collect()
    } else {
        let have: Vec<String> = dirs.iter().map(|(n, _)| n.clone()).collect();
        for w in wanted {
            if !have.contains(w) {
                return Err(format!(
                    "skill `{w}` absente de l'archive ; disponibles : {}",
                    have.join(", ")
                ));
            }
        }
        dirs.into_iter()
            .filter(|(n, _)| wanted.contains(n))
            .collect()
    };

    let mut out = Vec::new();
    for (name, dir) in selected {
        out.push(install_one(&mut archive, &name, &dir, dest_root, force)?);
    }
    Ok(out)
}

/// Nom de la skill d'un chemin d'archive `depot-ref/.../<slug>/SKILL.md`.
fn skill_name_of(entry: &str) -> Option<String> {
    let rest = entry.strip_suffix("SKILL.md")?;
    let dir = rest.trim_end_matches('/');
    let slug = dir.rsplit('/').next()?;
    // Le dossier de tête de l'archive GitHub (`skills-main/`) n'est pas une skill.
    (dir.contains('/') && penelope_platform::validate_slug(slug).is_ok()).then(|| slug.to_string())
}

fn install_one(
    archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    dir: &str,
    dest_root: &Path,
    force: bool,
) -> Result<Installed, String> {
    let dest = dest_root.join(name);
    let replaced = dest.exists();
    if replaced && !force {
        return Err(format!(
            "la skill `{name}` existe déjà dans {} : relancer avec `--force` pour la \
             remplacer",
            dest_root.display()
        ));
    }

    // Tout lire d'abord, écrire ensuite : une archive refusée à mi-chemin ne doit pas
    // laisser un dossier à moitié rempli.
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| format!("archive : {e}"))?;
        if f.is_dir() {
            continue;
        }
        let entry = f.name().to_string();
        let Some(rel) = entry.strip_prefix(dir) else {
            continue;
        };
        if rel.is_empty() {
            continue;
        }
        // Un lien symbolique dans une archive désigne ce qu'il veut : jamais écrit.
        if f.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err(format!(
                "`{entry}` est un lien symbolique : archive refusée"
            ));
        }
        let rel = safe_relative(rel)
            .ok_or_else(|| format!("chemin refusé dans l'archive : `{entry}`"))?;
        if f.size() > MAX_FILE_BYTES {
            return Err(format!(
                "`{entry}` fait {} octets, au-delà de la borne de {MAX_FILE_BYTES}",
                f.size()
            ));
        }
        total += f.size();
        if total > MAX_SKILL_BYTES {
            return Err(format!(
                "la skill `{name}` dépasse {MAX_SKILL_BYTES} octets décompressés"
            ));
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.take(MAX_FILE_BYTES)
            .read_to_end(&mut buf)
            .map_err(|e| format!("`{entry}` illisible : {e}"))?;
        files.push((rel, buf));
    }

    // Le corps, complété puis validé avant la moindre écriture.
    let body_index = files
        .iter()
        .position(|(p, _)| p == Path::new("SKILL.md"))
        .ok_or_else(|| format!("`{name}` : SKILL.md introuvable dans l'archive"))?;
    let raw = String::from_utf8(files[body_index].1.clone())
        .map_err(|_| format!("`{name}` : SKILL.md n'est pas de l'UTF-8"))?;
    let tools = tools_used(&raw);
    let normalised = normalise(&raw, &tools);
    let skill = parse_skill(&dest.join("SKILL.md"), &normalised, Scope::User)
        .map_err(|e| format!("`{name}` refusée : {}", e.message))?;
    if skill.name != name {
        return Err(format!(
            "`{name}` : le frontmatter annonce `{}`, le dossier dit `{name}`",
            skill.name
        ));
    }
    files[body_index].1 = normalised.into_bytes();

    if replaced {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("{} : {e}", dest.display()))?;
    }
    let count = files.len();
    for (rel, bytes) in files {
        let path = dest.join(&rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{} : {e}", parent.display()))?;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("{} : {e}", path.display()))?;
    }

    Ok(Installed {
        name: skill.name,
        description: skill.description,
        path: dest,
        files: count,
        bytes: total,
        allowed_tools: skill.allowed_tools,
        requires: skill.requires,
        replaced,
    })
}

/// Chemin relatif sûr : ni absolu, ni remontant, ni racine Windows.
fn safe_relative(rel: &str) -> Option<PathBuf> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            p => out.push(p),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Outils Pénélope qu'un corps écrit pour Claude Code appelle, d'après [`TOOL_MAP`].
/// Le nom doit apparaître comme un mot : « Read » compte, « Already » non.
pub fn tools_used(body: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for (claude, ours) in TOOL_MAP {
        if mentions_word(body, claude) {
            out.insert(ours.to_string());
        }
    }
    out.into_iter().collect()
}

fn mentions_word(haystack: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(at) = haystack[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = haystack[..start].chars().next_back();
        let after = haystack[end..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if boundary(before) && boundary(after) {
            return true;
        }
        from = end;
    }
    false
}

/// Complète le frontmatter d'une skill amont **sans la réécrire** : les lignes manquantes
/// sont ajoutées à la fin du bloc, le reste est laissé mot pour mot. Une mise à jour du
/// dépôt reste donc lisible en diff.
pub fn normalise(raw: &str, tools: &[String]) -> String {
    let Some(rest) = raw.strip_prefix("---\n") else {
        return raw.to_string();
    };
    let Some(end) = rest.find("\n---") else {
        return raw.to_string();
    };
    let (head, tail) = rest.split_at(end);
    let mut block = head.to_string();
    let has = |key: &str| {
        head.lines()
            .any(|l| l.trim_start().starts_with(&format!("{key}:")))
    };
    if !has("version") {
        block.push_str("\nversion: 1.0.0");
    }
    // Sans `allowed_tools`, une skill a droit à tout. Pour une skill venue d'ailleurs, ce
    // défaut est trop large : on écrit ce que son corps réclame, visible et modifiable.
    if !has("allowed_tools") && !tools.is_empty() {
        block.push_str(&format!("\nallowed_tools: [{}]", tools.join(", ")));
    }
    format!("---\n{block}{tail}")
}

/// Préambule ajouté au corps rendu par `skill_load`, jamais au fichier : un corps amont
/// parle en outils Claude Code et en chemins relatifs. Rendre `None` quand il n'y a rien
/// à traduire, pour ne pas alourdir les skills maison.
pub fn portage_note(skill: &Skill) -> Option<String> {
    let dir = skill.path.parent()?;
    let mapped: Vec<&(&str, &str)> = TOOL_MAP
        .iter()
        .filter(|(claude, _)| mentions_word(&skill.body, claude))
        .collect();
    let glued = glued_examples(&skill.body);
    if mapped.is_empty() && glued.is_empty() {
        return None;
    }
    let mut lines = vec![
        "## Portage (ajouté par Pénélope, absent du fichier)".to_string(),
        String::new(),
        format!(
            "Dossier de cette skill : `{}` — les scripts et fichiers qu'elle cite s'y \
             trouvent, à lire en chemin absolu.",
            dir.display()
        ),
        String::new(),
    ];
    if !mapped.is_empty() {
        lines.push(
            "Le corps ci-dessous est écrit pour Claude Code. Correspondance des outils :"
                .to_string(),
        );
        lines.push(String::new());
        // Deux noms amont peuvent viser le même outil (`Glob` et `Grep`) : les deux lignes
        // sont utiles, c'est le vocabulaire du corps qu'il faut traduire.
        for (claude, ours) in mapped {
            lines.push(format!("- `{claude}` → `{ours}`"));
        }
        lines.push(String::new());
    }
    // La routine YouTube lançait ses trois commandes en une ligne : le propriétaire ne
    // pouvait l'autoriser qu'une fois, et redemandait à chaque vidéo (issue #150).
    if !glued.is_empty() {
        lines.push(format!(
            "**Une commande par appel `shell_exec`.** {} exemple(s) de cette skill collent \
             plusieurs commandes sur une ligne (`{}`…) : lance-les en appels séparés, en \
             parallèle si elles sont indépendantes. Une ligne collée ne peut porter aucune \
             règle : le propriétaire la réautorise à chaque fois.",
            glued.len(),
            glued[0]
        ));
        lines.push(String::new());
    }
    lines.push("---".into());
    lines.push(String::new());
    Some(lines.join("\n"))
}

/// Exemples de la skill qui collent plusieurs commandes sur une ligne (issue #150), en
/// extrait tronqué.
///
/// Seuls les blocs marqués shell sont lus : un bloc Rust finit chaque ligne par `;` et
/// une prose qui parle de `&&` n'est pas un exemple.
pub fn glued_examples(body: &str) -> Vec<String> {
    const SHELL: &[&str] = &["bash", "sh", "shell", "zsh", "console", "terminal"];
    let mut out = Vec::new();
    let mut in_shell = false;
    let mut in_code = false;
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("```") {
            if in_code {
                in_code = false;
                in_shell = false;
            } else {
                in_code = true;
                in_shell = SHELL.contains(&rest.trim().to_ascii_lowercase().as_str());
            }
            continue;
        }
        if !in_shell || t.is_empty() || t.starts_with('#') {
            continue;
        }
        // Un `;` ou un `&&` dans un exemple : c'est une ligne que le propriétaire ne
        // pourra autoriser qu'une fois, ou pas du tout.
        if t.contains("&&") || t.contains(';') {
            out.push(t.chars().take(60).collect::<String>());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                z.start_file(*name, opts).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf
    }

    const DOCX: &str = "---\nname: docx\ndescription: Documents Word\n---\n\
        # DOCX\n\nUse the Read tool, then Bash to run scripts/ooxml.py.\n";

    /// #146 : `anthropics/skills:docx,pdf` se lit en une source et deux skills.
    #[test]
    fn a_source_reads_owner_repo_ref_and_skills() {
        let (s, w) = Source::parse("anthropics/skills").unwrap();
        assert_eq!(s.owner, "anthropics");
        assert_eq!(s.repo, "skills");
        assert_eq!(s.git_ref, "main", "la branche par défaut");
        assert!(w.is_empty(), "toutes les skills");

        let (s, w) = Source::parse("anthropics/skills@v2:docx, pdf").unwrap();
        assert_eq!(s.git_ref, "v2");
        assert_eq!(w, ["docx", "pdf"]);
        assert_eq!(
            s.archive_url(),
            "https://codeload.github.com/anthropics/skills/zip/v2"
        );

        for bad in ["skills", "an/../skills", "a/b:Pas Un Slug", "a b/c"] {
            assert!(Source::parse(bad).is_err(), "`{bad}` devrait être refusée");
        }
    }

    /// #146 : le dossier entier est copié — un documentaire embarque ses scripts et ses
    /// schémas —, le frontmatter est complété, et la skill se charge.
    #[test]
    fn a_skill_is_installed_with_all_its_files() {
        let dest = tempfile::tempdir().unwrap();
        let zip = zip_of(&[
            ("skills-main/", ""),
            ("skills-main/document-skills/docx/SKILL.md", DOCX),
            (
                "skills-main/document-skills/docx/scripts/ooxml.py",
                "print(1)",
            ),
            ("skills-main/document-skills/docx/schemas/wml.xsd", "<xsd/>"),
            ("skills-main/README.md", "pas une skill"),
        ]);

        assert_eq!(list_in_zip(&zip).unwrap(), ["docx"]);
        let done = install_from_zip(&zip, &[], dest.path(), false).unwrap();
        assert_eq!(done.len(), 1);
        let d = &done[0];
        assert_eq!(d.name, "docx");
        assert_eq!(d.files, 3, "corps, script et schéma");
        assert!(!d.replaced);
        assert!(dest.path().join("docx/scripts/ooxml.py").exists());
        assert!(dest.path().join("docx/schemas/wml.xsd").exists());

        // Frontmatter complété : version, et les outils que le corps réclame.
        let raw = std::fs::read_to_string(dest.path().join("docx/SKILL.md")).unwrap();
        assert!(raw.contains("version: 1.0.0"), "{raw}");
        assert!(
            raw.contains("allowed_tools: [fs_read, shell_exec]"),
            "{raw}"
        );
        assert!(
            raw.contains("Use the Read tool"),
            "le corps reste mot pour mot"
        );
        let s = parse_skill(Path::new("SKILL.md"), &raw, Scope::User).unwrap();
        assert_eq!(s.allowed_tools, ["fs_read", "shell_exec"]);

        // Deuxième passage : refusé, puis accepté avec `force`.
        let e = install_from_zip(&zip, &[], dest.path(), false).unwrap_err();
        assert!(e.contains("--force"), "{e}");
        assert!(install_from_zip(&zip, &[], dest.path(), true).unwrap()[0].replaced);

        // Une skill demandée mais absente se dit, avec ce qui existe.
        let e = install_from_zip(&zip, &["pptx".into()], dest.path(), true).unwrap_err();
        assert!(e.contains("pptx") && e.contains("docx"), "{e}");
    }

    /// #146 : une archive qui remonte hors du dossier, ou qui porte un lien symbolique,
    /// est refusée sans rien écrire.
    #[test]
    fn an_archive_never_writes_outside_the_skill_directory() {
        let dest = tempfile::tempdir().unwrap();
        let zip = zip_of(&[
            (
                "skills-main/evil/SKILL.md",
                "---\nname: evil\ndescription: d\n---\ncorps\n",
            ),
            ("skills-main/evil/../../../etc/passwd", "root"),
        ]);
        let e = install_from_zip(&zip, &[], dest.path(), false).unwrap_err();
        assert!(e.contains("chemin refusé"), "{e}");
        assert!(!dest.path().join("evil").exists(), "rien d'écrit");

        assert_eq!(
            safe_relative("scripts/a.py"),
            Some(PathBuf::from("scripts/a.py"))
        );
        for bad in ["../x", "a/../../x", "/etc/passwd", "C:\\x"] {
            assert!(safe_relative(bad).is_none(), "`{bad}`");
        }
    }

    /// #146 : la table de correspondance se lit en mots entiers, et le préambule dit le
    /// dossier absolu sans toucher au fichier.
    /// #150 : une skill dont les exemples collent des commandes reçoit la consigne au
    /// chargement. La routine YouTube lançait ses trois commandes en une ligne, et le
    /// propriétaire réautorisait à chaque vidéo.
    #[test]
    fn a_skill_whose_examples_glue_commands_is_told_at_load() {
        let body = "Routine\n\n```bash\nyt-dlp --print x URL \\\n  && yt-dlp -o tmp/z URL\n```\n";
        let found = glued_examples(body);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("yt-dlp"), "{found:?}");

        // Un bloc qui n'est pas du shell ne compte pas : chaque ligne de Rust finit par
        // un `;`.
        let rust = "```rust\nlet a = 1;\nlet b = 2;\n```\n";
        assert!(glued_examples(rust).is_empty());
        // Une prose qui parle de `&&` non plus.
        assert!(glued_examples("On évite `a && b` dans les exemples.\n").is_empty());
        // Un commentaire de bloc shell non plus.
        assert!(glued_examples("```sh\n# a && b\nls\n```\n").is_empty());

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("yt/SKILL.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw =
            format!("---\nname: yt\ndescription: transcription\nversion: 1.0.0\n---\n\n{body}");
        let mut skill = parse_skill(&path, &raw, Scope::User).unwrap();
        skill.path = path.clone();
        let note = portage_note(&skill).expect("une note");
        assert!(
            note.contains("Une commande par appel"),
            "la consigne est dans la note : {note}"
        );
    }

    #[test]
    fn the_portage_note_translates_tools_and_names_the_directory() {
        assert_eq!(tools_used("Use Read and Bash"), ["fs_read", "shell_exec"]);
        assert!(
            tools_used("Already reading").is_empty(),
            "`Read` dans `Already` n'est pas un outil"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docx/SKILL.md");
        let skill = parse_skill(&path, DOCX, Scope::User).unwrap();
        let note = portage_note(&skill).unwrap();
        assert!(
            note.contains(&dir.path().join("docx").display().to_string()),
            "{note}"
        );
        assert!(note.contains("`Read` → `fs_read`"), "{note}");
        assert!(note.contains("`Bash` → `shell_exec`"), "{note}");

        // Une skill maison ne parle pas de ces outils : rien n'est ajouté.
        let maison = parse_skill(
            Path::new("SKILL.md"),
            "---\nname: revue\ndescription: d\n---\nRelis le diff avec `fs_read`.\n",
            Scope::User,
        )
        .unwrap();
        assert!(portage_note(&maison).is_none());
    }
}
