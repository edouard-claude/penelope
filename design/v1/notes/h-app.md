# Lot H, crate `penelope-app` (T21)

Branche `v1-h-app`, dérivée de `v1` au commit b58273b (1.0.0-alpha.5). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §2.2, §3.2, §6 (T21), §8 ; charte
`design/v1/README.md` §3.1 à §3.3. Préalables : `d-kv-helpers.md` (T05, T06),
`h-ports.md` (T07 à T10).

## 0. Inventaire avant déplacement

Mesure de départ : `penelope-daemon/src` compte 86 462 lignes (`[crates]` de
`budget.toml`, `find crates/penelope-daemon/src -name '*.rs' | xargs cat | wc -l`).

### Ce qui part dans `penelope-app`

| Élément | Aujourd'hui | Dépendances internes à couper |
|---|---|---|
| `Services` (+ `bootstrap`, `for_tests`, `kv_*`, `publish_config`), `workflow_known`, `workflow_known_with`, `SUBSYSTEMS`, `BUNDLED_SKILLS`, `reload_skills` | `runtime.rs:29-330, 710-745` | `elicitation::Broker`, `tool_jobs::Running` (champs) |
| dossier `skills/wiki-markdown` | `crates/penelope-daemon/skills/` | `include_str!` relatif |
| `bus.rs` (Bus, Origin, ChannelDelivery) | `bus.rs` | `agent::{TurnEvent, TurnOutcome}` |
| `TurnOutcome`, `TurnEvent`, `TurnSink`, `NullSink`, `RecordingSink` | `agent/outcome.rs` | `use super::*` (explicite avant le déplacement) |
| `Conversation`, `Compactor`, `MemoryConversation` | `agent/conversation.rs` | `prompt_snapshot::PromptPrefix` (le type seul descend) |
| `CallInfo`, `ToolExecutor` | `agent/executor.rs` | aucune hors métier |
| `elicitation.rs` (Broker, OwnerChannel, Destination) | `elicitation.rs` | aucune |
| `tasks.rs` (Tasks, `spawn_supervised`, `doctor_check`) | `tasks.rs` | `ports::{Handle, Supervision}` ; tests sur `Daemon` (réécrits sur `Supervision`) |
| `ports.rs` (ProviderSource, McpAdmin, Slot, Handle, Supervision) | `ports.rs` | `mcp::ReloadReport` (le type descend), `executor::McpGateway`, `tasks::Tasks` |
| `Messenger`, `question_text`, `McpGateway`, `Orchestrator` | `executor/mod.rs:15-192` | `bus::Origin`, `elicitation::Destination`, `vision::Task` (l'énumération descend) |
| `Running` (registre des jobs du processus) | `tool_jobs.rs:325-418` | `insert`, `forget` privés : deviennent publics |
| `helpers.rs` | `helpers.rs` | `bus::Origin`, `runtime::Services` ; six `pub(crate)` deviennent `pub` |
| `testing.rs` (RecordingMessenger, MockProviders) | `testing.rs` | `Origin`, `Messenger`, `ProviderSource`, `question_text` |

### Ce qui reste au daemon, et pourquoi

- `runtime.rs` : `Daemon`, `Providers` (construit les jetons Codex par `codex_auth`,
  `codex_quota` : ils partent en T28 avec ops), `Hooks` (nomme `workflow::Ports`,
  `scheduler::Ports`), `recover`, `status`, `rss_mb`, `RecoveryReport`.
- `ToolEnv` : nomme `selfknow::TurnModel` (T24, avec `Admin`).
- `Admin` (`selfknow.rs`) et `SwitchHost` (`upgrade.rs`) : partent avec leur crate (T24, T28).
- `decide_approval` et compagnie (`agent/decisions.rs`, `agent/rules.rs`) : `rules.rs`
  appelle `executor::wants_network` (`rules.rs:38, 108`), que la spécification descend
  dans `penelope-tools` en T23 ; hors du périmètre de ce lot.
- `codex_scope`, `cache_audit`, `prompt_snapshot` (hors `PromptPrefix`), `audit`,
  `approval_mode`, `media`, `machine` : listés par §3.2, déplacés seulement s'il reste
  du temps (consigne du lot : `Services` et le bus d'abord).
- `history.rs` : réservé à `i-verify`.

### Cycles à couper

Aucun cycle de modules entre les éléments qui partent et le reste du daemon, sauf par
les types cités ci-dessus : ils descendent tous avec leurs utilisateurs (`ReloadReport`,
`vision::Task`, `PromptPrefix`, `Running`). Le daemon réexporte chaque module par son
ancien chemin (`pub use penelope_app::bus;` dans `lib.rs`, etc.) : `crate::bus::Origin`
et `penelope_daemon::bus::Origin` restent valides pour le daemon, la CLI, les évaluations,
`tests/` et `examples/`.

### Dépendance au canal

`Services` porte `templates: TemplateRegistry` et `actions: ActionStore`
(`penelope_telegram`), `workflow_known` lit `penelope_telegram::templates::CATALOG` :
`penelope-app` dépend donc de `penelope-telegram` tant que T36 n'a pas sorti ces champs.

## 1. Ce qui est livré

Crate `crates/penelope-app` (4 143 lignes, 17 modules, aucun fichier au-dessus de 776
lignes), sous le daemon, qui en dépend :

| Module | Contenu | Origine |
|---|---|---|
| `services` | `Services` (`bootstrap`, `for_tests`, `kv_*`, `publish_config`), `workflow_known*`, `SUBSYSTEMS`, `BUNDLED_SKILLS`, `reload_skills` | `runtime.rs` |
| `bus`, `outcome` | `Bus`, `Origin`, `ChannelDelivery` ; `TurnOutcome`, `TurnEvent`, `TurnSink`, `NullSink`, `RecordingSink` | `bus.rs`, `agent/outcome.rs` |
| `elicitation` | `Broker`, `OwnerChannel`, `Destination` | `elicitation.rs` |
| `tasks` | `Tasks`, `spawn_supervised(&Supervision)`, `doctor_check` | `tasks.rs` |
| `ports` | `ProviderSource`, `McpAdmin`, `Slot`, `Handle`, `Supervision`, `ReloadReport`, `Messenger`, `question_text`, `McpGateway`, `Orchestrator` | `ports.rs`, `mcp/mod.rs`, `executor/mod.rs` |
| `conversation`, `tool_executor` | `Conversation`, `Compactor`, `MemoryConversation`, `PromptPrefix` ; `CallInfo`, `ToolExecutor` | `agent/conversation.rs`, `prompt_snapshot.rs`, `agent/executor.rs` |
| `jobs` | `Running` (jobs d'outils du processus, champ de `Services`) | `tool_jobs.rs` |
| `helpers` | ceux de T06, plus `denied_reads`, `canonical_workspace`, `default_workspaces` | `helpers.rs`, `executor/mod.rs` |
| `testing` | `RecordingMessenger`, `MockProviders` | `testing.rs` |
| `vision` | `Task` (nommée par `Orchestrator::inspect_image`) | `vision.rs` |
| `codex_scope`, `machine`, `media` | tels quels | fichiers du même nom |

Le dossier `skills/` suit `reload_skills` (`crates/penelope-app/skills/wiki-markdown`).

Réexports de transition (à retirer en T30) : `lib.rs` du daemon
(`pub use penelope_app::{bus, codex_scope, elicitation, helpers, machine, media, ports,
tasks, testing}`), `runtime` (`Services`, `SUBSYSTEMS`, `BUNDLED_SKILLS`,
`reload_skills`, `workflow_known*`), `agent` (issue du tour, `Conversation`,
`Compactor`, `MemoryConversation`, `CallInfo`, `ToolExecutor`), `executor`
(`Messenger`, `McpGateway`, `Orchestrator`, `question_text`, `denied_reads`,
`canonical_workspace`, `default_workspaces`), `mcp` (`ReloadReport`), `vision`
(`Task`), `prompt_snapshot` (`PromptPrefix`), `tool_jobs` (`Running`). La CLI, les
évaluations, `tests/` et `examples/` n'ont pas bougé d'une ligne.

`penelope-archtest` connaît la crate : règle de dépendance `penelope-app` →
`APP_ALLOWED_DEPS` (les treize crates métier), test
`the_app_crate_does_not_depend_on_the_daemon`, `the_workspace_is_discovered` la cite ;
elle était déjà dans `CHANNEL_AGNOSTIC_CRATES`, donc soumise à R8.

## 2. Mesures

| Mesure | Avant (b58273b) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 86 462 | 82 409 |
| `penelope-app/src`, lignes | | 4 143 |
| `[files.oversized]` `tool_jobs.rs` | 1 754 | 1 666 |
| `[daemon].modules` | 66 | 57 (bus, codex_scope, elicitation, helpers, machine, media, ports, tasks, testing sortis) |
| `[channel.allowed]` `runtime.rs` | 10 | 5 (les 5 autres suivent `Services` dans `services.rs`) |

L'écart avec la cible de la spécification (≈ 73 000 lignes) tient à ce qui n'a pas été
déplacé (§4) : `decide_approval`, `cache_audit`, `prompt_snapshot`, `audit`,
`approval_mode`.

## 3. Choix

- Réexport de module plutôt que réécriture des chemins : `pub use penelope_app::bus;`
  garde `crate::bus::Origin` valide dans les 60 modules du daemon ; les commits de
  déplacement ne touchent donc que les `use` des fichiers déplacés et les réexports.
- Types descendus avec le port qui les nomme, plutôt que d'enrichir le port d'un
  paramètre générique : `ReloadReport` (`McpAdmin`), `vision::Task`
  (`Orchestrator::inspect_image`), `PromptPrefix` (`Conversation::prompt_prefix`),
  `Running` (champ de `Services`).
- `ToolEnv` reste dans `executor` : il nomme `selfknow::TurnModel` (T24).
- Commits de visibilité à part, sans changement de corps : `helpers` (six `pub(crate)`),
  `Running::insert` / `forget` publics et `Running::woken()` au lieu du champ `wake`,
  `vision::Task::max_tokens`, `executor::canonical_workspace`. Les tests de `tasks`
  composent leur `Supervision` sans `Daemon`.
- Budget : les entrées de `[channel.allowed]` suivent les fichiers renommés (le script
  les reconnaît) ; `services.rs` est une entrée nouvelle (5 mentions retirées de
  `runtime.rs`), d'où le trailer `Dérogation-budget: #208` sur 46ad2f5.
- `[crates]` : le plafond du daemon reste à 86 462, `UPDATE_BUDGET` ne le réécrit pas ;
  à abaisser à 82 409 par l'intégrateur.

## 4. Reste et blocages

- `penelope-app` dépend encore de `penelope-telegram` (T36 requis), pour quatre sites :
  `services.rs:21` (`TemplateRegistry`, `ActionStore`, champs de `Services`),
  `services.rs:308` (`templates::CATALOG` dans `workflow_known`), `helpers.rs:137`
  (`render::deep_link`), `elicitation.rs:488` (`forms::fields_from_schema`, que T36
  déplace dans `OwnerChannel::show`).
- Non déplacés : `decide_approval` et `approval_mode` (appellent
  `executor::wants_network`, qui descend dans `penelope-tools` en T23) ;
  `cache_audit`, `audit` (le code ou les tests prennent un `Daemon` et jouent un tour
  réel) ; `prompt_snapshot` (un test joue un tour par `Daemon`, cite `cache_audit`) ;
  `Admin`, `SwitchHost`, `ToolEnv`, `Gateway` (T24, T28, T29).
- `scripts/bump.sh` compte seize lignes `version =` dans `Cargo.toml` ; la ligne
  `penelope-app` en fait dix-sept : le prochain `make bump` échouera tant que le
  compte (et le « seize lignes » de `CLAUDE.md`) n'est pas porté à dix-sept. Hors de
  mon périmètre, à faire par l'intégrateur.
- `UPDATE_BUDGET=1` décale les commentaires de `[daemon].modules` quand des entrées
  sortent de la liste (le commentaire reste à l'indice, pas au module) : corrigé à la
  main dans 46ad2f5, défaut de `penelope-archtest` à reprendre.
- `design/v1/contrat-fonctionnel.md:718` cite encore
  `crates/penelope-daemon/skills/wiki-markdown/SKILL.md`.
- Bissection : 371feda à 8867630 exclu, `penelope-archtest` est rouge
  (`tool_jobs.rs` et le plafond du daemon au-dessus de leurs bornes, puis la frontière
  canal/cœur pour les fichiers déplacés) ; 46ad2f5 remet tout au vert.

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 61 suites, 1 960 tests passés, 0 échec
(dont `penelope-archtest`, `docs`, `scenarios`, `rpc_golden`, `telegram_e2e`), sur macOS :
les tests `cfg(target_os = "macos")` sont compilés. `scripts/check-budget.sh b58273b` :
accepté par dérogation.

## 6. Notes de version (pour docs/progress.md)

#### Crate `penelope-app` : Services, ports et bus sous le daemon (T21)

- Nouvelle crate `penelope-app`, sous le daemon, qui ne dépend que des crates métier
  (règle d'archtest) : `Services` et son assemblage, le bus des tours (`Origin`,
  `TurnOutcome`, `TurnEvent`), l'élicitation MCP, les boucles supervisées, les ports
  (`ProviderSource`, `McpAdmin`, `Messenger`, `McpGateway`, `Orchestrator`,
  `Conversation`, `Compactor`, `ToolExecutor`), les helpers sur `&Services` et les
  doubles de test ; `codex_scope`, `machine` et `media` avec eux.
- La skill livrée `wiki-markdown` suit `reload_skills` dans la nouvelle crate.
- Le daemon réexporte tout sous les anciens chemins : CLI, évaluations et tests
  inchangés. `penelope-daemon` passe de 86 462 à 82 409 lignes.
- `penelope-app` dépend encore de `penelope-telegram` (gabarits et actions de
  `Services`, lien profond, formulaire d'élicitation) jusqu'à T36.
