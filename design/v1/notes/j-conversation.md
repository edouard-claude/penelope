# Lot J : crate `penelope-conversation` (épopée #208, T23 côté conversation)

Agent `j-conversation`, branche `v1-j-conversation`, base 1.0.0-alpha.10 (`d662ca4`).
Spécification : `design/v1/README.md` §3.2, `design/v1/decoupage-daemon.md` §6 (T23 et
sa ligne de dépendances). Préalables : `j-agent-crate.md` (T10), `h-vault.md` (T22),
`i-bascule.md`, `i-lecteurs.md` (T14, T15, T17).

## 0. Inventaire avant déplacement

### Ce qui part dans `penelope-conversation`

| Module du daemon | Lignes | Dépendances hors app, vault, métier | Tests |
|---|---|---|---|
| `conversation.rs` (`SessionConversation`, lecture depuis le journal par `ContextEngine::projected_entries` et `tail`) | 321 | aucune | `conversation/tests.rs`, 472 lignes, 11 tests, aucun `Daemon` |
| `compaction.rs` + `compaction/view.rs` | 1 308 + 80 | `Daemon` (champs `compaction`, `bus`, `services`, méthodes `provider_for`, `pinned_model`) ; `cache_audit::CACHE_TTL_MS` (réexport de `penelope-agent`) | `compaction/tests.rs`, 977 lignes, 17 tests |
| `titles.rs` | 215 | aucune | 1 test inline |
| `budget_alert.rs` | 222 | aucune | 1 test inline |

Chemins à réécrire (seulement des `use`) : `crate::runtime::Services`,
`crate::bus`, `crate::ports`, `crate::helpers`, `crate::codex_scope`, `crate::testing`
vers `penelope_app::…` ; `crate::agent::{Conversation, Compactor}` et
`crate::prompt_snapshot::PromptPrefix` vers `penelope_app::conversation` ;
`crate::executor::Messenger` vers `penelope_app::ports::Messenger` ;
`crate::episodes`, `crate::vault_ops`, `crate::session_project`, `crate::session_notes`
vers `penelope_vault::…`.

`build_turn_prompt`, `build_tiers`, `build_tiers_in` et les instantanés vivent déjà
dans `penelope-vault` (T22) ; `conversation.rs` les réexporte, la crate garde ce
réexport.

### Ce qui coupe la dépendance au daemon (commit de signature, avant le déplacement)

- `compaction::State` sort de `Daemon` en `Arc<State>` ; la compaction reçoit un
  `compaction::Context { services, providers, bus, state }` au lieu de `&Arc<Daemon>`.
- `Daemon::pinned_model` : son corps descend dans `penelope_app::helpers::pinned_model`
  (avec `pin_key`, à côté de `last_model_key`, déjà descendue par T06) ; le moteur
  l'appelle, la compaction aussi. C'est la coupure `compaction → engine` de la ligne de
  dépendances de T23 (`last_model_key` était déjà dans `penelope_app::helpers`).
