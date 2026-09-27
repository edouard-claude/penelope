# Lot K : tentatives par un port, boucle coupée du moteur de contexte, événements typés (épopée #208, T15, T26)

Agent `k-attempts`, branche `v1-k-attempts`, base 1.0.0-alpha.12 (`3359b0d`).
Spécification : `design/v1/boucle-et-outils.md` §3.7, §4.1 et §5 (T15, T26).

## Commits

| Commit | Tâche | Quoi |
|---|---|---|
| `e8e2c70` | T15 (déplacement) | bornes de tour (`turn.rs`, `KIND_TURN_*`, `is_purged`) de `penelope-context` vers `penelope_kernel::journal` |
| `2b656d2` | T15 | `Attempt` et port `AttemptSink` ; `JournalAttempts` ; réponse vide épinglée à sa requête |
| `457e1f0` | T15 (coupure) | `Provenance.call` devient un `CallRecord` pur ; `TileMap::of(&Tiers)` devient `Tiers::tile_map()` |
| `3b11116` | T15 (déplacement) | `Provenance`, `UserSource`, `Tile`, `TileMap` descendent dans le noyau |
| `3aec1b8` | T15 | le daemon ne regrossit pas (plafond `[crates]`) |
| `0b8202c` | T15 (coupure) | `penelope_app::journal` ne réexporte plus le moteur de contexte ; archtest `reach.rs` |
| `0012ec6` | T26 | `TurnEventKind` ; les kinds de la boucle documentés dans `docs/runtime-events.md` |
| (après les notes) | T15 | dépendance morte à `penelope-context` retirée de `penelope-tools` et `penelope-workflow` |

## Ce qui est livré

### T15 : `Attempt` et `AttemptSink`

- `penelope-app/src/attempts.rs` : `Attempt` (le payload `conv.attempt`, type pur du
  noyau), trait `AttemptSink::record(session_id, &Attempt)`, `MemoryAttempts`.
- `penelope_app::journal::JournalAttempts(EventLog)` : l'implémentation de composition,
  qui écrit `ConvEvent::Attempt` comme avant (même `kind`, même payload, `"v": 1`).
  Branchée par le daemon (`agent::with_ports`) et par le banc de l'orchestrateur
  (`workflow/harness.rs`, une ligne). Elle vit dans `penelope-app` et non dans le
  daemon pour que le banc de l'orchestrateur, qui ne voit pas le daemon, écrive les
  mêmes événements.
