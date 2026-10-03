//! Migrations 0011 et suivantes, sorties de `migrations.rs` pour qu'il tienne sous sa
//! borne (R1). Immuables comme les autres.

/// Chaîne d'audit (issue #47) : deux événements d'une même session ne peuvent plus porter
/// le même `seq`. Un doublon rendrait le rejeu et `session_events(from_seq)` faux, sans
/// rien dire.
pub(super) const SQL_0011: &str = r#"
CREATE UNIQUE INDEX events_session_seq ON events(session_id, seq);
"#;

/// Reports de consolidation (issue #59) : un candidat reporté trois nuits de suite est
/// rejeté avec sa raison, au lieu de revenir indéfiniment dans le lot.
pub(super) const SQL_0012: &str = r#"
ALTER TABLE mem_candidates ADD COLUMN deferrals INTEGER NOT NULL DEFAULT 0;
"#;

/// Entrée vue dans les résultats du rappel automatique sans être retenue : le
/// dénominateur du retour d'usage (issue #86).
pub(super) const SQL_0013: &str = r#"
ALTER TABLE mem_signals ADD COLUMN seen INTEGER NOT NULL DEFAULT 0;
"#;

/// Empreinte d'un outil MCP (description, schéma, annotations) et date où elle a été vue
/// la première fois : un changement silencieux après `tools/list_changed` se voit (#92).
pub(super) const SQL_0014: &str = r#"
ALTER TABLE mcp_tools ADD COLUMN fingerprint TEXT NOT NULL DEFAULT '';
ALTER TABLE mcp_tools ADD COLUMN first_seen TEXT;
"#;

/// Jusqu'ici tout souvenir servi comptait comme utile : les deux compteurs, désormais lus
/// par le classement, repartent de zéro (issue #105). La date du dernier rappel, les
/// requêtes et les vues restent.
pub(super) const SQL_0015: &str = r#"
UPDATE mem_signals SET recalls = 0, useful_recalls = 0;
"#;

/// Messages distincts absorbés par un tour porteur ; la clé de déduplication reste
/// sur leur ligne et le lien survit à une reprise après crash (#161).
pub(super) const SQL_0016: &str = r#"
ALTER TABLE turn_queue ADD COLUMN merged_into TEXT;
CREATE INDEX turn_queue_merged_into ON turn_queue(merged_into, enqueued_at);
ALTER TABLE messages ADD COLUMN source_turn_id TEXT;
CREATE UNIQUE INDEX messages_source_turn ON messages(source_turn_id)
  WHERE source_turn_id IS NOT NULL;
"#;

/// Révoque les anciennes autorisations globales de `git_clone` : elles pouvaient
/// accepter un chemin local à la place d'un dépôt distant (#160).
pub(super) const SQL_0017: &str = r#"
UPDATE policies
SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE tool = 'git_clone' AND arg_match IS NULL AND window = 'always'
  AND revoked_at IS NULL;
"#;

