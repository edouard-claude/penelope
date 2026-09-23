//! Filet de migration depuis une vraie base 0.17 (lot B, épopée #208 ; tâche T10 de
//! `design/v1/gel-et-outillage.md`).
//!
//! `upgrade_from_each_previous_version` (`src/migrations.rs`) ne rejoue que `0001` puis
//! migre, sans base réelle. Ici, une base **produite par la 0.17 elle-même** (`Store::open`
//! puis semis de données représentatives) est commitée dans `tests/fixtures/`, et chaque
//! `Store::open` de la V1 doit la migrer entière et la relire.
//!
//! Deux tests :
//! - `generate_fixture` (`#[ignore]`) : le générateur. Avec `UPDATE_FIXTURE=1`, il écrit
//!   `tests/fixtures/penelope-<version du workspace>.db` ; sans la variable, il travaille
//!   dans un répertoire temporaire et ne touche pas au dépôt.
//! - `a_real_0_17_database_migrates_and_reads_back` : le filet, sur une copie de chaque
//!   fixture présente.
//!
//! Le semis passe par SQL direct pour les tables métier, et par les primitives du noyau
//! pour ce qui a une signature : la chaîne d'événements (`EventLog`) et le ledger
//! d'effets (`EffectLedger`). `tests/fixtures/README.md` dit quand régénérer, et
//! pourquoi pas à chaque migration.

use penelope_kernel::clock::{Clock, SharedClock, TestClock};
use penelope_kernel::effects::{EffectKind, EffectLedger, EffectSpec, Planned};
use penelope_kernel::event::{EventDraft, EventLog};
use penelope_store::rusqlite::{self, Connection, Transaction, params};
use penelope_store::{MIGRATIONS, Store, applied_versions};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ---------------------------------------------------------------- données semées
//
// Les identifiants sont partagés par le générateur et par le test : la fixture se relit
// par ces valeurs. Les changer impose de régénérer la fixture.

/// 2026-09-20T08:00:00Z, l'instant où la « 0.17 » a écrit la base.
const START_MS: i64 = 1_789_891_200_000;
const SESSION_ID: &str = "s_01K5N0Q5T3B9V8X2M4R7C6A1E0";
const CHAT_ID: i64 = 123_456_789;
const MODEL_ID: &str = "openrouter:deepseek/deepseek-v4-pro";
const TURN_ID: &str = "turn_01K5N0Q5T3B9V8X2M4R7C6A1E3";
const TOOL_CALL_ID: &str = "call_fs_list_01";
const LCM_ACTIVE_NODE: &str = "lcm_01K5N0Q5T3B9V8X2M4R7C6A1E1";
const LCM_LEAF_NODE: &str = "lcm_01K5N0Q5T3B9V8X2M4R7C6A1E2";
const MEM_UID: &str = "m_01K5N0Q5T3B9V8X2M4R7C6A1E4";
const SCHEDULE_ID: &str = "sch_01K5N0Q5T3B9V8X2M4R7C6A1E5";
const OUTBOX_ID: &str = "out_01K5N0Q5T3B9V8X2M4R7C6A1E6";
const APPROVAL_ID: &str = "apr_01K5N0Q5T3B9V8X2M4R7C6A1E7";
const POLICY_ID: &str = "pol_01K5N0Q5T3B9V8X2M4R7C6A1E8";
const LLM_REQUEST_ID: &str = "llm_01K5N0Q5T3B9V8X2M4R7C6A1E9";
const SYSTEM_HASH: &str = "3f1c9a7e5b2d4068c1e3a5b7d9f0246813579bdf02468ace13579bdf02468ace";
/// Événements écrits par `seed_kernel`.
const EVENT_COUNT: u64 = 7;

