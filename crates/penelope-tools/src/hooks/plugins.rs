//! Les hooks des plugins (#393) : `hooks/hooks.json` d'un plugin au format Claude Code,
//! traduit en hooks d'ici, sous les mêmes règles que `hooks.d` (#386) : forme exec sans
//! shell, jamais `allow`. Aucun ne tourne sans la politique du propriétaire
//! (`plugins.allowed_hooks`, paires `<plugin>/<événement>` ou `<plugin>/*`).
//!
//! ```json
//! {"hooks": {"PostToolUse": [{"matcher": "Write|Edit",
//!            "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/fmt.sh", "timeout": 30}]}]}}
//! ```
//!
//! Une commande qui demande un shell (tube, `;`, `$`, redirection…) est refusée avec sa
//! raison, comme un événement que la boucle ne sert pas. Un hook qui peut bloquer
//! (`pre_tool`, `prompt_submit`, `turn_end`) échoue fermé : un code 2 de Claude Code ou
//! tout résultat non compris refuse l'action.

use super::{Event, Hook, Match};
use std::path::Path;
use std::time::Duration;

/// L'événement de Claude Code et son nom ici, pour la politique.
fn event_of(name: &str) -> Option<(Event, &'static str)> {
    Some(match name {
        "SessionStart" => (Event::SessionStart, "session_start"),
        "UserPromptSubmit" => (Event::PromptSubmit, "prompt_submit"),
        "PreToolUse" => (Event::PreTool, "pre_tool"),
        "PostToolUse" => (Event::PostTool, "post_tool"),
        "Stop" => (Event::TurnEnd, "turn_end"),
        _ => return None,
    })
}

/// Les noms d'outils de Claude Code dans un `matcher` ; `None` : tous les outils. Un nom
/// sans équivalent ici est écarté ; un matcher dont aucun nom n'a d'équivalent est refusé
/// (il ne répondrait à rien, ou à tout si on l'ignorait).
fn tools_of(matcher: &str) -> Result<Option<String>, String> {
    let m = matcher.trim();
    if m.is_empty() || m == "*" || m == ".*" {
        return Ok(None);
    }
    let mut out: Vec<&str> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();
    for t in m.split('|') {
        let ours = match t.trim() {
            "Write" => "fs_write",
            "Edit" | "MultiEdit" => "fs_edit",
            "Read" => "fs_read",
            "Bash" => "shell_exec",
            "Grep" => "fs_search",
            "Glob" | "LS" => "fs_list",
            "WebFetch" => "http_fetch",
            "WebSearch" => "web_search",
            "Task" | "Agent" => "sub_agent_spawn",
            other => {
                unknown.push(other);
                continue;
            }
        };
        if !out.contains(&ours) {
            out.push(ours);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "aucun outil du matcher n'a d'équivalent ici ({})",
            unknown.join(", ")
        ));
    }
    Ok(Some(out.join("|")))
}

/// Une ligne de commande découpée en programme et arguments, sans shell : guillemets
/// simples et doubles reconnus, tout caractère que seul un shell interpréterait refusé.
fn split_exec(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (
                None,
                '|' | '&' | ';' | '<' | '>' | '$' | '`' | '(' | ')' | '*' | '?' | '~' | '\\',
            ) => {
                return Err(format!(
                    "`{c}` demande un shell : un hook tourne sans shell"
                ));
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("guillemet non fermé".into());
    }
    if started {
        out.push(cur);
    }
    if out.is_empty() {
        return Err("commande vide".into());
    }
    Ok(out)
}

