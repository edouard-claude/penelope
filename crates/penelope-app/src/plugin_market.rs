//! Marketplaces de plugins au format Claude Code (#393) : un dépôt qui porte
//! `.claude-plugin/marketplace.json` et la liste de ses plugins.
//!
//! - `penelope plugin marketplace add <source>` copie ou clone la marketplace sous
//!   `{data}/plugins/.marketplaces/<nom>` et l'inscrit dans `known_marketplaces.json` ;
//! - `penelope plugin install <plugin>@<marketplace>` dépose le plugin (chemin relatif
//!   resté dans la marketplace, ou dépôt `github`/`url`) et l'inscrit dans
//!   `installed_plugins.json`.
//!
//! Les deux registres suivent la forme de ceux du CLI (`~/.claude/plugins`), clés et
//! champs compris, pour qu'un humain ou un outil passe de l'un à l'autre. Installer
//! n'active rien qui exécute : serveurs et hooks attendent la politique du propriétaire.

use crate::plugin_install::{SOURCE_FILE, fetch, fetch_remote, name_of};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const KNOWN: &str = "known_marketplaces.json";
pub const INSTALLED: &str = "installed_plugins.json";
const MARKETS: &str = ".marketplaces";

fn plugins_dir(data: &Path) -> PathBuf {
    data.join("plugins")
}

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}))
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    penelope_kernel::config::atomic_write(path, format!("{raw}\n").as_bytes())
        .map_err(|e| e.to_string())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// La source d'un `known_marketplaces.json` : `github`, `git` ou `directory`.
fn source_entry(source: &str) -> Value {
    if Path::new(source).is_dir() {
        return json!({"source": "directory", "path": source});
    }
    if let Some(repo) = source
        .strip_prefix("https://github.com/")
        .map(|r| r.trim_end_matches(".git").trim_end_matches('/'))
    {
        return json!({"source": "github", "repo": repo});
    }
    json!({"source": "git", "url": source})
}

/// Inscrit la marketplace `source` ; rend son nom.
pub fn add_marketplace(data: &Path, source: &str) -> Result<String, String> {
    let markets = plugins_dir(data).join(MARKETS);
    std::fs::create_dir_all(&markets).map_err(|e| e.to_string())?;
    let staging = markets.join(".arrivee");
    let _ = std::fs::remove_dir_all(&staging);
    fetch(source, &staging, &markets.join(".clone"))?;
    let manifest = read_json(&staging.join(".claude-plugin").join("marketplace.json"));
    if !manifest["plugins"].is_array() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("pas de `.claude-plugin/marketplace.json` avec une liste `plugins`".into());
    }
    let declared = manifest["name"].as_str().unwrap_or_default();
    let name = if penelope_platform::validate_slug(declared).is_ok() {
        declared.to_string()
    } else {
        name_of(source)
    };
    if penelope_platform::validate_slug(&name).is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("nom de marketplace `{name}` invalide"));
    }
    let dest = markets.join(&name);
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&staging, &dest).map_err(|e| e.to_string())?;
    let known_path = plugins_dir(data).join(KNOWN);
    let mut known = read_json(&known_path);
    known[&name] = json!({
        "source": source_entry(source),
        "installLocation": dest,
        "lastUpdated": now(),
    });
    write_json(&known_path, &known)?;
    Ok(name)
}