/// La conversation : (rôle, contenu, `tool_call_id`, `tool_name`). Le contenu suit
/// `serialise_content` de `penelope-context` : blocs typés et appels d'outils.
const MESSAGES: &[(&str, &str, Option<&str>, Option<&str>)] = &[
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Peux-tu lister les fichiers du projet ?"}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[],"tool_calls":[{"id":"call_fs_list_01","name":"fs_list","arguments":{"path":"."}}]}"#,
        None,
        None,
    ),
    (
        "tool",
        r#"{"blocks":[{"type":"text","text":"Cargo.toml\nREADME.md\nsrc/\ntests/"}],"tool_calls":[]}"#,
        Some(TOOL_CALL_ID),
        Some("fs_list"),
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Quatre entrées : Cargo.toml, README.md, src/ et tests/."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Lance les tests."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[],"tool_calls":[{"id":"call_shell_02","name":"shell_exec","arguments":{"cmd":"cargo test"}}]}"#,
        None,
        None,
    ),
    (
        "tool",
        r#"{"blocks":[{"type":"text","text":"test result: ok. 12 passed; 0 failed"}],"tool_calls":[]}"#,
        Some("call_shell_02"),
        Some("shell_exec"),
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Les 12 tests passent."}],"tool_calls":[],"reasoning":"La sortie ne montre aucun échec."}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Merci. Rappelle-moi de relire les issues chaque matin de semaine."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"C'est planifié : à 8 h du lundi au vendredi."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "user",
        r#"{"blocks":[{"type":"text","text":"Parfait, réponses courtes à l'avenir."}],"tool_calls":[]}"#,
        None,
        None,
    ),
    (
        "assistant",
        r#"{"blocks":[{"type":"text","text":"Noté."}],"tool_calls":[]}"#,
        None,
        None,
    ),
];

// ---------------------------------------------------------------- chemins

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Le nom dit d'où vient la base : la version du workspace qui l'a écrite.
fn fixture_name() -> String {
    format!("penelope-{}.db", env!("CARGO_PKG_VERSION"))
}

/// Instant `secs` secondes après le début du semis, au format du noyau.
fn at(secs: i64) -> String {
    TestClock::new(START_MS + secs * 1000).now_rfc3339()
}

// ---------------------------------------------------------------- semis

/// Session `chat` liée à Telegram, ses messages, l'index plein texte, un tour terminé,
/// un nœud LCM actif (et la feuille qu'il remplace), un contexte figé.
fn seed_conversation(tx: &Transaction<'_>) -> penelope_store::Result<()> {
    tx.execute(
        "INSERT INTO sessions(id, kind, title, model_alias, model_id, created_at, updated_at,
            tg_chat_id, tg_topic_id, workspace, metadata, state, episode_seq, last_activity)
         VALUES(?1,'chat','Fichiers du projet','main',?2,?3,?4,?5,NULL,'~/projets/demo',
            '{\"origin\":\"telegram\"}','active',1,?4)",
        params![SESSION_ID, MODEL_ID, at(0), at(120), CHAT_ID],
    )?;
    tx.execute(
        "INSERT INTO episodes(session_id, ordinal, started_at, reason) VALUES(?1,1,?2,'new')",
        params![SESSION_ID, at(0)],
    )?;
    for (i, (role, content, call_id, tool_name)) in MESSAGES.iter().enumerate() {
        let seq = i as i64 + 1;
        tx.execute(
            "INSERT INTO messages(session_id, seq, role, content, tool_call_id, tool_name,
                tokens_est, ts, episode)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1)",
            params![
                SESSION_ID,
                seq,
                role,
                content,
                call_id,
                tool_name,
                (content.len() / 4) as i64,
                at(seq * 10)
            ],
        )?;
        let id = tx.last_insert_rowid();
        // Comme `HistoryStore::append` : le texte des blocs, pas le JSON.
        let v: serde_json::Value = serde_json::from_str(content).expect("contenu JSON");
        let searchable: String = v["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !searchable.is_empty() {
            tx.execute(
                "INSERT INTO messages_fts(content, session_id, msg_id) VALUES(?1,?2,?3)",
                params![searchable, SESSION_ID, id],
            )?;
        }
    }
    tx.execute(
        "INSERT INTO turn_queue(id, session_id, kind, payload, state, priority, enqueued_at,
            started_at, finished_at, attempts, dedup_key)
         VALUES(?1,?2,'message','{\"text\":\"Lance les tests.\"}','done',0,?3,?3,?4,1,
            'tg:123456789:1001')",
        params![TURN_ID, SESSION_ID, at(50), at(80)],
    )?;
    // Niveau 1 : la feuille des huit premiers messages, condensée ; la feuille reste,
    // remplacée par le nœud condensé, qui est le nœud actif.
    tx.execute(
        "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary, anchors,
            tokens_src, tokens_self, tokens_subtree, created_at, superseded_by)
         VALUES(?1,?2,'leaf',0,1,8,'Liste des fichiers puis tests lancés.','[]',
            900,900,900,?3,?4)",
        params![LCM_LEAF_NODE, SESSION_ID, at(90), LCM_ACTIVE_NODE],
    )?;
    tx.execute(
        "INSERT INTO lcm_nodes(id, session_id, kind, level, from_seq, to_seq, summary, anchors,
            tokens_src, tokens_self, tokens_subtree, created_at, superseded_by)
         VALUES(?1,?2,'condensed',1,1,8,
            'Le propriétaire a fait lister le projet (Cargo.toml, README.md, src/, tests/) puis lancer cargo test : 12 tests verts.',
            '[\"Cargo.toml\",\"cargo test\"]',900,80,980,?3,NULL)",
        params![LCM_ACTIVE_NODE, SESSION_ID, at(90)],
    )?;
    tx.execute(
        "INSERT INTO lcm_edges(parent_id, child_id) VALUES(?1,?2)",
        params![LCM_ACTIVE_NODE, LCM_LEAF_NODE],
    )?;
    tx.execute(
        "INSERT INTO message_context(session_id, seq, context) VALUES(?1,?2,?3)",
        params![
            SESSION_ID,
            MESSAGES.len() as i64,
            r#"{"prefix_hash":"3f1c9a7e","tools":["fs_list","shell_exec"],"memory":[]}"#
        ],
    )?;
    Ok(())
}

