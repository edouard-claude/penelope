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
#[cfg(unix)]
fn a_probe_reads_a_large_output_without_waiting_for_the_timeout() {
    // #372 : `plutil` sur l'`Info.plist` de Xcode écrit 157 Ko ; sans lecteur, le tuyau
    // (64 Ko) se remplissait et la sonde mourait au délai, sans la version.
    let dir = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.path().join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let big = script(
        "bavard",
        "head -c 1048576 /dev/zero | tr '\\0' x; echo; echo fin >&2",
    );
    let t = std::time::Instant::now();
    let p = probe_command(&big, &[], std::time::Duration::from_secs(5)).expect("sonde");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        t.elapsed()
    );
    assert!(p.ok);
    assert_eq!(p.stdout.trim().len(), 1_048_576);
    assert_eq!(p.stderr.trim(), "fin");

    // Un petit-enfant qui garde le tuyau ouvert après la sortie du programme : la sonde
    // rend ce qu'elle a lu à l'échéance, sans attendre le petit-enfant.
    let keeper = script("garde", "echo vu; sleep 5 &");
    let t = std::time::Instant::now();
    // 2 s : sous la charge de la suite complète, 300 ms ne laissaient pas au script le temps
    // de sortir. Le petit-enfant dort 5 s : rendre avant 4 s prouve qu'il n'est pas attendu.
    let p = probe_command(&keeper, &[], std::time::Duration::from_secs(2)).expect("sonde");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        t.elapsed()
    );
    assert!(p.ok);
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

#[cfg(unix)]
async fn sleeper(host: &UnixProcessHost, tag: Option<&str>) -> Child {
    let spec = ProcessSpec::new("sleep").arg("30");
    let spec = match tag {
        Some(t) => spec.pid_tag(t),
        None => spec,
    };
    host.spawn(spec, None).await.unwrap()
}

/// #454 : 270 fichiers de processus morts s'étaient accumulés en six jours, parce que
/// seul `terminate` retirait le sien.
#[tokio::test]
#[cfg(unix)]
async fn a_pid_file_leaves_with_its_process() {
    let dir = tempfile::tempdir().unwrap();
    let host = UnixProcessHost::new(dir.path());
    let file = |tag: &str| dir.path().join(format!("{tag}.pid"));

    // Fin normale.
    let spec = ProcessSpec::new("sleep").arg("0.2").pid_tag("court");
    let mut child = host.spawn(spec, None).await.unwrap();
    let noted = std::fs::read_to_string(file("court")).unwrap();
    assert_eq!(
        identity::noted(&noted).map(|(pid, _)| pid),
        Some(child.pid),
        "le fichier porte le pid et sa date de lancement : {noted:?}"
    );
    child.inner.wait().await.unwrap();
    drop(child);
    assert!(!file("court").exists(), "fin normale : fichier retiré");

    // Groupe tué après un délai, comme le fait `shell_exec`.
    let mut child = sleeper(&host, Some("long")).await;
    terminate_group(child.pid, std::time::Duration::from_millis(500)).await;
    child.inner.wait().await.unwrap();
    drop(child);
    assert!(!file("long").exists(), "groupe tué : fichier retiré");

    // Abandonné vivant : le fichier reste, c'est par lui que le démarrage suivant le
    // retrouve.
    let child = sleeper(&host, Some("vivant")).await;
    let pid = child.pid;
    drop(child);
    assert!(file("vivant").exists());
    assert_eq!(host.reap_orphans(dir.path()).unwrap(), vec![pid]);
    assert!(!file("vivant").exists());
}

/// #454 : un fichier de six jours désignait le pid d'un service du système. Un pid vivant
/// lancé à une autre date n'est pas le nôtre, et un fichier sans date ne désigne personne.
#[tokio::test]
#[cfg(unix)]
async fn reap_orphans_spares_a_pid_that_was_given_to_another_process() {
    let dir = tempfile::tempdir().unwrap();
    let host = UnixProcessHost::new(dir.path());
    let mut other = sleeper(&host, None).await;
    std::fs::write(
        dir.path().join("repris.pid"),
        format!("{}\nThu Jan 1 00:00:00 1970\n", other.pid),
    )
    .unwrap();
    std::fs::write(dir.path().join("ancien.pid"), other.pid.to_string()).unwrap();

    assert_eq!(host.reap_orphans(dir.path()).unwrap(), Vec::<u32>::new());
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        other.inner.try_wait().unwrap().is_none(),
        "le processus d'un autre a été tué"
    );
    assert!(!dir.path().join("repris.pid").exists());
    assert!(!dir.path().join("ancien.pid").exists());
    host.terminate(&mut other, std::time::Duration::from_millis(500))
        .await
        .unwrap();
}

/// #65 reste vrai : même pid, même date de lancement, le groupe est tué.
#[tokio::test]
#[cfg(unix)]
async fn reap_orphans_kills_the_process_it_noted() {
    let dir = tempfile::tempdir().unwrap();
    let host = UnixProcessHost::new(dir.path());
    let mut child = sleeper(&host, Some("orphelin")).await;

    assert_eq!(host.reap_orphans(dir.path()).unwrap(), vec![child.pid]);
    let status = child.inner.wait().await.unwrap();
    assert_eq!(ExitInfo::of(&status).signal, Some(9));
    assert!(!dir.path().join("orphelin.pid").exists());
}
