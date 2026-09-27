# Lot J : crate `penelope-orchestrator` (épopée #208, T27 et T11)

Agent `j-orchestrator`, branche `v1-j-orchestrator`, base 1.0.0-alpha.11 (`8f701ff`).
Spécification : `design/v1/decoupage-daemon.md` §3.2, §6 T27 et sa ligne de dépendances ;
`design/v1/boucle-et-outils.md` §5 T11. Préalables : T10 (`j-agent-crate.md`), T23
(`j-conversation.md`), T24 (`j-executor.md`), T26 (`j-dream-fin.md`).

## 0. Inventaire avant déplacement

### Ce qui part

| Module du daemon | Lignes | Ce qu'il lit du daemon |
|---|---|---|
| `workflow/` (10 fichiers hors tests) | 2 787 | `d.services` ; `d.workflows` (état, ports) ; `d.provider_for` ; `d.embedder()` ; `d.providers` (images, vision) ; `d.handle.is_shutting_down` ; `d.clone() as Arc<dyn Admin>` (exécuteur d'étape) ; `agent::services_of` (ports de la boucle sur les modules du daemon) |
| `scheduler.rs` + `scheduler/templating.rs` | 1 095 + 63 | `d.services` ; `d.handle` ; `d.bus.notify_enqueued` ; `d.dream()` ; `dream::DigestFeed(d)` |
| `dream/mod.rs` : `digest_inputs`, `DigestFeed` | 40 | `scheduler::label`, `scheduler::due_today`, `compaction::struggling_sessions` : que `Services` |

Chemins `crate::` à réécrire (seulement des `use`) : `agent` vers `penelope_agent`,
`executor`, `images`, `vision` vers `penelope_executor`, `conversation` vers
`penelope_conversation`, `embeddings` vers `penelope_vault`, `bus`, `ports`, `helpers`,
`codex_scope`, `runtime::Services`, `selfknow::Admin`, `testing` vers `penelope_app`.

### Coupure retenue (commit de signature, avant le déplacement)

- Un `Context` porte ce que workflow et ordonnanceur lisaient du daemon : `services`,
  `providers` (`Arc<dyn ProviderSource>`), `handle`, `bus`, `workflows`
  (`Arc<workflow::State>`), `embeddings`, `agent` (`Arc<AgentServices>`, que le daemon
  construit par `agent::services_of` : les étapes et les sous-agents appellent
  `penelope_agent::AgentLoop` directement, T11), `admin` (`Option<Arc<dyn Admin>>`).
- `Daemon.workflows` devient `Arc<workflow::State>`.
- La livraison vers le canal reste le `Slot<dyn ChannelDelivery>` de `scheduler::Ports`
  (port de `penelope-app`, lu au moment de s'en servir : la passerelle se branche après
  le daemon) ; `Slot::get` rend l'`Option<Arc<dyn ChannelDelivery>>` demandée.
- Ce qui ne lit que `Services` le prend directement : `origin_of`, `label`, `due_today`,
  `final_already_sent`, `digest_inputs`, `DigestFeed(Arc<Services>)`.

### Consommateurs à garder valides

Daemon : `supervisor.rs` (boucles, orchestrateur), `runner.rs` (`trigger_outcome_of`,
`final_already_sent`), `rpc/methods/workflows.rs`, `engine.rs` (`workflows.wake`),
`tool_jobs.rs` et deux tests (`WorkflowOrchestrator { daemon }`), `dream/mod.rs`.
Passerelle : `workflow::{answer, form_of, control, start_run, origin_of}`,
`scheduler::retarget`, et dans ses tests `drive`, `run_now`, `start_run_briefed`,
`brief_of`, `WorkflowOrchestrator`. Aucun n'est dans le périmètre : la façade du daemon
garde les anciennes signatures en `&Arc<Daemon>` jusqu'à T30.

### T11

- Étapes `agent` et sous-agents : `AgentLoop::new(ctx.agent.clone(), provider)` au lieu
  de `crate::agent::services_of(s)`.
- `skill_install` (`penelope-ops`) : plus aucun appel à la boucle depuis T28 (la ligne
  137 citée par la spécification rechargeait les skills, aujourd'hui
  `penelope_app::services::reload_skills`). Rien à faire.
- Le test « le sous-agent transforme `AwaitingApproval` en erreur » annoncé comme
  existant n'existe pas : il est écrit dans la crate.

## 1. Livré

| Commit | Quoi |
|---|---|
| `aaed02c` | inventaire (ce fichier, §0) |
| `addd37f` | déplacement : `scheduler.rs` (1 095, liste de référence) coupé en `triggers`, `fire`, `outcome`, `origin` ; sorti de `[files.oversized]` |
| `a1ebb4d` | déplacement : le code descend dans `workflow/engine/` et `scheduler/engine/`, `workflow/mod.rs` et `scheduler/mod.rs` deviennent des façades (`pub use engine::*`) |
| `3826867` | signature : `Context` au lieu du daemon, `Daemon.workflows: Arc<State>`, étapes et sous-agents sur `Context.agent` (T11), `digest_inputs` et `DigestFeed(Arc<Services>)` dans l'ordonnanceur, entrées en `&Arc<Daemon>` dans les façades, banc de test sans daemon, test du sous-agent |
| `e9eb825` | déplacement : `git mv` de `engine/` vers `crates/penelope-orchestrator` ; seuls les chemins `crate::` changent |
| `78e99b8` | déplacement : `workflow/tests.rs` (1 101) en `tests/{mod,steps,runs}.rs` |
| `c472e35` | archtest : `ORCHESTRATOR_ALLOWED_DEPS`, `the_orchestrator_crate_sees_neither_the_daemon_nor_the_channel` |
| `32ff863` | composition : le superviseur branche `penelope_orchestrator::WorkflowOrchestrator` |
| (suivant) | `compat.rs` retiré, `workflow::orchestrator_of` aux cinq sites et au superviseur ; notes |

Critères de T27 :

- la crate ne dépend ni du daemon, ni de la passerelle, ni de `penelope-telegram`, ni de
  `penelope-ops`, ni de `penelope-mcp-host` (règle et test d'archtest ; ces deux
  dernières en dépendances de test seulement) ; elle est dans `CHANNEL_AGNOSTIC_CRATES` ;
- `WorkflowOrchestrator: penelope_app::ports::Orchestrator` (`workflow/orchestrator.rs`),
  branché en production par le superviseur ;
- `Daemon.workflows: Arc<penelope_orchestrator::State>` ;
- la livraison vers le canal : `scheduler::Ports.delivery: Slot<dyn ChannelDelivery>`,
  port de `penelope-app` lu au moment de s'en servir (`get()` rend
  l'`Option<Arc<dyn ChannelDelivery>>`) ;
- `ticket_to_deploy_e2e` vert (passerelle, sans changement) ;
- 31 tests dans la crate, sans `Daemon` ; **aucun ne reste au daemon**.

T11 : `AgentLoop::new(ctx.agent.clone(), provider)` dans `step_agent.rs` (étape `agent`
et `run_sub_agent`) ; le daemon pose `agent::services_of` dans `context_of`. Nouveau
test `a_sub_agent_that_needs_an_approval_fails_instead_of_waiting` : vérifié rouge quand
`AwaitingApproval` rend `Ok`. `skill_install` : rien à faire (§0).

## 2. Choix

- **Une façade `engine/` avant le déplacement.** Les entrées en `&Arc<Daemon>` devaient
  garder leur nom (`workflow::start_run`, `answer`, `control`, `drive`, `origin_of`,
  `scheduler::run_now`, `tick`, `trigger_outcome_of`…) : la passerelle, le RPC et le
  coureur les appellent, hors périmètre. Dans le même module que le code, elles
  entraient en conflit de nom avec les fonctions en `&Context`. Sous une façade
  (`pub use engine::*` puis `pub use penelope_orchestrator::workflow::*`), une
  définition locale masque le glob, sans toucher au code déplacé.
- **`WorkflowOrchestrator` n'a pas de double au daemon** (arbitrage de l'intégrateur) :
  cinq sites hors périmètre le construisaient par littéral (`tool_jobs.rs`,
  `executor/tests.rs`, `engine/tests/models.rs`, passerelle `ticket_to_deploy_e2e.rs`
  et `telegram/tests/workflows.rs`) ; ils écrivent `workflow::orchestrator_of(&d)`,
  une expression par site, comme `x.dream()`. Un `WorkflowOrchestrator { daemon }` de
  transition (`workflow/compat.rs`, onze méthodes déléguées) a existé de `3826867` à
  l'avant-dernier commit.