/// Un souvenir promu, avec sa provenance, son index, ses signaux, son drapeau, son
/// historique et le candidat dont il vient.
fn seed_memory(tx: &Transaction<'_>) -> penelope_store::Result<()> {
    tx.execute(
        "INSERT INTO mem_entries(uid, file, anchor, level, etype, slug, text, quand, importance,
            projet, confiance, statut, depuis, maj, pinned, declencheurs, content_hash, retired_at)
         VALUES(?1,'memoire.md','Préférences','coeur','preference','reponses-courtes',
            'Le propriétaire préfère des réponses courtes.',NULL,8,NULL,0.9,'active',
            '2026-09-20',?2,0,'[\"style\",\"réponse\"]','sha256:c0ffee',NULL)",
        params![MEM_UID, at(200)],
    )?;
    tx.execute(
        "INSERT INTO mem_fts(text, declencheurs, uid)
         VALUES('Le propriétaire préfère des réponses courtes.','style réponse',?1)",
        params![MEM_UID],
    )?;
    tx.execute(
        "INSERT INTO mem_provenance(uid, origin, session_kind, observed_at, supersedes_uid,
            source_ref, session_id)
         VALUES(?1,'owner','chat',?2,NULL,?3,?4)",
        params![MEM_UID, at(110), format!("{SESSION_ID}:11"), SESSION_ID],
    )?;
    tx.execute(
        "INSERT INTO mem_signals(uid, occurrences, sessions, days, recalls, useful_recalls,
            successes, contradictions, last_recall, distinct_queries, seen)
         VALUES(?1,3,2,2,0,0,1,0,NULL,'[]',4)",
        params![MEM_UID],
    )?;
    tx.execute(
        "INSERT INTO mem_flags(uid, sensible, expire) VALUES(?1,0,NULL)",
        params![MEM_UID],
    )?;
    tx.execute(
        "INSERT INTO mem_history(uid, file, op, before, after, ts, dream_run)
         VALUES(?1,'memoire.md','insert',NULL,'Le propriétaire préfère des réponses courtes.',?2,NULL)",
        params![MEM_UID, at(200)],
    )?;
    tx.execute(
        "INSERT INTO mem_candidates(id, ctype, text, quand, importance, origin, session_id,
            session_kind, observed_at, day, subject_key, target_slug, state, reject_reason,
            source_ref, from_memory, deferrals)
         VALUES('cand_01K5N0Q5T3B9V8X2M4R7C6A1F0','preference',
            'Réponses courtes à l''avenir.',NULL,8,'owner',?1,'chat',?2,'2026-09-20',
            'style:reponses','reponses-courtes','promoted',NULL,?3,0,0)",
        params![SESSION_ID, at(110), format!("{SESSION_ID}:11")],
    )?;
    Ok(())
}

