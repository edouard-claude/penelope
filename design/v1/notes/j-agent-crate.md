# Lot J : crate `penelope-agent` (épopée #208, T10)

Agent `j-agent-crate`, branche `v1-j-agent-crate`, base 1.0.0-alpha.8.

## Livré

1. `agent/` n'importe plus `penelope_context` (`68b611f`).
2. T10 : la crate `crates/penelope-agent` (`c7fa968`), et sa règle archtest (`22ece1d`).

## Choix : le vocabulaire du journal passe par `penelope-app`

Les charges que la boucle écrit (`Provenance`, `AssistantPayload`, `AttemptCause`,
`AttemptPayload`, `ConvEvent`, `TurnIdentity`, `TurnCall`, `TurnEnd`,
`started_payload`, `finished_payload`, `interrupted_payload`, `is_purged`,
`KIND_TURN_*`) sont réexportées à l'identique par `penelope_app::journal`.

Pourquoi pas `penelope-kernel` : `AssistantPayload` et `Provenance` portent des types de
`penelope-llm` (`Content`, `ToolCall`), et `AttemptPayload::of_response` /
`AssistantPayload::of_response` sont des `impl` inhérentes sur `ChatResponse`, qui ne
peuvent vivre que dans la crate qui définit le type. Seules les bornes de tour (`turn.rs`)
auraient pu descendre ; les couper en deux n'aurait rien changé au graphe, puisque
`penelope-app` dépend de toute façon de `penelope-context`.

Pourquoi pas un port `AttemptSink` / `TurnJournal` maintenant : §4.1 le prévoit avec les
types `Attempt` et `Phase` de T15 ; l'écrire ici aurait changé la forme des écritures
avant T15. Le port `Conversation` de `penelope-app` parle déjà `Provenance` : ces
charges sont le vocabulaire de ses ports. Les payloads journalisés sont les mêmes
valeurs, construites par le même code : identiques octet pour octet par construction.

## T10

- `agent/` devient `crates/penelope-agent/src`, `agent.rs` son `lib.rs` (`git mv`, corps
  inchangés). Seuls changements : `crate::agent::` devient `crate::` ; `effect_kind`
  passe `pub` (appelé par `engine/tests/recovery.rs`) ; `decide_approval`,
  `close_unopened`, `close_interrupted_turns` sont réexportés.
- Le daemon garde `agent.rs` en façade : `pub use penelope_agent::*`, les ports sur
  `Services` (`services_of`, `DaemonJobs`) et les trois entrées en `&Services`, qui
  masquent les homonymes du glob. Aucun import ne change hors du daemon.
- Dépendances : kernel, llm, tools, hitl, observe, store, app ; externes `anyhow`,
  `async-trait`, `serde_json`, `tracing`, `tokio`, `futures` ; dev `tempfile`, `tokio`.
- `cargo check -p penelope-agent` seul passe ; 62 tests dans la crate (le critère en
  demandait 42).
- Archtest : `AGENT_ALLOWED_DEPS` (§4.2 plus `penelope-store`), et le test
  `the_agent_crate_sees_neither_context_memory_channel_nor_daemon`.

## Gel

- `crates/penelope-daemon/src/agent/` sort du daemon : le plafond `[crates]` du daemon
  redescend d'environ 6 100 lignes ; l'intégrateur peut l'abaisser.
- `[channel.allowed]` : la clé de `pipeline.rs` (`EffectKind::Telegram` dans
  `effect_kind`) suit le fichier sous `crates/penelope-agent/src/pipeline.rs`, même
  budget 1, renommée à la main avec l'accord de l'intégrateur (`UPDATE_BUDGET` retire une
  clé absente mais n'en ajoute pas).
- Aucun fichier au-dessus de 800 lignes, aucun `allow` nouveau.

## Notes de version

#### Boucle d'agent : crate `penelope-agent` (épopée #208, lot J, T10)

- La boucle d'agent et le pipeline d'outils quittent le daemon pour la crate
  `penelope-agent`, qui ne dépend ni du moteur de contexte, ni de la mémoire, ni du
  canal ; une règle d'architecture le vérifie.
- Les charges du journal qu'écrit la boucle passent par `penelope-app`, inchangées.
- Aucun comportement visible ne change.

## Reste

- Retirer les `pub use` de transition listés dans `j-agent-ports.md` avec leurs appelants.
- `AttemptSink` (T15) remplacera l'écriture directe de `conv.attempt` ; les bornes de tour
  pourront suivre le même chemin, et `penelope_app::journal` se réduira d'autant.
