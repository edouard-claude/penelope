# Notes de livraison : lot J, ports de la boucle (T09, épopée #208)

Branche `v1-j-agent-ports`, dérivée de `v1` à la 1.0.0-alpha.7 (`c5ecc92`).
Spécification : `design/v1/boucle-et-outils.md` §3.1, §4.1, §4.4, §5 (T09) ; charte
`design/v1/README.md` §3.2 et §3.3. Préalables : `f-boucle.md` (T02 à T06), `h-app.md`
(T21).

## Ce qui est livré

| Commit | Nature | Contenu |
|---|---|---|
| `dde92b5` | déplacement | `usd` dans `penelope_kernel::budget` ; `wants_network`, `call_arguments`, `effective_arguments` dans `agent/executor.rs` ; `ApprovalMode`, `local_draft_allow`, `declared_allow` dans `agent/pipeline/policy.rs` ; parties pures de `cache_audit.rs` dans `agent/cache.rs` ; tests qui passent par `Daemon` vers `engine/tests/` |
| `694f08f` | déplacement | `agent/mod.rs` devient `agent.rs` : la racine du module sort du répertoire, elle portera la façade |
| `695d6c6` | signature | `agent/ports.rs` : `AgentServices`, traits `SessionModes`, `SessionInfo`, `PromptSnapshots`, `CacheAudit`, `JobRunner` ; implémentations daemon ; façade ; tests de la boucle sur `AgentServices::for_tests` |

Critères de fin de T09 :

- `grep -rn 'crate::' crates/penelope-daemon/src/agent/` ne rend que des
  `crate::agent::` ; les chemins du daemon ne restent que dans `agent.rs`, la façade.
- `AgentServices::for_tests(dir, clock)` ouvre `Store::open_memory` : les tests de
  `agent/tests/` et `clone_policy_tests.rs` n'ouvrent plus ni `Services` ni `Daemon`.
- Comportement inchangé : aucun texte, événement ni ordre de contrôle ne bouge ; les
  tests de la boucle passent avec leurs assertions d'avant (un seul corps change, voir
  plus bas).

### `AgentServices`

Champs : `store`, `clock`, `events`, `effects`, `config`, `budget`, `catalog`,
`llm_state`, `approvals`, `policies` (les registres que la boucle lit et écrit
directement, types de `penelope-kernel`, `penelope-llm`, `penelope-hitl`,
`penelope-store`), puis cinq ports en `Arc<dyn …>` :

| Port | Méthodes | Implémentation daemon | Implémentation de test |
|---|---|---|---|
| `SessionModes` | `of_session` | `approval_mode::KvModes` (kv de la session) | `MemoryModes` (table en mémoire, `set`) |
| `SessionInfo` | `kind` | `SessionStore` lui-même (impl dans `ports.rs`) | `SessionStore` sur la base en mémoire |
| `PromptSnapshots` | `record`, `prefix_cause` | `prompt_snapshot::StoredSnapshots` | `NoAudit` |
| `CacheAudit` | `previous_call` | `cache_audit::UsageAudit` | `NoAudit` |
| `JobRunner` | `maybe_spawn` | `DaemonJobs` (façade, appelle `tool_jobs::maybe_spawn`) | `NoJobs` |

`JobRunner` n'était pas dans la liste de la tâche, mais `crate::tool_jobs::maybe_spawn`
était le dernier appel au daemon depuis `pipeline.rs` ; la spécification le range dans
`ports.rs` (§4.1). `JobRequest` y descend, `tool_jobs.rs` le réexporte.

### La façade

`agent.rs` est la racine du module. Jusqu'au commentaire « Façade du daemon », elle
est ce qui deviendra `lib.rs` de `penelope-agent` (en-tête d'imports, modules,
réexports). Après, ce qui reste au daemon (§4.4) :