- **`Context` au lieu d'un port par besoin.** Même motif que `compaction::Context` et
  `penelope_dream::Context` : ce que le code lisait du daemon, en champs. `admin` est
  optionnel (le daemon le pose, les tests non) ; `agent` porte les ports de la boucle
  que seul le daemon sait construire (`KvModes`, instantanés, audit de cache, jobs).
- **Ce qui ne lit que `Services` le prend** (`origin_of`, `form_of`, `label`, `due_today`,
  `final_already_sent`, `digest_inputs`) : les façades n'ont pas à construire un
  contexte pour eux.
- **Banc de test** (`workflow/harness.rs`, `#[cfg(test)]`) : `Services::for_tests`,
  `MockProviders`, registres de la boucle en mémoire (`MemoryModes`, `NoAudit`,
  `NoJobs`), `WorkflowOrchestrator` branché sur son propre contexte. Un test change de
  corps : la session d'origine de `a_recurring_prompt_survives_the_closing_of_its_conversation`
  est créée directement au lieu de `Daemon::chat_session_for`.
- `Context` vit dans `workflow/context.rs`, réexporté à la racine
  (`penelope_orchestrator::Context`) ; l'ordonnanceur le lit par `crate::workflow`.

## 3. Mesures

| Mesure | Avant (`8f701ff`) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 22 863 (plafond) | 17 076 |
| `penelope-orchestrator/src`, lignes | | 6 180 (plus gros : `workflow/tests/runs.rs` 717, code : `driver.rs` 438) |
| `[daemon.daemon_users]` workflow + scheduler | 45 | 20 (façades : `workflow/mod.rs` 13, `scheduler/mod.rs` 7) ; `dream/mod.rs` 3 → 2 |
| `[files.oversized]` | `scheduler.rs` 1 095, `tool_jobs.rs` 1 207 | `scheduler.rs` sorti, `tool_jobs.rs` 1 205 |

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` sur macOS (en deux passes, evals à part) :
80 suites, 2 016 tests, 0 échec, dont archtest, `docs`, `scenarios`, `rpc_golden`,
`ticket_to_deploy_e2e` (passerelle).

## 5. Reste et blocages

- Plafond `[crates]` du daemon : 22 863 dans `budget.toml`, 17 076 mesurées ; à
  abaisser par l'intégrateur.
- Bissection : entre `addd37f` et `e9eb825` exclu, `crates_stay_under_their_ceiling` est
  rouge (+23 lignes d'en-têtes de modules).
- T30 : retirer les façades `workflow/mod.rs`, `scheduler/mod.rs`
  et `dream::digest_inputs(&Daemon)` ; appelants sur `penelope_orchestrator::…` avec
  `workflow::context_of(&d)`.
- T36 : `scheduler/origin.rs` nomme encore le canal (30 mentions : noms de
  conversations, `retarget`) ; `fire.rs` et `outcome.rs` une chacun.
- T34 : l'exécuteur cite toujours `Orchestrator::schedule_*`.
- Hors périmètre, une expression par site (arbitrage de l'intégrateur) : `tool_jobs.rs`,
  `executor/tests.rs`, `engine/tests/models.rs`, passerelle `ticket_to_deploy_e2e.rs` et
  `telegram/tests/workflows.rs` ; `dream/mod.rs` du daemon (entrée `digest_inputs`).

## 6. Notes de version (pour docs/progress.md)

#### Orchestrateur : crate `penelope-orchestrator` (épopée #208, lot J, T27 et T11)

- Le moteur de workflows et l'ordonnanceur quittent le daemon pour la crate
  `penelope-orchestrator`, au-dessus de la boucle d'agent, de l'exécuteur, de la
  conversation et du rêve ; une règle d'architecture lui interdit le daemon, la
  passerelle et le canal, qu'il n'atteint que par les ports de `penelope-app`.
- Ils reçoivent un contexte (services, providers, état des runs, services de la boucle)
  au lieu du daemon ; les étapes `agent` et les sous-agents appellent la boucle
  directement. Un test vérifie qu'un sous-agent qui demande une approbation échoue au
  lieu d'attendre.
- `penelope-daemon` passe de 22 863 à 17 076 lignes. Aucun comportement visible ne
  change.
