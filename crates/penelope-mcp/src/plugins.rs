//! Les serveurs MCP des plugins (#393) : `.mcp.json` d'un plugin au format Claude Code,
//! lu en déclarations dérivées, jamais écrit dans le `mcp.d` du propriétaire.
//!
//! Un serveur prend le nom `<plugin>-<serveur>` ; `${CLAUDE_PLUGIN_ROOT}` est remplacé
//! par le dossier du plugin. Il ne démarre que si la politique du propriétaire
//! (`plugins.allowed_mcp_servers`) l'autorise ; sinon il est rendu en erreur avec la
//! raison, visible dans `mcp list` et `doctor`.

use crate::config::ServerConfig;
use std::collections::BTreeMap;
use std::path::Path;

/// Nom d'un plugin : celui de son `plugin.json` s'il est valide, sinon son dossier.
fn plugin_name(root: &Path) -> Option<String> {
    let manifest: serde_json::Value =
        std::fs::read_to_string(root.join(".claude-plugin").join("plugin.json"))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
    let declared = manifest["name"].as_str().unwrap_or_default();
    let name = if penelope_platform::validate_slug(declared).is_ok() {
        declared.to_string()
    } else {
        root.file_name()?.to_string_lossy().to_string()
    };
    penelope_platform::validate_slug(&name)
        .is_ok()
        .then_some(name)
}

/// Les déclarations d'un `.mcp.json` : `{"mcpServers": {…}}` ou la table seule.
fn declared(root: &Path) -> BTreeMap<String, serde_json::Value> {
    let raw = std::fs::read_to_string(root.join(".mcp.json")).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let table = if v.get("mcpServers").is_some() {
        &v["mcpServers"]
    } else {
        &v
    };
    table
        .as_object()
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn strings(v: &serde_json::Value, root: &str) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str())
        .map(|s| s.replace("${CLAUDE_PLUGIN_ROOT}", root))
        .collect()
}

fn table(v: &serde_json::Value, root: &str) -> BTreeMap<String, String> {
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, x)| {
            Some((
                k.clone(),
                x.as_str()?.replace("${CLAUDE_PLUGIN_ROOT}", root),
            ))
        })
        .collect()
}

/// Une déclaration de Claude Code en configuration d'ici.
fn to_config(name: &str, decl: &serde_json::Value, root: &str) -> Result<ServerConfig, String> {
    let kind = decl["type"].as_str().unwrap_or_default();
    let url = decl["url"].as_str().unwrap_or_default();
    let command = decl["command"].as_str().unwrap_or_default();
    let mut cfg = if !command.is_empty() && matches!(kind, "" | "stdio") {
        let mut c = ServerConfig::stdio(name, &command.replace("${CLAUDE_PLUGIN_ROOT}", root), &[]);
        c.args = strings(&decl["args"], root);
        c.env = table(&decl["env"], root);
        c.cwd = root.to_string();
        c
    } else if !url.is_empty() && matches!(kind, "" | "http" | "sse") {
        let mut c = ServerConfig::http(name, url);
        if kind == "sse" {
            c.transport = "sse".into();
        }
        c.headers = table(&decl["headers"], root);
        c
    } else {
        return Err("ni `command` ni `url` utilisable".into());
    };
    cfg.enabled = true;
    cfg.validate().map_err(|e| e.to_string())?;
    Ok(cfg)
}

/// Les serveurs des plugins de `plugins_dir` ; `allows(plugin, serveur)` est la politique
/// du propriétaire. Rend les serveurs autorisés et valides, puis les autres avec leur
/// raison.
pub fn load(
    plugins_dir: &Path,
    allows: impl Fn(&str, &str) -> bool,
) -> (Vec<ServerConfig>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut refused = Vec::new();
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return (ok, refused);
    };
    let mut roots: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    roots.sort();
    for root in roots {
        let Some(plugin) = plugin_name(&root) else {
            continue;
        };
        let root_str = root.to_string_lossy().to_string();
        for (server, decl) in declared(&root) {
            let name = format!("{plugin}-{server}");
            if penelope_platform::validate_slug(&name).is_err() {
                refused.push((name, "nom de serveur invalide".into()));
                continue;
            }
            if !allows(&plugin, &name) {
                refused.push((
                    name.clone(),
                    format!(
                        "serveur du plugin {plugin} non autorisé : ajouter `{name}` à \
                         `plugins.allowed_mcp_servers`"
                    ),
                ));
                continue;
            }
            match to_config(&name, &decl, &root_str) {
                Ok(c) => ok.push(c),
                Err(e) => refused.push((name, e)),
            }
        }
    }
    (ok, refused)
}

/// Empreinte des `.mcp.json` des plugins, pour le rechargement à chaud.
pub fn fingerprint(plugins_dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(plugins_dir) else {
        return String::new();
    };
    let mut parts: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let f = e.path().join(".mcp.json");
            let meta = std::fs::metadata(&f).ok()?;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            Some(format!("{}:{mtime}:{}", f.display(), meta.len()))
        })
        .collect();
    parts.sort();
    parts.join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(dir: &Path, name: &str, mcp: &str) {
        let root = dir.join(name);
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"{name}"}}"#),
        )
        .unwrap();
        std::fs::write(root.join(".mcp.json"), mcp).unwrap();
    }

    #[test]
    fn a_plugin_server_starts_only_when_the_owner_allows_it() {
        let dir = tempfile::tempdir().unwrap();
        plugin(
            dir.path(),
            "notes",
            r#"{"mcpServers":{"index":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/idx","args":["--root","${CLAUDE_PLUGIN_ROOT}"],"env":{"K":"v"}},
                "web":{"type":"http","url":"https://example.org/mcp"}}}"#,
        );
        plugin(dir.path(), "autre", r#"{"srv":{"command":"x"}}"#);
        let (ok, refused) = load(dir.path(), |p, n| p == "notes" && n == "notes-index");
        assert_eq!(ok.len(), 1);
        let root = dir.path().join("notes").to_string_lossy().to_string();
        assert_eq!(ok[0].name, "notes-index");
        assert_eq!(ok[0].command, format!("{root}/bin/idx"));
        assert_eq!(ok[0].args, ["--root".to_string(), root.clone()]);
        assert_eq!(ok[0].cwd, root);
        let names: Vec<&str> = refused.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["autre-srv", "notes-web"]);
        assert!(refused[0].1.contains("plugins.allowed_mcp_servers"));
        let (ok, _) = load(dir.path(), |_, _| true);
        assert_eq!(ok.len(), 3);
        assert_eq!(
            ok.iter().find(|c| c.name == "notes-web").unwrap().url,
            "https://example.org/mcp"
        );
    }

    #[test]
    fn a_broken_declaration_is_refused_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        plugin(dir.path(), "casse", r#"{"mcpServers":{"vide":{}}}"#);
        let (ok, refused) = load(dir.path(), |_, _| true);
        assert!(ok.is_empty());
        assert_eq!(refused[0].0, "casse-vide");
        assert!(!fingerprint(dir.path()).is_empty());
    }
}
