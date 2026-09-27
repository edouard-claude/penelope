use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod source_switch;

#[test]
fn sums_and_versions_are_parsed_strictly() {
    let sums = format!(
        "{}  ./penelope-v1.0.0-macos-universal.tar.gz\n{}  ./penelope-v1.0.0-macos-x86_64.tar.gz\n",
        "a".repeat(64),
        "b".repeat(64)
    );
    assert_eq!(
        expected_sum(&sums, "penelope-v1.0.0-macos-universal.tar.gz"),
        Some("a".repeat(64))
    );
    assert_eq!(expected_sum(&sums, "penelope-v1.0.0-macos.tar.gz"), None);
    assert_eq!(expected_sum("court  ./x", "x"), None, "somme malformée");

    assert_eq!(parse_version("v0.3.10"), Some((0, 3, 10)));
    assert_eq!(
        parse_version("1.2.3-rc1"),
        None,
        "#212 : un suffixe n'est pas installable d'office"
    );
    assert!(is_newer("0.3.10", "0.3.9"));
    assert!(!is_newer("0.3.1", "0.3.1"));
    assert!(announces("penelope 0.3.1", "0.3.1"));
    assert!(!announces("penelope 0.3.10", "0.3.1"), "pas de préfixe");
    assert!(
        announces("penelope 1.0.0-rc.1", "1.0.0-rc.1"),
        "le suffixe compte"
    );
    assert!(asset_name("v1.0.0", "linux").is_err());
}

/// #212 : une pré-release n'est jamais « plus récente ». `parse_version` coupait le
/// suffixe, donc une `v1.0.0-alpha.1` publiée par erreur valait `1.0.0 > 0.17.59` et
/// toutes les instances 0.17 l'auraient installée à leur prochaine vérification.
#[test]
fn a_prerelease_is_never_newer_than_the_running_version() {
    assert_eq!(parse_version("1.0.0-alpha.1"), None);
    assert_eq!(parse_version("v1.0.0-alpha.1"), None);
    assert_eq!(parse_version("0.17.60-rc1"), None);
    assert_eq!(parse_version("1.2.3+build"), None);
    assert_eq!(parse_version("1.0.0"), Some((1, 0, 0)));
    assert_eq!(parse_version("v0.17.59"), Some((0, 17, 59)));

    assert!(!is_newer("v1.0.0-alpha.1", "0.17.59"));
    assert!(!is_newer("0.17.60-rc1", "0.17.59"));
    assert!(is_newer("0.17.60", "0.17.59"));
    assert!(is_newer("1.0.0", "0.17.59"));
    assert!(!is_newer("0.17.59", "0.17.59"));
    assert!(!is_newer("0.17.58", "0.17.59"));
    assert!(
        !is_newer("n'importe quoi", "0.17.59"),
        "illisible : pas plus récente"
    );

    // Le binaire courant est une pré-release (branche v1) : sa version pleine le
    // dépasse, une 0.17 non, une autre pré-release jamais sans son tag.
    assert!(is_newer("1.0.0", "1.0.0-rc.1"));
    assert!(is_newer("1.0.1", "1.0.0-alpha.7"));
    assert!(!is_newer("0.17.61", "1.0.0-alpha.7"));
    assert!(!is_newer("1.0.0-rc.2", "1.0.0-rc.1"));
}

