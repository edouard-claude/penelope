//! Migrations versionnées (§18).
//!
//! Règles :
//! - une migration est **immuable** une fois livrée ; toute correction est une nouvelle
//!   migration ;
//! - la montée depuis une base vide et depuis chaque version antérieure est testée ;
//! - chaque migration s'applique dans une transaction unique.

use crate::{Result, StoreError};
use rusqlite::{Connection, TransactionBehavior};

pub struct Migration {
    pub version: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: "0001_init",
        sql: SQL_0001,
    },
    Migration {
        version: "0002_indexes",
        sql: SQL_0002,
    },
    Migration {
        version: "0003_event_purges",
        sql: SQL_0003,
    },
    Migration {
        version: "0004_usage_attribution",
        sql: SQL_0004,
    },
    Migration {
        version: "0005_single_chat_binding",
        sql: SQL_0005,
    },
    Migration {
        version: "0006_prompt_cache",
        sql: SQL_0006,
    },
    Migration {
        version: "0007_retry_origin_rejections",
        sql: SQL_0007,
    },
    Migration {
        version: "0008_memory_flags",
        sql: SQL_0008,
    },
    Migration {
        version: "0009_schedule_origin_session",
        sql: SQL_0009,
    },
    Migration {
        version: "0010_retention",
        sql: SQL_0010,
    },
    Migration {
        version: "0011_events_seq_unique",
        sql: SQL_0011,
    },
    Migration {
        version: "0012_candidate_deferrals",
        sql: SQL_0012,
    },
    Migration {
        version: "0013_memory_seen",
        sql: SQL_0013,
    },
    Migration {
        version: "0014_mcp_tool_fingerprint",
        sql: SQL_0014,
    },
    Migration {
        version: "0015_memory_usage_reset",
        sql: SQL_0015,
    },
    Migration {
        version: "0016_turn_merge",
        sql: SQL_0016,
    },
    Migration {
        version: "0017_clone_source",
        sql: SQL_0017,
    },
    Migration {
        version: "0018_prompt_snapshots",
        sql: SQL_0018,
    },
    Migration {
        version: "0019_tool_jobs",
        sql: SQL_0019,
    },
    Migration {
        version: "0020_history_journal",
        sql: SQL_0020,
    },
    Migration {
        version: "0021_history_seal",
        sql: SQL_0021,
    },
];

pub fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations(
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );",
    )?;

    let applied = applied_versions(conn)?;
    for m in MIGRATIONS {
        if applied.iter().any(|v| v == m.version) {
            continue;
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(m.sql)
            .map_err(|source| StoreError::Migration {
                version: m.version,
                source,
            })?;
        tx.execute(
            "INSERT INTO schema_migrations(version) VALUES(?1)",
            [m.version],
        )?;
        tx.commit()?;
        tracing::info!(version = m.version, "migration appliquée");
    }
    Ok(())
}

pub fn applied_versions(conn: &Connection) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let rows = st.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn current_version(conn: &Connection) -> Result<Option<String>> {
    Ok(applied_versions(conn)?.last().cloned())
}

const SQL_0002: &str = r#"
CREATE INDEX IF NOT EXISTS mem_entries_projet ON mem_entries(projet);
CREATE INDEX IF NOT EXISTS mem_candidates_day ON mem_candidates(day);
CREATE INDEX IF NOT EXISTS intents_state ON intents(etat);
CREATE INDEX IF NOT EXISTS artifacts_expiry ON artifacts(expires_at);
CREATE INDEX IF NOT EXISTS messages_episode ON messages(session_id, episode);
CREATE INDEX IF NOT EXISTS usage_model ON usage(model, day);
"#;

// Purge RGPD (§4.1) : le contenu de l'événement est effacé, mais le hash d'origine est
// conservé pour que `penelope audit verify` puisse encore prouver la continuité.
const SQL_0003: &str = r#"
CREATE TABLE event_purges(
  event_id      INTEGER PRIMARY KEY,
  purged_at     TEXT NOT NULL,
  original_hash TEXT NOT NULL,
  reason        TEXT NOT NULL DEFAULT ''
);
"#;

/// Coûts attribuables : à quel tour (requête du propriétaire) appartient un appel, quelle
/// génération OpenRouter le porte, quel provider amont l'a servi.
const SQL_0004: &str = r#"
ALTER TABLE usage ADD COLUMN turn_id TEXT;
ALTER TABLE usage ADD COLUMN generation_id TEXT;
ALTER TABLE usage ADD COLUMN upstream TEXT;
ALTER TABLE usage ADD COLUMN finish TEXT;
ALTER TABLE usage ADD COLUMN cache_write INTEGER NOT NULL DEFAULT 0;
CREATE INDEX usage_turn ON usage(turn_id);
"#;