/// Les marketplaces inscrites et leurs plugins.
pub fn list(data: &Path) -> Value {
    let known = read_json(&plugins_dir(data).join(KNOWN));
    let mut out = Vec::new();
    for (name, entry) in known.as_object().into_iter().flatten() {
        let dir = plugins_dir(data).join(MARKETS).join(name);
        let manifest = read_json(&dir.join(".claude-plugin").join("marketplace.json"));
        let plugins: Vec<Value> = manifest["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| json!({"name": p["name"], "description": p["description"], "version": p["version"]}))
            .collect();
        out.push(json!({"name": name, "source": entry["source"], "plugins": plugins}));
    }
    Value::Array(out)
}

/// Installe `<plugin>@<marketplace>` ; rend le chemin du plugin.
pub fn install(data: &Path, spec: &str) -> Result<PathBuf, String> {
    let (plugin, market) = spec
        .split_once('@')
        .ok_or("forme attendue : `<plugin>@<marketplace>`")?;
    for n in [plugin, market] {
        penelope_platform::validate_slug(n).map_err(|_| format!("nom `{n}` invalide"))?;
    }
    let mdir = plugins_dir(data).join(MARKETS).join(market);
    let manifest = read_json(&mdir.join(".claude-plugin").join("marketplace.json"));
    let entry = manifest["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == plugin)
        .ok_or_else(|| format!("aucun plugin `{plugin}` dans la marketplace `{market}`"))?;
    let mut remote = true;
    let source = match &entry["source"] {
        Value::String(rel) => {
            // Un chemin relatif reste dans la marketplace : `../../.ssh` est refusé.
            let root = std::fs::canonicalize(&mdir).map_err(|e| e.to_string())?;
            let path = std::fs::canonicalize(root.join(rel))
                .map_err(|e| format!("source `{rel}` : {e}"))?;
            if !path.starts_with(&root) {
                return Err(format!("source `{rel}` hors de la marketplace"));
            }
            remote = false;
            path.to_string_lossy().to_string()
        }
        v if v["source"] == "github" => {
            let repo = v["repo"].as_str().unwrap_or_default();
            let ok = |p: &str| {
                !p.is_empty()
                    && p != ".."
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            };
            match repo.split_once('/') {
                Some((o, r)) if ok(o) && ok(r) => format!("https://github.com/{repo}.git"),
                _ => return Err(format!("dépôt github `{repo}` invalide (propriétaire/nom)")),
            }
        }
        v if v["source"] == "url" || v["source"] == "git" => {
            v["url"].as_str().unwrap_or_default().to_string()
        }
        other => return Err(format!("source de plugin non servie : {other}")),
    };
    let dest = plugins_dir(data).join(plugin);
    if dest.exists() {
        return Err(format!(
            "le plugin `{plugin}` existe déjà : `penelope plugin remove {plugin}` d'abord"
        ));
    }
    let tmp = plugins_dir(data).join(format!(".clone-{plugin}"));
    if remote {
        fetch_remote(&source, &dest, &tmp)?;
    } else {
        fetch(&source, &dest, &tmp)?;
    }
    std::fs::write(dest.join(SOURCE_FILE), spec).map_err(|e| e.to_string())?;
    let at = now();
    record(
        data,
        spec,
        json!({
            "scope": "user",
            "installPath": dest,
            "version": entry["version"].as_str().unwrap_or("unknown"),
            "installedAt": at,
            "lastUpdated": at,
        }),
    )?;
    Ok(dest)
}

/// Inscrit `spec` dans `installed_plugins.json`, à la forme du CLI (version 2 : une
/// liste d'installations par plugin). Les autres entrées ne bougent pas.
fn record(data: &Path, spec: &str, row: Value) -> Result<(), String> {
    let path = plugins_dir(data).join(INSTALLED);
    let mut installed = read_json(&path);
    installed["version"] = json!(2);
    if !installed["plugins"].is_object() {
        installed["plugins"] = json!({});
    }
    installed["plugins"][spec] = json!([row]);
    write_json(&path, &installed)
}

/// Reprend des plugins installés par Claude Code, d'après son `installed_plugins.json`
/// (`~/.claude/plugins`, lu sans le modifier) : chaque plugin de portée `user` est copié
/// dans `{data}/plugins/<nom>` et inscrit ici avec ses champs d'origine. `only` : un seul
/// `<plugin>@<marketplace>`. Rend, par plugin, ce qui a été fait.
pub fn import_claude(
    data: &Path,
    registry: &Path,
    only: Option<&str>,
) -> Result<Vec<String>, String> {
    let raw =
        std::fs::read_to_string(registry).map_err(|e| format!("{} : {e}", registry.display()))?;
    let doc: Value =
        serde_json::from_str(&raw).map_err(|e| format!("{} : {e}", registry.display()))?;
    let plugins = doc["plugins"]
        .as_object()
        .ok_or("pas de table `plugins` : est-ce bien un installed_plugins.json ?")?;
    let mut done = Vec::new();
    for (spec, rows) in plugins {
        if only.is_some_and(|o| o != spec) {
            continue;
        }
        // Version 2 : une liste d'installations ; version 1 : une seule.
        let row = match rows {
            Value::Array(list) => list.iter().find(|r| r["scope"] == "user").cloned(),
            Value::Object(_) => Some(rows.clone()),
            _ => None,
        };
        let Some(row) = row else {
            done.push(format!(
                "{spec} : ignoré (installé pour un projet seulement)"
            ));
            continue;
        };
        let name = spec.split_once('@').map(|(p, _)| p).unwrap_or(spec);
        if penelope_platform::validate_slug(name).is_err() {
            done.push(format!("{spec} : nom invalide"));
            continue;
        }
        let dest = plugins_dir(data).join(name);
        if dest.exists() {
            done.push(format!("{spec} : déjà présent"));
            continue;
        }
        let from = row["installPath"].as_str().unwrap_or_default();
        if !Path::new(from).is_dir() {
            done.push(format!("{spec} : dossier `{from}` absent"));
            continue;
        }
        std::fs::create_dir_all(plugins_dir(data)).map_err(|e| e.to_string())?;
        crate::plugin_install::copy_dir(Path::new(from), &dest).map_err(|e| e.to_string())?;
        std::fs::write(dest.join(SOURCE_FILE), spec).map_err(|e| e.to_string())?;
        let mut mine = row.clone();
        mine["installPath"] = json!(dest);
        mine["scope"] = json!("user");
        record(data, spec, mine)?;
        done.push(format!("{spec} : repris"));
    }
    if only.is_some() && done.is_empty() {
        return Err(format!(
            "`{}` n'est pas dans {}",
            only.unwrap_or_default(),
            registry.display()
        ));
    }
    Ok(done)
}

/// Retire `plugin` de `installed_plugins.json`, quelle que soit sa marketplace.
pub fn forget(data: &Path, plugin: &str) -> Result<(), String> {
    let path = plugins_dir(data).join(INSTALLED);
    let mut installed = read_json(&path);
    let Some(map) = installed["plugins"].as_object_mut() else {
        return Ok(());
    };
    let before = map.len();
    map.retain(|k, _| k.split_once('@').map(|(p, _)| p) != Some(plugin));
    if map.len() != before {
        write_json(&path, &installed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(dir: &Path) -> PathBuf {
        let m = dir.join("ma-place");
        std::fs::create_dir_all(m.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(m.join("plugins/notes/skills/n")).unwrap();
        std::fs::write(
            m.join("plugins/notes/skills/n/SKILL.md"),
            "---\nname: n\ndescription: d\n---\nb\n",
        )
        .unwrap();
        std::fs::write(
            m.join(".claude-plugin/marketplace.json"),
            r#"{"name": "atelier", "owner": {"name": "moi"}, "plugins": [
                {"name": "notes", "source": "./plugins/notes", "version": "1.2.0", "description": "Notes"},
                {"name": "evade", "source": "../../", "description": "hors champ"},
                {"name": "local", "source": {"source": "git", "url": "/home/moi/prive.git"}},
                {"name": "fichier", "source": {"source": "url", "url": "file:///etc/x.git"}},
                {"name": "gh", "source": {"source": "github", "repo": "../../x"}}]}"#,
        )
        .unwrap();
        m
    }

    #[test]
    fn a_marketplace_lists_and_installs_its_plugins_in_the_cli_registries() {
        let src = tempfile::tempdir().unwrap();
        let m = market(src.path());
        let data = tempfile::tempdir().unwrap();
        assert_eq!(
            add_marketplace(data.path(), m.to_str().unwrap()).unwrap(),
            "atelier"
        );
        let known = read_json(&data.path().join("plugins").join(KNOWN));
        assert_eq!(known["atelier"]["source"]["source"], "directory");
        assert!(known["atelier"]["installLocation"].is_string());
        assert_eq!(list(data.path())[0]["plugins"][0]["name"], "notes");

        let dest = install(data.path(), "notes@atelier").unwrap();
        assert!(dest.join("skills/n/SKILL.md").exists());
        let installed = read_json(&data.path().join("plugins").join(INSTALLED));
        assert_eq!(installed["version"], 2);
        let row = &installed["plugins"]["notes@atelier"][0];
        assert_eq!(row["version"], "1.2.0");
        assert_eq!(row["scope"], "user");
        assert!(row["installedAt"].is_string() && row["installPath"].is_string());

        assert!(
            install(data.path(), "evade@atelier")
                .unwrap_err()
                .contains("hors de la marketplace")
        );
        assert!(install(data.path(), "absent@atelier").is_err());
        assert!(install(data.path(), "notes").is_err());
        for bad in ["local", "fichier", "gh"] {
            let e = install(data.path(), &format!("{bad}@atelier")).unwrap_err();
            assert!(
                e.contains("distant") || e.contains("invalide"),
                "{bad} : {e}"
            );
        }
        forget(data.path(), "notes").unwrap();
        let installed = read_json(&data.path().join("plugins").join(INSTALLED));
        assert!(installed["plugins"].as_object().unwrap().is_empty());
        // Le dossier des marketplaces n'est pas pris pour un plugin.
        assert_eq!(
            penelope_skills::plugins::scan(&data.path().join("plugins")).len(),
            1
        );
    }

    #[test]
    fn a_github_source_is_recorded_as_such() {
        assert_eq!(
            source_entry("https://github.com/acme/outils.git"),
            json!({"source": "github", "repo": "acme/outils"})
        );
        assert_eq!(source_entry("git@exemple.fr:a/b.git")["source"], "git");
    }
}

/// Le registre écrit par Claude Code (forme relevée sur un vrai `~/.claude/plugins`) :
/// lu sans le modifier, ses plugins de portée `user` repris, ce qu'on écrit ici garde sa
/// forme.
#[cfg(test)]
mod claude_registry {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/claude-installed_plugins.json");

    #[test]
    fn a_registry_written_by_claude_code_is_read_and_ours_keeps_its_shape() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join("cache");
        for p in [
            "claude-code-plugins/feature-dev/1.0.0",
            "claude-code-plugins/frontend-design/1.1.0",
        ] {
            std::fs::create_dir_all(cache.join(p).join("skills/s")).unwrap();
            std::fs::write(
                cache.join(p).join("skills/s/SKILL.md"),
                "---\nname: s\ndescription: d\n---\nb\n",
            )
            .unwrap();
        }
        let registry = home.path().join("installed_plugins.json");
        let raw = FIXTURE.replace("{CACHE}", &cache.to_string_lossy());
        std::fs::write(&registry, &raw).unwrap();
        let data = tempfile::tempdir().unwrap();

        let done = import_claude(data.path(), &registry, None).unwrap();
        assert_eq!(
            done,
            [
                "feature-dev@claude-code-plugins : repris",
                "frontend-design@claude-code-plugins : repris",
                "projet-seul@claude-plugins-official : ignoré (installé pour un projet seulement)",
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&registry).unwrap(),
            raw,
            "lu sans modification"
        );
        assert!(
            data.path()
                .join("plugins/feature-dev/skills/s/SKILL.md")
                .exists()
        );

        // Ce qu'on écrit : même version, même forme d'entrée, champs d'origine gardés.
        let ours = read_json(&data.path().join("plugins").join(INSTALLED));
        let theirs: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(ours["version"], theirs["version"]);
        let row = &ours["plugins"]["frontend-design@claude-code-plugins"][0];
        let orig = &theirs["plugins"]["frontend-design@claude-code-plugins"][0];
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(row), keys(orig));
        assert_eq!(row["gitCommitSha"], orig["gitCommitSha"]);
        assert_eq!(
            row["installPath"],
            json!(data.path().join("plugins/frontend-design"))
        );

        assert_eq!(
            import_claude(
                data.path(),
                &registry,
                Some("feature-dev@claude-code-plugins")
            )
            .unwrap(),
            ["feature-dev@claude-code-plugins : déjà présent"]
        );
        assert!(import_claude(data.path(), &registry, Some("absent@x")).is_err());
    }
}