/// Ce que l'exploitation laisse derrière elle : une planification, une ligne d'outbox
/// envoyée, une mise à jour Telegram traitée, une approbation décidée, une règle, une
/// ligne d'usage avec sa requête et son prompt figé, des clés de travail, une génération
/// de configuration.
fn seed_operations(tx: &Transaction<'_>) -> penelope_store::Result<()> {
    tx.execute(
        "INSERT INTO schedules(id, kind, spec, target, dedup, state, last_run, next_run,
            created_at, updated_at, last_error, runs)
         VALUES(?1,'cron','{\"cron\":\"0 8 * * 1-5\"}',?2,'{}','active',?3,
            '2026-09-21T08:00:00.000Z',?4,?3,NULL,3)",
        params![
            SCHEDULE_ID,
            format!(
                r#"{{"type":"prompt","prompt":"Résume les nouvelles issues du dépôt","origin_session":"{SESSION_ID}","origin":{{"kind":"telegram","chat_id":{CHAT_ID}}}}}"#
            ),
            at(300),
            at(100)
        ],
    )?;
    tx.execute(
        "INSERT INTO tg_outbox(id, chat_id, topic_id, method, payload, state, attempts,
            created_at, sent_at, message_id, error, not_before)
         VALUES(?1,?2,NULL,'sendMessage',?3,'sent',1,?4,?5,4242,NULL,NULL)",
        params![
            OUTBOX_ID,
            CHAT_ID,
            format!(
                r#"{{"chat_id":{CHAT_ID},"text":"Les 12 tests passent.","parse_mode":"HTML"}}"#
            ),
            at(81),
            at(82)
        ],
    )?;
    tx.execute(
        "INSERT INTO tg_updates(update_id, received_at, processed, payload)
         VALUES(1001,?1,1,?2)",
        params![
            at(49),
            format!(
                r#"{{"update_id":1001,"message":{{"message_id":4241,"chat":{{"id":{CHAT_ID},"type":"private"}},"text":"Lance les tests."}}}}"#
            )
        ],
    )?;
    tx.execute(
        "INSERT INTO approval_requests(id, kind, subject, risk, payload, choices, session_id,
            run_id, created_at, expires_at, state, decision, reason, decided_by, decided_via,
            decided_at, rule_created, reminded_at, quiet)
         VALUES(?1,'tool_call','shell_exec','write',
            '{\"tool\":\"shell_exec\",\"args\":{\"cmd\":\"cargo test\"}}',
            '[\"approve\",\"deny\",\"always\"]',?2,NULL,?3,?4,'approved','approve',NULL,
            'owner','telegram',?5,NULL,NULL,0)",
        params![APPROVAL_ID, SESSION_ID, at(60), at(3660), at(65)],
    )?;
    tx.execute(
        "INSERT INTO policies(id, scope, tool, server, arg_match, decision, window, window_ref,
            created_at, created_by, revoked_at, hits)
         VALUES(?1,'tool','shell_exec',NULL,'{\"family\":\"cargo test\"}','auto','always',NULL,
            ?2,'owner',NULL,2)",
        params![POLICY_ID, at(65)],
    )?;
    tx.execute(
        "INSERT INTO usage(ts, day, session_id, run_id, model, provider, role, prompt,
            completion, cached, reasoning, cost_usd, estimated, maybe_dup, turn_id,
            generation_id, upstream, finish, cache_write, msg_count, request_hash,
            system_hash, tools_hash, miss_cause)
         VALUES(?1,'2026-09-20',?2,NULL,?3,'openrouter','chat_default',1200,180,900,0,
            0.0123,0,0,?4,'gen-0d3f',?5,'stop',0,12,'req-7a1b',?6,'tools-9c2d',NULL)",
        params![
            at(79),
            SESSION_ID,
            MODEL_ID,
            TURN_ID,
            "DeepSeek",
            SYSTEM_HASH
        ],
    )?;
    tx.execute(
        "INSERT INTO llm_requests(id, session_id, run_id, model, provider, state, body_hash,
            created_at, updated_at, error, maybe_billed, system_hash, tools_hash, request_hash)
         VALUES(?1,?2,NULL,?3,'openrouter','completed','body-5e6f',?4,?5,NULL,0,?6,
            'tools-9c2d','req-7a1b')",
        params![
            LLM_REQUEST_ID,
            SESSION_ID,
            MODEL_ID,
            at(70),
            at(79),
            SYSTEM_HASH
        ],
    )?;
    tx.execute(
        "INSERT INTO prompt_snapshots(hash, rendered, tiers, first_seen_at, last_seen_at, uses)
         VALUES(?1,'Tu es Pénélope, agent personnel du propriétaire.',
            '{\"t0\":1,\"t1\":1,\"t2\":0}',?2,?3,1)",
        params![SYSTEM_HASH, at(70), at(79)],
    )?;
    for (k, v) in [
        ("tg.offset", "1002"),
        ("tg.bot_username", "penelope_demo_bot"),
        ("retention.last", "2026-09-20T03:00:00.000Z"),
    ] {
        tx.execute(
            "INSERT INTO kv(k, v, ts) VALUES(?1,?2,?3)",
            params![k, v, at(0)],
        )?;
    }
    tx.execute(
        "INSERT INTO config_generations(gen, ts, source, changed, snapshot)
         VALUES(1,?1,'boot','[]',?2)",
        params![
            at(0),
            format!(r#"{{"owner":{{"telegram_user_id":{CHAT_ID}}}}}"#)
        ],
    )?;
    tx.execute(
        "INSERT INTO subsystem_apply_results(gen, subsystem, result, reason, ts)
         VALUES(1,'telegram','applied_live',NULL,?1)",
        params![at(0)],
    )?;
    Ok(())
}

