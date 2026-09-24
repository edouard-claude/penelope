use super::*;

fn fresh() -> Connection {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    migrate(&mut c).unwrap();
    c
}

#[test]
fn migrations_apply_from_empty() {
    let c = fresh();
    assert_eq!(
        current_version(&c).unwrap().as_deref(),
        Some(MIGRATIONS.last().unwrap().version)
    );
}

#[test]
fn migrations_are_idempotent() {
    let mut c = fresh();
    migrate(&mut c).unwrap();
    migrate(&mut c).unwrap();
    assert_eq!(applied_versions(&c).unwrap().len(), MIGRATIONS.len());
}

#[test]
fn upgrade_from_each_previous_version() {
    // Applique 0001 seul, puis migre : simule une base d'une version antérieure.
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version TEXT PRIMARY KEY,
         applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));",
    )
    .unwrap();
    {
        let tx = c.transaction().unwrap();
        tx.execute_batch(SQL_0001).unwrap();
        tx.execute(
            "INSERT INTO schema_migrations(version) VALUES('0001_init')",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    migrate(&mut c).unwrap();
    assert_eq!(applied_versions(&c).unwrap().len(), MIGRATIONS.len());
}

/// #105 : une base d'avant la mesure de l'utilité garde ses dates et ses vues, pas
/// ses compteurs de rappels où tout comptait comme utile.
#[test]
fn usage_counters_restart_from_zero_once() {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(version TEXT PRIMARY KEY,
         applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));",
    )
    .unwrap();
    {
        let tx = c.transaction().unwrap();
        for m in MIGRATIONS
            .iter()
            .take_while(|m| m.version != "0015_memory_usage_reset")
        {
            tx.execute_batch(m.sql).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES(?1)",
                [m.version],
            )
            .unwrap();
        }
        tx.execute(
            "INSERT INTO mem_signals(uid, recalls, useful_recalls, last_recall, seen)
             VALUES('u1', 30, 30, '2026-09-01T10:00:00Z', 4)",
            [],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    migrate(&mut c).unwrap();
    let row: (i64, i64, String, i64) = c
        .query_row(
            "SELECT recalls, useful_recalls, last_recall, seen FROM mem_signals",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, (0, 0, "2026-09-01T10:00:00Z".into(), 4));
}

/// #205 : le prompt système devient une ligne adressée par son empreinte, et
/// `llm_requests` troque une colonne jamais écrite contre les trois clés déjà
/// calculées.
#[test]
fn prompt_snapshots_replace_the_dead_request_column() {
    let c = fresh();
    c.execute(
        "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at, last_seen_at, uses)
         VALUES('h1','Tu es Pénélope.','{}','2026-09-23T10:00:00Z','2026-09-23T10:00:00Z',1)",
        [],
    )
    .unwrap();
    let st = c.prepare("SELECT * FROM llm_requests").unwrap();
    let columns: Vec<String> = st.column_names().iter().map(|c| c.to_string()).collect();
    assert!(!columns.contains(&"request".to_string()), "{columns:?}");
    for k in ["system_hash", "tools_hash", "request_hash"] {
        assert!(columns.contains(&k.to_string()), "{k} absent : {columns:?}");
    }
}

#[test]
fn a_legacy_unbounded_clone_rule_is_revoked_on_upgrade() {
    let mut c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_migrations(version TEXT PRIMARY KEY,
         applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));",
    )
    .unwrap();
    {
        let tx = c.transaction().unwrap();
        for m in MIGRATIONS
            .iter()
            .take_while(|m| m.version != "0017_clone_source")
        {
            tx.execute_batch(m.sql).unwrap();
            tx.execute(
                "INSERT INTO schema_migrations(version) VALUES(?1)",
                [m.version],
            )
            .unwrap();
        }
        for (id, pattern) in [
            ("legacy", None),
            (
                "scoped",
                Some(r#"{"url":{"$origin":"https://github.com"}}"#),
            ),
        ] {
            tx.execute(
                "INSERT INTO policies(id,scope,tool,arg_match,decision,window,created_at)
                 VALUES(?1,'tool','git_clone',?2,'auto','always','2026-09-18T00:00:00Z')",
                rusqlite::params![id, pattern],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    migrate(&mut c).unwrap();
    let revoked: Option<String> = c
        .query_row(
            "SELECT revoked_at FROM policies WHERE id='legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let scoped: Option<String> = c
        .query_row(
            "SELECT revoked_at FROM policies WHERE id='scoped'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(revoked.is_some());
    assert!(scoped.is_none());
    let visible: i64 = c
        .query_row(
            "SELECT count(*) FROM policies WHERE tool='git_clone' AND revoked_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(visible, 1);
}

#[test]
fn fts5_is_available() {
    let c = fresh();
    c.execute(
        "INSERT INTO mem_fts(text, declencheurs, uid) VALUES('déploiement critique', 'deploy', 'u1')",
        [],
    )
    .unwrap();
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM mem_fts WHERE mem_fts MATCH 'deploiement'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "FTS5 avec remove_diacritics doit matcher sans accent");
}

#[test]
fn all_prd_tables_exist() {
    let c = fresh();
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
        "tool_jobs",
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

/// Issue #39 : une planification liée à sa session garde la référence, sans en
/// dépendre.
#[test]
fn schedule_session_ids_become_informative_references() {
    let c = fresh();
    c.execute(
        "INSERT INTO schedules(id, kind, spec, target, dedup, state, created_at, updated_at)
         VALUES('sch_1', 'cron', '{}', ?1, '{}', 'active', 't', 't')",
        [r#"{"type":"prompt","prompt":"veille","session_id":"s_1","origin":{"kind":"cli"}}"#],
    )
    .unwrap();
    c.execute_batch(SQL_0009).unwrap();
    let target: String = c
        .query_row("SELECT target FROM schedules WHERE id = 'sch_1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&target).unwrap();
    assert_eq!(v["origin_session"], "s_1");
    assert!(v.get("session_id").is_none());
    assert_eq!(v["prompt"], "veille");
    assert_eq!(v["origin"]["kind"], "cli");
}
