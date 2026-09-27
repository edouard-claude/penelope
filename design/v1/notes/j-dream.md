# Lot J : crate `penelope-dream` (agent j-dream, T26)

## Inventaire

| Module | Lignes | Ce qui le tenait au daemon |
|---|---|---|
| `onboarding.rs` | 766 | rien : `Services`, helpers, `vault_ops`, `machine` |
| `ingest.rs` | 837 (+ 476 de tests) | `d.services`, `d.embedder()`, `d.provider_for()` ; `Messenger` déjà reçu en paramètre |
| `dream/` | 3 457 | `Daemon` 20 fois (budget), `scheduler::label`, `scheduler::due_today`, `compaction::struggling_sessions`, `d.embedder()`, `codex_scope::background`, `bus::Origin`, `hooks.messenger` |

## Livré

- Crate `crates/penelope-dream` (app, vault, kernel, llm, hitl, observe, platform, memory),
  ajoutée au workspace et à l'archtest : `DREAM_ALLOWED_DEPS` (app, vault et métier, sans
  `penelope-telegram`), présence dans `the_workspace_is_discovered`, test
  `the_dream_crate_does_not_depend_on_the_daemon`.
- `onboarding` descendu tel quel (seul l'import de `Services` change) ; réexporté par le
  daemon (`pub use penelope_dream::onboarding`).
- `penelope_dream::Context { services, providers, embeddings }` : ce que le rêve et
  l'ingestion lisent du daemon. Mêmes noms que sur `Daemon` (`services`, `embedder()`,
  `provider_for()`), les corps déplacés ne changent pas. Construit par `Daemon::dream()`
  (`runtime.rs`, à côté de `embedder()` : le gel n'admet `impl Daemon` que là).
- `ingest` : signatures en `&Context` (commit à part), puis déplacement dans la crate ;
  réexport `pub use penelope_dream::ingest::*` dans `penelope-daemon/src/ingest.rs`.
  Les appelants passent `d.dream()` (RPC `approvals`, `scheduler`, passerelle Telegram
  ×4, `wiki_e2e`, eval `mem_longitudinal`) : écart de périmètre assumé, une expression
  par site.
- `penelope_dream::DigestInputs { failing_schedules, struggling_sessions, due_today }` :
  `digest_text` (même signature) calcule ces trois entrées puis appelle `digest_with`
  (Services + `McpAdmin`), qui ne lit plus rien au-dessus du rêve.
- `budget.toml` par `UPDATE_BUDGET=1` : `onboarding` sort de la liste R5, `ingest.rs` du
  couplage au `Daemon`.

## Tests restés au daemon

Tous ceux de `ingest/tests.rs` (9) : ils construisent un `Daemon` (`from_services`,
`set_provider_override`, `chat_session_for`). Deux seraient déplaçables sans rien
(`proposals_pass_the_memory_write_filter`, `a_plain_text_answer_still_gives_a_summary`),
les autres avec un `Context` monté sur `penelope_app::testing::MockProviders`.

## Choix

- `DigestInputs` vit dans `penelope-dream`, pas dans `penelope-app` comme le proposait la
  spécification : seul le digest le lit et seul son appelant le remplit ; le poser dans
  app aurait touché une crate partagée par tous les lots sans rien découpler de plus.
- `struggling_sessions` n'est pas descendu dans `penelope-context` : il lit `Services`
  (app, au-dessus de context). Passé en donnée par `DigestInputs`, il ne bloque plus.

## Reste (T26)

1. `dream/` dans la crate : `digest_with` et le reste du digest, puis les phases. Il reste
   à remplacer `&Arc<Daemon>` par `&Context` dans `run`, `run_as`, `run_locked`,
   `consolidate`, `batches`, `apply`, `candidates`, `clash`, `nightly` (les corps lisent
   `d.services`, `d.embedder()` et, pour `nightly`, le déclenchement du digest avec
   `digest_text`, qui restera au daemon ou prendra ses `DigestInputs` en paramètre).
   `Messenger` y est déjà reçu en paramètre (`Slot<dyn Messenger>`).
2. `scheduler` citera alors `penelope_dream::{digest_text, system_crons}`.
3. `decide_approval` : aucun appel dans `dream/`, `ingest`, `onboarding` à ce jour ; rien
   à faire pour la crate.
4. Evals `mem_bench` et `wiki_e2e` : verts à ce stade.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` : verts (archtest compris, plafond du daemon inclus).

## Notes de version

#### Crate penelope-dream : accueil et ingestion hors du daemon

L'entretien d'accueil et l'ingestion de documents quittent le daemon pour la crate
`penelope-dream`, qui ne dépend pas de lui : l'ingestion reçoit un contexte (services,
providers, embeddings) au lieu du daemon entier. Le digest du matin reçoit en données ce
qu'il lisait des planifications et du compactage, première étape de la descente du rêve
nocturne. Aucun comportement ne change ; les anciens chemins restent réexportés (épopée
#208, lot J, T26).
