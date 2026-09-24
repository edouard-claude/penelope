# Notes de livraison : lot F-boucle (boucle d'agent en modules, épopée #208)

Branche `v1-f-boucle`, dérivée de `v1` à `0f1c41a`. Cinq tâches de
`design/v1/boucle-et-outils.md` §5, une par commit : T02 à T06. Périmètre tenu :
`crates/penelope-daemon/src/agent.rs` (supprimé), `crates/penelope-daemon/src/agent/**`,
`crates/penelope-archtest/budget.toml`, ce fichier. Aucun autre fichier du daemon n'est
touché ; les appels aux kv et aux aides (`crate::approval_mode`, `crate::cache_audit`,
`crate::budget_alert`, `crate::tool_jobs`) sont restés tels quels.

## Ce qui est livré

| Tâche | Commit | Contenu |
|---|---|---|
| T02 | `60ea09b` | `agent.rs` (2 513 lignes) éclaté en douze modules sous `agent/` ; déplacement pur |
| T03 | `25ad9e1` | `RetryPlan` pur (`agent/model/retry.rs`) appelé par `call_model`, table de vérité |
| T04 | `cbff065` | `TurnGuard`, `GuardVerdict`, quatre gardes de tour en chaîne fixe |
| T05 | `e828640` | `CallGuards` typées (`agent/pipeline/decide.rs`) : `DescribedCall`, `Refusal`, `Suspension`, `GuardStop` |
| T06 | `123ca91` | `PolicyStage`, `VerdictLayer` (`agent/pipeline/policy.rs`), test doré des huit raisons |

Taille des modules (lignes, tests inline compris) : `pipeline.rs` 571, `guards.rs` 344,
`model.rs` 338, `turn.rs` 330, `pipeline/decide.rs` 231, `model/retry.rs` 200,
`decisions.rs` 180, `rules.rs` 165, `pipeline/policy.rs` 149, `loop_abort.rs` 142,
`outcome.rs` 104, `spec.rs` 96, `conversation.rs` 84, `executor.rs` 79, `mod.rs` 59,
`pending.rs` 30. Aucun au-dessus de 800.

### T02 : le découpage

