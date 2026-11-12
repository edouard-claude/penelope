use super::*;

/// #158 : SQLite embarqué à jour. La 3.46.0 (mai 2024) portait un faux positif de
/// l'`integrity-check` FTS5, corrigé en 3.46.1 ; c'est la source du verdict que le
/// reste de ce lot apprend à ne pas croire sur parole. Depuis RUSTSEC-2026-0459
/// (CVE-2026-119375, lecture de mémoire non initialisée avec un `-wal` forgé), le plancher
/// est 3.54 : redescendre sous lui rouvrirait l'avis.
#[test]
fn the_bundled_sqlite_is_recent_enough() {
    let v = rusqlite::version();
    let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
    assert!(parts.len() >= 2, "version illisible : {v}");
    let (major, minor) = (parts[0], parts[1]);
    assert!(
        (major, minor) >= (3, 54),
        "SQLite {v} : au moins 3.54 attendu (issue #158, RUSTSEC-2026-0459)"
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

/// SQLite 3.54 nomme la même panne autrement : FTS5 dit où il l'a trouvée, et le contrôle
/// rend sa phrase au lieu de « malformed inverted index ». Ne pas la reconnaître, c'était
/// refuser de démarrer pour un index qui se reconstruit (#158, #153).
#[test]
fn the_fts5_wording_of_sqlite_3_54_names_the_table_too() {
    for line in [
        "fts5: corruption found reading blob 10 from table \"messages_fts\"",
        "fts5: corruption on page 3, segment 7, table \"messages_fts\"",
        "fts5: corruption in table \"messages_fts\"",
        "fts5: corrupt structure record for table \"messages_fts\"",
        "fts5: checksum mismatch for table \"messages_fts\"",
    ] {
        assert_eq!(
            fts_tables(line),
            Some(vec!["messages_fts".to_string()]),
            "{line}"
        );
    }
    let both = "fts5: corruption in table \"mem_fts\"\n\
                malformed inverted index for FTS5 table main.messages_fts";
    assert_eq!(
        fts_tables(both),
        Some(vec!["mem_fts".to_string(), "messages_fts".to_string()])
    );
    // Rien d'autre qu'une panne d'index, et rien d'autre qu'un identifiant.
    for line in [
        "fts5: syntax error near \"x\"",
        "fts5: missing row 4 from content table \"main\".\"messages\"",
        "fts5: corruption in table \"x\"; DROP TABLE y--\"",
        "fts5: corruption in table messages_fts",
        "fts5: corruption in table \"mem_fts\"\nrow 12 missing from index messages_session",
    ] {
        assert_eq!(fts_tables(line), None, "{line}");
    }
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

/// #158 : une base saine est dite saine, et rien n'est à reconstruire.
#[test]
fn a_sound_database_is_sound() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db")).unwrap();
    let r = store.integrity_report().unwrap();
    assert_eq!(r.verdict(), "ok");
    assert!(r.sound());
    assert_eq!(r.fts_only(), None);
    assert!(store.repaired_fts().is_empty());
}

/// #158 : un verdict qui ne parle que d'un index dérivé nomme la table à reconstruire,
/// et jamais rien à restaurer.
#[test]
fn an_index_fault_names_the_table() {
    let r = IntegrityReport::of("malformed inverted index for FTS5 table main.messages_fts");
    assert!(!r.sound());
    assert_eq!(r.fts_only(), Some(vec!["messages_fts".to_string()]));

    // Une atteinte aux données, elle, n'est pas reconstructible.
    let hard = IntegrityReport::of("row 12 missing from index messages_session");
    assert!(!hard.sound());
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

/// #158 : un index abîmé **après** l'ouverture est vu, et nommé comme seul dérivé à
/// reconstruire.
#[test]
fn a_fault_after_opening_is_seen_and_names_the_table() {
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
    assert!(!r.sound());
    assert_eq!(r.fts_only(), Some(vec!["messages_fts".to_string()]));
    // Le verdict ne s'use pas : le contrôle suivant dit la même chose.
    assert_eq!(store.integrity().unwrap(), r.verdict());
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

/// Chaque lecteur du pool cherche une fois dans `messages_fts` : les quatre connexions
/// sont tenues ensemble, aucune ne sert deux fois.
fn every_reader_searches(store: &Store) -> Vec<i64> {
    let n = store.inner.readers.capacity();
    let together = std::sync::Arc::new(std::sync::Barrier::new(n));
    let hands: Vec<_> = (0..n)
        .map(|_| {
            let (store, together) = (store.clone(), together.clone());
            std::thread::spawn(move || {
                store
                    .read_blocking(move |c| {
                        let hits = c.query_row(
                            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'bonjour'",
                            [],
                            |r| r.get(0),
                        )?;
                        together.wait();
                        Ok(hits)
                    })
                    .unwrap()
            })
        })
        .collect();
    hands.into_iter().map(|h| h.join().unwrap()).collect()
}

fn say_hello(store: &Store, from: i64, count: i64) {
    store
        .write_blocking(move |tx| {
            for i in from..from + count {
                tx.execute(
                    "INSERT INTO messages_fts(content, session_id, msg_id) \
                     VALUES('bonjour à tous', 's1', ?1)",
                    [i],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

/// #468 : la cause du « lecteur qui ment » de #158. FTS5 garde, par connexion, la
/// structure de son index ; une recherche la revalide, le contrôle d'intégrité non. Une
/// connexion qui a cherché (ou qui s'est déjà contrôlée) puis voit **une autre** connexion
/// écrire dans l'index rend donc « corruption » sur un fichier sain, à tous les coups. Le
/// daemon vit ainsi : quatre lecteurs qui cherchent, un écrivain qui range chaque message.
/// Le contrôle se fait sur une connexion neuve, la seule qui n'ait rien en cache.
#[test]
fn a_reader_that_searched_before_a_write_does_not_accuse_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("t.db")).unwrap();
    say_hello(&store, 0, 50);
    assert_eq!(every_reader_searches(&store), [50; 4]);

    say_hello(&store, 50, 1);
    let r = store.integrity_report().unwrap();
    assert!(r.sound(), "{}", r.verdict());
    assert_eq!(store.integrity().unwrap(), "ok");

    // Un contrôle laisse lui aussi la structure en cache : le suivant, après une
    // écriture, accusait à son tour.
    say_hello(&store, 51, 1);
    assert_eq!(store.integrity().unwrap(), "ok");
    assert!(store.integrity_report().unwrap().sound());

    // Et la recherche, elle, n'a jamais menti.
    assert_eq!(every_reader_searches(&store), [52; 4]);
    // Le contrôle ne coûte aucun lecteur : avant, chaque faux verdict en fermait un sans
    // le remplacer, et la lecture qui trouvait le pool vide attendait cinq secondes.
    assert_eq!(store.inner.readers.idle(), store.inner.readers.capacity());
}