- `CACHE_TTL_MS` : la conversation ne peut pas dépendre de `penelope-agent`. La
  constante est posée dans `penelope_app::helpers` ; un test du daemon vérifie qu'elle
  vaut celle de `penelope-agent` (qui n'est pas dans le périmètre de ce lot).

### Tests qui restent au daemon (ils ont besoin de `Daemon`)

Sept tests de `compaction/tests.rs` jouent un tour réel (`Daemon::run_turn`), le digest
du rêve ou le fork :

- `three_failures_compact_without_a_model_and_say_so` (`dream::digest_text`) ;
- `a_turn_over_the_threshold_compacts_in_the_background` (`run_turn`) ;
- `max_prompt_tokens_compacts_a_huge_window_in_the_background` (`run_turn`) ;
- `a_proven_overflow_compacts_then_retries_once` (`run_turn`) ;
- `the_billed_prompt_size_requests_a_background_compaction` (`run_turn`) ;
- `a_cold_session_is_compacted_before_the_model_call` (`run_turn`) ;
- `notes_survive_compaction_are_copied_by_fork_and_harvested_once`
  (`session_ops::fork`).

Les dix autres (fidélité, compaction manuelle, résumé différé, reprises, repli, refroidissement,
budgets) passent dans la crate avec un `Context` de test : `Services::for_tests`,
`MockProviders`, `Bus::new()`.

### Consommateurs à garder valides

Passerelle (`crate::compaction::{compact, context_view, report_text, Trigger}`,
`crate::titles::{label, clean}`, `crate::budget_alert::usd`), évaluations
(`penelope_daemon::compaction::{compact, Trigger, report_text}`,
`penelope_daemon::conversation::vault_dir`), `rpc`, `audit`, `purge`, `selfknow`,
`workflow`, `runner`, `supervisor`, `dream`. Le daemon garde `compaction.rs` en façade
(`pub use penelope_conversation::compaction::*` et les entrées en `&Arc<Daemon>`) ;
`conversation`, `titles`, `budget_alert` sont réexportés depuis `lib.rs`.

## 1. Ce qui est livré

| Commit | Quoi |
|---|---|
| `8b1ba4a` | déplacement : l'observation de fidélité sort dans `compaction/fidelity.rs` (`UPDATE_BUDGET` 1 308 → 1 208) |
| `9feab1e` | signature : `compaction::Context` au lieu de `&Arc<Daemon>`, `Daemon` tient `Arc<compaction::State>`, `pinned_model` et `pin_key` descendent dans `penelope_app::helpers`, `CACHE_TTL_MS` aussi |
| `8fb0d1e` | déplacement : `compaction/summarise.rs`, `health.rs`, `publish.rs` ; `compaction.rs` à 725 lignes, hors liste de référence |
| `224c441` | crate `penelope-conversation` : `git mv` de conversation, compaction, titres, alerte de budget ; façade du daemon ; tests répartis |
| `1011458` | archtest : `CONVERSATION_ALLOWED_DEPS` et `the_conversation_crate_sees_neither_daemon_loop_nor_channel` |

Critères de T23 (côté conversation) :

- la crate ne dépend ni de `penelope-daemon`, ni de `penelope-agent`, ni de
  `penelope-telegram` (règle et test d'archtest) ; aucun fichier au-dessus de 800
  lignes (le plus gros : `compaction.rs`, 719) ;
- `Daemon` tient `Arc<penelope_conversation::compaction::State>` ; la coupure
  `compaction → engine` est faite (`pinned_model` par `penelope_app::helpers`) ;
- 22 tests dans la crate, sans `Daemon` ; restent au daemon les sept tests listés au §0
  et `the_cache_ttl_is_the_same_for_the_loop_and_the_conversation` ;
- scénarios verts sans régénération, dont `both_history_sources_send_the_same_requests` ;
- réexports de transition : `pub use penelope_conversation as conversation` et
  `{budget_alert, titles}` dans `lib.rs` du daemon ; `compaction.rs` du daemon est une
  façade (`pub use` et `context_of`).

## 2. Choix

- **Un `Context` plutôt que des enveloppes en `&Arc<Daemon>`.** Les fonctions de la
  compaction prennent `&Context` ; les appelants écrivent `compact(&context_of(&d), …)`.
  Des enveloppes homonymes dans la façade auraient évité de toucher cinq appelants
  (rpc, audit, passerelle, deux évaluations), mais elles auraient été une API de plus à
  retirer en T30, et elles rendaient impossible un commit de signature séparé du
  déplacement (même nom, même module).
- **`OverflowCompactor` tient le `Context`** et vit avec lui (`compaction/context.rs`).
- **`CACHE_TTL_MS` en double, tenu par un test.** La boucle (`penelope-agent`) n'est
  pas dans le périmètre de ce lot : la constante est posée dans `penelope_app::helpers`
  et un test du daemon les tient égales. À unifier quand la boucle lira celle de
  `penelope-app`.
- **`MECHANICAL_MODEL` et `load_cooldown` publics**, lus par
  `three_failures_compact_without_a_model_and_say_so`, resté au daemon pour le digest.
- **Le test `docs` lit aussi `penelope-conversation/src`** pour trouver les événements
  émis (`context.compaction_skipped` y vit désormais).
- **`[channel.allowed]`** : la clé de `conversation.rs` (6 : fusion des rafales du canal
  dans `request_messages`) est renommée à la main vers
  `crates/penelope-conversation/src/lib.rs`, même budget, comme pour `penelope-agent`.
  La crate est dans `CHANNEL_AGNOSTIC_CRATES`.

## 3. Mesures

| Mesure | Avant (`d662ca4`) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 38 177 (plafond) | 35 241 |
| `penelope-conversation/src`, lignes | | 3 149 |
| `[daemon].modules` | 33 | 30 |
| `[daemon.daemon_users]` `compaction.rs` | 17 | 1 |
| `[files.oversized]` | `compaction.rs` 1 308, `engine.rs` 1 105 | `compaction.rs` sorti, `engine.rs` 1 095 |

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` sur macOS : 76 suites, 2 006 tests
passés, 0 échec (archtest, `docs`, `ca_matrix`, scénarios compris).

## 5. Reste et blocages

- Plafond `[crates]` du daemon : 38 177 dans `budget.toml`, 35 241 mesurées ; à
  abaisser par l'intégrateur.
- Hors périmètre, touchés d'une ligne par le changement de signature : passerelle
  (`commands/session.rs`), évaluations (`harness/steps.rs`, `tests/ctx_recall.rs`,
  `tests/docs.rs`), `rpc/methods/sessions.rs`, `audit.rs`.
- T30 : retirer les réexports et la façade ; `penelope-agent` lit
  `penelope_app::helpers::CACHE_TTL_MS`.
- T36 : `request_messages` lit encore `cfg.telegram.burst_*` et `Origin::Telegram`
  (fusion des rafales) : à passer derrière `ChannelDelivery`.
- La doc de tête de `lib.rs` dit encore « l'historique canonique vit en base » : à
  réécrire avec T19 (décision 0017).

## 6. Notes de version (pour docs/progress.md)

#### Conversation : crate `penelope-conversation` (épopée #208, lot J, T23)

- La conversation d'une session, la compaction de fond et son état, les titres de
  session et l'alerte de budget quittent le daemon pour la crate
  `penelope-conversation`, au-dessus de `penelope-app` et de `penelope-vault` ; une
  règle d'architecture lui interdit le daemon, la boucle d'agent et le canal.
- La compaction ne reçoit plus le daemon mais son contexte (services, providers, bus
  des tours, état) ; l'alias épinglé d'une session se lit dans `penelope-app`.
- `penelope-daemon` passe de 38 177 à 35 241 lignes. Aucun comportement visible ne
  change.
