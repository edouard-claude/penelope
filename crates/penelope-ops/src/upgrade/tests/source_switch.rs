use super::*;

/// Hôte simulé : signataire et service en mémoire.
#[derive(Default)]
struct FakeHost {
    sign_fails: bool,
    file: PathBuf,
    signed: std::sync::Mutex<Vec<PathBuf>>,
    relays: std::sync::Mutex<Vec<HandOff>>,
}

impl SwitchHost for FakeHost {
    fn sign(&self, path: &Path, _identity: &str, _identifier: &str) -> Result<(), String> {
        if self.sign_fails {
            return Err("errSecInternalComponent".into());
        }
        self.signed.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
    fn service_file(&self) -> Option<PathBuf> {
        self.file.is_file().then(|| self.file.clone())
    }
    fn service_program(&self, content: &str) -> Option<String> {
        penelope_platform::service::launchd_program(content)
    }
    fn service_with_program(&self, content: &str, exe: &Path) -> Option<String> {
        penelope_platform::service::launchd_with_program(content, exe)
    }
    fn domain(&self) -> Result<String, String> {
        Ok("gui/501".into())
    }
    fn hand_off(&self, h: &HandOff) -> Result<(), String> {
        self.relays.lock().unwrap().push(h.clone());
        Ok(())
    }
}

/// Installation source simulée : binaire de compilation, service qui le lance, release
/// 9.9.9 publiée.
#[cfg(unix)]
async fn source_install(dir: &Path) -> (PathBuf, PathBuf, String, String) {
    let current = dir.join("code/penelope/target/release/penelope");
    std::fs::create_dir_all(current.parent().unwrap()).unwrap();
    fake_binary(&current, crate::VERSION);
    let plist = dir.join("LaunchAgents/com.penelope.daemon.plist");
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    let original = penelope_platform::service::launchd_plist(
        &current,
        None,
        &dir.join("logs"),
        "/usr/bin:/bin",
    );
    std::fs::write(&plist, &original).unwrap();

    let pack = dir.join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    fake_binary(&pack.join("penelope"), "9.9.9");
    let archive = dir.join("a.tar.gz");
    assert!(
        std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&pack)
            .arg("penelope")
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(&archive).unwrap();
    let name = "penelope-v9.9.9-macos-universal.tar.gz";
    let sums = format!(
        "{}  ./{name}\n",
        penelope_kernel::canonical::sha256_hex(&bytes)
    );
    let server = fake_releases(|base| {
        vec![
            (
                "/r".to_string(),
                json!([{
                    "tag_name": "v9.9.9",
                    "assets": [
                        {"name": name, "browser_download_url": format!("{base}/dl/{name}")},
                        {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/sums")},
                    ]
                }])
                .to_string()
                .into_bytes(),
            ),
            (format!("/dl/{name}"), bytes.clone()),
            ("/dl/sums".to_string(), sums.into_bytes()),
        ]
    })
    .await;
    (current, plist, original, format!("{server}/r"))
}

/// Issue #33 : « Basculer » installe le binaire dans le répertoire cible, le re-signe,
/// réécrit le service et le recharge ; la santé confirmée supprime `upgrade.json`, et
/// la mise à jour suivante suit le parcours normal.
#[cfg(unix)]
#[tokio::test]
async fn a_source_install_switches_to_releases() {
    let dir = tempfile::tempdir().unwrap();
    let (current, plist, original, releases_url) = source_install(dir.path()).await;
    let host = FakeHost {
        file: plist.clone(),
        ..Default::default()
    };
    let source = Source {
        releases_url,
        os: "macos".into(),
        pubkey: None,
    };
    let install_dir = dir.path().join("home/.local/bin");
    let state = dir.path().join("state");
    assert!(
        installable_binary(&current)
            .unwrap_err()
            .contains("--switch")
    );

    let v = switch_to_releases(Switch {
        source: &source,
        tag: None,
        current: &current,
        install_dir: &install_dir,
        state_dir: &state,
        now: "t".into(),
        codesign: Some(("Penelope Test", "io.github.edouard-claude.penelope")),
        host: &host,
    })
    .await
    .unwrap();
    assert_eq!(v["switched"], true);
    let target = install_dir.join("penelope");
    assert!(std::fs::read_to_string(&target).unwrap().contains("9.9.9"));
    let signed = host.signed.lock().unwrap().clone();
    assert_eq!(signed.len(), 2, "essai de signature puis binaire installé");
    assert_eq!(signed[1], target);
    let rewritten = std::fs::read_to_string(&plist).unwrap();
    assert_eq!(
        penelope_platform::service::launchd_program(&rewritten).as_deref(),
        Some(target.to_string_lossy().as_ref())
    );
    let backup = PathBuf::from(format!("{}.sources", plist.display()));
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    // Issue #36 : le rechargement part dans un job launchd à part, avec son garde-fou.
    let relays = host.relays.lock().unwrap().clone();
    assert_eq!(relays.len(), 1);
    let relay = &relays[0];
    assert!(relay.reload);
    assert_eq!(relay.service_plist, plist);
    assert_eq!(relay.backup_plist.as_deref(), Some(backup.as_path()));
    assert_eq!(relay.binary, target);
    assert_eq!(relay.previous.as_deref(), Some(current.as_path()));
    assert_eq!(relay.state_file, state_file(&state));
    assert_eq!(relay.helper_label(), "com.penelope.daemon.reloader");
    assert_eq!(relay.to_version, "9.9.9");
    let p = pending(&state).unwrap();
    assert_eq!(p.previous, current);
    assert_eq!(p.binary, target);
    assert!(render(&v).contains("Bascule vers les releases"));

    assert_eq!(on_boot(&state, "9.9.9", 1_000), Boot::Trial { attempt: 1 });
    assert_eq!(
        confirm(&state, "9.9.9"),
        Some(Confirmation::Upgraded {
            from: crate::VERSION.into(),
            to: "9.9.9".into()
        })
    );
    assert!(
        pending(&state).is_none(),
        "santé confirmée : upgrade.json supprimé"
    );
    // Après la bascule, le binaire lancé se met à jour par le parcours normal.
    assert!(!is_source_build(&target));
    assert_eq!(installable_binary(&target).unwrap(), target);
}

/// Issues #33 et #36 : santé non confirmée, le binaire de compilation revient au chemin
/// stable ; le fichier de service n'est plus touché, `KeepAlive` relance l'ancien code.
#[cfg(unix)]
#[tokio::test]
async fn an_unconfirmed_switch_brings_the_source_build_back_to_the_stable_path() {
    let dir = tempfile::tempdir().unwrap();
    let (current, plist, original, releases_url) = source_install(dir.path()).await;
    let host = FakeHost {
        file: plist.clone(),
        ..Default::default()
    };
    let source = Source {
        releases_url,
        os: "macos".into(),
        pubkey: None,
    };
    let install_dir = dir.path().join("bin");
    let state = dir.path().join("state");
    switch_to_releases(Switch {
        source: &source,
        tag: None,
        current: &current,
        install_dir: &install_dir,
        state_dir: &state,
        now: "t".into(),
        codesign: Some(("Penelope Test", "io.github.edouard-claude.penelope")),
        host: &host,
    })
    .await
    .unwrap();

    assert_eq!(on_boot(&state, "9.9.9", 1_000), Boot::Trial { attempt: 1 });
    assert_eq!(
        on_boot(&state, "9.9.9", 1_000 + HEALTH_WINDOW_MS + 1),
        Boot::RolledBack {
            from: "9.9.9".into(),
            to: crate::VERSION.into(),
        }
    );
    let target = install_dir.join("penelope");
    assert_eq!(
        penelope_platform::service::launchd_program(&std::fs::read_to_string(&plist).unwrap())
            .as_deref(),
        Some(target.to_string_lossy().as_ref()),
        "le service garde le chemin stable"
    );
    assert_ne!(std::fs::read_to_string(&plist).unwrap(), original);
    assert!(
        std::fs::read_to_string(&target)
            .unwrap()
            .contains(crate::VERSION),
        "l'ancien code tourne au chemin stable"
    );
    assert_eq!(on_boot(&state, crate::VERSION, 80_000), Boot::Normal);
    assert!(matches!(
        confirm(&state, crate::VERSION),
        Some(Confirmation::RolledBack { .. })
    ));
}

/// Issue #36 : le relais lit `"first_boot_ms": null` dans `upgrade.json` pour savoir si
/// le nouveau binaire a démarré ; un `upgrade.json` d'une version antérieure (champ
/// `service`) reste lisible.
#[test]
fn the_state_file_speaks_the_relay_contract() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    write_pending(&state, &pending_for(&dir.path().join("penelope"))).unwrap();
    let raw = std::fs::read_to_string(state_file(&state)).unwrap();
    assert!(raw.contains("\"first_boot_ms\": null"), "{raw}");
    on_boot(&state, "1.1.0", 5);
    let raw = std::fs::read_to_string(state_file(&state)).unwrap();
    assert!(!raw.contains("\"first_boot_ms\": null"), "{raw}");

    let legacy = r#"{"from_version":"0.12.0","to_version":"0.13.0","binary":"/b","previous":"/p",
            "attempts":0,"installed_at":"t","first_boot_ms":null,
            "service":{"file":"/f.plist","backup":"/f.plist.sources"}}"#;
    std::fs::write(state_file(&state), legacy).unwrap();
    assert_eq!(pending(&state).unwrap().to_version, "0.13.0");
}

