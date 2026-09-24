# Lot D fin, décrocher `Daemon` : ports du daemon (T07, T08, T09, T10)

Branche `v1-h-ports`, dérivée de `v1` au commit fbf1fe5 (1.0.0-alpha.4). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §1.2, §2.2 (ports), §6 (T07 à T10), §8.
Suite de `design/v1/notes/d-kv-helpers.md` (T05, T06).

## 1. Ce qui est livré

### T07 : `ProviderSource`, `Handle`, `Supervision`

Nouveau module `ports` du daemon (`crates/penelope-daemon/src/ports.rs`, 137 lignes) :

- `ProviderSource` (`provider_for`, `provider_override_active`). Le cache des providers
  quitte les champs privés de `Daemon` pour `runtime::Providers`, que `Daemon` tient en
  `Arc` (`d.providers`) ; `Daemon::provider_for`, `set_provider_override`,
  `invalidate_providers` délèguent, leurs appelants ne bougent pas.
- `Handle` : `DaemonHandle` déplacé tel quel (commit à part), puis renommé ;
  `DaemonHandle::new` le construit.
- `Supervision` (registre des boucles, `Handle`, horloge, journal) :
  `tasks::spawn_supervised`, `report_panic` et `tasks::doctor_check` le prennent ;
  `Daemon::supervision()` le compose.

Critère (`sigs.pl`, fonctions dont un paramètre est `Daemon`, hors tests) sur les dix-huit
modules de T07 : **67 avant, 0 après** (concepts 6, vault_inventory 1, vault_git 1,
embeddings 6, mem_split 2, mem_audit 3, review 2, episodes 6, titles 2, vision 2, images 1,
voice 4, purge 4, backup 9, onboarding 12, session_ops 6 ; vault_ops et session_notes
étaient déjà à 0). Ils reçoivent `&Services` et, selon le besoin, `&dyn ProviderSource`
(`Arc` pour ce qui part en fond), un `embeddings::Embedder` (services, providers, état ;
`Daemon::embedder()`), le `Bus` (session_ops : annulation, tour actif). `codex_scope`
et `dream::vault_sync` prennent `&Services`. `onboarding::rpc` reçoit la session CLI en
futur, attendu seulement par `onboard.write`, comme avant.

### T08 : `Messenger` reçu explicitement

Lectures de `hooks.` dans le code (hors tests) des modules autres que runtime, engine,
runner, supervisor, rpc, telegram, mcp : **32 avant, 0 après** (backup, budget_alert,
codex_auth, codex_quota, compaction, dream, hermes, ingest, mcp_auth, mem_audit,
scheduler, titles, upgrade, voice, workflow). Mesure :
`scratchpad/h/hooks.sh` (coupe au premier `#[cfg(test)]`, ignore `tests.rs`, `tests/`,
`*_e2e.rs`).

- `ports::Slot<T>` : la case partagée d'un branchement tardif. Les champs de `Hooks`
  deviennent des `Slot` (`read()` et `write()` gardés). Les boucles partent avant
  Telegram et MCP (`supervisor.rs`) : un `Option` lu à leur lancement serait vide pour
  toujours, elles reçoivent donc le `Slot` et le lisent au moment d'envoyer.
- Feuilles : `Option<Arc<dyn Messenger>>` (codex_quota, codex_auth, budget_alert,
  mcp_auth, hermes, backup, voice).
- `scheduler::Ports` (canal, livraison, MCP, orchestrateur), composé par
  `Hooks::scheduler()`, suit `tick`, `run_now`, `fire`, `alert`, `trigger_outcome*`.
- `compaction::State` et `workflow::State` reçoivent leurs `Slot` de
  `Daemon::from_services` (`workflow::Ports`, `Hooks::workflow()`).
