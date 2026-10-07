//! Sauvegarde : archive, restauration, phrase de passe, rotation, `doctor`.

use super::*;
use penelope_kernel::config::Backup;

use std::sync::Arc;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (dir, s)
}

/// #42, #327 : sauvegarde puis restauration dans un répertoire vide : la base, le vault,
/// le workspace et `mcp-data` reviennent identiques ; les valeurs des secrets sont dans
/// l'archive, sous une seconde couche, illisibles sans la phrase de passe.
#[tokio::test]
async fn a_backup_restores_the_database_and_the_vault() {
    let (_dir, s) = services().await;
    let s = &s;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("memoire.md"), "- un souvenir précis ^01UID\n").unwrap();
    let data = s.platform.dirs.data();
    std::fs::create_dir_all(data.join("workspace/projet")).unwrap();
    std::fs::write(data.join("workspace/projet/notes.txt"), "brouillon").unwrap();
    std::fs::create_dir_all(data.join("mcp-data/pont/session")).unwrap();
    std::fs::write(data.join("mcp-data/pont/session/creds.json"), "{}").unwrap();
    std::fs::create_dir_all(data.join("models/whisper")).unwrap();
    std::fs::write(data.join("models/whisper/poids.bin"), "lourd").unwrap();
    s.platform
        .secrets
        .set(PASSPHRASE_SECRET, "phrase de passe de sauvegarde")
        .unwrap();
    s.platform
        .secrets
        .set("openrouter_api_key", "sk-or-v1-valeur-secrete")
        .unwrap();
    // Une trace dans la base, pour vérifier qu'elle revient.
    s.sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Atlas".into()),
        )
        .await
        .unwrap();

    let (archive, report) = build(s, false).await.unwrap();
    assert!(archive.is_file());
    assert!(report["bytes"].as_u64().unwrap_or(0) > 0);
    assert!(report["sha256"].as_str().is_some());
    let manifest = &report["manifest"];
    assert_eq!(manifest["secrets_included"], json!(["openrouter_api_key"]));
    assert_eq!(manifest["secrets_expected"], json!([]));
    let names: Vec<&str> = manifest["contents"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["name"].as_str())
        .collect();
    for n in ["vault", "workspace", "mcp-data"] {
        assert!(names.contains(&n), "{n} : {names:?}");
    }
    assert!(!names.contains(&"models"), "{names:?}");
    let models = manifest["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "models")
        .unwrap();
    assert_eq!(models["items"], json!(["whisper"]), "{models}");
    assert!(models["why"].as_str().unwrap().contains("rechargeables"));
    let raw = std::fs::read(&archive).unwrap();
    assert!(
        !String::from_utf8_lossy(&raw).contains("sk-or-v1-valeur-secrete"),
        "aucune valeur de secret lisible dans l'archive"
    );

    // Restauration dans un répertoire vide.
    let fresh = tempfile::tempdir().unwrap();
    let tar = fresh.path().join("s.tar.gz");
    penelope_platform::archive::open(&archive, &tar, "phrase de passe de sauvegarde").unwrap();
    penelope_platform::process::extract_tar_gz(&tar, fresh.path()).unwrap();
    let root = fresh.path().join("penelope");
    assert_eq!(
        std::fs::read_to_string(root.join("vault/memoire.md")).unwrap(),
        "- un souvenir précis ^01UID\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("workspace/projet/notes.txt")).unwrap(),
        "brouillon"
    );
    assert!(root.join("mcp-data/pont/session/creds.json").is_file());
    assert!(!root.join("models").exists());
    // Extrait, le fichier des secrets reste chiffré : il faut la phrase une seconde fois.
    let sealed = std::fs::read(root.join(secrets::FILE)).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("sk-or-v1-valeur-secrete"));
    assert!(secrets::open(&sealed, "autre phrase").is_err());
    let values = secrets::open(&sealed, "phrase de passe de sauvegarde").unwrap();
    assert_eq!(values["openrouter_api_key"], "sk-or-v1-valeur-secrete");
    assert!(!values.contains_key(PASSPHRASE_SECRET), "{values:?}");
    // La base restaurée porte la session créée.
    let restored = penelope_store::Store::open(root.join("penelope.db")).unwrap();
    let titles: Vec<String> = restored
        .read(|c| {
            let mut st = c.prepare("SELECT title FROM sessions")?;
            let rows = st.query_map([], |r| r.get::<_, Option<String>>(0))?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?.unwrap_or_default());
            }
            Ok(out)
        })
        .await
        .unwrap();
    assert!(titles.contains(&"Atlas".to_string()), "{titles:?}");
}

