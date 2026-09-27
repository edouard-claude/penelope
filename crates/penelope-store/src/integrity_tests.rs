use super::*;

/// #158 : SQLite embarqué à jour. La 3.46.0 (mai 2024) portait un faux positif de
/// l'`integrity-check` FTS5, corrigé en 3.46.1 ; c'est la source du verdict que le
/// reste de ce lot apprend à ne pas croire sur parole.
#[test]
fn the_bundled_sqlite_is_recent_enough() {
    let v = rusqlite::version();
    let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
    assert!(parts.len() >= 2, "version illisible : {v}");
    let (major, minor) = (parts[0], parts[1]);
    assert!(
        (major, minor) >= (3, 50),
        "SQLite {v} : au moins 3.50 attendu (issue #158)"
    );
}

/// #158 : un verdict qui ne parle que d'index FTS5 désigne du **dérivé**, pas des
/// données. Une seule ligne qui parle d'autre chose, et on ne touche plus à rien.
#[test]
fn only_a_verdict_about_derived_indexes_is_repairable() {
    let one = "malformed inverted index for FTS5 table main.messages_fts";
    assert_eq!(fts_tables(one), Some(vec!["messages_fts".to_string()]));

    // Les deux tables vues le 21/09, d'un appel à l'autre.
    let two = "malformed inverted index for FTS5 table main.mem_fts\n\
                   malformed inverted index for FTS5 table main.messages_fts";
    assert_eq!(
        fts_tables(two),
        Some(vec!["mem_fts".to_string(), "messages_fts".to_string()])
    );

    // Une vraie atteinte, seule ou mêlée à une ligne FTS : rien n'est reconstruit.
    assert_eq!(fts_tables("ok"), None);
    assert_eq!(
        fts_tables("row 12 missing from index messages_session"),
        None
    );
    let mixed = "malformed inverted index for FTS5 table main.mem_fts\n\
                     row 12 missing from index messages_session";
    assert_eq!(
        fts_tables(mixed),
        None,
        "une corruption de données cachée derrière une ligne d'index ne passe pas"
    );
    // Un nom qui n'est pas un identifiant n'entre pas dans une requête.
    assert_eq!(
        fts_tables("malformed inverted index for FTS5 table main.x\"; DROP TABLE y--"),
        None
    );
}

/// #158 : `quick_check` rend **plusieurs** lignes. N'en lire qu'une cachait une
/// corruption de données derrière une ligne d'index.
#[test]
fn every_line_of_a_verdict_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let c = open_connection(&path, false).unwrap();
    assert_eq!(check_lines(&c, "PRAGMA quick_check;").unwrap(), "ok");
    assert_eq!(check_lines(&c, "PRAGMA integrity_check;").unwrap(), "ok");
}

/// #158 : le fichier fait foi, pas une connexion. Sur une base saine, les deux
/// verdicts concordent et le rapport le dit sans ouvrir de connexion neuve.
#[test]
fn a_sound_database_needs_no_second_opinion() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let r = store.integrity_report().unwrap();
    assert_eq!(r.pool, "ok");
    assert!(
        r.fresh.is_none(),
        "connexion neuve inutile quand le pool dit ok"
    );
    assert!(r.sound());
    assert!(!r.reader_lied());
    assert_eq!(r.fts_only(), None);
    assert!(store.repaired_fts().is_empty());
}

/// #158 : un lecteur qui accuse une base que le fichier dément ne ment qu'une fois —
/// il est fermé, et le pool en rouvre un à sa place.
#[test]
fn a_lying_reader_is_recognised_and_replaced() {
    let r = IntegrityReport {
        pool: "malformed inverted index for FTS5 table main.mem_fts".into(),
        fresh: Some("ok".into()),
        reader: crate::pool::ReaderStats {
            age_s: 11_000,
            served: 4_200,
        },
    };
    assert!(r.reader_lied(), "le fichier dément le lecteur");
    assert!(r.sound(), "c'est le fichier qui fait foi");
    assert_eq!(r.verdict(), "ok");
    assert_eq!(r.fts_only(), None, "rien à reconstruire : rien n'est cassé");
}

/// #158 : quand les deux sont d'accord sur un index dérivé, il y a bien quelque chose
/// à reconstruire — et toujours rien à restaurer.
#[test]
fn a_confirmed_index_fault_names_the_table() {
    let bad = "malformed inverted index for FTS5 table main.messages_fts";
    let r = IntegrityReport {
        pool: bad.into(),
        fresh: Some(bad.into()),
        reader: crate::pool::ReaderStats {
            age_s: 10,
            served: 1,
        },
    };
    assert!(!r.sound());
    assert!(!r.reader_lied());
    assert_eq!(r.fts_only(), Some(vec!["messages_fts".to_string()]));

    // Une atteinte aux données, elle, n'est pas reconstructible.
    let hard = IntegrityReport {
        pool: "row 12 missing from index messages_session".into(),
        fresh: Some("row 12 missing from index messages_session".into()),
        reader: crate::pool::ReaderStats {
            age_s: 10,
            served: 1,
        },
    };
    assert_eq!(hard.fts_only(), None);
}

