use super::*;

#[test]
fn the_shell_env_never_carries_tokens() {
    for k in SHELL_EXTRA_ENV {
        let u = k.to_uppercase();
        assert!(
            !u.contains("TOKEN") && !u.contains("KEY") && !u.contains("SECRET"),
            "{k}"
        );
    }
    assert!(profile_for("workspace-write", Path::new("/w"), true).allow_network);
}

/// #68 : sous bac à sable, un chemin refusé n'est pas lisible, même par une commande
/// qui a le droit de lire le disque ; le workspace reste lisible.
#[tokio::test]
#[cfg(target_os = "macos")]
async fn a_denied_path_is_unreadable_under_the_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    let secrets = dir.path().join("secrets");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::create_dir_all(&secrets).unwrap();
    std::fs::write(secrets.join("cle.txt"), "CLE-PRIVEE").unwrap();
    std::fs::write(ws.join("note.txt"), "dans le workspace").unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
    let profile = profile_with_denied_reads(
        "workspace-write",
        &ws,
        false,
        std::slice::from_ref(&secrets),
    );

    let refused = exec(
        &host,
        &format!("cat {}", secrets.join("cle.txt").display()),
        ExecOptions {
            profile: Some(&profile),
            cwd: Some(&ws),
            timeout: std::time::Duration::from_secs(20),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            cancel: None,
        },
    )
    .await
    .unwrap();
    assert_ne!(
        refused.exit_code, 0,
        "la lecture doit échouer : {refused:?}"
    );
    assert!(
        !refused.stdout.contains("CLE-PRIVEE"),
        "le contenu ne doit pas sortir : {refused:?}"
    );

    let allowed = exec(
        &host,
        "cat note.txt",
        ExecOptions {
            profile: Some(&profile),
            cwd: Some(&ws),
            timeout: std::time::Duration::from_secs(20),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            cancel: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(allowed.exit_code, 0, "{allowed:?}");
    assert!(allowed.stdout.contains("dans le workspace"), "{allowed:?}");
}

/// #111 : les lectures simples sont reconnues ; un enchaînement, une redirection, une
/// substitution, une écriture ou une option qui lance autre chose ne le sont jamais.
#[test]
fn read_commands_are_told_apart_from_the_rest() {
    for read in [
        "ls -la /tmp",
        "cat x",
        "grep -r foo src",
        "grep -rn \"fn main\" crates",
        "find . -name '*.rs'",
        "head -n 50 README.md",
        "wc -l src/lib.rs",
        "git status",
        "git log --oneline -5",
        "git diff HEAD~1",
        "git branch -a",
        "sort notes.txt",
        // #141 : un opérateur entre guillemets est un caractère, et une affectation
        // de tête qui ne détourne rien laisse la lecture à son programme.
        "grep -n 'x | y' f",
        "echo \"a & b\"",
        "printf \"(%s)\" x",
        "jq -r '.[] | .path' data.json",
        "FOO=1 ls",
        "TZ=UTC date",
        // Un tube vers une lecture pure reste une lecture.
        "cat f | grep -n x",
        "git log --oneline -20 | head -5",
        "ls -la | wc -l",
    ] {
        assert!(is_read_command(read), "{read}");
    }
    for not in [
        "rm -rf x",
        "sh -c \"ls\"",
        "ls > fichier",
        "curl https://example.com",
        "cd /x && ls",
        "cd /x",
        "ls; rm x",
        "cat $(ls)",
        "cat `ls`",
        "echo $HOME",
        "find . -delete",
        "find . -exec rm {} +",
        "find . '-exec' rm x",
        "sort -o out.txt in.txt",
        "git push",
        "git -c core.pager=x log",
        "git diff --output=x",
        "git branch -D main",
        "./ls",
        // #141 : une affectation qui détourne l'interpréteur n'est jamais une lecture.
        "PATH=/tmp ls",
        "LD_PRELOAD=/tmp/x.so cat f",
        "DYLD_INSERT_LIBRARIES=x.dylib ls",
        "IFS=, ls",
        "cat f | sh",
        "cat f | xargs rm",
        "cat f | tee /tmp/x",
        "cat f | sed -i s/a/b/ g",
        "ls | sort -o out.txt",
        "glab api h \"p\" | jq -r '.x'",
        "l\\s",
        "export A=1",
        "",
    ] {
        assert!(!is_read_command(not), "{not}");
    }
}

/// #130 : les formes de commande sans test jusqu'ici (multi-lignes, heredoc, guillemet
/// non fermé, un seul mot, vide) traversent classement, préfixe `cd` et vérification
/// sans paniquer ; une commande multi-lignes n'est jamais une lecture.
#[test]
fn unusual_command_shapes_never_panic() {
    let shapes = [
        "",
        " ",
        "ls",
        "cd",
        "cd ",
        "cd /x &&",
        "echo \"non fermé",
        "grep 'x",
        "python3 - <<'PYEOF'\nprint(\"a\"[:10])\nPYEOF",
        "cd /x && python3 - <<'PYEOF'\nimport json\nPYEOF",
        "cat <<EOF\n{{.Name}}\nEOF",
        "ls\nrm -rf /",
        "\n\n",
        "é",
    ];
    for c in shapes {
        let _ = split_cd_prefix(c);
        let _ = may_destroy(c);
        let _ = check_command(c);
        if c.contains('\n') {
            assert!(!is_read_command(c), "{c:?}");
        }
    }
    assert!(check_command("").is_err());
}

/// #123 : un `cd <chemin> &&` seul en tête se sépare de la commande qui suit ; tout
/// ce qui ferait du chemin autre chose qu'un chemin laisse la ligne entière.
#[test]
fn a_single_cd_prefix_is_split_from_the_command() {
    let split = |c: &str| split_cd_prefix(c);
    assert_eq!(
        split("cd /Users/essai/depot && grep -rn \"BaseURL\" src"),
        Some((
            "/Users/essai/depot".into(),
            "grep -rn \"BaseURL\" src".into()
        ))
    );
    assert_eq!(
        split("  cd 'mon depot'&&ls"),
        Some(("mon depot".into(), "ls".into()))
    );
    assert_eq!(
        split("cd src && echo x; grep foo"),
        Some(("src".into(), "echo x; grep foo".into())),
        "la suite reste composée, à elle de se classer"
    );
    for kept in [
        "cd $HOME && ls",
        "cd `pwd` && ls",
        "cd $(pwd) && ls",
        "cd ~/depot && ls",
        "cd /x* && ls",
        "cd - && ls",
        "cd /x; ls",
        "cd /x || ls",
        "cd /x & ls",
        "cd /x &&& ls",
        "cd /x && ",
        "cd /x",
        "cd /x /y && ls",
        "cd /x&&ls",
        "cdx /x && ls",
        "ls && cd /x",
        "cd \"/x && ls",
    ] {
        assert_eq!(split(kept), None, "{kept}");
    }
}

/// #111 : ce qui peut détruire, ou qu'on ne peut pas juger, n'est jamais automatique.
#[test]
fn destructive_or_opaque_commands_are_spotted() {
    for bad in [
        "rm -rf x",
        "cd /x && ls",
        "echo $(rm x)",
        "git push --force origin main",
        "git reset --hard HEAD~3",
        "git clean -fdx",
        "find . -delete",
        "sudo ls",
        "/bin/rm x",
        "cp -r a b",
        "ls | xargs rm",
    ] {
        assert!(may_destroy(bad), "{bad}");
    }
    for fine in [
        "cargo test",
        "npm run build",
        "git commit -m x",
        "mkdir build",
        "cp a b",
    ] {
        assert!(!may_destroy(fine), "{fine}");
    }
}

/// #106 : un échec de résolution ou de connexion, ou une commande qui ne vit que du
/// réseau, se reconnaît ; un échec ordinaire non.
#[test]
fn a_network_failure_is_told_apart_from_a_command_failure() {
    assert!(looks_like_network_failure(
        "curl -sS https://example.com",
        6,
        "",
        "curl: (6) Could not resolve host: example.com"
    ));
    assert!(looks_like_network_failure(
        "git ls-remote origin",
        128,
        "",
        "fatal : impossible d'accéder à 'https://github.com/x/y/' : Could not resolve host: github.com"
    ));
    assert!(looks_like_network_failure(
        "nc -z 127.0.0.1 8080",
        1,
        "",
        ""
    ));
    assert!(looks_like_network_failure(
        "git push origin main",
        1,
        "",
        ""
    ));
    assert!(!looks_like_network_failure(
        "cargo test",
        101,
        "test result: FAILED",
        ""
    ));
    assert!(!looks_like_network_failure(
        "ls /nope",
        1,
        "",
        "No such file or directory"
    ));
    assert!(!looks_like_network_failure(
        "curl https://example.com",
        0,
        "ok",
        ""
    ));
}

/// #65 : au dépassement du délai, la commande et son groupe sont terminés : rien ne
/// survit pour être relancé par le modèle.
#[tokio::test]
#[cfg(unix)]
async fn a_timed_out_command_leaves_no_process_behind() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
    let marker = "penelope-essai-delai-65";
    let e = exec(
        &host,
        &format!("sleep 37 # {marker}"),
        ExecOptions {
            timeout: std::time::Duration::from_millis(200),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::Timeout(_)), "{e}");

    // Le groupe est parti : plus aucun processus ne porte la marque.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let out = std::process::Command::new("/bin/ps")
        .args(["-Ao", "command"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let alive: Vec<&str> = out
        .lines()
        .filter(|l| l.contains(marker) && !l.contains("/bin/ps"))
        .collect();
    assert!(alive.is_empty(), "processus survivants : {alive:?}");
}

/// #65 : une sortie énorme est lue sous plafond, tête et queue gardées, sans tenir en
/// mémoire.
#[tokio::test]
#[cfg(unix)]
async fn a_huge_output_is_capped_while_reading() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
    // ~8 Mio de sortie pour un plafond de 8 Kio.
    let out = exec(
        &host,
        "echo DEBUT; for i in $(seq 1 2000); do head -c 16384 /dev/zero | tr '\\0' 'a';              done; echo FIN",
        ExecOptions {
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 8192,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(out.truncated, "la sortie doit être coupée");
    assert!(
        out.stdout.len() < 12_000,
        "rendu : {} octets",
        out.stdout.len()
    );
    assert!(out.stdout.starts_with("DEBUT"), "tête gardée");
    assert!(out.stdout.trim_end().ends_with("FIN"), "queue gardée");
    assert!(out.stdout.contains("octets élidés"), "{}", out.stdout);
}

/// #57 : `/stop` pendant une commande longue termine le processus et son groupe,
/// au lieu d'attendre le délai.
#[tokio::test]
async fn a_cancelled_command_is_terminated_quickly() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path().join("pids"));
    let cancel = penelope_llm::CancelToken::new();
    let stopper = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        stopper.cancel();
    });
    let started = std::time::Instant::now();
    let e = exec(
        &host,
        "sleep 60",
        ExecOptions {
            timeout: std::time::Duration::from_secs(60),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            cancel: Some(&cancel),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::Cancelled), "{e}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "arrêt trop lent : {:?}",
        started.elapsed()
    );
}