- `services_of(&Arc<Services>) -> Arc<AgentServices>` : les **mêmes** registres que
  `Services` (clones de poignées, pas de reconstruction : le `BudgetLedger` garde
  l'observateur d'alerte posé par le daemon), ports du daemon. Appelé par `engine.rs`
  et `workflow/step_agent.rs` (une ligne chacun : `AgentLoop::new(services_of(&s), …)`).
- `decide_approval`, `close_unopened`, `close_interrupted_turns` avec `&Services`, sous
  leur nom : aucun des vingt appelants (telegram, rpc, évaluations, tests d'engine, de
  workflow, d'ingestion) ne change. Ces trois entrées ne lisent aucun port : elles
  reçoivent les registres du daemon avec des ports neutres (`registries_of`), parce que
  `rpc/methods/approvals.rs` n'a qu'un `&Services` (pas d'`Arc` pour un `JobRunner`).
- `DaemonJobs` : l'implémentation de `JobRunner` (dans la façade plutôt que dans
  `tool_jobs.rs`, qui est sur la liste de référence du gel et ne pouvait pas grossir).

À l'intérieur d'`agent/`, les noms de la façade masquent ceux des sous-modules :
`spec.rs` appelle `decisions::decide_approval`, les tests importent
`decisions::decide_approval` et `turn_log::close_unopened` explicitement.

### Tests déplacés ou touchés

- Vers `engine/tests/recovery.rs` : `a_turn_open_at_the_crash_is_closed_as_interrupted_then_replayed`
  (ancien `agent/tests/recovery.rs`), `crashed_push` et les trois tests d'effets
  incertains (#83) qui en dépendent. Tous passent par `Daemon::recover`.
- Vers `engine/tests/attempts.rs` : les deux tests de tentatives qui passent par un
  tour de la file (`Daemon::enqueue_message`, `run_turn`).
- Les corps sont inchangés, sauf `AgentLoop::new(s.clone(), …)` qui devient
  `AgentLoop::new(crate::agent::services_of(&s), …)` et l'exécuteur de comptage recopié
  sans son cas d'échec (inutilisé par ces tests).
- `the_eight_policy_reasons_are_byte_identical` fixe le mode des deux sessions par
  `MemoryModes::set` au lieu d'`approval_mode::set` (kv) ; les huit raisons attendues
  sont identiques. La lecture du kv reste couverte par les tests d'`approval_mode.rs` et
  les scénarios.

## Préparer T10 : dépendances exactes de `penelope-agent`

Relevé sur le code d'`agent/` et la partie haute d'`agent.rs` (hors façade).

Normales :

- `penelope-kernel` (effets, budget dont `usd`, risque, événements, config, ids,
  horloge, sessions, `canonical::sha256_hex`) ;
- `penelope-llm` (types, `Provider`, `collect_stream_observed`, `LlmStateMachine`,
  `Catalog`, `catalog::strip_provider`, `RequestKeys`) ;
- `penelope-tools` (`LoopDetector`, `ToolOutcome`, `ToolError`, `ToolResult`, `shell`,
  `git::normalize_clone_url`) ;
- `penelope-hitl` (`ApprovalStore`, `PolicyEngine`, `Decision`, `cmdline`, `policy`) ;
- `penelope-observe` (métriques) ;
- `penelope-store` : **absente de §4.2**, nécessaire (`Store` dans `AgentServices`,
  `Store::open_memory` pour `for_tests`, requête SQL de `close_interrupted_turns`) ;
- `penelope-app` (charte §3.2) : `Conversation`, `Compactor`, `MemoryConversation`,
  `TurnOutcome` et compagnie, `ToolExecutor`, `CallInfo`, `PromptPrefix` ;
- **`penelope-context`, à couper avant T10** : `journal::{Provenance, AttemptCause,
  AttemptPayload, ConvEvent, AssistantPayload}` (`turn.rs`, `model.rs`, `attempts.rs`,
  `pipeline.rs`, `loop_abort.rs`) et les charges de bornes de tour (`turn_log.rs` :
  `TurnIdentity`, `started_payload`, `finished_payload`…). Le module `journal` dépend
  d'`anchors`, `compaction`, `tiers` : il ne descend pas tel quel. Deux voies : les
  charges du journal (types serde purs) descendent dans `penelope-kernel` ou
  `penelope-app`, ou un port `AttemptSink` / `TurnJournal` les écrit (§4.1 prévoit
  `AttemptSink`). La règle archtest de §4.2 le refuserait sinon.
- Externes : `anyhow`, `async-trait`, `serde_json`, `tracing`, `tokio`, `futures`.
  Ni `reqwest`, ni `url` (les `url` du code sont des clés JSON).

Développement : `tokio` (`macros`, `rt`, `test-util` pour `start_paused`), `tempfile`,
`penelope-llm` (`mock` est public, pas de feature), `penelope-kernel`
(`clock::TestClock`).

Règle archtest à poser en T10 : `m.insert("penelope-agent", vec!["penelope-kernel",
"penelope-llm", "penelope-tools", "penelope-hitl", "penelope-observe",
"penelope-store", "penelope-app"])`.

Autres points pour T10 :

- `effect_kind` (`pub(crate)`) est utilisé par `engine/tests/recovery.rs` : il devra
  être `pub` dans la crate.
- `pub use` de transition posés par ce lot (à retirer avec les appelants) :
  `executor::{wants_network, call_arguments, effective_arguments}`,
  `approval_mode::{ApprovalMode, declared_allow, local_draft_allow}`,
  `cache_audit::{Fingerprint, PreviousCall, Observed, sticky_upstream, miss_cause,
  CACHE_TTL_MS, STICKY_MS}`, `tool_jobs::JobRequest`, `budget_alert::usd`.
- T27 prévoit les parties pures du cache dans `penelope-llm` : elles sont aujourd'hui
  dans `agent/cache.rs`, prêtes à partir d'un bloc.

## Hors périmètre, touché à la marge

`executor/defs.rs`, `executor/precheck.rs`, `executor/mod.rs` (les trois fonctions
sortent, remplacées par des réexports), `tool_jobs.rs` (`JobRequest` réexporté),
`engine.rs:599`, `workflow/step_agent.rs:100,295` (une ligne chacun),
`engine/tests/mod.rs` (deux `mod`). Aucune ligne de `history_*`, `purge.rs`, `audit`,
`doctor/`, `upgrade/`, `hermes`.

## Gel

- `UPDATE_BUDGET=1` abaisse `tool_jobs.rs` de 1 666 à 1 655.
- Rouge toléré, à poser par l'intégrateur : `crates_stay_under_their_ceiling`,
  `penelope-daemon/src` à 54 294 lignes pour un plafond de 53 836 (+458 : `ports.rs`,
  la façade, les aides de test recopiées dans `engine/tests/`). Revient à zéro en T10,
  quand `agent/` sort du daemon.
- Aucun module de premier niveau nouveau (R5 inchangé), aucun `allow` nouveau.

## Notes de version

#### Boucle d'agent : ports (épopée #208, lot J, T09)

- La boucle ne reçoit plus tous les services du daemon mais `AgentServices` : les
  registres qu'elle touche et cinq ports (mode d'approbation d'une session, nature d'une
  session, instantanés du prompt, dernier appel pour le cache, jobs d'outils).
- Plus aucun chemin du daemon dans `agent/` : le répertoire est prêt à devenir la
  crate `penelope-agent` (T10). Les tests de la boucle tournent sur une base en
  mémoire, sans daemon.
- Le formatage des montants (`usd`) descend dans `penelope-kernel`.
- Aucun comportement visible ne change.

## Vérifications

`cargo fmt --all --check` et `cargo clippy --workspace --all-targets -- -D warnings`
propres. `cargo test --workspace --no-fail-fast` : tout vert (dont `docs`,
`resilience`, `scenarios`, sans régénération) sauf `crates_stay_under_their_ceiling`,
rouge toléré ci-dessus.

## Reste et blocages

- T10 : couper `penelope-context` (voir plus haut) avant de créer la crate.
- Aucun blocage.
