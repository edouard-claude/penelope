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

## 1. Ce qui est livré

Crate `crates/penelope-vault` (6 210 lignes, 16 modules, aucun fichier au-dessus de 796 lignes), au-dessus de `penelope-app`,
sous le daemon, qui en dépend :

| Module | Origine |
|---|---|
| `vault_ops`, `concepts` (+ `index_source`), `vault_inventory`, `vault_git` (+ `vault_sync`), `session_notes`, `secret_shelf`, `embeddings` (+ `State`), `usage_feedback`, `session_project`, `mem_split`, `mem_audit`, `review`, `episodes` | modules du même nom du daemon, par `git mv` |
| `snapshot` : `snapshot_uids`, `fresh_snapshot`, `frozen_snapshot`, `core_overflow` | `conversation.rs`, `dream/candidates.rs` |
| `tiers` : `build_tiers`, `build_tiers_in`, `build_turn_prompt` | `conversation.rs` |

Critères de T22 :

- la crate ne dépend pas de `penelope-daemon` (règle d'archtest
  `VAULT_ALLOWED_DEPS`, test `the_vault_crate_does_not_depend_on_the_daemon`) ;
- ses tests ne construisent pas de `Daemon` : `grep -rn Daemon crates/penelope-vault`
  vide ; sessions par `SessionStore::create`, providers par
  `penelope_app::testing::MockProviders` ;
- `penelope-evals` importe `penelope_vault::vault_ops::reindex` et
  `penelope_vault::review::record_candidates` ;
- `docs/ca-matrix.md` régénéré : ca_6_14 et ca_6_15 dans
  `crates/penelope-vault/src/episodes.rs` ;
- réexports de transition dans le daemon (`lib.rs` pour les treize modules,
  `conversation` pour tuiles et instantanés, `ingest::index_source`,
  `dream::{vault_sync, core_overflow}`, `cache_audit::prefix_key`) : CLI, évaluations
  hors `mem_bench`, `tests/` et `examples/` n'ont pas bougé.

Cycles coupés, par déplacement seul (commit 55d9fba et suivants) : `vault_ops → ingest`,
`vault_git → dream`, `dream → core_overflow`, et `episodes → cache_audit` (clé
`prefix_key` descendue dans `penelope_app::helpers`). `session_project::set` prend
`&Services` (commit de signature à part).

## 2. Mesures

| Mesure | Avant (5669220) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 82 637 (plafond `[crates]`) | 76 508 |
| `penelope-vault/src`, lignes | | 6 210 |
| `[daemon].modules` | 57 | 44 |
| `[daemon.daemon_users]` `session_project.rs` | 1 | sorti |
| `[channel.allowed]` `session_project.rs` | 2 (daemon) | 2 (vault, même fichier) |

## 3. Choix

- `build_turn_prompt` part dans le vault avec `build_tiers` et `build_tiers_in` :
  §3.2 cite ces deux-là pour le vault et `build_turn_prompt` pour la future crate
  `penelope-conversation`, mais elles ne font que l'envelopper. Le corps ne lit que le
  vault, la mémoire, `Services` et les crates métier ; la conversation (T23) le
  consommera depuis le vault.
- Trois tests restent au daemon parce qu'ils exercent l'étage au-dessus : concepts par
  l'ingestion (`ingest/tests.rs`, la lecture des pages se fait sur le fichier),
  notes de session à travers compaction et fork (`compaction/tests.rs`), revue sur
  deux tours réels (`engine/tests/turns.rs`).
- `penelope-vault` ne dépend ni de `penelope-telegram` (R8) ni de `penelope-skills`
  ou `penelope-workflow` (lus par `Services`) : la règle d'archtest les refuse.
- Chemins réécrits (`crate::runtime::Services` vers `penelope_app::services::Services`,
  etc.) plutôt qu'un module `runtime` factice dans la crate : le diff de déplacement
  reste limité aux `use`.
- `vault_ops.rs` (881 lignes) : tests sortis dans `vault_ops/tests.rs`, commit de
  déplacement seul, pour qu'aucun fichier de la crate ne dépasse 800 lignes.

## 4. Reste et blocages

- Plafond `[crates]` du daemon : 82 637 dans `budget.toml`, 76 508 mesurées ;
  `UPDATE_BUDGET` ne le réécrit pas, à abaisser par l'intégrateur.
- Les autres tests de tuiles (`conversation/tests.rs` : sujet de session, rappel,
  pratiques, préfixe stable, âme et serveurs MCP) restent au daemon, verts par les
  réexports ; ils peuvent rejoindre `tiers/tests.rs` quand `i-bascule` aura fini avec
  `conversation/tests.rs`.
- `session_project::resolve` lit encore `tg_chat_id` et `tg_topic_id` (liste blanche
  R8, 2) : T36/T37.
- `ingest`, `dream`, `doctor` citent encore les fonctions déplacées par leurs anciens
  chemins réexportés ; T26 et T28 les réécriront, T30 retire les réexports.

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 63 suites, 1 977 tests passés, 0 échec
(dont `penelope-archtest`, `docs`, `ca_matrix`), sur macOS : les tests
`cfg(target_os = "macos")` sont compilés. `scripts/check-budget.sh 5669220` : rien ne
remonte, aucune dérogation. `UPDATE_BUDGET=1` passé après le dernier déplacement.

## 6. Notes de version (pour docs/progress.md)

#### Crate `penelope-vault` : la mémoire en fichiers hors du daemon (T22)

- Nouvelle crate `penelope-vault`, entre `penelope-app` et le daemon : vault et
  réindexation, wiki de concepts, inventaire, historique git, notes de travail, secrets
  mis de côté, embeddings, retour d'usage, sujet des sessions, découpage et audit de la
  mémoire, revue des tours, épisodes, instantanés mémoire et tuiles du prompt.
- Quatre dépendances circulaires coupées au passage : l'indexation d'une fiche source,
  le commit du vault, la mesure du budget Cœur et la clé du préfixe stable descendent
  vers ce qui les utilise.
- Les tests de la crate n'ouvrent pas de daemon : une session et un provider factice
  suffisent. `penelope-archtest` interdit à la crate de dépendre du daemon ou du canal.
- Le daemon réexporte tout sous les anciens chemins ; `penelope-daemon` passe de
  82 637 à 76 508 lignes.