/// Les hooks des plugins de `plugins_dir` ; `allows(plugin, événement)` est la politique
/// du propriétaire. Rend les hooks permis et valides, puis les refusés avec leur raison.
pub fn load(
    plugins_dir: &Path,
    allows: &dyn Fn(&str, &str) -> bool,
) -> (Vec<Hook>, Vec<(String, String)>) {
    let (mut ok, mut refused) = (Vec::new(), Vec::new());
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
        let Some(plugin) = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| penelope_platform::validate_slug(n).is_ok())
        else {
            continue;
        };
        let Ok(raw) = std::fs::read_to_string(root.join("hooks").join("hooks.json")) else {
            continue;
        };
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let root_str = root.to_string_lossy().to_string();
        let Some(events) = doc["hooks"].as_object() else {
            refused.push((plugin.clone(), "hooks.json sans table `hooks`".into()));
            continue;
        };
        for (event_name, groups) in events {
            let Some((event, ours)) = event_of(event_name) else {
                refused.push((
                    format!("{plugin}/{event_name}"),
                    "événement que la boucle ne sert pas".into(),
                ));
                continue;
            };
            for (g, group) in groups.as_array().into_iter().flatten().enumerate() {
                for (h, decl) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                    let name = format!("{plugin}-{ours}-{}", g * 10 + h + 1);
                    if !allows(&plugin, ours) {
                        refused.push((
                            name,
                            format!("hook du plugin {plugin} non autorisé : ajouter `{plugin}/{ours}` à `plugins.allowed_hooks`"),
                        ));
                        continue;
                    }
                    let built = (|| -> Result<Hook, String> {
                        if decl["type"].as_str().unwrap_or("command") != "command" {
                            return Err("seul `type = command` est servi".into());
                        }
                        let line = decl["command"]
                            .as_str()
                            .unwrap_or_default()
                            .replace("${CLAUDE_PLUGIN_ROOT}", &root_str);
                        let mut argv = split_exec(&line)?;
                        let command = argv.remove(0);
                        let hook = Hook {
                            name: name.clone(),
                            on: event,
                            matcher: Match {
                                // Seuls les hooks d'outils filtrent par outil ; ailleurs, le
                                // matcher de Claude Code dit autre chose (`startup`, `resume`
                                // pour SessionStart) et ne restreint rien ici.
                                tool: match event {
                                    Event::PreTool | Event::PostTool => {
                                        tools_of(group["matcher"].as_str().unwrap_or_default())?
                                    }
                                    _ => None,
                                },
                                path: None,
                            },
                            command,
                            args: argv,
                            cwd: None,
                            timeout: decl["timeout"]
                                .as_u64()
                                .map(Duration::from_secs)
                                .unwrap_or(super::DEFAULT_TIMEOUT)
                                .min(super::DEFAULT_TIMEOUT),
                            sandbox: super::Sandbox::WorkspaceWrite,
                            // Un hook de garde de Claude Code refuse par le code 2 et
                            // un motif sur stderr, que la boucle ne lit pas comme un
                            // verdict : là où il peut bloquer, un résultat non compris
                            // refuse (défaut fermé) ; ailleurs, il laisse une note.
                            on_failure: match event {
                                Event::PreTool | Event::PromptSubmit | Event::TurnEnd => {
                                    super::OnFailure::Block
                                }
                                Event::PostTool | Event::SessionStart => super::OnFailure::Continue,
                            },
                            deliver: super::Deliver::Note,
                            env: Default::default(),
                            input: super::Input::None,
                        };
                        super::validate(&hook)?;
                        Ok(hook)
                    })();
                    match built {
                        Ok(hk) => ok.push(hk),
                        Err(e) => refused.push((name, e)),
                    }
                }
            }
        }
    }
    (ok, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(dir: &Path, name: &str, hooks: &str) {
        let root = dir.join(name).join("hooks");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("hooks.json"), hooks).unwrap();
    }

    #[test]
    fn a_plugin_hook_is_translated_in_exec_form_and_only_when_allowed() {
        let dir = tempfile::tempdir().unwrap();
        plugin(
            dir.path(),
            "fmt",
            r#"{"hooks": {
                "PostToolUse": [{"matcher": "Write|Edit|MultiEdit",
                    "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/fmt --fix 'a b'", "timeout": 30},
                              {"type": "command", "command": "cat x | tee y"}]}],
                "Notification": [{"hooks": [{"type": "command", "command": "say"}]}],
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "garde"}]}]}}"#,
        );
        let (ok, refused) = load(dir.path(), &|p, e| p == "fmt" && e == "post_tool");
        assert_eq!(ok.len(), 1);
        let root = dir.path().join("fmt").to_string_lossy().to_string();
        assert_eq!(ok[0].name, "fmt-post_tool-1");
        assert_eq!(ok[0].on, Event::PostTool);
        assert_eq!(ok[0].command, format!("{root}/bin/fmt"));
        assert_eq!(ok[0].args, ["--fix", "a b"]);
        assert_eq!(ok[0].matcher.tool.as_deref(), Some("fs_write|fs_edit"));
        assert_eq!(ok[0].timeout, Duration::from_secs(30));
        assert_eq!(ok[0].on_failure, super::super::OnFailure::Continue);
        let (guards, _) = load(dir.path(), &|_, e| e == "pre_tool");
        assert_eq!(
            guards[0].on_failure,
            super::super::OnFailure::Block,
            "défaut fermé"
        );
        let reasons: Vec<String> = refused.iter().map(|(n, r)| format!("{n}: {r}")).collect();
        assert!(
            reasons
                .iter()
                .any(|r| r.starts_with("fmt-post_tool-2") && r.contains("shell")),
            "{reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("Notification")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|r| r.starts_with("fmt-pre_tool-1") && r.contains("`fmt/pre_tool`")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_matcher_keeps_what_has_an_equivalent_and_session_start_ignores_it() {
        assert_eq!(
            tools_of("NotebookEdit|Write|Edit").unwrap().as_deref(),
            Some("fs_write|fs_edit")
        );
        assert!(tools_of("Skill").is_err());
        assert_eq!(
            tools_of("Agent").unwrap().as_deref(),
            Some("sub_agent_spawn")
        );
        let dir = tempfile::tempdir().unwrap();
        plugin(
            dir.path(),
            "s",
            r#"{"hooks":{"SessionStart":[{"matcher":"startup","hooks":[{"type":"command","command":"accueil"}]}]}}"#,
        );
        let (ok, refused) = load(dir.path(), &|_, _| true);
        assert!(refused.is_empty(), "{refused:?}");
        assert_eq!(ok[0].matcher.tool, None);
    }

    #[test]
    fn exec_splitting_refuses_what_needs_a_shell() {
        assert_eq!(split_exec("a 'b c' \"d\"").unwrap(), ["a", "b c", "d"]);
        for bad in ["a; b", "a && b", "echo $HOME", "a > f", "`x`", "a 'b"] {
            assert!(split_exec(bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::super::Registry;
    use std::sync::{Arc, Mutex};

    /// Le registre ajoute les hooks d'un plugin quand la politique les autorise, et se
    /// relit quand elle change.
    #[test]
    fn the_registry_follows_the_owners_policy() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        let root = plugins.join("fmt/hooks");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"bilan"}]}]}}"#,
        )
        .unwrap();
        let list = Arc::new(Mutex::new(Vec::<String>::new()));
        let policy = {
            let list = list.clone();
            Arc::new(move || list.lock().unwrap().clone())
        };
        let reg = Registry::new(dir.path().join("hooks.d")).with_plugins(plugins, policy);
        let first = reg.current();
        assert!(first.hooks.is_empty());
        assert!(
            first
                .invalid
                .iter()
                .any(|(n, r)| n == "fmt-turn_end-1" && r.contains("fmt/turn_end"))
        );
        list.lock().unwrap().push("fmt/*".into());
        let second = reg.current();
        assert_eq!(second.hooks.len(), 1);
        assert_eq!(second.hooks[0].command, "bilan");
    }
}