/// #205 : le prompt système rendu devient une ligne, dédupliquée par le `system_hash`
/// que l'empreinte calculait déjà. `llm_requests.request`, jamais écrite depuis l'origine,
/// cède la place aux trois clés qui rendent une requête rejouable.
pub(super) const SQL_0018: &str = r#"
CREATE TABLE prompt_snapshots(
  hash          TEXT PRIMARY KEY,          -- = usage.system_hash
  rendered      TEXT NOT NULL,             -- préfixe stable T0-T2, tel qu'envoyé
  tiers         TEXT,                      -- découpe en tuiles (sans texte), NULL si inconnue
  first_seen_at TEXT NOT NULL,
  last_seen_at  TEXT NOT NULL,
  uses          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX prompt_snapshots_last_seen ON prompt_snapshots(last_seen_at);

ALTER TABLE llm_requests DROP COLUMN request;
ALTER TABLE llm_requests ADD COLUMN system_hash TEXT;
ALTER TABLE llm_requests ADD COLUMN tools_hash TEXT;
ALTER TABLE llm_requests ADD COLUMN request_hash TEXT;
"#;

/// Jobs d'outils natifs (#204) : un appel long sort du tour et son résultat revient seul.
///
/// Table distincte de `mcp_tasks` : celle-ci décrit une tâche **chez un serveur MCP**
/// (`server`, `task_ref` non nuls, sondée de l'extérieur). Un job natif n'est pas sondé,
/// il tourne ici ; il porte un outil, ses arguments, l'effet du ledger (§4.2) et le tour
/// d'origine. Mêmes états, mêmes règles de purge et de rétention.
///
/// Pas de `poll_at` : rien ne sonde un job natif, et une colonne morte qui prétend porter
/// une échéance est pire qu'une colonne absente (décisions 0011 et 0012). `updated_at`
/// donne l'âge, `delivered_at` la livraison.
pub(super) const SQL_0019: &str = r#"
CREATE TABLE tool_jobs(
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL,
  run_id       TEXT,
  turn_id      TEXT,                        -- tour d'origine, clos ou non
  call_id      TEXT,                        -- appel d'outil qui l'a lancé
  tool         TEXT NOT NULL,
  request      TEXT NOT NULL DEFAULT '{}',  -- arguments de l'appel
  state        TEXT NOT NULL,               -- working|input_required|completed|failed|cancelled
  result       TEXT,                        -- valeur rendue, ou message d'erreur
  effect_id    TEXT,                        -- effet du ledger resté `dispatching`
  delivered_at TEXT,                        -- résultat remis à la session d'origine
  created_at   TEXT NOT NULL,
  updated_at   TEXT NOT NULL
);
CREATE INDEX tool_jobs_session ON tool_jobs(session_id, state);
CREATE INDEX tool_jobs_delivery ON tool_jobs(delivered_at, updated_at);
"#;

/// Scellement de l'historique V0 (épopée #208, T11, `design/v1/source-de-verite.md` §2.4,
/// §4.5). Les lignes à sceller se trouvent par un index partiel : l'étape de boot qui
/// pose les `conv.import` le relit à chaque démarrage, et il est vide dès que tout est
/// scellé. Le scellement lui-même n'est pas ici : il a besoin du journal.
///
/// `projections_workflow` et `projections_approval` (0001) n'ont jamais été lues ni
/// écrites ; `projections_session` reste, elle portera le filigrane du projecteur (T13).
pub(super) const SQL_0021: &str = r#"
CREATE INDEX messages_unsealed ON messages(session_id, seq)
  WHERE event_id IS NULL AND sealed = 0;
DROP TABLE projections_workflow;
DROP TABLE projections_approval;
"#;

/// Série d'échecs d'une planification (#229) : une planification cassée n'alerte plus à
/// chaque exécution, mais au premier échec, au changement de motif et à des paliers.
/// `failures_in_a_row` compte les exécutions ratées à la suite ; `alerted_reason` garde
/// le motif normalisé de la dernière alerte de la série (NULL : aucune alerte encore).
/// Les deux reviennent à zéro au premier succès.
pub(super) const SQL_0022: &str = r#"
ALTER TABLE schedules ADD COLUMN failures_in_a_row INTEGER NOT NULL DEFAULT 0;
ALTER TABLE schedules ADD COLUMN alerted_reason TEXT;
"#;

/// Phrase du propriétaire derrière un candidat (#245) : retrouvée dans son message du
/// tour, elle est montrée au tri et rend le candidat endossé. NULL : aucune.
pub(super) const SQL_0023: &str = r#"
ALTER TABLE mem_candidates ADD COLUMN owner_quote TEXT;
"#;

/// Jeu de décisions du juge d'approbation (#233) : un échantillon par ligne `shell_exec`
/// vue par la politique, quand `observability.dataset.approvals` est vrai. La ligne y est
/// telle que le juge la reçoit (secrets masqués, commentaires retirés). Une ligne est
/// écrite une fois, puis complétée une fois par la décision (`outcome`) et une fois par
/// l'exécution (`executed_at`) : jamais réécrite. Sa rétention est la sienne
/// (`observability.dataset.retention_days`), la purge d'une session la supprime.
pub(super) const SQL_0024: &str = r#"
CREATE TABLE approval_samples (
  id           INTEGER PRIMARY KEY,
  v            INTEGER NOT NULL DEFAULT 1,  -- version du schéma de l'échantillon
  created_at   TEXT NOT NULL,
  created_ms   INTEGER NOT NULL,
  session_id   TEXT NOT NULL,
  turn_id      TEXT,
  call_id      TEXT NOT NULL,
  command_sha  TEXT NOT NULL,               -- même empreinte que `approval.judged`
  input        TEXT NOT NULL,               -- {command, cwd, workspaces, network}
  floors       TEXT NOT NULL,               -- {policy, layer, risk, rule, sans_motif}
  judge        TEXT,                        -- sortie du juge ou échec ; NULL : pas appelé
  approval_id  TEXT,                        -- carte posée, s'il y en a une
  outcome      TEXT,                        -- auto|approved|denied|expired|cancelled
  via          TEXT,
  decided_at   TEXT,
  decision_ms  INTEGER,
  exit_code    INTEGER,
  exec_ms      INTEGER,
  executed_at  TEXT,
  UNIQUE(session_id, call_id)
);
CREATE INDEX approval_samples_created ON approval_samples(created_at);
CREATE INDEX approval_samples_approval ON approval_samples(approval_id)
  WHERE approval_id IS NOT NULL;
"#;

/// Les appels d'outils entrent dans l'index plein texte des messages (#300) : les lignes
/// écrites avant n'y ont que leur texte, et un message assistant qui n'était qu'un appel y
/// est vide. La migration ne réécrit pas l'index, qui est un cache tenu par
/// `penelope-context` : elle pose une marque dans `kv`, que la passe de maintenance du
/// daemon honore en refaisant `messages_fts` depuis `messages` (`penelope store rebuild`
/// le fait aussi). Une base sans message n'a rien à refaire. Le journal n'est pas touché.
pub(super) const SQL_0025: &str = r#"
INSERT INTO kv(k, v, ts)
SELECT 'store.messages_fts_pending', 'appels d''outils (#300)',
       strftime('%Y-%m-%dT%H:%M:%fZ','now')
WHERE EXISTS (SELECT 1 FROM messages)
ON CONFLICT(k) DO NOTHING;
"#;