/// #77 : sur un runtime à un seul worker, une sauvegarde complète laisse passer les
/// écritures : l'instantané ne prend pas l'écrivain, et le tar, Argon2 et le
/// chiffrement ne monopolisent pas le runtime. `doctor` dit sa durée.
#[tokio::test(flavor = "current_thread")]
async fn a_backup_neither_blocks_the_runtime_nor_the_writer() {
    let (_dir, s) = services().await;
    s.platform
        .secrets
        .set(PASSPHRASE_SECRET, "phrase de passe")
        .unwrap();
    s.publish_config("test", |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = "{data}/sauvegardes".into();
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    let job = {
        let s = s.clone();
        tokio::spawn(async move { run(&s, false, None).await })
    };
    let mut during = 0;
    while !job.is_finished() {
        s.store
            .write(|tx| penelope_store::kv_set(tx, "battement", "1"))
            .await
            .unwrap();
        if !job.is_finished() {
            during += 1;
        }
        tokio::task::yield_now().await;
    }
    let report = job.await.unwrap().unwrap();
    assert!(during > 0, "aucune écriture pendant la sauvegarde");
    assert!(report["snapshot_ms"].is_u64(), "{report}");
    assert!(report["duration_ms"].is_u64(), "{report}");

    let events = s
        .events
        .range(0, 500)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "store.backup")
        .count();
    assert_eq!(events, 1);
    let check = doctor_check(&s).await;
    assert!(check.detail.contains("d'instantané"), "{}", check.detail);
}