/// #212, côté `release` : sans tag, la plus haute version publiée ignore les
/// pré-releases, même numérotées au-dessus ; par son tag, une pré-release reste
/// installable (c'est ainsi que la `1.0.0-rc.1` s'installera à la bascule).
#[tokio::test]
async fn the_latest_release_skips_prereleases_unless_asked_by_tag() {
    let server = fake_releases(|base| {
        let assets = |tag: &str| {
            json!([
                {"name": format!("penelope-{tag}-macos-universal.tar.gz"),
                 "browser_download_url": format!("{base}/dl/{tag}")},
                {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/{tag}.sums")},
            ])
        };
        let list = json!([
            {"tag_name": "v1.0.0-alpha.1", "prerelease": true, "assets": assets("v1.0.0-alpha.1")},
            {"tag_name": "v0.17.60", "prerelease": true, "assets": assets("v0.17.60")},
            {"tag_name": "v0.17.59", "prerelease": true, "assets": assets("v0.17.59")},
        ]);
        vec![
            ("/r".to_string(), list.to_string().into_bytes()),
            (
                "/r/tags/v1.0.0-alpha.1".to_string(),
                list[0].to_string().into_bytes(),
            ),
        ]
    })
    .await;
    let source = Source {
        releases_url: format!("{server}/r"),
        os: "macos".into(),
        pubkey: None,
    };
    let client = client().unwrap();
    let latest = release(&client, &source, None).await.unwrap();
    assert_eq!(
        (latest.tag.as_str(), latest.version.as_str()),
        ("v0.17.60", "0.17.60")
    );
    let asked = release(&client, &source, Some("v1.0.0-alpha.1"))
        .await
        .unwrap();
    assert_eq!(asked.version, "1.0.0-alpha.1");
}

/// `check` retient la version sans l'archive : sous Linux, où aucun artefact n'est
/// publié, il disait « pas d'artefact publié pour linux » au lieu de la version.
#[tokio::test]
async fn check_names_the_latest_version_without_an_archive_for_the_os() {
    let server = fake_releases(|_| {
        let list = json!([
            {"tag_name": "v9.9.9", "prerelease": true, "assets": []},
            {"tag_name": "v0.1.0", "prerelease": true, "assets": []},
        ]);
        vec![("/r".to_string(), list.to_string().into_bytes())]
    })
    .await;
    let source = Source {
        releases_url: format!("{server}/r"),
        os: "linux".into(),
        pubkey: None,
    };
    let v = check(&source).await.unwrap();
    assert_eq!(v["latest"], "9.9.9");
    assert_eq!(v["up_to_date"], false);
    let err = release(&client().unwrap(), &source, None)
        .await
        .unwrap_err();
    assert!(err.contains("pas d'artefact publié pour linux"), "{err}");
}

#[cfg(unix)]
fn fake_binary(path: &Path, version: &str) {
    std::fs::write(path, format!("#!/bin/sh\necho \"penelope {version}\"\n")).unwrap();
    set_executable(path).unwrap();
}

fn pending_for(bin: &Path) -> Pending {
    Pending {
        from_version: "1.0.0".into(),
        to_version: "1.1.0".into(),
        binary: bin.to_path_buf(),
        previous: previous_path(bin),
        attempts: 0,
        installed_at: "t".into(),
        first_boot_ms: None,
    }
}

/// CA 2 : un upgrade volontairement cassé est annulé automatiquement.
/// #153 : quand la sortie d'erreur du binaire à l'essai dit pourquoi, la carte le
/// répète. Deux versions sont revenues en arrière sans que rien ne dise « stack
/// overflow » ailleurs que dans un fichier que personne ne lit.
#[test]
fn the_rollback_card_repeats_the_last_error_of_the_failed_binary() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    std::fs::write(
        state.join("daemon.err.log"),
        "démarrage\nthread 'penelope-store-writer' has overflowed its stack\n\
             fatal runtime error: stack overflow, aborting\n",
    )
    .unwrap();
    let why = last_error_line(&state).expect("une ligne d'erreur");
    assert!(why.contains("stack overflow"), "{why}");

    let card = Confirmation::RolledBack {
        from: "0.17.31".into(),
        to: "0.17.27".into(),
        why: Some(why),
    }
    .text();
    assert!(
        card.contains("0.17.31") && card.contains("0.17.27"),
        "{card}"
    );
    assert!(
        card.contains("stack overflow"),
        "la carte dit pourquoi : {card}"
    );

    // Sans fichier lisible, la phrase d'avant, sans rien inventer.
    let vide = tempfile::tempdir().unwrap();
    assert!(last_error_line(vide.path()).is_none());
}

