# Lot J : crate `penelope-executor` (épopée #208, T24)

Agent `j-executor`, branche `v1-j-executor`, base 1.0.0-alpha.10 (`d662ca4`).
Spécification : `design/v1/decoupage-daemon.md` §3.2, §4 (ligne T24), §6 T24, §8
(ressources embarquées).

## 0. Inventaire avant déplacement

### Ce qui part

| Module du daemon | Lignes | Dépendances vers le daemon à couper |
|---|---|---|
| `executor/` (13 fichiers) | 3 815 | `agent::{without_intention, call_arguments, effective_arguments, wants_network}` (crate `penelope-agent`) ; `runtime_events::bounded_redacted` ; `scheduler::{create, listing, retarget}` ; `tool_jobs::{background_hint, tool}` ; `selfknow`, `selfdocs`, `tools_on_demand`, `vision` (partent avec) |
| `selfknow.rs` | 777 | `runtime::rss_mb` ; `compaction::context_view` (sort avec `j-conversation`) ; `codex_auth`, `codex_quota` (`penelope-ops`, interdit à l'exécuteur) ; `tool_jobs::{store, NewJob}` ; `crate::VERSION` |
| `selfdocs.rs` + `build.rs` | 462 | `include!(OUT_DIR/docs.rs)` : le `build.rs` part avec |
| `vision.rs` | 546 | aucune (`media`, `ports` sont dans app) |
| `images.rs` | 186 | aucune |
| `voice.rs` | 495 | aucune (`Messenger`, `codex_scope` dans app) |
| `tools_on_demand.rs` | 109 | aucune |

Aucun de ces modules n'est parti ailleurs (vault, app) : `vision::Task` seule est déjà
dans `penelope-app` (T21).

### Coupures retenues

- Les quatre fonctions d'arguments (pures, sur `serde_json`) descendent dans
  `penelope-tools` (`args.rs`) ; `penelope-agent` les réexporte. §6 T23 le prévoyait pour
  `wants_network`.
- `bounded_redacted` descend dans `penelope_app::helpers` ; `runtime_events` le réexporte.
- `Admin` descend dans `penelope_app::ports`, étendu de `rss_mb`, `codex_view`,
  `context_view` (même motif que `backup_status`, `memory_search`) : le daemon les sert
  depuis `engine.rs`.
- `Orchestrator` étendu de `schedule_create`, `schedule_list`, `schedule_move`,
  `schedule_delete` ; `WorkflowOrchestrator` les sert par `scheduler`.
- `tool_jobs.rs` coupé en deux : le magasin et les outils `job_*` partent dans
  l'exécuteur (`jobs.rs`) ; le lancement (`maybe_spawn`, qui nomme `JobRequest` de
  `penelope-agent`), la livraison (`Daemon`) et les tests restent au daemon.