/// #42 : sans phrase de passe, rien n'est écrit et le message dit quoi faire.
#[tokio::test]
async fn without_a_passphrase_nothing_is_written() {
    let (_dir, s) = services().await;
    let e = build(&s, false).await.unwrap_err();
    assert!(e.to_string().contains("penelope backup setup"), "{e}");
    let out = s.platform.dirs.data().join("backups");
    let archives = std::fs::read_dir(&out)
        .map(|r| {
            r.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".enc"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(archives, 0, "aucune archive ne doit rester");
}

/// #42 : l'état des sauvegardes remonte dans `doctor`.
#[tokio::test]
async fn doctor_says_when_there_is_no_backup_yet() {
    let (_dir, s) = services().await;
    let c = doctor_check(&s).await;
    assert!(!c.ok, "{c:?}");
    assert!(c.detail.contains("phrase de passe"), "{c:?}");

    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    let c = doctor_check(&s).await;
    assert!(c.detail.contains("aucun fournisseur"), "{c:?}");
    s.publish_config("test", |c| {
        c.backup.provider = "dir".into();
        c.backup.dir = "{data}/sauvegardes".into();
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    let c = doctor_check(&s).await;
    assert!(c.detail.contains("aucune sauvegarde"), "{c:?}");
}

/// #42 : 7 quotidiennes, 4 hebdomadaires, 12 mensuelles ; les autres partent.
#[test]
fn rotation_keeps_seven_four_and_twelve() {
    let dir = tempfile::tempdir().unwrap();
    // Trente sauvegardes quotidiennes consécutives.
    for day in 1..=30 {
        let name = format!("penelope-2026-09-{day:02}T04-00-00-000Z.tar.gz.enc");
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    let cfg = Backup::default();
    let removed = rotation_plan(&provider::archive_names(dir.path()), &cfg);
    for n in &removed {
        std::fs::remove_file(dir.path().join(n)).unwrap();
    }
    let left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    // 7 quotidiennes + une par semaine (4 au plus) + une par mois (1 ici).
    assert!(left.len() >= 7 && left.len() <= 12, "{left:?}");
    assert_eq!(removed.len(), 30 - left.len());
    assert!(
        left.iter().any(|n| n.contains("2026-09-30")),
        "la plus récente reste : {left:?}"
    );
    assert!(
        !left.iter().any(|n| n.contains("2026-09-02")),
        "les vieilles du même mois partent : {left:?}"
    );
}

/// Lit un compte dans une base SQLite fermée (l'instantané extrait de l'archive).
fn count(db: &Path, table: &str) -> i64 {
    let conn = penelope_store::rusqlite::Connection::open(db).unwrap();
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// Déchiffre et extrait une archive ; renvoie la racine `penelope/`.
fn unpack(archive: &Path, into: &Path, passphrase: &str) -> PathBuf {
    let tar = into.join("s.tar.gz");
    penelope_platform::archive::open(archive, &tar, passphrase).unwrap();
    penelope_platform::process::extract_tar_gz(&tar, into).unwrap();
    into.join("penelope")
}

/// Une instance avec de quoi chercher : deux souvenirs indexés et vectorisés, un échange,
/// un vecteur en cache. Renvoie la session.
async fn populate(s: &Services) -> String {
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Infrastructure\n\
         - Le serveur Atlas tourne sous Debian. <!-- uid: ATL1 -->\n\
         - Le devis ACME est signé depuis mars. <!-- uid: ACM1 -->\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    for uid in ["ATL1", "ACM1"] {
        s.memory
            .put_embedding(uid, "test-model", &[0.25; 8])
            .await
            .unwrap();
    }
    s.store
        .write(|tx| {
            tx.execute(
                "INSERT INTO embeddings_cache(content_hash, model, dim, embedding, created_at)
                 VALUES('h1', 'test-model', 8, x'00', '2026-01-01T00:00:00Z')",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let sid = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("CLI".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    let h = &s.context.history;
    h.append(
        &sid,
        &penelope_llm::types::ChatMessage::user("on relit le devis ACME demain"),
        5,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    h.append(
        &sid,
        &penelope_llm::types::ChatMessage::assistant("d'accord, je le prépare"),
        5,
        0,
        false,
        None,
    )
    .await
    .unwrap();
    sid
}

/// Ce que l'historique et la mémoire rendent pour les mêmes questions.
async fn searches(s: &Services) -> (Vec<(String, i64)>, Vec<String>) {
    let history: Vec<(String, i64)> = s
        .context
        .history
        .grep("devis", None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|h| (h.session_id, h.seq))
        .collect();
    let filter = penelope_memory::index::SearchFilter {
        limit: 10,
        ..Default::default()
    };
    let memory: Vec<String> = s
        .memory
        .search("Atlas Debian", None, &filter, &[])
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.entry.uid)
        .collect();
    (history, memory)
}

/// Services ouverts sur une base restaurée à sa place, dans une racine neuve.
async fn restored_services(snapshot: &Path) -> (tempfile::TempDir, Arc<Services>) {
    let fresh = tempfile::tempdir().unwrap();
    let platform = penelope_platform::Platform::for_tests(fresh.path().to_path_buf()).unwrap();
    std::fs::copy(snapshot, platform.dirs.db_path()).unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Arc::new(
        Services::for_tests(fresh.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (fresh, s)
}

/// #289 : l'archive laisse les index dérivés dehors (vecteurs, cache, plein texte), la
/// base restaurée porte la marque, et la reconstruction rend les mêmes recherches
/// qu'avant ; une base ordinaire n'a rien à reconstruire.
#[tokio::test]
async fn a_restored_base_searches_like_the_original_once_its_indexes_are_rebuilt() {
    let (_dir, s) = services().await;
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    let sid = populate(&s).await;
    let (history_before, memory_before) = searches(&s).await;
    assert_eq!(history_before, vec![(sid.clone(), 1)]);
    assert_eq!(memory_before, vec!["ATL1".to_string()]);
    assert!(rebuild_if_pending(&s).await.unwrap().is_none());

    let (archive, report) = build(&s, false).await.unwrap();
    let excluded = report["manifest"]["derived_excluded"].as_array().unwrap();
    assert_eq!(excluded.len(), DERIVED_TABLES.len(), "{report}");
    assert!(
        report["manifest"]["db_full_bytes"].as_u64().unwrap()
            >= report["manifest"]["db_bytes"].as_u64().unwrap(),
        "{report}"
    );

    // L'instantané : sources intactes, dérivés vides, marque posée.
    let out = tempfile::tempdir().unwrap();
    let snapshot = unpack(&archive, out.path(), "phrase").join("penelope.db");
    for table in DERIVED_TABLES {
        assert_eq!(count(&snapshot, table), 0, "{table}");
    }
    assert_eq!(count(&snapshot, "messages"), 2);
    assert_eq!(count(&snapshot, "mem_entries"), 2);
    {
        let conn = penelope_store::rusqlite::Connection::open(&snapshot).unwrap();
        let marked = penelope_store::kv_get(&conn, REBUILD_PENDING_KEY).unwrap();
        assert_eq!(marked.as_deref(), report["manifest"]["created_at"].as_str());
    }
    // La base vivante, elle, n'a rien perdu.
    let (history_live, memory_live) = searches(&s).await;
    assert_eq!(
        (history_live, memory_live),
        (history_before.clone(), memory_before.clone())
    );

    // Restaurée : muette tant que les index manquent, puis identique.
    let (_fresh, r) = restored_services(&snapshot).await;
    let (history_blind, memory_blind) = searches(&r).await;
    assert!(history_blind.is_empty() && memory_blind.is_empty());
    let rebuilt = rebuild_if_pending(&r).await.unwrap().unwrap();
    assert_eq!(rebuilt["messages_fts"], 2, "{rebuilt}");
    assert_eq!(rebuilt["mem_fts"], 2, "{rebuilt}");
    assert_eq!(rebuilt["mcp_tools_fts"], 0, "{rebuilt}");
    let (history_after, memory_after) = searches(&r).await;
    assert_eq!(history_after, history_before);
    assert_eq!(memory_after, memory_before);
    assert!(r.kv_get(REBUILD_PENDING_KEY).await.unwrap().is_none());
    assert!(rebuild_if_pending(&r).await.unwrap().is_none());
    let events = r
        .events
        .range(0, 500)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "store.rebuilt" && e.payload["reason"] == "restore")
        .count();
    assert_eq!(events, 1);
}

/// Mesure, à lancer à la main (`--ignored --nocapture`) : la taille de l'instantané et de
/// l'archive avec et sans les tables dérivées, sur une base gonflée de vecteurs et de
/// messages. Les chiffres de la 1.0.33 viennent de là.
#[tokio::test]
#[ignore = "mesure manuelle : construit une base de plusieurs dizaines de Mo"]
async fn measure_the_archive_with_and_without_derived_tables() {
    let (_dir, s) = services().await;
    s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
    let sid = populate(&s).await;
    // 4 000 messages de 400 caractères, 4 000 vecteurs de 1 024 flottants en mémoire et
    // autant en cache : l'ordre de grandeur d'une instance de quelques mois.
    // Du texte et des vecteurs qui ne se compressent pas mieux que les vrais : des mots
    // tirés d'une suite pseudo-aléatoire, des flottants distincts par vecteur.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..4_000 {
        let words: Vec<String> = (0..50)
            .map(|_| format!("{:x}", next() % 1_000_000))
            .collect();
        s.context
            .history
            .append(
                &sid,
                &penelope_llm::types::ChatMessage::user(format!(
                    "{i} devis {} facture {}",
                    words.join(" "),
                    i % 7
                )),
                100,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    let blobs: Vec<Vec<u8>> = (0..4_000)
        .map(|_| {
            let v: Vec<f32> = (0..1_024)
                .map(|_| (next() % 2_000_000) as f32 / 1_000_000.0 - 1.0)
                .collect();
            penelope_store::encode_embedding(&v)
        })
        .collect();
    s.store
        .write(move |tx| {
            for (i, blob) in blobs.iter().enumerate() {
                tx.execute(
                    "INSERT INTO mem_vec(uid, dim, model, embedding, updated_at)
                     VALUES(?1, 1024, 'm', ?2, '2026-01-01T00:00:00Z')",
                    penelope_store::rusqlite::params![format!("U{i}"), blob],
                )?;
                tx.execute(
                    "INSERT INTO embeddings_cache(content_hash, model, dim, embedding, created_at)
                     VALUES(?1, 'm', 1024, ?2, '2026-01-01T00:00:00Z')",
                    penelope_store::rusqlite::params![format!("H{i}"), blob],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();

    // Sans allègement : l'instantané brut, archivé et chiffré de la même façon.
    let work = tempfile::tempdir().unwrap();
    let full_root = work.path().join("penelope");
    std::fs::create_dir_all(&full_root).unwrap();
    s.store.backup_to(full_root.join("penelope.db")).unwrap();
    let full_db = std::fs::metadata(full_root.join("penelope.db"))
        .unwrap()
        .len();
    let tar = work.path().join("full.tar.gz");
    penelope_platform::process::create_tar_gz(&tar, work.path(), &["penelope".into()]).unwrap();
    let sealed = work.path().join("full.tar.gz.enc");
    let full_archive = penelope_platform::archive::seal(&tar, &sealed, "phrase").unwrap();

    let (_archive, report) = build(&s, false).await.unwrap();
    let light_db = report["manifest"]["db_bytes"].as_u64().unwrap();
    let light_archive = report["bytes"].as_u64().unwrap();
    println!(
        "base : {} Ko -> {} Ko ; archive : {} Ko -> {} Ko (x{:.1})",
        full_db / 1024,
        light_db / 1024,
        full_archive / 1024,
        light_archive / 1024,
        full_archive as f64 / light_archive.max(1) as f64
    );
    assert!(light_archive * 2 < full_archive, "{report}");
}