#[cfg(unix)]
#[test]
fn ca_2_8_a_broken_upgrade_is_rolled_back_automatically() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("penelope");
    let fresh = dir.path().join("fresh");
    let state = dir.path().join("state");
    fake_binary(&bin, "1.0.0");
    fake_binary(&fresh, "1.1.0");

    swap_in(&fresh, &bin).unwrap();
    assert!(std::fs::read_to_string(&bin).unwrap().contains("1.1.0"));
    write_pending(&state, &pending_for(&bin)).unwrap();

    // Plantages rapprochés dans la fenêtre de santé : le binaire garde sa chance.
    assert_eq!(on_boot(&state, "1.1.0", 1_000), Boot::Trial { attempt: 1 });
    assert_eq!(on_boot(&state, "1.1.0", 11_000), Boot::Trial { attempt: 2 });
    // Toujours pas confirmé passé 60 s : retour arrière.
    assert_eq!(
        on_boot(&state, "1.1.0", 1_000 + HEALTH_WINDOW_MS + 1),
        Boot::RolledBack {
            from: "1.1.0".into(),
            to: "1.0.0".into(),
        }
    );
    assert!(std::fs::read_to_string(&bin).unwrap().contains("1.0.0"));

    // L'ancien binaire redémarre et l'annonce, une seule fois.
    assert_eq!(on_boot(&state, "1.0.0", 80_000), Boot::Normal);
    assert_eq!(
        confirm(&state, "1.0.0"),
        Some(Confirmation::RolledBack {
            from: "1.1.0".into(),
            to: "1.0.0".into(),
            // Aucune sortie d'erreur lisible dans ce bac à sable de test : la carte
            // retombe sur sa phrase d'avant (#153).
            why: None,
        })
    );
    assert!(confirm(&state, "1.0.0").is_none());
}

#[cfg(unix)]
#[test]
fn repeated_fast_crashes_also_roll_back_and_confirmation_is_announced_once() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("penelope");
    let state = dir.path().join("state");
    fake_binary(&bin, "1.0.0");
    std::fs::copy(&bin, previous_path(&bin)).unwrap();
    write_pending(&state, &pending_for(&bin)).unwrap();
    for i in 1..=MAX_BOOT_ATTEMPTS {
        assert_eq!(
            on_boot(&state, "1.1.0", i as i64),
            Boot::Trial { attempt: i }
        );
    }
    assert!(matches!(
        on_boot(&state, "1.1.0", 10),
        Boot::RolledBack { .. }
    ));

    write_pending(&state, &pending_for(&bin)).unwrap();
    let _ = std::fs::remove_file(rolled_back_note(&state));
    assert_eq!(on_boot(&state, "1.1.0", 0), Boot::Trial { attempt: 1 });
    assert_eq!(
        confirm(&state, "1.1.0").map(|c| c.text()),
        Some("⬆️ Pénélope est passée de 1.0.0 à 1.1.0.".to_string())
    );
    assert!(confirm(&state, "1.1.0").is_none());
    assert_eq!(
        on_boot(&state, "1.1.0", 5),
        Boot::Normal,
        "plus rien en attente"
    );
}

#[cfg(unix)]
#[test]
fn manual_rollback_toggles_between_the_two_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("penelope");
    let state = dir.path().join("state");
    fake_binary(&bin, "1.1.0");
    assert!(manual_rollback(&bin, &state).is_err(), "rien à restaurer");
    fake_binary(&previous_path(&bin), "1.0.0");

    let v = manual_rollback(&bin, &state).unwrap();
    assert_eq!(v["to"], "penelope 1.0.0");
    assert!(std::fs::read_to_string(&bin).unwrap().contains("1.0.0"));
    assert!(
        std::fs::read_to_string(previous_path(&bin))
            .unwrap()
            .contains("1.1.0")
    );
}

