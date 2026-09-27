use super::*;

#[test]
fn which_finds_a_standard_binary() {
    assert!(which("sh").is_some() || which("cmd").is_some());
    assert!(which("ce-binaire-nexiste-vraiment-pas-42").is_none());
}

#[test]
fn which_accepts_absolute_paths() {
    if cfg!(unix) {
        assert_eq!(which("/bin/sh"), Some(PathBuf::from("/bin/sh")));
    }
}

#[test]
fn default_inherited_env_has_no_secrets() {
    let v = default_inherited_env();
    for k in &v {
        let up = k.to_uppercase();
        assert!(
            !up.contains("KEY") && !up.contains("TOKEN") && !up.contains("SECRET"),
            "variable sensible dans la liste blanche : {k}"
        );
    }
}

#[tokio::test]
#[cfg(unix)]
async fn spawn_and_terminate_a_child() {
    let dir = tempfile::tempdir().unwrap();
    let host = UnixProcessHost::new(dir.path());
    let spec = ProcessSpec::new("sleep").arg("30").pid_tag("test");
    let mut child = host.spawn(spec, None).await.unwrap();
    assert!(child.pid > 0);
    assert!(dir.path().join("test.pid").exists());

    host.terminate(&mut child, std::time::Duration::from_millis(500))
        .await
        .unwrap();
    assert!(
        !dir.path().join("test.pid").exists(),
        "le fichier PID doit être nettoyé"
    );
}

#[test]
fn environment_is_filtered() {
    let spec = ProcessSpec::new("sleep").env("PENELOPE_VISIBLE", "1");
    let env = effective_env(&spec);
    assert_eq!(env.get("PENELOPE_VISIBLE").map(String::as_str), Some("1"));
    // Seules les variables de la liste blanche (plus celles posées explicitement)
    // peuvent apparaître : aucune clé d'API du daemon ne fuit vers un serveur MCP.
    for k in env.keys() {
        assert!(
            spec.inherit_env.contains(k) || spec.env.contains_key(k),
            "variable hors liste blanche transmise : {k}"
        );
    }
    assert!(!env.contains_key("OPENROUTER_API_KEY"));
}

#[test]
fn paths_keep_the_user_order_and_add_existing_extras() {
    let dir = tempfile::tempdir().unwrap();
    let brew = dir.path().join("brew/bin");
    let missing = dir.path().join("absent/bin");
    std::fs::create_dir_all(&brew).unwrap();
    let current = std::env::join_paths(["/usr/bin", "/mon/outil"]).unwrap();
    let merged = merge_paths(Some(&current), &[brew.clone(), missing, "/usr/bin".into()]);
    let parts: Vec<PathBuf> = std::env::split_paths(&merged).collect();
    assert_eq!(
        parts,
        vec![PathBuf::from("/usr/bin"), PathBuf::from("/mon/outil"), brew],
        "ordre de l'utilisateur, extras existants seulement, pas de doublon"
    );
}

#[test]
#[cfg(unix)]
fn versions_are_probed_with_a_timeout() {
    let git = which("git").expect("git est requis pour les tests");
    let v = probe_version(&git, std::time::Duration::from_secs(5)).unwrap();
    assert!(v.starts_with("git version"), "{v}");
    // Un programme qui échoue à `--version`, ou qui ne répond pas dans le délai, ne
    // passe pas pour sain. Des scripts plutôt que `sleep` : le `sleep` de GNU répond à
    // `--version`, celui de BSD non (issue #102).
    let dir = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let fails = script("echoue", "exit 1");
    assert!(probe_version(&fails, std::time::Duration::from_secs(5)).is_none());
    let mute = script("muet", "sleep 5");
    let t = std::time::Instant::now();
    assert!(probe_version(&mute, std::time::Duration::from_millis(200)).is_none());
    assert!(t.elapsed() < std::time::Duration::from_secs(3));
}

#[test]
fn the_newest_nvm_node_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".nvm/versions/node");
    for v in ["v18.20.4", "v22.11.0", "v9.0.0"] {
        std::fs::create_dir_all(root.join(v).join("bin")).unwrap();
    }
    assert_eq!(
        newest_nvm_bin(&root),
        Some(root.join("v22.11.0").join("bin"))
    );
    let extras = extra_bin_dirs(Some(dir.path()));
    assert!(extras.contains(&root.join("v22.11.0").join("bin")));
    assert!(extras.contains(&dir.path().join(".local/bin")));
}

#[test]
#[cfg(target_os = "macos")]
fn a_service_path_still_finds_homebrew_tools() {
    // Sous `launchd`, le PATH ne contient que les répertoires système.
    let minimal = std::env::join_paths(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]).unwrap();
    let merged = merge_paths(Some(&minimal), &extra_bin_dirs(None));
    let parts: Vec<PathBuf> = std::env::split_paths(&merged).collect();
    if Path::new("/opt/homebrew/bin").is_dir() {
        assert!(parts.contains(&PathBuf::from("/opt/homebrew/bin")));
    }
}

#[test]
fn explicit_env_overrides_inherited() {
    let mut spec = ProcessSpec::new("sleep");
    spec.inherit_env = vec!["PATH".into()];
    let spec = spec.env("PATH", "/chemin/impose");
    assert_eq!(
        effective_env(&spec).get("PATH").map(String::as_str),
        Some("/chemin/impose")
    );
}

#[test]
fn default_shell_is_reasonable() {
    let (prog, args) = default_shell();
    assert!(!prog.is_empty());
    assert!(!args.is_empty());
}

#[test]
fn reap_orphans_removes_stale_pid_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mort.pid"), "999999999").unwrap();
    std::fs::write(dir.path().join("pasunpid.pid"), "abc").unwrap();
    let host = UnixProcessHost::new(dir.path());
    host.reap_orphans(dir.path()).unwrap();
    assert!(!dir.path().join("mort.pid").exists());
    assert!(!dir.path().join("pasunpid.pid").exists());
}
