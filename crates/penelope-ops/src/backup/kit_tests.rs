//! Le kit de secours et le contrôle de chaque sauvegarde (#328).

use super::*;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (dir, s)
}

/// Six mots de trois syllabes, des lettres attendues, jamais deux fois la même phrase ;
/// une source qui ne rend que des caractères écartés est relancée, pas ramenée par modulo.
#[test]
fn a_generated_passphrase_has_six_typable_words() {
    let a = kit::generate_passphrase().unwrap();
    let b = kit::generate_passphrase().unwrap();
    assert_ne!(a, b);
    let w = kit::words(&a);
    assert_eq!(w.len(), kit::WORDS, "{a}");
    for word in w {
        assert_eq!(word.len(), 6, "{a}");
        for (i, c) in word.bytes().enumerate() {
            let set: &[u8] = if i % 2 == 0 {
                b"bdfgjklmnprstvz"
            } else {
                b"aeiou"
            };
            assert!(set.contains(&c), "{a}");
        }
    }
    let mut calls = 0;
    let p = kit::generate_from(|n| {
        calls += 1;
        // Premier lot : rien que des indices 60 et 61, tous écartés.
        Ok(if calls == 1 {
            "YZ".repeat(n / 2)
        } else {
            "0".repeat(n)
        })
    })
    .unwrap();
    assert_eq!(p, ["bababa"; 6].join("-"));
    assert!(calls >= 2);
}

/// Quatre positions distinctes et rangées ; la ressaisie se compare sans casse.
#[test]
fn four_words_confirm_the_kit() {
    let pos = kit::confirm_positions(6);
    assert_eq!(pos.len(), 4, "{pos:?}");
    assert!(
        pos.windows(2).all(|w| w[0] < w[1]) && pos[3] <= 6,
        "{pos:?}"
    );
    let pass = "bado-kifu-lemo-nipa-ruso-tavi";
    let typed = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(kit::confirmed(
        pass,
        &[1, 3, 4, 6],
        &typed(&["bado", "LEMO", "nipa", " tavi "])
    ));
    assert!(!kit::confirmed(
        pass,
        &[1, 3, 4, 6],
        &typed(&["bado", "lemo", "nipa", "ruso"])
    ));
    assert!(!kit::confirmed(pass, &[1, 3], &typed(&["bado"])));
}

/// Le kit dit la phrase, le fournisseur, l'emplacement, les clés S3 et la commande de
/// restauration ; il ne se rend pas sans phrase de passe ni fournisseur.
#[tokio::test]
async fn the_kit_says_everything_needed_on_a_new_machine() {
    let (dir, s) = services().await;
    let e = rpc(&s, &json!({"kit": true})).await.unwrap_err();
    assert!(e.to_string().contains("aucun fournisseur"), "{e}");
    let nas = dir.path().join("nas");
    let d = nas.display().to_string();
    s.publish_config("test", move |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = d;
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    let e = rpc(&s, &json!({"kit": true})).await.unwrap_err();
    assert!(e.to_string().contains("phrase de passe"), "{e}");
    s.platform
        .secrets
        .set(PASSPHRASE_SECRET, "bado-kifu-lemo-nipa-ruso-tavi")
        .unwrap();
    let text = rpc(&s, &json!({"kit": true})).await.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text.contains("Phrase de passe : bado-kifu-lemo-nipa-ruso-tavi"),
        "{text}"
    );
    assert!(
        text.contains(&format!("penelope restore {}", nas.display())),
        "{text}"
    );
    assert!(text.contains("illisibles"), "{text}");

    let mut c = penelope_kernel::config::Backup {
        provider: "s3".into(),
        ..Default::default()
    };
    c.s3.endpoint = provider::SCALEWAY_ENDPOINT.into();
    c.s3.region = provider::SCALEWAY_REGION.into();
    c.s3.bucket = "penelope-sauvegardes".into();
    let t = provider::Target::S3(c.s3.clone());
    let text = kit::render(&c, &t, "p", Some(("SCWID", "cle-secrete")), "2026-10-07");
    assert!(text.contains("Clé d'accès S3  : SCWID") && text.contains("cle-secrete"));
    assert!(
        text.contains(
            "penelope restore s3://penelope-sauvegardes/penelope/ --endpoint \
             https://s3.fr-par.scw.cloud --region fr-par"
        ),
        "{text}"
    );
    c.provider = "icloud".into();
    let t = provider::Target::Dir {
        path: PathBuf::from("/Users/moi/Library/Mobile Documents/com~apple~CloudDocs/Penelope"),
        icloud: true,
    };
    assert!(kit::render(&c, &t, "p", None, "j").contains("penelope restore icloud\n"));
}