#[test]
fn forbidden_commands_are_refused() {
    for c in ["rm -rf /", "sudo rm x", "  rm   -rf   /  "] {
        let e = check_command(c).unwrap_err();
        assert!(
            e.to_string().contains("interdit"),
            "commande acceptée à tort : {c} → {e}"
        );
    }
    assert!(check_command("").is_err());
}

#[test]
fn ordinary_commands_pass() {
    for c in [
        "cargo test",
        "git status",
        "rm -rf target/debug",
        "npm run build",
    ] {
        check_command(c).unwrap_or_else(|e| panic!("refusée à tort : {c} → {e}"));
    }
}

#[test]
fn sudo_in_a_pipeline_is_caught() {
    assert!(check_command("echo x | sudo tee /etc/hosts").is_err());
}

#[test]
fn truncation_keeps_head_and_tail() {
    let s = format!("{}ERREUR FINALE", "a".repeat(10_000));
    let (out, truncated) = truncate(&s, 1000);
    assert!(truncated);
    assert!(out.starts_with("aaaa"));
    assert!(
        out.ends_with("ERREUR FINALE"),
        "la fin doit survivre : {}",
        &out[out.len() - 40..]
    );
    assert!(out.contains("octets élidés"));
}

#[test]
fn short_output_is_untouched() {
    let (out, truncated) = truncate("court", 1000);
    assert_eq!(out, "court");
    assert!(!truncated);
}