/// Issue #36 : une mise à jour ordinaire arme un garde-fou hors du daemon quand le
/// service lance ce binaire ; rien sinon.
#[cfg(unix)]
#[test]
fn an_ordinary_upgrade_arms_a_guard_outside_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin/penelope");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fake_binary(&bin, "1.1.0");
    let state = dir.path().join("state");
    let plist = dir.path().join("com.penelope.daemon.plist");
    let host = FakeHost {
        file: plist.clone(),
        ..Default::default()
    };
    // Aucune mise à jour en attente : rien.
    assert!(!guard_install(&host, &state).unwrap());
    write_pending(&state, &pending_for(&bin)).unwrap();
    // Service absent : rien.
    assert!(!guard_install(&host, &state).unwrap());
    // Le service lance un autre binaire : rien.
    let other = penelope_platform::service::launchd_plist(
        &dir.path().join("ailleurs/penelope"),
        None,
        &dir.path().join("logs"),
        "/usr/bin",
    );
    std::fs::write(&plist, other).unwrap();
    assert!(!guard_install(&host, &state).unwrap());
    assert!(host.relays.lock().unwrap().is_empty());

    let ours =
        penelope_platform::service::launchd_plist(&bin, None, &dir.path().join("logs"), "/usr/bin");
    std::fs::write(&plist, &ours).unwrap();
    assert!(guard_install(&host, &state).unwrap());
    let relay = host.relays.lock().unwrap()[0].clone();
    assert!(
        !relay.reload,
        "surveillance seule : le daemon redémarre de lui-même"
    );
    assert!(relay.backup_plist.is_none());
    assert_eq!(relay.binary, bin);
    assert_eq!(relay.previous, Some(previous_path(&bin)));
    assert_eq!(relay.from_version, "1.0.0");
    assert_eq!(
        std::fs::read_to_string(&plist).unwrap(),
        ours,
        "service intact"
    );
}

