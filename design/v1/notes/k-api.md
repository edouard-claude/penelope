# Lot K : couture PTC, nettoyage d'API, cache audit (épopée #208, T24, T25, T27)

Agent `k-api`, branche `v1-k-api`, base 1.0.0-alpha.13 (`12b187e`).
Spécification : `design/v1/boucle-et-outils.md` §3.8, §5 (T24, T25, T27).

## Livré

| Tâche | Commit | Critère | Preuve |
|---|---|---|---|
| T24 (préparation) | `cf5bb63` | la décision d'un appel se teste seule | `decide_call` sorti de `resolve_pending`, qui perd son `allow(too_many_lines)` (`[lints]` 22 → 21) |
| T24 | `ed2af5b` | un appel avec `parent` et verdict `Ask` est refusé sans carte | `tests/call_guards.rs` : `a_nested_call_that_asks_is_refused_without_a_card` ; décision 0016 |
| T25 | `ff20d32` | aucun appelant hors tests ; tests migrés | `TurnRequest`, `AgentLoop::run`, `resume_after_approval` supprimés ; `cargo check --workspace --all-targets` vert |
| T27 (déplacement) | `5e6b2ff`, `e245918` | parties pures dans `penelope-llm` | `penelope-llm/src/cache.rs`, test `a_miss_is_explained_by_what_changed` déplacé avec |
| T27 | `37f57dd` | le port `CacheAudit` disparaît ; `ca_5_4` inchangé | `BudgetLedger::previous_call`, test `the_previous_call_is_the_last_chat_call_of_the_session` ; `ca_5_4_each_request_extends_the_previous_one` vert sans modification |

## Choix

- **Deux `CallContext`** : le nom existait déjà pour le contexte des gardes d'appel
  (`pipeline/decide.rs`). Il devient `GuardContext` ; `CallContext { call_id, parent,
  root }` et `CallId` (newtype sur `String`) sont publics dans `penelope_agent`, pour qu'un
  futur exécuteur `run_code` construise ses sous-appels par `CallContext::child`.
- **Où vit la règle** : `nested_ask` dans `decide.rs`, appelée dans `decide_call` juste
  après la politique et avant la création de la carte. Le refus (`Refusal::NestedApproval`)
  revient au modèle comme résultat : « Non exécuté : un appel imbriqué ne peut pas demander
  d'approbation (raison de la politique). Propose une autre approche ou demande. » Un refus
  de la politique (`Deny`) reste un refus de politique ; un `Auto` passe.
- **Le test passe par `decide_call`**, pas par un tour entier : rien ne crée encore d'appel
  imbriqué, et le tour ne sait pas en recevoir. Il vérifie aussi le témoin : le même
  appel, racine, pose sa carte.
- **Décision 0016, pas 0012** : 0012 est prise (jobs d'outils durables). Format de 0015.
- **T25** : `resume_after_approval` n'avait aucun appelant, pas même dans le daemon ; les
  trois tests qui l'appelaient passent par `decide_approval`. Les tests construisent un
  `TurnSpec` (`request()` = `spec()` sans tour d'origine) et un `MemoryConversation` par
  l'aide `run_memory`, définie dans `tests/mod.rs` : même prompt, même demande qu'avant.
- **T27, `PreviousCall` dans le kernel** : `previous_call` lit la table `usage` que le
  `BudgetLedger` écrit ; le kernel ne dépend pas de `penelope-llm`, donc le type descend au
  kernel et `penelope_llm::cache` le réexporte. `cache_audit::previous_call(s, sid)` reste
  un relais d'une ligne : le moteur (`engine.rs`, hors périmètre) et `ca_5_4` l'appellent
  tel quel.
- `budget.rs` était à 936 lignes : ses tests sont sortis dans `budget/tests.rs` avant d'y
  ajouter la lecture (déplacement pur, commit séparé).

## Hors périmètre touché

- `docs/ca-matrix.md` (`66f2513`) : régénérée par `UPDATE_CA_MATRIX=1`, `ca_10_4` suit les
  tests de `budget.rs` dans `budget/tests.rs`.

- `crates/penelope-daemon/src/agent.rs` : `with_ports` perd son paramètre `cache` (deux
  lignes), conséquence directe de la disparition du port.
- `crates/penelope-orchestrator/src/workflow/harness.rs` (harnais de test) : la ligne
  `cache: Arc::new(NoAudit)` retirée.

## Notes de version

#### Boucle : couture PTC, API réduite, cache audit sans port (épopée #208, lot K)

- **Couture PTC** (décision [0016](decisions/0016-ptc-hors-v1.md)) : `run_code` reste hors
  V1. Chaque appel porte son `CallContext { call_id, parent, root }` dans le pipeline ; un
  appel imbriqué dont la politique demanderait une approbation est refusé sans carte, car
  une approbation suspend le tour, pas un programme en vol.
- **API de la boucle** : `TurnRequest`, `AgentLoop::run` et l'alias
  `resume_after_approval` sont retirés ; un tour se lance par `TurnSpec` et une
  `Conversation`, une approbation se tranche par `decide_approval`.
- **Cache de prompt** : l'empreinte, le fournisseur collant et la cause d'un raté vivent
  dans `penelope_llm::cache` ; le dernier appel d'une session se lit dans le
  `BudgetLedger` (`previous_call`). Le port `CacheAudit` disparaît. Aucun changement
  visible : même requête, mêmes causes de raté.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast` : 2 038 tests, 0 échec, scénarios compris sans
régénération ; `cargo test -p penelope-evals --test docs` vert (documentation embarquée
inchangée hors décision 0016 et index).

## Blocages

Aucun.
