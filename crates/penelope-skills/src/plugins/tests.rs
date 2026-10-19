use super::*;

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// #393 : un plugin au format Claude Code apporte ses skills, ses commandes (en skills
/// `<plugin>-<commande>`) et ses sous-agents, outils de Claude Code traduits.
#[test]
fn a_claude_code_plugin_brings_skills_commands_and_agents() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    let root = data.join("plugins").join("revue-pr");
    write(
        &root.join(".claude-plugin/plugin.json"),
        r#"{"name": "revue-pr", "version": "1.2.0", "description": "Relire une PR"}"#,
    );
    write(
        &root.join("skills/checklist/SKILL.md"),
        "---\nname: checklist-revue\ndescription: La liste de relecture\n---\nRelis les tests.\n",
    );
    write(
        &root.join("commands/review.md"),
        "---\ndescription: Lance une revue\nargument-hint: <pr>\n---\nRelis la PR $ARGUMENTS.\n",
    );
    write(
        &root.join("agents/relecteur.md"),
        "---\nname: relecteur\ndescription: Relit un diff\ntools: Read, Grep, Bash, Teleport\nmodel: sonnet\n---\nTu relis un diff.\n",
    );
    write(
        &data.join("agents/chercheur.md"),
        "---\nname: chercheur\ndescription: Cherche\ntools:\n  - WebFetch\n---\nTu cherches.\n",
    );

    let plugins = scan(&data.join("plugins"));
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].version, "1.2.0");

    let (skills, errors) = skills(&data.join("plugins"));
    assert!(errors.is_empty(), "{errors:?}");
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["checklist-revue", "revue-pr-review"]);
    assert!(skills.iter().all(|s| s.scope == Scope::Plugin));
    assert_eq!(skills[1].description, "Lance une revue");

    let all = agents(data);
    let names: Vec<&str> = all.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        ["chercheur", "relecteur"],
        "{data:?} d'abord, puis les plugins"
    );
    let r = agent(data, "relecteur").unwrap();
    assert_eq!(
        r.tools,
        ["fs_read", "fs_search", "shell_exec"],
        "Teleport écarté"
    );
    assert_eq!(r.model, "sonnet");
    assert_eq!(r.prompt, "Tu relis un diff.");
    assert_eq!(agent(data, "chercheur").unwrap().tools, ["http_fetch"]);
    assert!(agent(data, "absent").is_none());
}