/// Chaque sauvegarde se relit : la mauvaise phrase et une archive incomplète sont
/// refusées ; le rapport et `doctor` datent le dernier contrôle.
#[tokio::test]
async fn each_backup_is_checked_by_decrypting_it() {
    let (dir, s) = services().await;
    let work = dir.path().join("w");
    std::fs::create_dir_all(work.join("penelope")).unwrap();
    std::fs::write(work.join("penelope/MANIFEST.json"), "{}").unwrap();
    std::fs::write(work.join("penelope/penelope.db"), "db").unwrap();
    let tar = dir.path().join("a.tar.gz");
    penelope_platform::process::create_tar_gz(&tar, &work, &["penelope".into()]).unwrap();
    let sealed = dir.path().join("a.tar.gz.enc");
    penelope_platform::archive::seal(&tar, &sealed, "phrase").unwrap();
    let e = verify(&sealed, "phrase").unwrap_err().to_string();
    assert!(e.contains("secrets.enc absent"), "{e}");
    let e = verify(&sealed, "autre").unwrap_err().to_string();
    assert!(e.contains("phrase de passe incorrecte"), "{e}");

    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    s.publish_config("test", |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = "{data}/sauvegardes".into();
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    let report = run(&s, true, Some(false)).await.unwrap();
    assert!(
        report["verified"]["entries"].as_u64().unwrap() >= 3,
        "{report}"
    );
    assert_eq!(report["verified"]["at"], report["manifest"]["created_at"]);
    let c = doctor_check(&s).await;
    assert!(
        c.detail.contains("déchiffrement vérifié le 2026-09-16"),
        "{c:?}"
    );
}

/// L'essai de mise en place écrit puis efface : un dossier en lecture seule est refusé.
#[tokio::test]
async fn the_provider_is_tried_before_the_first_night() {
    let dir = tempfile::tempdir().unwrap();
    let t = provider::Target::Dir {
        path: dir.path().join("nas"),
        icloud: false,
    };
    let at = provider::probe(&t, None).await.unwrap();
    assert!(at.ends_with("nas"), "{at}");
    assert_eq!(
        std::fs::read_dir(dir.path().join("nas")).unwrap().count(),
        0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = dir.path().join("lecture-seule");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
        let t = provider::Target::Dir {
            path: ro.clone(),
            icloud: false,
        };
        let e = provider::probe(&t, None).await.unwrap_err().to_string();
        assert!(e.contains("écriture dans"), "{e}");
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let e = provider::probe(&provider::Target::S3(Default::default()), None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("clés S3"), "{e}");
}

/// #329 : l'inventaire relit les serveurs d'inférence en LaunchAgent et les commandes
/// MCP ; une commande absente de la machine est nommée.
#[test]
fn the_inventory_reads_launch_agents_and_mcp_commands() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    let args = vec![
        "/opt/mlx/bin/mlx_lm.server".to_string(),
        "--port".into(),
        "8080".into(),
    ];
    let plist = penelope_platform::service::agent_plist(
        "com.penelope.inference.local",
        &args,
        Path::new("/tmp/o"),
        Path::new("/tmp/e"),
        &[],
    );
    std::fs::write(agents.join("com.penelope.inference.local.plist"), plist).unwrap();
    std::fs::write(agents.join("com.autre.plist"), "<plist/>").unwrap();
    let services = inventory::services(Some(home.path()));
    assert_eq!(
        services,
        vec![json!({"label": "com.penelope.inference.local", "args": args})]
    );
    assert!(inventory::services(None).is_empty());

    let mcp = home.path().join("mcp.d");
    std::fs::create_dir_all(&mcp).unwrap();
    std::fs::write(mcp.join("a.toml"), "command = \"npx\"\n").unwrap();
    std::fs::write(
        mcp.join("b.toml"),
        "command = \"absente\"\nenabled = false\n",
    )
    .unwrap();
    let manifest = json!({"mcp_commands": inventory::mcp_commands(&mcp)});
    assert_eq!(
        manifest["mcp_commands"],
        json!([{"server": "a", "command": "npx"}])
    );
    let missing = inventory::missing_commands(&manifest, |c| c == "npx");
    assert!(missing.is_empty());
    let missing = inventory::missing_commands(&manifest, |_| false);
    assert_eq!(missing.len(), 1);
}