- `Hooks.telegram` devient `Hooks.delivery` (le port qu'il porte) ; `Hooks::telegram()`
  reste, `Hooks::delivery()` s'ajoute.

### T09 : `McpAdmin`

`ports::McpAdmin: McpGateway` (état, invalides, dossier, détail, prompts, journaux,
redémarrage, essai, configuration, ajout, édition, activation, retrait, rechargement,
tâches, avis) ; `McpSupervisor` l'implémente par délégation (`mcp/admin.rs`).
`Hooks.mcp_supervisor` est un `Slot<dyn McpAdmin>`, `set_mcp` accepte tout `McpAdmin`.
Mentions de `McpSupervisor` hors de `mcp/` et `supervisor.rs` (tests compris) :
**17 avant, 0 après** ; les tests construisent par `mcp::testing::supervisor`.

### T10 : doubles partagés

Module public `testing` (139 lignes) : `RecordingMessenger` (textes avec origine,
approbations ; `with_cards` range à part cartes et questions, sinon repli du trait) et
`MockProviders` (`ProviderSource` sur un `MockProvider`). Les **sept** `Recorder`
(budget_alert, supervisor, ingest, compaction, scheduler, workflow, dream) sont
supprimés. `executor::question_text` sort du défaut de `send_question` pour que double et
trait rendent le même texte. Les tests d'images et de la première revue passent par
`MockProviders` sans construire de `Daemon`.

## 2. Mesures

| Mesure | Avant (fbf1fe5) | Après |
|---|---|---|
| `sigs.pl`, `fn_Daemon` des 18 modules de T07 | 67 | 0 |
| `hooks.` hors colle, code seul | 32 | 0 |
| `McpSupervisor` hors `mcp/` et `supervisor.rs` | 17 | 0 |
| `[daemon.daemon_users]`, somme | 254 | 148 |
| copies de `Recorder` | 7 | 0 |
| `[files.oversized]` : compaction, hermes, scheduler, upgrade | 2 225, 1 971, 1 818, 2 018 | 1 308, 1 210, 1 095, 1 203 |
| `[files.oversized]` : mcp_auth, ingest | 1 246, 1 225 | sortis |
| `[channel.allowed]` : runtime, scheduler, titles | 12, 33, 1 | 10, 32, sorti |

## 3. Choix

- `Slot` plutôt que `Option<Arc<dyn Messenger>>` partout (écart à §2.2) : l'ordre de
  démarrage l'impose pour les boucles. Les feuilles reçoivent bien un `Option`.
- `compaction` et `workflow` reçoivent leurs branchements par leur `State` : c'est ce que
  `Daemon` leur transmettra une fois en crate (T23, T27), sans enfiler un paramètre dans
  vingt fonctions. `scheduler` et `dream`, sans état, les reçoivent en paramètres.
- Moments de lecture qui bougent, sans effet puisque les branchements sont posés au
  démarrage : `titles::spawn` lit la livraison au lancement du titre (avant : à sa fin) ;
  `backup::nightly_tick`, `hermes::rpc` et `mem_audit::run` lisent leur canal ou leur
  superviseur à l'appel (avant : après le travail).
- Place faite aux signatures sans relever la liste de référence : tests de hermes,
  mcp_auth, ingest, upgrade, scheduler, compaction dans `<module>/tests.rs` ;
  `hermes/yaml.rs`, `scheduler/templating.rs`, `compaction/view.rs` en sous-modules.
  Commits de déplacement seul. codex_auth garde ses tests inline : le test
  `the_budget_file_is_readable` exige au moins 30 entrées dans la liste de référence,
  il en reste 30.
- Hors périmètre, pour suivre un déplacement : `docs/ca-matrix.md` régénéré (ca_2_8 dans
  `upgrade/tests.rs`) ; `crates/penelope-evals/tests/docs.rs` lit les sources du daemon
  récursivement dans `the_context_page_follows_the_code` (sinon `models.routing.fallback`,
  cité par `compaction/tests.rs`, n'était plus trouvé). `penelope-evals` suit les
  signatures de purge, session_ops, dream::run, digest_text.
- `runtime.rs` : changements limités au type `Providers` (ex-corps de `provider_for`),
  à `Hooks` et à `from_services` ; rien de déplacé hors `DaemonHandle`.

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 57 suites, 1 931 tests passés, seuls
rouges les deux tolérés de `penelope-archtest` (ci-dessous). `scenarios` vert sans
régénération. Sur macOS : les tests `cfg(target_os = "macos")` sont compilés.

## 5. Reste et blocages

- Rouges tolérés : `daemon_modules_are_whitelisted` (`ports` et `testing` à ajouter à
  `[daemon].modules`) et `crates_stay_under_their_ceiling` (85 394 lignes pour 84 819).
- Les tests des modules hors colle composent encore leur daemon par `d.hooks` (89 lignes :
  scheduler, compaction, workflow, dream, wiki_e2e, ticket_to_deploy_e2e).
- Les tests de concepts, session_notes, embeddings, mem_audit, episodes, purge, backup,
  session_ops construisent encore un `Daemon`, surtout pour `chat_session_for` (port
  `TurnIntake`, différé en T33).
- Modules du périmètre de T07 qui prennent encore `Daemon` : ingest, dream, compaction,
  workflow, scheduler, supervisor (hors du critère `sigs.pl`).
- Bissection : de 92ee7e6 à 6c42ec4 exclu, `penelope-evals --test docs` est rouge
  (lecture à plat des sources, corrigée par 6c42ec4).

## 6. Notes de version (pour docs/progress.md)

#### Ports du daemon : providers, supervision, canal, MCP (T07 à T10)

- Nouveau module `ports` : `ProviderSource`, `Handle` (ex `DaemonHandle`), `Supervision`,
  `Slot` (branchement posé après le démarrage) et `McpAdmin`.
- Dix-huit modules (vault, concepts, embeddings, épisodes, revue, titres, vision, images,
  voix, purge, sauvegarde, accueil, sessions…) ne prennent plus `&Daemon` : 67 fonctions
  passent à `&Services` et aux ports.
- Plus aucun module hors de la colle ne lit `d.hooks` : canal, livraison, MCP et
  orchestrateur sont reçus en paramètre, par un `Slot` pour les boucles de fond, ou par
  l'état de la compaction et des workflows.
- `McpSupervisor` n'est plus nommé hors de `mcp/` et du superviseur : ses consommateurs
  passent par `McpAdmin`.
- Doubles de test partagés dans `testing` : `RecordingMessenger` remplace sept copies,
  `MockProviders` sert un `MockProvider` comme `ProviderSource`.
- Tests de six modules sortis dans `<module>/tests.rs` ; `hermes::yaml`, les gabarits de
  l'ordonnanceur et `context_view` en sous-modules. Occurrences de `Daemon` 254 → 148.
