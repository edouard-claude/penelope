//! Plugins au format Claude Code (#393), lot 1 : un dossier `{data}/plugins/<nom>` avec
//! `.claude-plugin/plugin.json`, `skills/<n>/SKILL.md`, `commands/*.md` et `agents/*.md`.
//!
//! - Les skills d'un plugin rejoignent le registre (portée `plugin`, sous les skills du
//!   propriétaire à nom égal) ; ses commandes deviennent des skills `<plugin>-<commande>`.
//! - Les sous-agents déclarés en fichiers (`agents/*.md` d'un plugin, ou `{data}/agents`)
//!   servent `sub_agent_spawn` par leur nom, en plus des rôles câblés.
//!
//! Ni hook ni serveur MCP de plugin dans ce lot : ils demandent la politique du
//! propriétaire (`plugins.allowed_mcp_servers`) et viendront ensuite.

use crate::{Scope, Skill, SkillError, parse_skill, scan_dir};
use penelope_kernel::frontmatter;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Un plugin déposé.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub root: PathBuf,
}

/// Les plugins d'un dossier : chaque sous-dossier au nom valide, nommé par lui-même.
pub fn scan(plugins_dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return Vec::new();
    };
    let mut out: Vec<Plugin> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|root| {
            let manifest: serde_json::Value =
                std::fs::read_to_string(root.join(".claude-plugin").join("plugin.json"))
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default();
            let dir_name = root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let field = |k: &str| manifest[k].as_str().unwrap_or_default().to_string();
            // Le nom est celui du dossier, que le plugin ne choisit pas : c'est lui que
            // nomme la politique du propriétaire (`plugins.allowed_mcp_servers`).
            Plugin {
                name: dir_name,
                version: field("version"),
                description: field("description"),
                root,
            }
        })
        .filter(|p| penelope_platform::validate_slug(&p.name).is_ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Skills et commandes des plugins d'un dossier.
pub fn skills(plugins_dir: &Path) -> (Vec<Skill>, Vec<SkillError>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    for plugin in scan(plugins_dir) {
        let (skills, errs) = scan_dir(&plugin.root.join("skills"), Scope::Plugin);
        ok.extend(skills);
        errors.extend(errs);
        let Ok(entries) = std::fs::read_dir(plugin.root.join("commands")) else {
            continue;
        };
        let mut commands: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .collect();
        commands.sort();
        for path in commands {
            match command_skill(&plugin.name, &path) {
                Ok(s) => ok.push(s),
                Err(e) => errors.push(e),
            }
        }
    }
    (ok, errors)
}

/// Une commande `commands/<c>.md` lue comme une skill `<plugin>-<c>`.
fn command_skill(plugin: &str, path: &Path) -> Result<Skill, SkillError> {
    let fail = |m: String| SkillError {
        path: path.to_path_buf(),
        message: m,
    };
    let raw = std::fs::read_to_string(path).map_err(|e| fail(e.to_string()))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let (description, body) = match frontmatter::parse(&raw) {
        Ok(fm) => (fm.string("description"), fm.body),
        Err(_) => (String::new(), raw.clone()),
    };
    let description = if description.trim().is_empty() {
        format!("Commande /{stem} du plugin {plugin}")
    } else {
        description
    };
    let name = format!("{plugin}-{stem}");
    let synthetic = format!(
        "---\nname: {name}\ndescription: {}\n---\n{body}",
        description.replace('\n', " ")
    );
    parse_skill(path, &synthetic, Scope::Plugin)
}

/// Un sous-agent déclaré en fichier (`agents/*.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Alias de modèle ; vide : celui du parent.
    pub model: String,
    /// Outils de Pénélope permis (les noms de Claude Code sont traduits) ; vide : les
    /// outils en lecture.
    pub tools: Vec<String>,
    /// La consigne du sous-agent : le corps du fichier.
    pub prompt: String,
    pub path: PathBuf,
}

/// Les noms d'outils de Claude Code et leur équivalent ici ; un nom inconnu est écarté.
fn translate_tool(name: &str) -> Option<&'static str> {
    Some(match name.trim() {
        "Read" | "fs_read" => "fs_read",
        "Grep" | "fs_search" => "fs_search",
        "Glob" | "LS" | "fs_list" => "fs_list",
        "Edit" | "MultiEdit" | "fs_edit" => "fs_edit",
        "Write" | "fs_write" => "fs_write",
        "Bash" | "shell_exec" => "shell_exec",
        "WebFetch" | "http_fetch" => "http_fetch",
        "WebSearch" | "web_search" => "web_search",
        "git_diff" => "git_diff",
        _ => return None,
    })
}

/// Les dossiers où chercher des sous-agents : `{data}/agents`, puis les `agents/` des
/// plugins.
pub fn agent_dirs(data: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![data.join("agents")];
    dirs.extend(
        scan(&data.join("plugins"))
            .into_iter()
            .map(|p| p.root.join("agents")),
    );
    dirs
}

/// Lit un sous-agent déclaré.
pub fn parse_agent(path: &Path, raw: &str) -> Option<AgentDef> {
    let fm = frontmatter::parse(raw).ok()?;
    let name = fm.string("name");
    if penelope_platform::validate_slug(&name).is_err() || fm.body.trim().is_empty() {
        return None;
    }
    // `tools` : une liste, ou une chaîne « Read, Grep » à la manière de Claude Code.
    let mut raw_tools: Vec<String> = fm
        .list("tools")
        .iter()
        .flat_map(|t| t.split(',').map(str::to_string).collect::<Vec<_>>())
        .collect();
    if raw_tools.is_empty() {
        raw_tools = fm.string("tools").split(',').map(str::to_string).collect();
    }
    let mut tools: Vec<String> = raw_tools
        .iter()
        .filter_map(|t| translate_tool(t))
        .map(String::from)
        .collect();
    tools.sort();
    tools.dedup();
    Some(AgentDef {
        name,
        description: fm.string("description"),
        model: fm.string("model"),
        tools,
        prompt: fm.body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

/// Tous les sous-agents déclarés, le premier trouvé l'emportant à nom égal.
pub fn agents(data: &Path) -> Vec<AgentDef> {
    let mut out: Vec<AgentDef> = Vec::new();
    for dir in agent_dirs(data) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .collect();
        paths.sort();
        for path in paths {
            if let Some(def) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| parse_agent(&path, &raw))
                && !out.iter().any(|a| a.name == def.name)
            {
                out.push(def);
            }
        }
    }
    out
}

/// Le sous-agent déclaré de ce nom.
pub fn agent(data: &Path, name: &str) -> Option<AgentDef> {
    agents(data).into_iter().find(|a| a.name == name)
}

#[cfg(test)]
mod tests;