- La boucle garde ce qui est de la politique : cause, rédaction (#134), plafond de dix,
  ligne de journal « tentative sans réponse » ; le port ne fait que garder.
  `AgentServices.attempts` ; `for_tests` branche `MemoryAttempts`.
- **Réponse vide épinglée** : `Attempts::sent(llm_id)` retient la ligne `llm_requests`
  de l'appel qui part ; une tentative sans `llm_request_id` (la réponse vide) prend
  celle-là. Les autres causes le portaient déjà.
- **La table `turn_attempts` de T16 est remplacée par le journal** (`conv.attempt`,
  lot i-tentatives, `source-de-verite.md` §2.2) : elle n'est pas créée. Plafond, purge
  de session et `penelope logs --turn` sont déjà tenus par le journal.
- Critères de T15, en mémoire (`penelope-agent/src/tests/attempts.rs`) : flux coupé après
  200 caractères, une tentative avec le texte ; trois échecs, trois tentatives chacune
  épinglée, une seule ligne d'usage ; plafond de dix ; relance vide, tentative avec sa
  consigne (`retry_prompt`, l'injection « request only ») et sa requête ;
  `request_messages()` identique avec et sans tentatives. La boucle n'écrit plus aucun
  `conv.attempt` elle-même (vérifié).

### T15 : couper `penelope-agent` de `penelope-context`

Types purs descendus dans `penelope_kernel::journal`, réexportés à l'identique par
`penelope-context` (`penelope_context::journal::*`, `penelope_context::tiers::{Tile,
TileMap}`) : `TurnReason`, `TurnEnd`, `TurnIdentity`, `TurnCall`, `started_payload`,
`finished_payload`, `interrupted_payload`, `KIND_TURN_*`, `is_purged`, `AttemptCause`,
`AttemptPayload`, `TokenUsage`, `Provenance`, `UserSource`, `Tile`, `TileMap`, et le
nouveau `CallRecord`.

Deux changements de forme, sans changement d'octet :

- `Provenance.call` portait un `AssistantPayload` entier pour n'en remplir que les
  métadonnées d'appel ; il porte un `CallRecord` (modèle, usage, coût, empreintes,
  interruption) que `AssistantPayload::of_call` verse dans le `conv.assistant`.
  `AssistantPayload::of_response` et `AttemptPayload::of_response` deviennent
  `call_record` et `of_response` dans la boucle, mêmes champs.
- `TileMap::of(&Tiers)` devient `Tiers::tile_map()`. `PromptPrefix::of(&Tiers)` garde son
  nom et ses appelants, mais son `impl` vit dans `penelope_app::journal`.

`penelope_app::journal` ne réexporte plus rien du moteur de contexte ; c'est le seul module
des ports qui lui parle (`JournalAttempts`, `PromptPrefix::of`), et la boucle ne le nomme
pas. Archtest `penelope-archtest/src/reach.rs` :
`the_agent_crate_reaches_no_context_type_through_its_ports` suit le chemin au grain des
modules (les modules de `penelope-app` que la boucle nomme, puis ceux qu'ils nomment par
`crate::`, jusqu'au point fixe) et refuse toute ligne de code qui nomme
`penelope_context` ; `a_module_that_names_the_target_is_found` vérifie qu'il détecte
(le daemon atteint `journal` et `services`).

**Ce qui reste du graphe cargo** : agent → app → context existe toujours, parce que
`penelope-app` porte `Services` (et son `ContextEngine`, `services.rs:39`) avec les ports.
Le chemin agent → tools → context, lui, est coupé : `penelope-tools` et
`penelope-workflow` déclaraient `penelope-context` sans s'en servir, la ligne est retirée
(accord de l'intégrateur). Couper le reste demande de sortir les ports de `penelope-app`
dans une crate plus basse : hors du périmètre de ce lot.

### T26 : `TurnEventKind`

`penelope-agent/src/events.rs` : neuf variantes (`turn.started`, `turn.finished`,
`turn.merged`, `turn.empty_answer`, `turn.loop_aborted`, `tool.result`, `llm.retried`,
`llm.fallback_used`, `approval.decided`), `as_str`, `draft(payload)`. Plus aucun littéral
de kind à l'écriture dans la boucle. `docs/runtime-events.md` reçoit un tableau des sept
qui n'étaient pas documentés ; `every_turn_event_kind_is_documented` refuse une variante
sans sa ligne. Écarts au §4.1 : `tool.job.*` sont écrits par le daemon (`tool_jobs.rs`,
zone `k-jobs`), pas par la boucle ; `llm.attempt` et `approval.judged` n'existent pas.
Le `turn.merged` de phase `queued` (runner.rs, engine.rs) reste un littéral du daemon.

## Scénarios

Seul `reponse-vide-relancee/expected.jsonl` bouge : sa `conv.attempt` gagne
`"llm_request_id":"{{llm:1}}"`. Aucune autre ligne, aucun `surface.jsonl`. Le contrôle
CA 4.5 ne compte plus cette tentative `unpinned`.

## Gel

- Le daemon est au plafond `[crates]` exact de la base (15 109) : l'assertion sur le
  `llm_request_id` ajoutée dans `engine/tests/attempts.rs` est retirée (le champ reste
  vérifié par la crate et par le scénario), et un commentaire de deux lignes de
  `agent.rs` tient en une pour absorber la ligne du port.
- `budget.toml` inchangé (`UPDATE_BUDGET` passé, rien à descendre). Fichiers nouveaux ou déplacés
  sous 250 lignes ; aucun `allow`.
- Écarts de périmètre, d'une ligne chacun : `penelope-orchestrator/src/workflow/harness.rs`
  (le champ `attempts`) ; `penelope-context/src/store/dual.rs` (`tiers.tile_map()`).

## Notes de version

```markdown
#### Tentatives par un port, boucle coupée du moteur de contexte (#208, lot K, T15, T26)

- **Une réponse vide est épinglée à sa requête** : sa tentative (`conv.attempt`, cause
  `empty_answer`) porte le `llm_request_id` de l'appel qui l'a rendue, comme les autres
  causes ; l'audit retrouve la requête exacte au lieu de la recalculer.
- La boucle remet ses tentatives au port `AttemptSink` ; le journal les écrit comme avant.
  La table `turn_attempts` prévue n'est pas créée : le journal la remplace.
- La boucle d'agent ne voit plus aucun type du moteur de contexte, même par les ports : le
  vocabulaire du journal qu'elle écrit (bornes de tour, provenance, tentatives, tuiles du
  préfixe) vit dans le noyau. Une règle d'architecture le vérifie.
- Les événements de la boucle sont nommés par un type, et `docs/runtime-events.md` décrit
  désormais `turn.merged`, `turn.empty_answer`, `turn.loop_aborted`, `tool.result`,
  `llm.retried`, `llm.fallback_used` et `approval.decided`.
```

## Blocages

Aucun. Reste : la coupure au niveau cargo (ports hors de `penelope-app`).