/// Issue #33 : identité absente ou inutilisable, rien n'est modifié et le message dit
/// quoi configurer.
#[cfg(unix)]
#[tokio::test]
async fn a_switch_without_a_usable_identity_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (current, plist, original, releases_url) = source_install(dir.path()).await;
    let source = Source {
        releases_url,
        os: "macos".into(),
        pubkey: None,
    };
    let install_dir = dir.path().join("bin");
    let state = dir.path().join("state");
    for (host, codesign, expected) in [
        (
            FakeHost {
                file: plist.clone(),
                ..Default::default()
            },
            None,
            "upgrade.codesign_identity",
        ),
        (
            FakeHost {
                file: plist.clone(),
                sign_fails: true,
                ..Default::default()
            },
            Some(("Penelope Test", "io.github.edouard-claude.penelope")),
            "n'est pas utilisable depuis le daemon",
        ),
    ] {
        let err = switch_to_releases(Switch {
            source: &source,
            tag: None,
            current: &current,
            install_dir: &install_dir,
            state_dir: &state,
            now: "t".into(),
            codesign,
            host: &host,
        })
        .await
        .unwrap_err();
        assert!(err.contains(expected), "{err}");
        assert!(!install_dir.join("penelope").exists());
        assert_eq!(std::fs::read_to_string(&plist).unwrap(), original);
        assert!(pending(&state).is_none());
        assert!(host.relays.lock().unwrap().is_empty());
    }
}
