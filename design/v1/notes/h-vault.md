# Lot H, crate `penelope-vault` (T22)

Branche `v1-h-vault`, dérivée de `v1` au commit 5669220 (1.0.0-alpha.6). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §1.2 (cycles), §3.2, §6 (T22), §8.
Préalables : `d-kv-helpers.md` (T05, T06), `h-ports.md` (T07 à T10), `h-app.md` (T21).

## 0. Inventaire avant déplacement

### Ce qui part dans `penelope-vault`

Les treize modules de §3.2 (6 038 lignes, tests compris) :

| Module | Lignes | Dépendances internes hors vault (code) | Tests qui construisent un `Daemon` |
|---|---|---|---|
| `vault_ops` | 881 | `ingest::index_source` (cycle), `media` (app) | aucun |
| `concepts` | 883 | aucune | `two_sources_sharing_a_term_meet_on_a_concept_page` (`ingest::ingest`) |
| `vault_inventory` | 285 | aucune | aucun |
| `vault_git` | 203 | `dream::vault_sync` (cycle) | aucun |
| `session_notes` | 546 | aucune | `notes_survive_compaction_…` (`compaction::compact`, `session_ops::fork`) |
| `secret_shelf` | 152 | aucune | aucun |
| `embeddings` | 440 | `codex_scope` (app) | `backfill…` (session CLI, provider forcé) |
| `usage_feedback` | 95 | aucune | aucun |
| `session_project` | 183 | `set(&Daemon)` (ne lit que `services`), `helpers::topic_name_key` (app) | aucun |
| `mem_split` | 400 | `codex_scope` (app) | aucun |
| `mem_audit` | 452 | `ports::McpAdmin` (app) | un test (session CLI) |
| `review` | 767 | `codex_scope` (app) | `an_agreement_turns_the_proposal_…` (joue deux tours réels) |
| `episodes` | 751 | `cache_audit::prefix_key` (reste au daemon) | `ca_6_15`, `ca_6_14`, `three_messages_…` (session CLI, provider forcé) |

Fonctions de tiers et d'instantané de `conversation.rs` (la spécification citait
347-373 et 559-664 ; aujourd'hui 361-682) :

- instantanés : `snapshot_uids`, `fresh_snapshot`, `frozen_snapshot` (lit
  `episodes::snapshot_key`) ;
- tiers : `build_tiers`, `build_tiers_in`, et `build_turn_prompt` que les deux premières
  enveloppent (on ne peut pas emporter l'enveloppe sans le corps), avec ses fonctions
  privées `practices_of`, `current_context`, `strip_frontmatter`. Dépendances : helpers
  et `machine` (app), `session_notes`, `session_project`, `episodes` (vault), crates
  métier. Aucune vers le daemon.

Cycles de §1.2 et du périmètre :

| Cycle | Cassure retenue |
|---|---|
| `vault_ops → ingest::index_source` | `index_source` (indexe une fiche source) descend dans `concepts`, l'ingestion le réexporte |
| `vault_git → dream::vault_sync` | `vault_sync` (commit et push du vault) descend dans `vault_git`, `dream` le réexporte |
| `dream → core_overflow` (et `doctor/memory.rs`) | `core_overflow` (budget du niveau Cœur) descend avec les instantanés, `dream` le réexporte |
| `episodes → cache_audit::prefix_key` | la clé kv descend dans `penelope_app::helpers` (constructeur de clé, comme ceux de T06), `cache_audit` la réexporte |
| `session_project::set(&Daemon)` | commit de signature à part : `&Services` |

`embeddings::State` : le type part avec `embeddings` ; `Daemon` le tient en
`Arc<penelope_vault::embeddings::State>` (`runtime.rs:33`), comme §3.1 le prévoit.

### Ce qui reste au daemon, et pourquoi

- Les trois tests qui ont besoin de l'étage au-dessus : celui de `concepts` (ingestion,
  futur `penelope-dream`) va dans `ingest/tests.rs` ; celui de `session_notes`
  (compaction et fork) dans `compaction/tests.rs` ; le test de `review` qui joue deux
  tours réels dans `engine/tests/`. Les autres tests se passent de `Daemon` : session
  créée par `SessionStore::create` + `cli.session`, providers par
  `penelope_app::testing::MockProviders`.
- `SessionConversation` et le reste de `conversation.rs` (T23, et `i-bascule`).

### Consommateurs à garder valides

`pub use penelope_vault::{…}` dans `lib.rs` du daemon pour les treize modules et les
deux nouveaux ; `conversation` réexporte `build_tiers*`, `build_turn_prompt`,
`fresh_snapshot`, `snapshot_uids`. Évaluations : `mem_bench.rs` importe
`penelope_vault::{vault_ops::reindex, review::record_candidates}`.