/// Une seule session liée par chat (et sujet) Telegram (issue #10) : la plus récemment
/// active garde la liaison, les autres sont détachées.
const SQL_0005: &str = r#"
UPDATE sessions SET tg_chat_id = NULL, tg_topic_id = NULL
WHERE tg_chat_id IS NOT NULL AND id NOT IN (
  SELECT id FROM (
    SELECT id, ROW_NUMBER() OVER (
      PARTITION BY tg_chat_id, COALESCE(tg_topic_id, -1)
      ORDER BY CASE state WHEN 'active' THEN 0 ELSE 1 END, updated_at DESC
    ) AS rn
    FROM sessions WHERE tg_chat_id IS NOT NULL AND state = 'active'
  ) WHERE rn = 1
);
"#;

/// Cache de prompt (issue #17) : contexte volatil figé avec son message, empreinte de
/// chaque requête et cause probable d'un raté de cache.
const SQL_0006: &str = r#"
CREATE TABLE message_context(
  session_id TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  context    TEXT NOT NULL,
  PRIMARY KEY(session_id, seq)
);
ALTER TABLE usage ADD COLUMN msg_count INTEGER;
ALTER TABLE usage ADD COLUMN request_hash TEXT;
ALTER TABLE usage ADD COLUMN system_hash TEXT;
ALTER TABLE usage ADD COLUMN tools_hash TEXT;
ALTER TABLE usage ADD COLUMN miss_cause TEXT;
CREATE INDEX usage_session_ts ON usage(session_id, ts);
"#;

/// Règles notées par l'agent et rejetées pour leur seule origine (issue #24) : remises à
/// consolider, elles seront demandées au propriétaire.
const SQL_0007: &str = r#"
UPDATE mem_candidates SET state = 'new', reject_reason = NULL
WHERE state = 'rejected' AND reject_reason IN (
  'une préférence doit venir du propriétaire',
  'une correction doit venir du propriétaire',
  'une décision doit être confirmée par le propriétaire'
);
"#;

/// Entrées sensibles ou datées (issue #25) : jamais injectées d'office, toujours
/// trouvables par une recherche explicite.
const SQL_0008: &str = r#"
CREATE TABLE mem_flags(
  uid      TEXT PRIMARY KEY,
  sensible INTEGER NOT NULL DEFAULT 0,
  expire   TEXT
);
"#;

/// Une planification ne dépend plus de sa session d'origine (issue #39) : `session_id`
/// devient la référence informative `origin_session`, chaque exécution ouvre sa session.
const SQL_0009: &str = r#"
UPDATE schedules
SET target = json_remove(
  json_set(target, '$.origin_session', json_extract(target, '$.session_id')),
  '$.session_id'
)
WHERE json_valid(target) AND json_extract(target, '$.session_id') IS NOT NULL;
"#;

/// Rétention et purge (issue #46) : `kv` date ses clés pour qu'un balayage puisse retirer
/// les éphémères, et les tables qui grossissent sans borne reçoivent l'index de leur
/// colonne de date.
const SQL_0010: &str = r#"
ALTER TABLE kv ADD COLUMN ts TEXT;
UPDATE kv SET ts = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE ts IS NULL;
CREATE INDEX kv_ts ON kv(ts);
CREATE INDEX turn_queue_finished ON turn_queue(state, finished_at);
CREATE INDEX tg_updates_received ON tg_updates(processed, received_at);
CREATE INDEX llm_requests_updated ON llm_requests(updated_at);
CREATE INDEX mem_history_ts ON mem_history(ts);
"#;

mod init;
mod since_0011;
use init::SQL_0001;
use since_0011::{
    SQL_0011, SQL_0012, SQL_0013, SQL_0014, SQL_0015, SQL_0016, SQL_0017, SQL_0018, SQL_0019,
    SQL_0021,
};

/// Double écriture de l'historique (épopée #208, T5, `design/v1/source-de-verite.md`
/// §2.4, §4.1, §4.5) : une ligne de `messages` ou de `lcm_nodes` cite l'événement `conv.*`
/// qui la porte ; `sealed` marque le préfixe V0 scellé par `conv.import` (posé au boot,
/// pas ici : le scellement a besoin du journal). L'index partiel retrouve un message de
/// la file par sa clé d'idempotence dans le journal (§2.7).
const SQL_0020: &str = r#"
ALTER TABLE messages ADD COLUMN event_id INTEGER;
ALTER TABLE messages ADD COLUMN sealed INTEGER NOT NULL DEFAULT 0;
ALTER TABLE lcm_nodes ADD COLUMN event_id INTEGER;
CREATE INDEX messages_event ON messages(event_id);
CREATE INDEX events_turn_message ON events(session_id, json_extract(payload, '$.turn_message_id'))
  WHERE kind = 'conv.user';
"#;

#[cfg(test)]
mod tests;