Les corps sont identiques ligne pour ligne (vérifié par comparaison des lignes non vides
de l'ancien fichier et des nouveaux). Seules changent les visibilités qu'exige un appel
entre modules frères : `Pending`, `call_model`, `resolve_pending`, `answer_after_loop`,
`turn_limits`, `delegation_nudge` passent en `pub(super)`. `agent/mod.rs` garde l'en-tête
d'imports de l'ancien fichier (les sous-modules font `use super::*;`) et réexporte chaque
élément public à son ancienne visibilité : tous les chemins `crate::agent::*` restent
valides, aucun appelant ne change. Les tests (`agent/tests/`, `clone_policy_tests.rs`)
n'ont pas bougé.

Écart à la table de §4.1 : `effect_kind` et `server_of` sont dans `pipeline.rs` (la spec
les met dans `pipeline/effect.rs`, tâche T08) ; `call_intention`, `turn_goal`,
`without_intention`, `line_shape` aussi (T07 et T08 les déplaceront).

### T03 : `RetryPlan`

`RetryPlan::on_error(erreur, phase, arrêt demandé) -> RetryAction::{RetrySame { wait_s },
Fallback { model_id }, GiveUp}`, avec `Phase::{BeforeStream, InStream, AfterText}`. Les six
variables mutables de `call_model` sont l'état du plan. `call_model` garde tous les effets
(machine d'état LLM, `llm.retried`, attente annulable, messages d'échec). Une table de
quinze cas couvre : attente unique tant qu'un repli reste, budget complet sur le dernier
candidat, attente doublée, `Retry-After` court honoré et long ignoré, arrêt demandé (pas
d'attente, le repli reste permis avant le flux), OpenRouter (replis d'alias pris en main),
flux coupé avant texte (un essai puis repli), après texte (jamais de repli). `call_model`
repasse sous 200 lignes : son `#[allow(clippy::too_many_lines)]` est retiré (27 → 26).

Choix : la phase `AfterText` est dans le plan (elle rend toujours `GiveUp`) pour que la
table dise aussi « jamais de repli silencieux » ; le message « coupée en cours
d'écriture » reste dans `call_model`.

### T04 : gardes de tour

`default_chain()` = `CancelGuard`, `BudgetGuard`, `CallCapGuard`, `CostCheckpointGuard` ;
`run_guards` s'arrête à la première qui ne rend pas `Proceed`. `TurnContext` porte
l'agent, la spécification, le sink, l'itération et le coût. Le contrôle d'annulation du
début d'itération (avant `resolve_pending`) reste en place ; celui d'après devient
`CancelGuard`. Seul écart de comportement : `CallCapGuard` et `CostCheckpointGuard` lisent
chacun `turn_totals` (une lecture de plus par itération quand le tour a un `turn_id`).

### T05 : gardes d'appel

`call_chain()` = `Allowlist`, `PriorDecision`, `LoopGuard`, `Precheck`, sur un
`DescribedCall`, avec un `CallContext` qui porte le détecteur de boucles en `&mut`.
`GuardStop::Approved` est la seule sortie qui n'est pas un refus : la décision déjà prise
par le propriétaire, définitive comme avant (l'appel ne repasse ni par la garde de boucle
ni par la politique). La spec ne la prévoyait pas ; sans elle, `PriorDecision` ne pouvait
pas se couler dans `Option<GuardStop>`.

### T06 : couches de politique

`VerdictLayer::{Rule { id }, Default, ServerDeclaration, DeclaredAllow, SessionMode,
SensitiveConfig}`. La spec listait aussi `Floor(Destructive)` et `Judge` : aucune couche
d'aujourd'hui ne produit l'un ou l'autre (le destructif n'est qu'une condition du mode
`auto`), ils viendront avec leur couche (#203). Le réseau reste une annotation de la
raison, sans couche propre. Le test doré passe par `PolicyStage::evaluate` sur de vrais
services (règles, configuration, mode de session), pas par une fonction pure : les
raisons des couches « règle » et « déclaré » viennent de `penelope-hitl` et
d'`approval_mode`, c'est là qu'elles doivent être lues.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` : fmt et clippy propres ; 1 878 tests passent, 3 échouent, tous
dans `penelope-archtest` et tous dus au budget du gel (section suivante). Les 42 tests d'agent passent sans
modification ; 11 tests s'ajoutent (table `RetryPlan` et trois voisins, ordre des gardes
de tour et d'appel, textes de refus, huit raisons).

## Gel

`UPDATE_BUDGET=1` retire `agent.rs` de la liste de référence et abaisse
`allow_too_many_lines` à 26. Trois points demandent une dérogation (arbitrage demandé au
lead) : treize noms de modules nouveaux dans `[daemon].modules` (R5 est une liste de noms
pour tout le daemon), l'entrée canal `agent.rs = 1` (`EffectKind::Telegram`) qui passe à
`agent/pipeline.rs`, et le plafond de `penelope-daemon` (R4) dépassé de 947 lignes, surtout
par les tests nouveaux. Ce dernier point revient à zéro quand la boucle sort en crate
(T10).

## Notes de version

#### Boucle d'agent en modules (épopée #208, lot F : T02 à T06)

- `agent.rs` (2 513 lignes) devient `agent/` : douze modules, le plus gros à 571 lignes,
  tous les chemins `crate::agent::*` inchangés. Le fichier sort de la liste de référence
  du gel.
- La politique de nouvelles tentatives d'un appel au modèle est une table pure,
  `RetryPlan`, testée cas par cas ; `call_model` repasse sous 200 lignes.
- Avant chaque appel au modèle, quatre gardes nommées dans un ordre fixe : arrêt demandé,
  budget, plafond d'appels, palier de coût.
- Avant la politique, quatre gardes d'appel nommées : liste blanche, décision antérieure,
  garde de boucle, arguments ; les textes de refus sont typés (`Refusal`) et inchangés.
- La politique d'un appel dit quelle couche a tranché (`VerdictLayer`) ; ses huit raisons
  sont fixées par un test doré.
- Aucun comportement visible ne change : textes, cartes, événements et ordre des
  contrôles sont ceux de la 1.0.0-alpha.2.

## Blocages

- Budget du gel : les trois dérogations ci-dessus attendent l'accord du lead ; tant
  qu'elles ne sont pas posées, `penelope-archtest` échoue sur R4, R5 et la frontière canal.
- T07 à T27 non commencées (hors brief).
