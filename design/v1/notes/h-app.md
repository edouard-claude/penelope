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