/// Faux GitHub : chaque chemin sert un contenu fixe, construit une fois l'adresse connue.
async fn fake_releases(make: impl FnOnce(&str) -> Vec<(String, Vec<u8>)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(make(&base));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let files = files.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let target = head.split_whitespace().nth(1).unwrap_or("");
                let path = target.split('?').next().unwrap_or("").to_string();
                let (status, body) = match files.iter().find(|(p, _)| *p == path) {
                    Some((_, b)) => ("200 OK", b.clone()),
                    None => ("404 Not Found", b"{}".to_vec()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    base
}

#[cfg(unix)]
#[tokio::test]
async fn install_verifies_the_archive_then_swaps_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("pack");
    std::fs::create_dir_all(&pack).unwrap();
    fake_binary(&pack.join("penelope"), "9.9.9");
    let archive = dir.path().join("a.tar.gz");
    let status = std::process::Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&pack)
        .arg("penelope")
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&archive).unwrap();
    let name = "penelope-v9.9.9-macos-universal.tar.gz";
    let good_sums = format!(
        "{}  ./{name}\n",
        penelope_kernel::canonical::sha256_hex(&bytes)
    );
    let bad_sums = format!("{}  ./{name}\n", "0".repeat(64));

    let server = fake_releases(|base| {
        let meta = |sums: &str| {
            json!([
                {"tag_name": "v9.9.10", "draft": true, "assets": []},
                {"tag_name": "v1.0.0", "assets": []},
                {
                    "tag_name": "v9.9.9",
                    "prerelease": true,
                    "assets": [
                        {"name": name, "browser_download_url": format!("{base}/dl/{name}")},
                        {"name": "SHA256SUMS", "browser_download_url": format!("{base}/dl/{sums}")},
                    ]
                },
            ])
            .to_string()
            .into_bytes()
        };
        vec![
            ("/good".to_string(), meta("good-sums")),
            ("/bad".to_string(), meta("bad-sums")),
            (format!("/dl/{name}"), bytes.clone()),
            ("/dl/good-sums".to_string(), good_sums.into_bytes()),
            ("/dl/bad-sums".to_string(), bad_sums.into_bytes()),
        ]
    })
    .await;

    let bin = dir.path().join("bin").join("penelope");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fake_binary(&bin, crate::VERSION);
    let state = dir.path().join("state");
    let source = |prefix: &str| Source {
        releases_url: format!("{server}/{prefix}"),
        os: "macos".into(),
        pubkey: None,
    };

    let bad = source("bad");
    let err = install(Install {
        source: &bad,
        tag: None,
        force: false,
        binary: &bin,
        state_dir: &state,
        now: "t".into(),
        codesign: None,
    })
    .await
    .unwrap_err();
    assert!(err.contains("SHA-256"), "{err}");
    assert!(
        std::fs::read_to_string(&bin)
            .unwrap()
            .contains(crate::VERSION),
        "rien n'est touché sans somme valide"
    );
    assert!(pending(&state).is_none());

    let good = source("good");
    let v = install(Install {
        source: &good,
        tag: None,
        force: false,
        binary: &bin,
        state_dir: &state,
        now: "t".into(),
        codesign: None,
    })
    .await
    .unwrap();
    assert_eq!(v["installed"], "9.9.9");
    assert_eq!(v["signature"], "non vérifiée (aucune clé publique)");
    assert!(std::fs::read_to_string(&bin).unwrap().contains("9.9.9"));
    assert!(
        std::fs::read_to_string(previous_path(&bin))
            .unwrap()
            .contains(crate::VERSION)
    );
    let p = pending(&state).unwrap();
    assert_eq!(
        (p.from_version.as_str(), p.to_version.as_str()),
        (crate::VERSION, "9.9.9")
    );
    assert_eq!(on_boot(&state, "9.9.9", 0), Boot::Trial { attempt: 1 });

    // Clé publique configurée : une release sans signature est refusée, rien n'est
    // téléchargé au-delà des sommes.
    let _ = std::fs::remove_file(state.join("upgrade.json"));
    let mut signed_only = source("good");
    signed_only.pubkey = Some(TEST_PUBKEY.into());
    let err = install(Install {
        source: &signed_only,
        tag: None,
        force: true,
        binary: &bin,
        state_dir: &state,
        now: "t".into(),
        codesign: None,
    })
    .await
    .unwrap_err();
    assert!(err.contains("pas signée"), "{err}");
    assert!(pending(&state).is_none());
}

/// Vecteur de la crate `minisign-verify` : « test » signé par sa clé de test.
const TEST_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
const TEST_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

#[test]
fn release_sums_are_checked_against_the_minisign_key() {
    verify_signature(b"test", TEST_SIGNATURE, TEST_PUBKEY).unwrap();
    let file_form =
        format!("untrusted comment: minisign public key E7620F1842B4E81F\n{TEST_PUBKEY}");
    verify_signature(b"test", TEST_SIGNATURE, &file_form).unwrap();
    let tampered = verify_signature(b"Test", TEST_SIGNATURE, TEST_PUBKEY).unwrap_err();
    assert!(tampered.contains("invalide"), "{tampered}");
    assert!(verify_signature(b"test", "n'importe quoi", TEST_PUBKEY).is_err());
    assert_eq!(release_pubkey("  RWQx  ").as_deref(), Some("RWQx"));
}