- Tests qui ont besoin du daemon (planification par l'orchestrateur) : restent au daemon.

### Consommateurs

Daemon (engine, workflow, rpc, tool_jobs), passerelle, évaluations (`selfdocs::anchor`),
`examples/runtime_pathlayer_demo.rs` : tous par `crate::executor::…` ou
`penelope_daemon::…`, gardés par réexports de transition jusqu'à T30.

## 1. Livré

Six commits sur `v1-j-executor` :

1. `b9dfde3` : `wants_network`, `call_arguments`, `effective_arguments`,
   `without_intention` descendent de `penelope-agent` dans `penelope_tools::args`
   (déplacement ; l'agent les réexporte).
2. `eba3d79` : `Admin` dans `penelope_app::ports`, `bounded_redacted` dans
   `penelope_app::helpers` (déplacement ; anciens chemins réexportés).
3. `82cbe69` (signatures) : `Orchestrator` gagne `schedule_create`, `schedule_list`,
   `schedule_move`, `schedule_delete`, servis par `WorkflowOrchestrator` avec
   `scheduler` ; `Admin` gagne `rss_mb`, `codex_view`, `context_view`, servis par
   `impl Admin for Daemon` (`engine.rs`).
4. `9cd93ad` : `BusSink` descend dans `penelope_app::bus` (déplacement), pour rendre à
   `engine.rs` (liste de référence, 1 105) la place des trois méthodes d'`Admin`.
5. `f275254` : la crate `crates/penelope-executor` (`git mv`, 19 renommages) :
   `executor/`, `selfknow`, `selfdocs` + `build.rs`, `vision`, `images`, `voice`,
   `tools_on_demand`, et `jobs` (magasin et outils `job_*` coupés de `tool_jobs.rs`).
   Trailer `Dérogation-budget: #208` (clés `[channel.allowed]` renommées).
6. `5d98a3d` : archtest, `EXECUTOR_ALLOWED_DEPS` et
   `the_executor_crate_sees_neither_the_daemon_nor_the_agent_loop`.

Réexports de transition (à retirer en T30) : `lib.rs` du daemon
(`pub use penelope_executor::{images, selfdocs, tools_on_demand, vision, voice}`),
façades `executor.rs` (`pub use penelope_executor::executor::*`), `selfknow.rs`
(`pub use penelope_executor::selfknow::*` plus `codex_view`), `tool_jobs.rs`
(`pub use penelope_executor::jobs::*`), `runtime_events::bounded_redacted`,
`engine::BusSink`, et dans `penelope-agent` les quatre fonctions d'arguments.

## 2. Mesures

| Mesure | Avant (d662ca4) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 38 177 (plafond) | 31 486 |
| `penelope-executor/src`, lignes | | 6 789 (plus gros fichier de code : `selfknow.rs`, 729 ; tests : `executor/tests.rs`, 1 177) |
| `[files.oversized]` `tool_jobs.rs` | 1 655 | 1 207 |
| `[daemon].modules` | | `images`, `selfdocs`, `tools_on_demand`, `vision`, `voice` sortis |

## 3. Choix

- `Admin` étendu plutôt qu'une dépendance vers `penelope-ops` ou la future
  `penelope-conversation` : même motif que `backup_status`, `memory_search`,
  `mcp_servers`. Sans `Admin` (tests seulement ; en production engine et workflow le
  posent toujours), `rss_mb`, `providers.codex` et `costs.context` de `self_status`
  valent `null`.
- `schedule_move` garde dans l'exécuteur la lecture de `to` (`here`, `private`) et
  reçoit `(chat, topic)` : sortir cette lecture dans l'orchestrateur aurait déplacé sept
  mentions du canal dans le daemon ; T36 la fera passer par
  `ChannelDelivery::describe_origin`. `schedule_delete` passe aussi par le port (§6),
  alors qu'il n'appelle que `Services` : un seul chemin pour la planification.
  Sans orchestrateur, les quatre outils répondent « planificateur indisponible ici ».
- `tool_jobs.rs` coupé plutôt que déplacé entier : `maybe_spawn` nomme `JobRequest` de
  `penelope-agent`, `deliver_due` prend `&Daemon`.
- Le test `schedule_move_sends_a_schedule_here_or_home` reste au daemon
  (`executor/tests.rs`) : il passe par `WorkflowOrchestrator`.
- `crate::VERSION` : la crate définit le sien (`CARGO_PKG_VERSION`, même version de
  workspace). `selfdocs` embarque `docs/` par son propre `build.rs`.
- `penelope-telegram` reste une dépendance (inventaire des commandes de `self_status`),
  permise par la règle jusqu'à T36.

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast` : 76 suites, 2 005 tests, 0 échec, sur macOS
(tests `cfg(target_os = "macos")` compilés, dont celui de l'exécuteur). Dont `docs`
(`every_native_tool_is_documented`), `penelope-archtest`, `scenarios`, `rpc_golden`,
`telegram_e2e`. `examples/runtime_pathlayer_demo.rs` compile (`cargo check --examples`).
`scripts/check-budget.sh d662ca4` : accepté par dérogation.

## 5. Reste et blocages

- `[crates]` : plafond du daemon à abaisser de 38 177 à 31 486 par l'intégrateur.
- `scripts/bump.sh` compte les lignes `version =` : une de plus avec la crate.
- Bissection : entre `82cbe69` et `9cd93ad` exclu, `penelope-archtest` est rouge
  (`engine.rs` à 1 121 lignes pour une borne de 1 105).
- Sorties de périmètre, signalées au chef d'équipe : `penelope-agent` et
  `penelope-tools` (fonctions d'arguments), `engine.rs` (`impl Admin`, `BusSink`),
  `tool_jobs.rs`, `engine/tests/turns.rs` (un chemin qualifié).
- T34 : l'exécuteur cite encore `Orchestrator::schedule_*` ; T36 : `selfknow` nomme
  le canal (12 mentions) et `schedules_messaging` (7).

## 6. Notes de version (pour docs/progress.md)

#### Exécuteur des outils natifs : crate `penelope-executor` (épopée #208, lot J, T24)

- Les outils natifs, `self_status`, la documentation embarquée, la vision, les images,
  la voix, les outils à la demande et le magasin des jobs d'outils quittent le daemon
  pour la crate `penelope-executor`, qui ne dépend ni de la boucle d'agent ni de
  l'orchestrateur ; une règle d'architecture le vérifie.
- La planification (`schedule_*`) passe par le port `Orchestrator`, et ce que
  `self_status` lit du processus (mémoire, Codex, contexte) par le port `Admin`.
- `penelope-daemon` passe de 38 177 à 31 486 lignes. Aucun comportement visible ne
  change.
