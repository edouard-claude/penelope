//! Vérifications de la base migrée : lignes semées relues, schéma, tables du PRD.

use super::*;

pub(super) fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql} : {e}"))
}

pub(super) fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql} : {e}"))
}

/// Chaque donnée semée se relit : un compte et une valeur par table.
pub(super) fn check_seeded_rows(c: &Connection) {
    assert_eq!(count(c, "SELECT count(*) FROM sessions"), 1);
    let (kind, chat, state): (String, i64, String) = c
        .query_row(
            "SELECT kind, tg_chat_id, state FROM sessions WHERE id = ?1",
            [SESSION_ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("la session semée");
    assert_eq!(
        (kind.as_str(), chat, state.as_str()),
        ("chat", CHAT_ID, "active")
    );

    assert_eq!(
        count(c, "SELECT count(*) FROM messages"),
        MESSAGES.len() as i64
    );
    assert_eq!(
        count(c, "SELECT count(*) FROM messages WHERE role = 'tool'"),
        2
    );
    let tool: String = c
        .query_row(
            "SELECT tool_name FROM messages WHERE tool_call_id = ?1",
            [TOOL_CALL_ID],
            |r| r.get(0),
        )
        .expect("le résultat d'outil semé");
    assert_eq!(tool, "fs_list");
    assert!(
        count(
            c,
            "SELECT count(*) FROM messages_fts WHERE messages_fts MATCH 'tests'"
        ) >= 2,
        "l'index plein texte des messages répond encore"
    );

    assert_eq!(count(c, "SELECT count(*) FROM lcm_nodes"), 2);
    let active: String = one(c, "SELECT id FROM lcm_nodes WHERE superseded_by IS NULL");
    assert_eq!(active, LCM_ACTIVE_NODE);
    assert_eq!(count(c, "SELECT count(*) FROM lcm_edges"), 1);
    assert_eq!(count(c, "SELECT count(*) FROM message_context"), 1);
    let first_tool: String = one(
        c,
        "SELECT json_extract(context, '$.tools[0]') FROM message_context",
    );
    assert_eq!(first_tool, "fs_list");
    assert_eq!(
        count(c, "SELECT count(*) FROM turn_queue WHERE state = 'done'"),
        1
    );

    assert_eq!(count(c, "SELECT count(*) FROM events"), EVENT_COUNT as i64);
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM events WHERE kind = 'turn.finished' AND session_id IS NOT NULL"
        ),
        1
    );
    let last_seq: i64 = c
        .query_row(
            "SELECT max(seq) FROM events WHERE session_id = ?1",
            [SESSION_ID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        last_seq, 5,
        "cinq événements de session, numérotés par session"
    );

    assert_eq!(count(c, "SELECT count(*) FROM effects"), 1);
    let (state, tool, exit): (String, String, i64) = c
        .query_row(
            "SELECT state, tool, json_extract(result, '$.exit_code') FROM effects",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (state.as_str(), tool.as_str(), exit),
        ("completed", "shell_exec", 0)
    );

    assert_eq!(count(c, "SELECT count(*) FROM usage"), 1);
    let model: String = one(c, "SELECT model FROM usage");
    assert_eq!(model, MODEL_ID);
    let cost: f64 = one(c, "SELECT cost_usd FROM usage");
    assert!((cost - 0.0123).abs() < 1e-9, "{cost}");
    assert_eq!(count(c, "SELECT count(*) FROM llm_requests"), 1);
    let (req_state, req_hash): (String, String) = c
        .query_row("SELECT state, system_hash FROM llm_requests", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(
        (req_state.as_str(), req_hash.as_str()),
        ("completed", SYSTEM_HASH)
    );
    assert_eq!(count(c, "SELECT count(*) FROM prompt_snapshots"), 1);

    assert_eq!(count(c, "SELECT count(*) FROM mem_entries"), 1);
    let text: String = c
        .query_row(
            "SELECT text FROM mem_entries WHERE uid = ?1",
            [MEM_UID],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(text, "Le propriétaire préfère des réponses courtes.");
    let origin: String = c
        .query_row(
            "SELECT origin FROM mem_provenance WHERE uid = ?1",
            [MEM_UID],
            |r| r.get(0),
        )
        .expect("la provenance semée");
    assert_eq!(origin, "owner");
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM mem_fts WHERE mem_fts MATCH 'courtes'"
        ),
        1
    );
    let seen: i64 = one(c, "SELECT seen FROM mem_signals");
    assert_eq!(seen, 4);
    assert_eq!(count(c, "SELECT count(*) FROM mem_flags"), 1);
    assert_eq!(count(c, "SELECT count(*) FROM mem_history"), 1);
    assert_eq!(
        count(
            c,
            "SELECT count(*) FROM mem_candidates WHERE state = 'promoted'"
        ),
        1
    );

    assert_eq!(count(c, "SELECT count(*) FROM schedules"), 1);
    let (kind, origin_session, legacy): (String, String, Option<String>) = c
        .query_row(
            "SELECT kind, json_extract(target, '$.origin_session'),
                    json_extract(target, '$.session_id') FROM schedules WHERE id = ?1",
            [SCHEDULE_ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("la planification semée");
    assert_eq!(
        (kind.as_str(), origin_session.as_str()),
        ("cron", SESSION_ID)
    );
    assert!(legacy.is_none(), "forme d'après la migration 0009");

    assert_eq!(count(c, "SELECT count(*) FROM tg_outbox"), 1);
    let (out_state, message_id): (String, i64) = c
        .query_row(
            "SELECT state, message_id FROM tg_outbox WHERE id = ?1",
            [OUTBOX_ID],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("la ligne d'outbox semée");
    assert_eq!((out_state.as_str(), message_id), ("sent", 4242));
    assert_eq!(
        count(c, "SELECT count(*) FROM tg_updates WHERE processed = 1"),
        1
    );

    assert_eq!(count(c, "SELECT count(*) FROM approval_requests"), 1);
    let (a_state, decision, via): (String, String, String) = c
        .query_row(
            "SELECT state, decision, decided_via FROM approval_requests WHERE id = ?1",
            [APPROVAL_ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("l'approbation semée");
    assert_eq!(
        (a_state.as_str(), decision.as_str(), via.as_str()),
        ("approved", "approve", "telegram")
    );

    assert_eq!(count(c, "SELECT count(*) FROM policies"), 1);
    let (p_decision, revoked): (String, Option<String>) = c
        .query_row(
            "SELECT decision, revoked_at FROM policies WHERE id = ?1",
            [POLICY_ID],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("la règle semée");
    assert_eq!(p_decision, "auto");
    assert!(revoked.is_none(), "0017 ne révoque que git_clone");

    let offset: String = one(c, "SELECT v FROM kv WHERE k = 'tg.offset'");
    assert_eq!(offset, "1002");
    assert_eq!(count(c, "SELECT count(*) FROM config_generations"), 1);
}

/// Les `penelope-*.db` du répertoire des fixtures, triés.
pub(super) fn fixture_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("penelope-") && n.ends_with(".db"))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `major.minor.patch` d'une version, suffixe (`-alpha.N`) ignoré.
pub(super) fn version_triple(v: &str) -> (u64, u64, u64) {
    let core = v.split('-').next().unwrap_or(v);
    let mut it = core.split('.').map(|n| n.parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// Le schéma tel que SQLite le voit : colonnes de chaque table (nom, type, NOT NULL,
/// défaut, clé primaire) et définition de chaque index, hors objets internes de SQLite.
/// Une base migrée depuis la fixture doit rendre exactement celui d'une base neuve.
pub(super) fn schema_signature(
    c: &Connection,
) -> penelope_store::Result<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    let mut st = c.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master
         WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (kind, name, table, sql) = row?;
        let value = if kind == "table" {
            let mut cols = c.prepare(&format!("PRAGMA table_info(\"{name}\")"))?;
            let cols = cols.query_map([], |r| {
                Ok(format!(
                    "{} {} notnull={} default={} pk={}",
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    r.get::<_, i64>(5)?
                ))
            })?;
            cols.collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            vec![table, sql]
        };
        out.insert(format!("{kind}:{name}"), value);
    }
    Ok(out)
}

/// `all_prd_tables_exist` (src/migrations.rs), sur la base migrée.
pub(super) fn check_prd_tables(c: &Connection) {
    for t in [
        "events",
        "sessions",
        "messages",
        "messages_fts",
        "lcm_nodes",
        "lcm_edges",
        "artifacts",
        "turn_queue",
        "leases",
        "effects",
        "llm_requests",
        "usage",
        "mem_entries",
        "mem_fts",
        "mem_vec",
        "mem_links",
        "mem_provenance",
        "mem_signals",
        "mem_candidates",
        "mem_history",
        "dream_runs",
        "intents",
        "episodes",
        "skills",
        "mcp_servers",
        "mcp_tools",
        "mcp_tools_fts",
        "mcp_tools_vec",
        "mcp_tasks",
        "oauth_clients",
        "oauth_state",
        "approval_requests",
        "policies",
        "workflows",
        "workflow_runs",
        "schedules",
        "seen_items",
        "tg_updates",
        "tg_outbox",
        "tg_actions",
        "tg_topics",
        "config_generations",
        "subsystem_apply_results",
        "event_purges",
        "prompt_snapshots",
    ] {
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "table manquante : {t}");
    }
}

/// Ouvre une **copie** de `fixture`, la migre par `Store::open`, et vérifie : toutes les
/// migrations, le schéma d'une base neuve, les tables du PRD, chaque donnée semée, la
/// chaîne d'événements (qui doit aussi pouvoir continuer), l'intégrité du fichier.
pub(super) async fn check_fixture(fixture: &Path) {
    let name = fixture
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("penelope.db");
    std::fs::copy(fixture, &copy).unwrap();
    let store = Store::open(&copy).unwrap_or_else(|e| panic!("{name} : {e}"));
    assert!(
        store.repaired_fts().is_empty(),
        "{name} : index FTS reconstruits à l'ouverture : {:?}",
        store.repaired_fts()
    );

    // 1. Toutes les migrations, dans l'ordre.
    let applied = store.read_blocking(applied_versions).unwrap();
    let expected: Vec<String> = MIGRATIONS.iter().map(|m| m.version.to_string()).collect();
    assert_eq!(applied, expected, "{name} : schema_migrations incomplète");

    // 2. Le schéma migré est celui d'une base neuve, table par table, index par index.
    let fresh = Store::open(dir.path().join("fresh.db")).unwrap();
    let migrated = store.read_blocking(schema_signature).unwrap();
    let neuf = fresh.read_blocking(schema_signature).unwrap();
    let mut diff = Vec::new();
    for k in migrated.keys().chain(neuf.keys()).collect::<BTreeSet<_>>() {
        match (migrated.get(k), neuf.get(k)) {
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => diff.push(format!("{k} : migré {a:?}, neuf {b:?}")),
            (Some(_), None) => diff.push(format!("{k} : seulement dans la base migrée")),
            (None, Some(_)) => diff.push(format!("{k} : seulement dans la base neuve")),
            (None, None) => unreachable!(),
        }
    }
    assert!(
        diff.is_empty(),
        "{name} : le schéma migré diffère d'une base neuve :\n{}",
        diff.join("\n")
    );

    // 3. Les tables du PRD ; 4. chaque donnée semée.
    store
        .read_blocking(|c| {
            check_prd_tables(c);
            check_seeded_rows(c);
            Ok(())
        })
        .unwrap();

    // 5. La chaîne d'événements se vérifie, et continue après la migration.
    let log = EventLog::new(
        store.clone(),
        Arc::new(TestClock::new(START_MS + 86_400_000)),
    );
    let report = log.verify().await.unwrap();
    assert!(report.ok, "{name} : chaîne rompue : {:?}", report.detail);
    assert_eq!(report.checked, EVENT_COUNT, "{name}");
    log.append(EventDraft::new(
        "store.migrated",
        json!({"fixture": name, "migrations": MIGRATIONS.len()}),
    ))
    .await
    .unwrap();
    let report = log.verify().await.unwrap();
    assert!(
        report.ok,
        "{name} : la chaîne ne continue pas : {:?}",
        report.detail
    );
    assert_eq!(report.checked, EVENT_COUNT + 1);

    // 6. Le fichier est intègre après la migration.
    assert_eq!(store.integrity().unwrap(), "ok", "{name}");
    store.close();
    fresh.close();
}