#[test]
fn truncation_never_splits_utf8() {
    let s = "é".repeat(5000);
    let (out, _) = truncate(&s, 1000);
    assert!(!out.contains('\u{FFFD}'));
}

#[test]
fn profiles_follow_the_configuration() {
    let ws = Path::new("/tmp/ws");
    assert_eq!(
        profile_for("readonly", ws, false).kind,
        penelope_platform::ProfileKind::ReadOnly
    );
    assert_eq!(
        profile_for("workspace-write", ws, false).kind,
        penelope_platform::ProfileKind::WorkspaceWrite
    );
    assert!(profile_for("workspace-write", ws, true).allow_network);
    assert!(!profile_for("full", ws, false).enforced());
}

#[tokio::test]
#[cfg(unix)]
async fn timeout_is_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path());
    let e = exec(
        &host,
        "sleep 30",
        ExecOptions {
            timeout: std::time::Duration::from_millis(200),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(e, ToolError::Timeout(_)), "{e}");
}

#[tokio::test]
#[cfg(unix)]
async fn exit_code_and_streams_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let host = penelope_platform::UnixProcessHost::new(dir.path());
    let out = exec(
        &host,
        "echo bonjour; echo souci >&2; exit 3",
        ExecOptions {
            timeout: std::time::Duration::from_secs(10),
            max_output_bytes: 4096,
            shell: Some(("/bin/sh".into(), vec!["-c".into()])),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(out.exit_code, 3);
    assert!(out.stdout.contains("bonjour"));
    assert!(out.stderr.contains("souci"));
    assert_eq!(out.to_json()["success"], false);
}
