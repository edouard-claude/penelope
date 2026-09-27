# Lot J : fin de `penelope-dream` (agent j-dream-fin, T26)

Suite de `j-dream.md` : l'accueil et l'ingestion étaient descendus, le rêve restait au
daemon.

## Livré

- `dream/` entier dans `penelope-dream::dream` (passe, lots, application des opérations,
  candidats, contradictions, digest, crons système, journal des passes, instantané du
  vault), en quatre commits : tests de dimensionnement sortis de `tests/batches.rs`
  (892 lignes, au-dessus des 800 admises pour un fichier nouveau) ; signatures en
  `&penelope_dream::Context` ; déplacement seul ; renommage `digest_with` en
  `digest_text`.
- `Context` suffit à tout le rêve : les corps ne lisaient du daemon que `services`,
  `embedder()` et `provider_for()`. Aucun verrou ni provider imposé à faire passer : le
  verrou de passe est une clé kv (`Services`), le provider imposé passe par
  `Context.providers` (le même `Arc<Providers>` que le daemon).
- Port `penelope_dream::DigestSource` (`async fn digest_inputs() -> DigestInputs`) :
  les crons système déclenchent le digest sans connaître l'ordonnanceur ni le
  compactage. Le daemon l'implémente (`dream::DigestFeed(Arc<Daemon>)`), avec
  `dream::digest_inputs(&Daemon)` (planifications en échec, sessions qui peinent,
  départs du jour).
- `penelope_dream::{digest_text, system_crons}` à la racine de la crate ; l'ordonnanceur
  appelle `penelope_dream::system_crons(&d.dream(), feed, …)`, ses tests
  `penelope_dream::digest_text(&d.dream(), inputs, mcp)`. Imports de `scheduler.rs`
  regroupés pour qu'il ne dépasse pas sa borne (1 095).
- Réexports de transition : `penelope_daemon::dream` = `pub use penelope_dream::dream::*`
  plus `digest_text(&Daemon, mcp)`, qui calcule les entrées puis appelle la crate
  (passerelle, compaction, evals `mem_longitudinal` l'appellent encore).
- Le digest compte les runs par `RunState::as_str()` : la crate ne dépend pas de
  `penelope-workflow` (absent de `DREAM_ALLOWED_DEPS`).
- Tests de la crate sans daemon : un `Harness` (contexte + `hooks.messenger`, déréférencé
  en `Context`) monté sur `Services::for_tests` et `MockProviders`. 35 tests du rêve
  descendus ; `a_rule_dictated_by_the_owner_is_promoted` reste au daemon
  (`dream/tests.rs`) : il passe par `chat_session_for` et `NativeToolExecutor`.
- `budget.toml` par `UPDATE_BUDGET=1` : les huit entrées `dream/*` de
  `[daemon.daemon_users]` (20) deviennent `dream/mod.rs` = 3.

## Écarts de périmètre assumés

`run` prend `&Context` : ses appelants passent `x.dream()` au lieu de `x` (RPC
`memory.rs`, passerelle `commands/memory.rs`, `wiki_e2e.rs`, evals `mem_bench` et
`mem_longitudinal`), une expression par site, comme l'ingestion au lot précédent.

## Choix

- `DigestSource` vit dans `penelope-dream`, pas dans `penelope-app` : le port porte
  `DigestInputs`, qui y vit déjà (même raison qu'au lot précédent) ; l'orchestrateur
  (T27) pourra l'implémenter à la place du daemon.
- Pas d'enveloppe `run(&Daemon)` au daemon : elle aurait gardé `Daemon` dans la
  signature publique du rêve, ce que T26 retire.
- Evals : chemins `penelope_daemon::dream::` gardés (réexport), T30 les bascule.

## Reste

- T30 : retirer `pub use penelope_dream::dream::*` et `dream::digest_text(&Daemon)` ;
  appelants sur `penelope_dream::`.
- T27 : `DigestFeed` et `digest_inputs` suivront l'ordonnanceur dans
  `penelope-orchestrator` (seul le compactage les retient au daemon).

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` : verts (74 binaires de test, archtest compris). Plafond
`[crates]` du daemon non abaissé : l'intégrateur le pose.

## Notes de version

#### Crate penelope-dream : le rêve nocturne hors du daemon

La consolidation nocturne, le digest du matin et les crons qui les déclenchent quittent
le daemon pour la crate `penelope-dream`, qui ne dépend pas de lui : ils reçoivent un
contexte (services, providers, embeddings), et le digest obtient ce qu'il lit des
planifications et du compactage par un port. Aucun comportement ne change ; les anciens
chemins restent réexportés (épopée #208, lot J, T26).
