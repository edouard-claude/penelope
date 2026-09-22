//! Migrations versionnées (§18).
//!
//! Règles :
//! - une migration est **immuable** une fois livrée ; toute correction est une nouvelle
//!   migration ;
//! - la montée depuis une base vide et depuis chaque version antérieure est testée ;
//! - chaque migration s'applique dans une transaction unique.

use crate::{Result, StoreError};
use rusqlite::Connection;

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
        let tx = conn.transaction()?;
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

const SQL_0001: &str = r#"
-- ============================================================ magasin générique
CREATE TABLE kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);

-- ============================================================ §4.1 noyau
CREATE TABLE events(
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT,
  run_id     TEXT,
  seq        INTEGER NOT NULL,
  ts         TEXT NOT NULL,
  kind       TEXT NOT NULL,
  payload    TEXT NOT NULL,
  hash       TEXT NOT NULL,
  prev_hash  TEXT NOT NULL
);
CREATE INDEX events_session ON events(session_id, seq);
CREATE INDEX events_kind ON events(kind, id);
CREATE INDEX events_run ON events(run_id, id);

-- Projections reconstruisibles (penelope store rebuild).
CREATE TABLE projections_session(
  session_id TEXT PRIMARY KEY,
  state      TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_event INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE projections_workflow(
  run_id     TEXT PRIMARY KEY,
  state      TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_event INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE projections_approval(
  approval_id TEXT PRIMARY KEY,
  state       TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  last_event  INTEGER NOT NULL DEFAULT 0
);

-- ============================================================ §5 sessions et contexte
CREATE TABLE sessions(
  id            TEXT PRIMARY KEY,
  kind          TEXT NOT NULL,              -- chat | workflow_run | sub_agent | scheduled
  title         TEXT,
  model_alias   TEXT,
  model_id      TEXT,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL,
  closed_at     TEXT,
  parent_id     TEXT,
  tg_chat_id    INTEGER,
  tg_topic_id   INTEGER,
  workspace     TEXT,
  metadata      TEXT NOT NULL DEFAULT '{}', -- session_metadata (§5.1)
  usage_anchor  TEXT,                       -- §5.3 ancre de tokens persistée
  budget_usd    REAL,
  spent_usd     REAL NOT NULL DEFAULT 0,
  state         TEXT NOT NULL DEFAULT 'active',
  episode_seq   INTEGER NOT NULL DEFAULT 0,
  last_activity TEXT
);
CREATE INDEX sessions_kind ON sessions(kind, state);

CREATE TABLE messages(
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id  TEXT NOT NULL,
  seq         INTEGER NOT NULL,
  role        TEXT NOT NULL,               -- system | user | assistant | tool
  content     TEXT NOT NULL,               -- JSON: blocs typés
  tool_call_id TEXT,
  tool_name   TEXT,
  tokens_est  INTEGER NOT NULL DEFAULT 0,
  ts          TEXT NOT NULL,
  episode     INTEGER NOT NULL DEFAULT 0,
  eager       INTEGER NOT NULL DEFAULT 0,  -- résultat volatil, candidat niveau 0
  artifact_id TEXT,                        -- corps externalisé (§5.4 niveau 1)
  compacted   INTEGER NOT NULL DEFAULT 0,
  UNIQUE(session_id, seq)
);
CREATE INDEX messages_session ON messages(session_id, seq);

CREATE VIRTUAL TABLE messages_fts USING fts5(
  content, session_id UNINDEXED, msg_id UNINDEXED, tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE lcm_nodes(
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL,
  kind        TEXT NOT NULL,               -- leaf | condensed
  level       INTEGER NOT NULL DEFAULT 0,
  from_seq    INTEGER,
  to_seq      INTEGER,
  summary     TEXT NOT NULL,
  anchors     TEXT NOT NULL DEFAULT '[]',  -- index d'ancres mécanique
  tokens_src  INTEGER NOT NULL DEFAULT 0,
  tokens_self INTEGER NOT NULL DEFAULT 0,
  tokens_subtree INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  superseded_by TEXT
);
CREATE INDEX lcm_nodes_session ON lcm_nodes(session_id, level);

CREATE TABLE lcm_edges(
  parent_id TEXT NOT NULL,
  child_id  TEXT NOT NULL,
  PRIMARY KEY(parent_id, child_id)
);

CREATE TABLE artifacts(
  id          TEXT PRIMARY KEY,
  session_id  TEXT,
  run_id      TEXT,
  kind        TEXT NOT NULL,               -- json | code | csv | log | html | text | image | binary
  media_type  TEXT,
  filename    TEXT,
  bytes       INTEGER NOT NULL DEFAULT 0,
  path        TEXT,                        -- relatif à {data}/artifacts
  inline      BLOB,
  head        TEXT,                        -- aperçu tête
  tail        TEXT,                        -- aperçu queue
  summary     TEXT,
  sha256      TEXT,
  created_at  TEXT NOT NULL,
  expires_at  TEXT
);
CREATE INDEX artifacts_session ON artifacts(session_id);

CREATE TABLE episodes(
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  ordinal    INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  ended_at   TEXT,
  reason     TEXT,                          -- task_done | idle | topic_shift | new
  ingested   INTEGER NOT NULL DEFAULT 0,
  UNIQUE(session_id, ordinal)
);

-- ============================================================ §3.3/§4.2 exécution
CREATE TABLE turn_queue(
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL,
  kind         TEXT NOT NULL,              -- message | trigger | resume | nudge
  payload      TEXT NOT NULL,
  state        TEXT NOT NULL,              -- pending | leased | done | failed | cancelled
  priority     INTEGER NOT NULL DEFAULT 0,
  enqueued_at  TEXT NOT NULL,
  started_at   TEXT,
  finished_at  TEXT,
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  dedup_key    TEXT
);
CREATE INDEX turn_queue_ready ON turn_queue(state, priority DESC, enqueued_at);
CREATE UNIQUE INDEX turn_queue_dedup ON turn_queue(dedup_key) WHERE dedup_key IS NOT NULL;

CREATE TABLE leases(
  resource    TEXT PRIMARY KEY,            -- turn:<id> | session:<id> | cron:<id>
  holder      TEXT NOT NULL,
  acquired_at TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  heartbeat_at TEXT NOT NULL
);
CREATE INDEX leases_expiry ON leases(expires_at);

CREATE TABLE effects(
  id        TEXT PRIMARY KEY,
  run_id    TEXT,
  session_id TEXT,
  step_id   TEXT,
  idem_key  TEXT NOT NULL UNIQUE,
  kind      TEXT NOT NULL,                 -- tool | mcp | shell | telegram | fs | git | http
  tool      TEXT,
  request   TEXT NOT NULL,
  state     TEXT NOT NULL,                 -- planned|dispatching|completed|failed|unknown
  result    TEXT,
  error     TEXT,
  attempts  INTEGER NOT NULL DEFAULT 0,
  idempotent INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX effects_state ON effects(state);
CREATE INDEX effects_run ON effects(run_id, step_id);

CREATE TABLE llm_requests(
  id          TEXT PRIMARY KEY,
  session_id  TEXT,
  run_id      TEXT,
  model       TEXT NOT NULL,
  provider    TEXT NOT NULL,
  state       TEXT NOT NULL,               -- planned|dispatching|response_started|completed|failed|send_unknown
  body_hash   TEXT NOT NULL,
  request     TEXT,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  error       TEXT,
  maybe_billed INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX llm_requests_state ON llm_requests(state);

CREATE TABLE usage(
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          TEXT NOT NULL,
  day         TEXT NOT NULL,
  session_id  TEXT,
  run_id      TEXT,
  model       TEXT NOT NULL,
  provider    TEXT NOT NULL,
  role        TEXT,
  prompt      INTEGER NOT NULL DEFAULT 0,
  completion  INTEGER NOT NULL DEFAULT 0,
  cached      INTEGER NOT NULL DEFAULT 0,
  reasoning   INTEGER NOT NULL DEFAULT 0,
  cost_usd    REAL NOT NULL DEFAULT 0,
  estimated   INTEGER NOT NULL DEFAULT 0,
  maybe_dup   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX usage_day ON usage(day);
CREATE INDEX usage_session ON usage(session_id);

-- ============================================================ §6 mémoire
CREATE TABLE mem_entries(
  uid        TEXT PRIMARY KEY,
  file       TEXT NOT NULL,
  anchor     TEXT,                         -- section du fichier
  level      TEXT NOT NULL,                -- instruction|profil|coeur|projet|cure|episodic|revue
  etype      TEXT NOT NULL,                -- fait|preference|pratique|exception|ecart|decision|entite|note
  slug       TEXT,
  text       TEXT NOT NULL,
  quand      TEXT,                         -- prédicats clé=valeur (§6.4)
  importance INTEGER,
  projet     TEXT,
  confiance  REAL,
  statut     TEXT NOT NULL DEFAULT 'active',
  depuis     TEXT,
  maj        TEXT NOT NULL,
  pinned     INTEGER NOT NULL DEFAULT 0,
  declencheurs TEXT NOT NULL DEFAULT '[]',
  content_hash TEXT NOT NULL,
  retired_at TEXT
);
CREATE INDEX mem_entries_level ON mem_entries(level, statut);
CREATE INDEX mem_entries_slug ON mem_entries(slug);
CREATE INDEX mem_entries_file ON mem_entries(file);

CREATE VIRTUAL TABLE mem_fts USING fts5(
  text, declencheurs, uid UNINDEXED, tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE mem_vec(
  uid       TEXT PRIMARY KEY,
  dim       INTEGER NOT NULL,
  model     TEXT NOT NULL,
  embedding BLOB NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE mem_links(
  from_uid TEXT NOT NULL,
  to_slug  TEXT NOT NULL,
  PRIMARY KEY(from_uid, to_slug)
);

-- Non reconstruisible depuis les fichiers : sauvegardé explicitement (§6.2).
CREATE TABLE mem_provenance(
  uid          TEXT PRIMARY KEY,
  origin       TEXT NOT NULL,              -- owner | agent | untrusted | system
  session_kind TEXT NOT NULL,
  observed_at  TEXT NOT NULL,
  supersedes_uid TEXT,
  source_ref   TEXT,
  session_id   TEXT
);
CREATE INDEX mem_provenance_session ON mem_provenance(session_id);

CREATE TABLE mem_signals(
  uid            TEXT PRIMARY KEY,
  occurrences    INTEGER NOT NULL DEFAULT 0,
  sessions       INTEGER NOT NULL DEFAULT 0,
  days           INTEGER NOT NULL DEFAULT 0,
  recalls        INTEGER NOT NULL DEFAULT 0,
  useful_recalls INTEGER NOT NULL DEFAULT 0,
  successes      INTEGER NOT NULL DEFAULT 0,
  contradictions INTEGER NOT NULL DEFAULT 0,
  last_recall    TEXT,
  distinct_queries TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE mem_candidates(
  id          TEXT PRIMARY KEY,
  ctype       TEXT NOT NULL,               -- fait|preference|correction|ecart|decision|procedure_candidate
  text        TEXT NOT NULL,
  quand       TEXT,
  importance  INTEGER NOT NULL DEFAULT 5,
  origin      TEXT NOT NULL,
  session_id  TEXT,
  session_kind TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  day         TEXT NOT NULL,
  subject_key TEXT,
  target_slug TEXT,
  state       TEXT NOT NULL DEFAULT 'new', -- new|grouped|promoted|rejected|deferred|expired
  reject_reason TEXT,
  source_ref  TEXT,
  from_memory INTEGER NOT NULL DEFAULT 0   -- anti-boucle (§6.5)
);
CREATE INDEX mem_candidates_state ON mem_candidates(state, ctype);
CREATE INDEX mem_candidates_subject ON mem_candidates(subject_key);

CREATE TABLE mem_history(
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  uid       TEXT,
  file      TEXT NOT NULL,
  op        TEXT NOT NULL,
  before    TEXT,
  after     TEXT,
  ts        TEXT NOT NULL,
  dream_run TEXT
);
CREATE INDEX mem_history_uid ON mem_history(uid);

CREATE TABLE dream_runs(
  id         TEXT PRIMARY KEY,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  phase      TEXT NOT NULL,                -- light|rem|deep|done|failed
  stats      TEXT NOT NULL DEFAULT '{}',
  error      TEXT,
  since      TEXT
);

CREATE TABLE intents(
  id          TEXT PRIMARY KEY,
  texte       TEXT NOT NULL,
  declencheurs TEXT NOT NULL DEFAULT '[]',
  portee      TEXT,
  created_at  TEXT NOT NULL,
  expire_at   TEXT,
  budget_tirs INTEGER NOT NULL DEFAULT 3,
  tirs        INTEGER NOT NULL DEFAULT 0,
  cooldown_ms INTEGER NOT NULL DEFAULT 86400000,
  last_fired  TEXT,
  etat        TEXT NOT NULL DEFAULT 'armee' -- armee|tiree|terminee|annulee|expiree
);
CREATE TABLE intent_vec(
  id        TEXT PRIMARY KEY,
  dim       INTEGER NOT NULL,
  embedding BLOB NOT NULL
);

CREATE TABLE embeddings_cache(
  content_hash TEXT NOT NULL,
  model        TEXT NOT NULL,
  dim          INTEGER NOT NULL,
  embedding    BLOB NOT NULL,
  created_at   TEXT NOT NULL,
  PRIMARY KEY(content_hash, model)
);

-- ============================================================ §7/§8 skills et MCP
CREATE TABLE skills(
  name        TEXT PRIMARY KEY,
  scope       TEXT NOT NULL,               -- bundled | user | workspace
  path        TEXT NOT NULL,
  version     TEXT,
  description TEXT NOT NULL DEFAULT '',
  allowed_tools TEXT NOT NULL DEFAULT '[]',
  activation  TEXT,
  sub_agent   INTEGER NOT NULL DEFAULT 0,
  declencheurs TEXT NOT NULL DEFAULT '[]',
  body_hash   TEXT NOT NULL,
  valid       INTEGER NOT NULL DEFAULT 1,
  error       TEXT,
  updated_at  TEXT NOT NULL
);

CREATE TABLE mcp_servers(
  name        TEXT PRIMARY KEY,
  transport   TEXT NOT NULL,               -- stdio | http | sse
  config      TEXT NOT NULL,
  state       TEXT NOT NULL,               -- configured|connecting|ready|degraded|failed|disabled|auth_required
  protocol    TEXT,
  server_info TEXT,
  capabilities TEXT,
  extensions  TEXT,
  tool_count  INTEGER NOT NULL DEFAULT 0,
  lazy_start  INTEGER NOT NULL DEFAULT 1,
  eager_schemas INTEGER NOT NULL DEFAULT 0,
  failures    INTEGER NOT NULL DEFAULT 0,
  last_error  TEXT,
  last_ok     TEXT,
  updated_at  TEXT NOT NULL,
  p50_ms      REAL NOT NULL DEFAULT 0,
  p95_ms      REAL NOT NULL DEFAULT 0,
  calls       INTEGER NOT NULL DEFAULT 0,
  errors      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE mcp_tools(
  qualified   TEXT PRIMARY KEY,            -- mcp__<server>__<tool>
  server      TEXT NOT NULL,
  name        TEXT NOT NULL,
  title       TEXT,
  description TEXT NOT NULL DEFAULT '',
  input_schema TEXT NOT NULL DEFAULT '{}',
  output_schema TEXT,
  annotations TEXT NOT NULL DEFAULT '{}',
  risk        TEXT NOT NULL DEFAULT 'unknown',
  icons       TEXT,
  schema_bytes INTEGER NOT NULL DEFAULT 0,
  generation  INTEGER NOT NULL DEFAULT 0,
  updated_at  TEXT NOT NULL,
  UNIQUE(server, name)
);
CREATE INDEX mcp_tools_server ON mcp_tools(server);

CREATE VIRTUAL TABLE mcp_tools_fts USING fts5(
  name, title, description, server, qualified UNINDEXED,
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TABLE mcp_tools_vec(
  qualified TEXT PRIMARY KEY,
  dim       INTEGER NOT NULL,
  embedding BLOB NOT NULL
);

CREATE TABLE mcp_tasks(
  id         TEXT PRIMARY KEY,
  server     TEXT NOT NULL,
  task_ref   TEXT NOT NULL,
  session_id TEXT,
  run_id     TEXT,
  request    TEXT NOT NULL,
  state      TEXT NOT NULL,                -- working|input_required|completed|failed|cancelled
  result     TEXT,
  poll_at    TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX mcp_tasks_state ON mcp_tasks(state);

CREATE TABLE mcp_resources_cache(
  server     TEXT NOT NULL,
  uri        TEXT NOT NULL,
  payload    TEXT NOT NULL,
  expires_at TEXT,
  PRIMARY KEY(server, uri)
);

CREATE TABLE oauth_clients(
  issuer        TEXT PRIMARY KEY,
  client_id     TEXT NOT NULL,
  secret_ref    TEXT,
  registration  TEXT,
  metadata      TEXT,
  scopes        TEXT NOT NULL DEFAULT '',
  created_at    TEXT NOT NULL
);

CREATE TABLE oauth_state(
  state        TEXT PRIMARY KEY,
  server       TEXT NOT NULL,
  issuer       TEXT NOT NULL,
  verifier     TEXT NOT NULL,
  resource     TEXT,
  scopes       TEXT,
  redirect_uri TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  consumed     INTEGER NOT NULL DEFAULT 0
);

-- ============================================================ §9 HITL
CREATE TABLE approval_requests(
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,
  subject     TEXT NOT NULL,
  risk        TEXT NOT NULL,
  payload     TEXT NOT NULL,
  choices     TEXT NOT NULL DEFAULT '[]',
  session_id  TEXT,
  run_id      TEXT,
  created_at  TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  state       TEXT NOT NULL,               -- pending|approved|denied|expired|cancelled
  decision    TEXT,
  reason      TEXT,
  decided_by  TEXT,
  decided_via TEXT,
  decided_at  TEXT,
  rule_created TEXT,
  reminded_at TEXT,
  quiet       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX approval_state ON approval_requests(state, expires_at);

CREATE TABLE policies(
  id          TEXT PRIMARY KEY,
  scope       TEXT NOT NULL,               -- global|server|tool
  tool        TEXT,
  server      TEXT,
  arg_match   TEXT,
  decision    TEXT NOT NULL,               -- auto|ask|ask_twice|deny
  window      TEXT NOT NULL DEFAULT 'always', -- once|run|session|always
  window_ref  TEXT,
  created_at  TEXT NOT NULL,
  created_by  TEXT NOT NULL DEFAULT 'owner',
  revoked_at  TEXT,
  hits        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX policies_tool ON policies(tool) WHERE revoked_at IS NULL;

-- ============================================================ §12 workflows
CREATE TABLE workflows(
  id         TEXT PRIMARY KEY,
  scope      TEXT NOT NULL,                -- bundled|user|workspace
  path       TEXT NOT NULL,
  name       TEXT NOT NULL,
  version    TEXT,
  definition TEXT NOT NULL,
  valid      INTEGER NOT NULL DEFAULT 1,
  error      TEXT,
  platforms  TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE workflow_runs(
  id          TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL,
  session_id  TEXT NOT NULL,
  params      TEXT NOT NULL DEFAULT '{}',
  state       TEXT NOT NULL,               -- running|paused|blocked|done|failed|cancelled
  current_step TEXT,
  phase       TEXT,
  iterations  INTEGER NOT NULL DEFAULT 0,
  max_iterations INTEGER NOT NULL DEFAULT 40,
  step_outputs TEXT NOT NULL DEFAULT '{}',
  workdir     TEXT,
  spent_usd   REAL NOT NULL DEFAULT 0,
  spent_tokens INTEGER NOT NULL DEFAULT 0,
  started_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL,
  finished_at TEXT,
  result      TEXT,
  error       TEXT,
  parent_run  TEXT,
  depth       INTEGER NOT NULL DEFAULT 0,
  tg_message  TEXT
);
CREATE INDEX workflow_runs_state ON workflow_runs(state);

CREATE TABLE workflow_step_log(
  id        INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id    TEXT NOT NULL,
  step_id   TEXT NOT NULL,
  attempt   INTEGER NOT NULL DEFAULT 1,
  started_at TEXT NOT NULL,
  ended_at  TEXT,
  result    TEXT,
  output    TEXT,
  error     TEXT
);
CREATE INDEX workflow_step_log_run ON workflow_step_log(run_id, id);

CREATE TABLE schedules(
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL,                -- cron|interval|mcp_poll|watch_file|event
  spec       TEXT NOT NULL,
  target     TEXT NOT NULL,
  dedup      TEXT,
  state      TEXT NOT NULL DEFAULT 'active', -- active|paused|deleted
  last_run   TEXT,
  next_run   TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_error TEXT,
  runs       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX schedules_next ON schedules(state, next_run);

CREATE TABLE seen_items(
  schedule_id TEXT NOT NULL,
  item_id     TEXT NOT NULL,
  first_seen  TEXT NOT NULL,
  last_seen   TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  fired       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(schedule_id, item_id)
);

-- ============================================================ §14 Telegram
CREATE TABLE tg_updates(
  update_id   INTEGER PRIMARY KEY,
  received_at TEXT NOT NULL,
  processed   INTEGER NOT NULL DEFAULT 0,
  payload     TEXT NOT NULL
);

CREATE TABLE tg_outbox(
  id          TEXT PRIMARY KEY,
  chat_id     INTEGER NOT NULL,
  topic_id    INTEGER,
  method      TEXT NOT NULL,
  payload     TEXT NOT NULL,
  state       TEXT NOT NULL DEFAULT 'pending', -- pending|sent|failed
  attempts    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  sent_at     TEXT,
  message_id  INTEGER,
  error       TEXT,
  not_before  TEXT
);
CREATE INDEX tg_outbox_state ON tg_outbox(state, not_before);

CREATE TABLE tg_actions(
  token       TEXT PRIMARY KEY,
  action      TEXT NOT NULL,
  target      TEXT NOT NULL,
  args        TEXT NOT NULL DEFAULT '{}',
  created_at  TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  single_use  INTEGER NOT NULL DEFAULT 1,
  consumed_at TEXT,
  consumed_by INTEGER
);

CREATE TABLE tg_topics(
  chat_id    INTEGER NOT NULL,
  topic_id   INTEGER NOT NULL,
  purpose    TEXT NOT NULL,                -- general|approvals|system|run|session
  ref        TEXT,
  name       TEXT,
  created_at TEXT NOT NULL,
  closed_at  TEXT,
  PRIMARY KEY(chat_id, topic_id)
);
CREATE INDEX tg_topics_purpose ON tg_topics(purpose, ref);

CREATE TABLE tg_render_mode(
  chat_id    INTEGER PRIMARY KEY,
  mode       TEXT NOT NULL,                -- rich | html
  until      TEXT NOT NULL
);

-- ============================================================ §4.4 configuration
CREATE TABLE config_generations(
  gen        INTEGER PRIMARY KEY,
  ts         TEXT NOT NULL,
  source     TEXT NOT NULL,                -- file|cli|telegram|agent|boot
  changed    TEXT NOT NULL DEFAULT '[]',
  snapshot   TEXT NOT NULL
);

CREATE TABLE subsystem_apply_results(
  gen        INTEGER NOT NULL,
  subsystem  TEXT NOT NULL,
  result     TEXT NOT NULL,                -- applied_live|rejected|requires_restart
  reason     TEXT,
  ts         TEXT NOT NULL,
  PRIMARY KEY(gen, subsystem)
);
"#;

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

/// Chaîne d'audit (issue #47) : deux événements d'une même session ne peuvent plus porter
/// le même `seq`. Un doublon rendrait le rejeu et `session_events(from_seq)` faux, sans
/// rien dire.
const SQL_0011: &str = r#"
CREATE UNIQUE INDEX events_session_seq ON events(session_id, seq);
"#;

/// Reports de consolidation (issue #59) : un candidat reporté trois nuits de suite est
/// rejeté avec sa raison, au lieu de revenir indéfiniment dans le lot.
const SQL_0012: &str = r#"
ALTER TABLE mem_candidates ADD COLUMN deferrals INTEGER NOT NULL DEFAULT 0;
"#;

/// Entrée vue dans les résultats du rappel automatique sans être retenue : le
/// dénominateur du retour d'usage (issue #86).
const SQL_0013: &str = r#"
ALTER TABLE mem_signals ADD COLUMN seen INTEGER NOT NULL DEFAULT 0;
"#;

/// Empreinte d'un outil MCP (description, schéma, annotations) et date où elle a été vue
/// la première fois : un changement silencieux après `tools/list_changed` se voit (#92).
const SQL_0014: &str = r#"
ALTER TABLE mcp_tools ADD COLUMN fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE mcp_tools ADD COLUMN first_seen TEXT;
"#;

/// Jusqu'ici tout souvenir servi comptait comme utile : les deux compteurs, désormais lus
/// par le classement, repartent de zéro (issue #105). La date du dernier rappel, les
/// requêtes et les vues restent.
const SQL_0015: &str = r#"
UPDATE mem_signals SET recalls = 0, useful_recalls = 0;
"#;

/// Messages distincts absorbés par un tour porteur ; la clé de déduplication reste
/// sur leur ligne et le lien survit à une reprise après crash (#161).
const SQL_0016: &str = r#"
ALTER TABLE turn_queue ADD COLUMN merged_into TEXT;
CREATE INDEX turn_queue_merged_into ON turn_queue(merged_into, enqueued_at);
ALTER TABLE messages ADD COLUMN source_turn_id TEXT;
CREATE UNIQUE INDEX messages_source_turn ON messages(source_turn_id)
  WHERE source_turn_id IS NOT NULL;
"#;

/// Révoque les anciennes autorisations globales de `git_clone` : elles pouvaient
/// accepter un chemin local à la place d'un dépôt distant (#160).
const SQL_0017: &str = r#"
UPDATE policies
SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE tool = 'git_clone' AND arg_match IS NULL AND window = 'always'
  AND revoked_at IS NULL;
"#;

#[cfg(test)]
mod tests {
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
}