/// La chaîne d'événements et le ledger, par les primitives du noyau : le hachage et
/// l'idempotence sont ceux de la 0.17, pas une imitation.
async fn seed_kernel(store: &Store, clock: &Arc<TestClock>) {
    let shared: SharedClock = clock.clone();
    let log = EventLog::new(store.clone(), shared.clone());
    let drafts = [
        EventDraft::new(
            "message.received",
            json!({"chat_id": CHAT_ID, "chars": 16, "turn_id": TURN_ID}),
        )
        .session(SESSION_ID),
        EventDraft::new(
            "turn.started",
            json!({"turn_id": TURN_ID, "model": MODEL_ID}),
        )
        .session(SESSION_ID),
        EventDraft::new(
            "tool.result",
            json!({"tool": "shell_exec", "ok": true, "ms": 4210}),
        )
        .session(SESSION_ID),
        EventDraft::new(
            "approval.decided",
            json!({"id": APPROVAL_ID, "decision": "approve", "via": "telegram"}),
        )
        .session(SESSION_ID),
        EventDraft::new(
            "turn.finished",
            json!({"turn_id": TURN_ID, "cost_usd": 0.0123, "tool_calls": 2}),
        )
        .session(SESSION_ID),
        EventDraft::new("schedule.fired", json!({"schedule_id": SCHEDULE_ID}))
            .run("run_01K5N0Q5T3B9V8X2M4R7C6A1F1"),
        EventDraft::new("store.backup", json!({"path": "backups/2026-09-20.db.age"})),
    ];
    assert_eq!(
        drafts.len() as u64,
        EVENT_COUNT,
        "EVENT_COUNT suit la liste"
    );
    for (i, d) in drafts.into_iter().enumerate() {
        clock.set_ms(START_MS + (50 + i as i64 * 5) * 1000);
        log.append(d).await.expect("événement semé");
    }

    let ledger = EffectLedger::new(store.clone(), shared);
    clock.set_ms(START_MS + 66 * 1000);
    let planned = ledger
        .plan(
            EffectSpec::new(
                EffectKind::Shell,
                "shell_exec",
                json!({"cmd": "cargo test"}),
            )
            .session(SESSION_ID),
        )
        .await
        .expect("effet planifié");
    let Planned::Fresh(id) = planned else {
        panic!("effet neuf attendu : {planned:?}");
    };
    ledger.dispatching(&id).await.expect("effet en cours");
    clock.set_ms(START_MS + 78 * 1000);
    ledger
        .complete(
            &id,
            json!({"exit_code": 0, "stdout": "test result: ok. 12 passed; 0 failed"}),
        )
        .await
        .expect("effet terminé");
}

