//! Ce qu'apportent les plugins déposés (#393) : pour chacun, ses skills et commandes, ses
//! sous-agents et ses serveurs MCP, autorisés ou refusés avec la raison. Lu sur disque,
//! sans le daemon : `penelope plugin list` et `show`.

use penelope_kernel::config::Plugins;
use serde_json::{Value, json};
use std::path::Path;

/// Le rapport de tous les plugins de `{data}/plugins`.
pub fn report(data: &Path, policy: &Plugins) -> Vec<Value> {
    let dir = data.join("plugins");
    let (skills, skill_errors) = penelope_skills::plugins::skills(&dir);
    let agents = penelope_skills::plugins::agents(data);
    let (servers, refused) = penelope_mcp::plugins::load(&dir, |p, s| policy.allows_mcp(p, s));
    let (hooks, hooks_refused) = penelope_tools::hooks::plugins::load(&dir, &|p, e| {
        penelope_tools::hooks::allowed(&policy.allowed_hooks, p, e)
    });
    // `<plugin>-<événement>-<n>` : le reste après le plugin commence par un événement.
    const EVENTS: &[&str] = &[
        "session_start",
        "prompt_submit",
        "pre_tool",
        "post_tool",
        "turn_end",
    ];
    let hook_of = |plugin: &str, name: &str| {
        name.strip_prefix(&format!("{plugin}-"))
            .is_some_and(|rest| EVENTS.iter().any(|e| rest.starts_with(&format!("{e}-"))))
            || name.starts_with(&format!("{plugin}/"))
            || name == plugin
    };
    penelope_skills::plugins::scan(&dir)
        .into_iter()
        .map(|p| {
            let mine = |path: &Path| path.starts_with(&p.root);
            let names = penelope_mcp::plugins::server_names(&p.root);
            json!({
                "name": p.name,
                "version": p.version,
                "description": p.description,
                "root": p.root,
                "skills": skills.iter().filter(|s| mine(&s.path)).map(|s| &s.name).collect::<Vec<_>>(),
                "skill_errors": skill_errors.iter().filter(|e| mine(&e.path))
                    .map(|e| format!("{} : {}", e.path.display(), e.message)).collect::<Vec<_>>(),
                "agents": agents.iter().filter(|a| mine(&a.path)).map(|a| &a.name).collect::<Vec<_>>(),
                "mcp_servers": servers.iter().filter(|c| names.contains(&c.name))
                    .map(|c| &c.name).collect::<Vec<_>>(),
                "mcp_refused": refused.iter().filter(|(n, _)| names.contains(n))
                    .map(|(n, r)| json!({"name": n, "reason": r})).collect::<Vec<_>>(),
                "hooks": hooks.iter().filter(|h| hook_of(&p.name, &h.name)).map(|h| &h.name).collect::<Vec<_>>(),
                "hooks_refused": hooks_refused.iter().filter(|(n, _)| hook_of(&p.name, n))
                    .map(|(n, r)| json!({"name": n, "reason": r})).collect::<Vec<_>>(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_report_names_what_it_brings_and_what_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins/notes");
        for (p, body) in [
            (
                "skills/resume/SKILL.md",
                "---\nname: resume\ndescription: Résumer\n---\nRésume.\n",
            ),
            (
                "agents/lecteur.md",
                "---\nname: lecteur\ndescription: Lit\n---\nTu lis.\n",
            ),
            (
                ".mcp.json",
                r#"{"mcpServers":{"index":{"command":"idx"},"web":{"url":"https://example.org/mcp"}}}"#,
            ),
        ] {
            std::fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            std::fs::write(root.join(p), body).unwrap();
        }
        let policy = Plugins {
            allowed_mcp_servers: vec!["notes/index".into()],
            ..Default::default()
        };
        let r = report(dir.path(), &policy);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["name"], "notes");
        assert_eq!(r[0]["skills"], json!(["resume"]));
        assert_eq!(r[0]["agents"], json!(["lecteur"]));
        assert_eq!(r[0]["mcp_servers"], json!(["notes-index"]));
        assert_eq!(r[0]["mcp_refused"][0]["name"], "notes-web");
    }
}