/// #158 : une base dont **seul** un index FTS5 est abîmé s'ouvre, se répare et le
/// dit. Refuser de démarrer pour ça aurait déclenché le retour arrière de #153.
#[test]
fn a_broken_search_index_does_not_stop_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    {
        let store = Store::open(&path).unwrap();
        assert!(store.repaired_fts().is_empty());
    }
    // L'index inversé de `messages_fts` est vidé sous les pieds de SQLite : la table
    // `%_data` porte l'index, `%_content` le texte. C'est exactement la panne décrite.
    {
        let c = open_connection(&path, false).unwrap();
        c.execute_batch(
            "INSERT INTO messages_fts(content, session_id, msg_id) VALUES('bonjour', 's1', 1);
                 DELETE FROM messages_fts_data WHERE id > 1;",
        )
        .unwrap();
        let verdict = check_lines(&c, "PRAGMA quick_check;").unwrap();
        assert_ne!(verdict, "ok", "l'index est bien abîmé : {verdict}");
        assert!(fts_tables(&verdict).is_some(), "{verdict}");
    }
    // Et pourtant la base s'ouvre, parce que l'index se reconstruit.
    let store = Store::open(&path).unwrap();
    assert_eq!(store.repaired_fts(), ["messages_fts"]);
    assert_eq!(store.integrity().unwrap(), "ok");
}

/// #158 : un index abîmé **après** l'ouverture est vu par le pool, confirmé par une
/// connexion neuve, et nommé comme seul dérivé à reconstruire.
#[test]
fn a_fault_seen_by_the_pool_is_confirmed_by_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let store = Store::open(&path).unwrap();
    {
        let c = open_connection(&path, false).unwrap();
        c.execute_batch(
            "INSERT INTO messages_fts(content, session_id, msg_id) VALUES('bonjour', 's1', 1);
             DELETE FROM messages_fts_data WHERE id > 1;",
        )
        .unwrap();
    }
    let r = store.integrity_report().unwrap();
    assert_ne!(r.pool, "ok");
    assert_eq!(
        r.fresh.as_deref(),
        Some(r.pool.as_str()),
        "le fichier confirme"
    );
    assert!(!r.sound());
    assert!(!r.reader_lied());
    assert_eq!(r.fts_only(), Some(vec!["messages_fts".to_string()]));
    // Le lecteur qui a accusé la base n'est pas rendu au pool : le suivant répond encore.
    assert_ne!(store.integrity().unwrap(), "ok");
}

/// #158 : un index si abîmé que SQLite ne construit plus sa table virtuelle est recréé
/// vide depuis sa déclaration ; son contenu revient de la source de vérité.
#[test]
fn an_unconstructible_search_index_is_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    drop(Store::open(&path).unwrap());
    {
        let c = open_connection(&path, false).unwrap();
        c.execute_batch("DROP TABLE messages_fts_config;").unwrap();
        let err = check_lines(&c, "PRAGMA quick_check;").unwrap_err();
        assert!(
            err.to_string().contains("vtable constructor failed"),
            "{err}"
        );
    }
    let store = Store::open(&path).unwrap();
    assert_eq!(store.repaired_fts(), ["messages_fts"]);
    assert_eq!(store.integrity().unwrap(), "ok");
    let rows: i64 = store
        .read_blocking(|c| Ok(c.query_row("SELECT count(*) FROM messages_fts", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(rows, 0, "recréé vide");
}

/// Une erreur « vtable constructor failed » ne désigne un index à recréer que si la
/// table nommée est déclarée `USING fts5` : une table ordinaire n'est jamais touchée.
#[test]
fn only_a_declared_fts5_table_is_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    drop(Store::open(&path).unwrap());
    let c = open_connection(&path, false).unwrap();
    let err = |name: &str| StoreError::other(format!("vtable constructor failed: {name}"));
    assert_eq!(
        unconstructible_fts(&c, &err("messages_fts")).as_deref(),
        Some("messages_fts")
    );
    assert_eq!(unconstructible_fts(&c, &err("kv")), None, "table ordinaire");
    assert_eq!(unconstructible_fts(&c, &err("absente")), None);
    assert_eq!(unconstructible_fts(&c, &err("x'; DROP")), None);
    assert_eq!(
        unconstructible_fts(&c, &StoreError::other("disk I/O error")),
        None
    );
}