/// Un seul fichier, sans journal WAL, en pages de 1 Kio : une base neuve compte plus de
/// cent cinquante objets (tables, index, tables d'ombre FTS5) et chacun occupe au moins
/// une page ; en pages de 4 Kio, elle pèserait plus de 600 Ko presque vides. `Store::open`
/// lit toute taille de page et remet la base en WAL.
fn compact(path: &Path) {
    let c = Connection::open(path).expect("base produite");
    c.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA page_size=1024; VACUUM;")
        .expect("compaction");
    let mode: String = c
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "delete", "la fixture ne traîne pas de journal WAL");
    let page: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap();
    assert_eq!(page, 1024);
    let verdict: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(verdict, "ok");
}

/// Produit la fixture dans `dir` et rend son chemin.
async fn generate(dir: &Path) -> PathBuf {
    let work = dir.join("penelope.db");
    let store = Store::open(&work).expect("base neuve");
    let clock = Arc::new(TestClock::new(START_MS));
    store
        .write_blocking(|tx| {
            seed_conversation(tx)?;
            seed_memory(tx)?;
            seed_operations(tx)?;
            Ok(())
        })
        .expect("semis SQL");
    seed_kernel(&store, &clock).await;

    // `VACUUM INTO` par la sauvegarde de la 0.17 : un instantané cohérent, WAL compris.
    let out = dir.join(fixture_name());
    store.backup_to(&out).expect("instantané");
    store.close();
    drop(store);
    compact(&out);
    out
}

// ---------------------------------------------------------------- vérifications

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql} : {e}"))
}

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{sql} : {e}"))
}

/// Chaque donnée semée se relit : un compte et une valeur par table.
fn check_seeded_rows(c: &Connection) {
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

/// Réouvre la base produite et vérifie qu'elle porte toutes les migrations et toutes les
/// données semées.
fn check_generated(path: &Path) {
    let store = Store::open(path).expect("la fixture produite s'ouvre");
    let applied = store.read_blocking(applied_versions).unwrap();
    let expected: Vec<String> = MIGRATIONS.iter().map(|m| m.version.to_string()).collect();
    assert_eq!(applied, expected);
    store
        .read_blocking(|c| {
            check_seeded_rows(c);
            Ok(())
        })
        .unwrap();
    store.close();
}

// ---------------------------------------------------------------- tests

/// Générateur de la fixture. Ignoré : ne tourne que sur demande.
///
/// ```bash
/// UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored
/// ```
///
/// Sans `UPDATE_FIXTURE=1`, la base est produite et vérifiée dans un répertoire
/// temporaire, sans rien écrire dans le dépôt.
#[tokio::test]
#[ignore = "générateur : UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored"]
async fn generate_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let produced = generate(dir.path()).await;
    let size = std::fs::metadata(&produced).unwrap().len();
    // Le générateur relit ce qu'il vient d'écrire : la copie vérifiée est celle de
    // travail, la fixture reste intacte jusqu'à la copie finale.
    let checked = dir.path().join("check.db");
    std::fs::copy(&produced, &checked).unwrap();
    check_generated(&checked);

    if std::env::var("UPDATE_FIXTURE").as_deref() == Ok("1") {
        std::fs::create_dir_all(fixtures_dir()).unwrap();
        let dest = fixtures_dir().join(fixture_name());
        std::fs::copy(&produced, &dest).unwrap();
        eprintln!("fixture écrite : {} ({size} octets)", dest.display());
    } else {
        eprintln!(
            "fixture produite dans {} ({size} octets), non copiée dans le dépôt : \
             UPDATE_FIXTURE=1 pour l'écrire dans tests/fixtures/",
            produced.display()
        );
    }
}
