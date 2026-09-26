# Architecture de Pénélope

Ce que le code de la branche `v1` contient à la 1.0.0-alpha.13 : les crates, qui dépend de
qui, les ports de `penelope-app` et qui les implémente, la frontière entre le cœur et le
canal, le journal source unique de la conversation, et les règles mécaniques qui tiennent le tout.
Chaque chiffre se relit avec la commande donnée à côté ; ce qui n'est pas encore fait est
dit dans la dernière section, jamais présenté comme livré.

Les décisions qui fondent ce découpage sont [0013](decisions/0013-decoupage-du-daemon.md)
(le daemon découpé en crates, la passerelle au-dessus) et
[0014](decisions/0014-boucle-pipeline.md) (la boucle d'agent en étapes typées) ; le gel
qui les encadre est [0015](decisions/0015-gel-0.17-et-branche-v1.md). La charte et les
spécifications de la V1 sont dans `design/v1/` (suivi par git, non embarqué dans le
binaire).

## Les couches

Le niveau d'une crate est la longueur du plus long chemin de ses dépendances internes
(`[dependencies]` seulement, les dépendances de test ne comptent pas). Une crate ne
dépend que de crates de niveau strictement inférieur : il n'y a aucun cycle, et
`penelope-archtest` le vérifie (`there_is_no_dependency_cycle`).

```
 niveau
   12   cli
   11   gateway-telegram        evals
   10   daemon
    9   orchestrator
    8   conversation   dream   executor   ops
    7   agent   vault   mcp-host
    6   app
    5   workflow
    4   tools
    3   context   mcp   memory
    2   llm   hitl   skills   telegram
    1   kernel
    0   store   observe   platform   archtest
```

Les arêtes qui portent la structure, une fois retirées celles qu'une autre implique
(réduction transitive) :

```
 cli ──────────────► gateway-telegram ──► daemon
  └────────────────► evals ─────────────► daemon
                     gateway-telegram, evals ──► telegram   (bibliothèque Bot API)

 daemon ───────────► orchestrator, ops, mcp-host
 orchestrator ─────► agent, conversation, dream, executor
 conversation, dream, executor, ops ──► vault
 agent, vault, mcp-host ─────────────► app
 app ──────────────► context, workflow      (et par eux toutes les crates métier)
```

Deux lectures. Au-dessus de `app`, une crate ne connaît sa voisine que par un port de
`penelope-app` : la boucle ne voit ni la conversation ni l'exécuteur, l'exécuteur ne voit
ni la boucle ni l'orchestrateur. La passerelle Telegram est **au-dessus** du daemon : elle
l'appelle, et le daemon ne la connaît que par ses ports (décision 0013).

Pour relire le graphe :

```bash
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | .name + " : " + ([.dependencies[]
      | select(.kind == null and (.name | startswith("penelope-"))) | .name] | join(" "))'
```

## Les crates

Lignes de `src/` (tests inline et fichiers `tests.rs` compris, `tests/` et `examples/`
exclus), mesurées à la 1.0.0-alpha.13 :
`find crates/<crate>/src -name '*.rs' | xargs cat | wc -l`.

| Crate | Rôle | Dépendances internes directes | Lignes |
|---|---|---|---|
| `penelope-store` | SQLite en WAL, migrations versionnées, acteur écrivain unique | aucune | 2 628 |
| `penelope-observe` | journaux JSON, caviardage, détection d'injection, métriques | aucune | 1 905 |
| `penelope-platform` | la seule crate qui connaît l'OS : dossiers, processus, secrets, bac à sable, service | aucune | 6 421 |
| `penelope-kernel` | journal d'événements, ledger d'effets, configuration, sessions, file des tours, budget | store | 10 761 |
| `penelope-llm` | fournisseurs, catalogue, routage, coûts | kernel, observe, platform, store | 8 227 |
| `penelope-hitl` | politiques, demandes d'approbation | kernel, observe, store | 2 744 |
| `penelope-skills` | skills, rechargement à chaud | kernel, observe, platform, store | 1 381 |
| `penelope-telegram` | client Bot API typé, rendu, gabarits, formulaires | kernel, observe, store | 6 087 |
| `penelope-context` | moteur de contexte, compaction, pliage du journal, projecteur | kernel, llm, observe, store | 13 411 |
| `penelope-mcp` | client MCP, transports, OAuth | kernel, llm, observe, platform, store | 6 643 |
| `penelope-memory` | vault, index, rappel, consolidation | kernel, llm, observe, platform, store | 9 791 |
| `penelope-tools` | définitions des outils natifs, détecteur de boucles, arguments | hitl, kernel, llm, mcp, memory, observe, platform, skills, store | 6 426 |
| `penelope-workflow` | schéma, validation, runs et planifications | hitl, kernel, llm, observe, platform, store, tools | 6 188 |
| `penelope-app` | `Services`, les ports, le bus des tours, les boucles supervisées, l'élicitation | les douze crates métier ci-dessus sauf telegram | 4 660 |
| `penelope-agent` | la boucle d'agent et le pipeline d'outils | app, hitl, kernel, llm, observe, store, tools | 7 043 |
| `penelope-mcp-host` | superviseur des serveurs MCP, OAuth des serveurs | app, kernel, llm, mcp, observe, platform, store | 4 397 |
| `penelope-vault` | la mémoire en fichiers : pages, concepts, épisodes, notes, revue | app, context, hitl, kernel, llm, mcp, memory, observe, platform, store, tools | 6 210 |
| `penelope-conversation` | conversation d'une session, compaction de fond, titres, alerte de budget | app, context, kernel, llm, observe, store, vault | 3 244 |
| `penelope-dream` | consolidation nocturne, digest, accueil, ingestion | app, hitl, kernel, llm, memory, observe, platform, store, vault | 7 542 |
| `penelope-executor` | outils natifs, `self_status`, documentation embarquée, vision, voix, magasin des jobs | app, context, kernel, llm, mcp, memory, observe, platform, skills, store, tools, vault, workflow | 6 790 |
| `penelope-ops` | doctor, mise à jour, sauvegarde, Hermes, Codex, purge, fork et retour arrière | app, context, kernel, llm, mcp, memory, observe, platform, skills, store, vault | 10 762 |
| `penelope-orchestrator` | moteur de workflows et ordonnanceur | agent, app, conversation, dream, executor, hitl, kernel, llm, mcp, observe, platform, store, tools, vault, workflow | 6 237 |
| `penelope-daemon` | composition, moteur des tours, coureurs, supervision, RPC | toutes les crates ci-dessus sauf telegram | 14 986 |
| `penelope-gateway-telegram` | la passerelle Telegram, adaptateur pilotant | agent, app, context, conversation, daemon, dream, executor, hitl, kernel, llm, mcp-host, memory, observe, ops, orchestrator, platform, skills, store, telegram, vault, workflow | 18 703 |
| `penelope-evals` | suites déterministes, scénarios rejouables, rejeu | agent, app, context, conversation, daemon, dream, executor, hitl, kernel, llm, mcp, memory, observe, ops, platform, skills, store, telegram, tools, vault, workflow | 4 701 |
| `penelope-cli` | le binaire `penelope` : CLI, client RPC, composition | agent, daemon, evals, gateway-telegram, kernel, observe, ops, platform, store, telegram, tools, workflow | 3 436 |
| `penelope-archtest` | les règles d'architecture et le gel | aucune | 3 311 |

Le plus gros fichier source du dépôt fait 2 148 lignes (`penelope-kernel/src/config.rs`) ;
les fichiers au-dessus de 1 000 lignes sont nommés dans la liste de référence du gel
(voir plus bas).

### Ce que le daemon garde

`penelope-daemon/src` compte 20 modules (liste blanche `[daemon].modules` du budget) :

- la composition : `runtime` (`Daemon`, `Hooks`, `Providers`, reprise au démarrage,
  état), `supervisor` (`Daemon::run`, boucles supervisées), `runner` (coureurs de la file
  des tours) ;
- le moteur des tours : `engine` (admission d'un message, choix du modèle, `impl Admin`),
  `tool_jobs` (lancement et livraison des jobs d'outils), `history` (rattrapage des caches
  à l'ouverture d'une session) ;
- les implémentations des ports de la boucle qui lisent la base : `approval_mode`
  (`KvModes`), `prompt_snapshot` (`StoredSnapshots`), et à côté `cache_audit`
  (contexte volatil et préfixe stable de l'audit du cache) et `audit` ;
- la façade RPC (`rpc/`) et le flux runtime (`runtime_events`) ;
- les adaptateurs qui dérivent d'un `Daemon` ce qu'une crate du dessous attend, faute
  d'autre place : `agent` (`services_of` et les entrées `decide_approval`,
  `close_unopened`, `close_interrupted_turns` en `&Services` : `penelope-agent` ne nomme
  pas `Services`, qui porte le moteur de contexte), `workflow` (`context_of`,
  `orchestrator_of` : le contexte de l'orchestrateur lit les providers, le bus, l'état
  des runs), `compaction` (`context_of`), `selfknow` (`codex_view`, servi par `Admin`) ;
- des modules de tests seuls, qui jouent un tour réel sur le daemon : `dream`,
  `executor`, `ingest`, `wiki_e2e`.

Le daemon ne réexporte plus rien d'une autre crate (T30) : `lib.rs` exporte ses modules
propres, `Daemon` et `VERSION`. Un appelant nomme la crate où vit le code
(`penelope_app::services::Services`, `penelope_app::bus::Origin`,
`penelope_orchestrator::workflow::start_run(&penelope_daemon::workflow::context_of(d), …)`).
Hors de la passerelle, la CLI et les évaluations ne citent du daemon que `Daemon`,
`VERSION`, `runner::{process, run_pool}`, `rpc::Rpc` et deux adaptateurs
(`agent::decide_approval`, `compaction::context_of`) :

```bash
grep -rhoE 'penelope_daemon::[A-Za-z_]+(::[A-Za-z_]+)?' crates/penelope-evals crates/penelope-cli | sort | uniq -c
```

### La composition

`penelope-cli` compose le processus (`commands.rs`, commande `daemon`) :

1. `Daemon::new(home, Some(penelope_gateway_telegram::cards))` : les cartes du canal
   arrivent avant `Services::bootstrap`, pour que les workflows soient validés contre les
   gabarits dès leur chargement ;
2. `penelope_gateway_telegram::compose(&d)` construit la passerelle depuis la
   configuration, sans réseau ;
3. `d.run(gateway)` annonce un propriétaire joignable, crée le superviseur MCP, puis
   démarre la passerelle (ordre de l'issue #12, testé par
   `a_gateway_is_announced_before_mcp_and_started_after_it`).

`Daemon::new(home, None)` démarre sans canal ; c'est ce que font les tests du daemon.

## Les ports

Un port est un trait défini sous la crate qui le consomme, implémenté au-dessus. Presque
tous vivent dans `penelope-app`, sous toutes les crates du cœur. La colonne « consommé
par » liste les crates qui en tiennent un `dyn`, hors tests.

### Ports de `penelope-app`

| Port | Module | Rôle | Implémenté par | Consommé par |
|---|---|---|---|---|
| `ProviderSource` | `ports` | le fournisseur d'un modèle, l'override actif | `Providers` (daemon, `runtime.rs`) | conversation, dream, executor, ops, orchestrator, vault |
| `McpGateway` | `ports` | appeler un outil MCP, politique déclarée | `McpSupervisor` (mcp-host) | daemon, executor, orchestrator, evals |
| `McpAdmin` | `ports` | administrer les serveurs MCP (`mcp.*`) | `McpSupervisor` (mcp-host) | daemon, dream, mcp-host, ops, orchestrator, vault |
| `Messenger` | `ports` | envoyer texte, fichier, carte, question au propriétaire | `TelegramGateway` (passerelle) | conversation, daemon, dream, executor, mcp-host, ops, orchestrator |
| `Orchestrator` | `ports` | lancer un workflow, un sous-agent, une image ; planifier (`schedule_*`) | `WorkflowOrchestrator` (orchestrator) | daemon, executor, orchestrator |
| `Admin` | `ports` | ce que `self_status` et `config_set` lisent du processus | `Daemon` (daemon, `engine.rs`) | daemon, executor, orchestrator |
| `ChannelDelivery` | `bus` | livraison durable d'un tour, seuils de rafale, nom et destination d'une origine | `TelegramGateway` ; `DeliveryChannel`, `BurstChannel` (daemon, `runner.rs`) | app, conversation, daemon, orchestrator |
| `TurnSink` | `outcome` | fragments d'un tour en cours | `BusSink` (app), `NullSink`, `RecordingSink` | agent, daemon |
| `OwnerChannel` | `elicitation` | montrer une élicitation MCP au propriétaire | `TelegramGateway` | app (`Broker`) |
| `Cards` | `channel` | gabarits de cartes, catalogue, liens profonds, commandes du canal | `TelegramCards` (passerelle) | app (`Services.channel`) |
| `Gateway` | `gateway` | démarrer un canal composé au-dessus du daemon | `TelegramGateway` | daemon |
| `Conversation` | `conversation` | les messages d'une session vus par la boucle | `SessionConversation` (conversation), `MemoryConversation` (app) | agent |
| `Compactor` | `conversation` | compacter sur débordement prouvé | `OverflowCompactor` (conversation) | conversation |
| `ToolExecutor` | `tool_executor` | exécuter un appel d'outil | `NativeToolExecutor` (executor) | agent, app, daemon, executor |
| `Inbox` | `steering` | réclamer les messages arrivés pendant un tour, à un `Checkpoint` | `TurnInbox` (conversation) | agent, conversation |
| `AttemptSink` | `attempts` | garder une tentative d'appel au modèle | `JournalAttempts` (app, écrit `conv.attempt`), `MemoryAttempts` | agent |

Ce que `penelope-app` fournit en plus des traits :

- `Services` (`services.rs`) : tous les registres assemblés par `Services::bootstrap`
  (base, horloge, journal, ledger, configuration, sessions, file des tours, budget,
  moteur de contexte, mémoire, skills, outils MCP, approbations, workflows, jobs), et
  `Services.channel` (`Channel { cards, delivery }`) ;
- `Slot<T>` : la case d'un branchement posé après le démarrage (la passerelle et le
  superviseur MCP arrivent après les boucles), lue au moment de s'en servir ;
- `Handle` (arrêt, redémarrage, compteurs) et `Supervision` (ce qu'une boucle supervisée
  reçoit : registre, `Handle`, horloge, journal) ;
- `Bus` et `Origin` (`bus.rs`), les helpers sur `&Services` (`helpers.rs`) et les doubles
  de test partagés (`testing.rs` : `RecordingMessenger`, `MockProviders`).

### Ports de la boucle, dans `penelope-agent`

La boucle ne reçoit pas `Services` mais `AgentServices` (`ports.rs`) : les registres
qu'elle touche, quatre ports que seul le daemon sait implémenter sur la base, et
`AttemptSink`. Le dernier appel d'une session, pour l'audit du cache, se lit dans le
`BudgetLedger` du noyau (`previous_call`) ; les parties pures de l'audit (empreinte,
fournisseur collant, cause d'un raté) sont dans `penelope_llm::cache`.

| Port | Rôle | Implémenté par (daemon) | Double de test |
|---|---|---|---|
| `SessionModes` | mode d'approbation d'une session | `KvModes` (`approval_mode.rs`) | `MemoryModes` |
| `SessionInfo` | nature d'une session | `SessionStore` (kernel) | le même, sur une base en mémoire |
| `PromptSnapshots` | instantanés du préfixe, cause d'un raté de cache | `StoredSnapshots` (`prompt_snapshot.rs`) | `NoAudit` |
| `JobRunner` | détacher un appel long en job | `DaemonJobs` (`agent.rs`) | `NoJobs` |

### Autres ports

| Port | Crate | Implémenté par |
|---|---|---|
| `DigestSource` | `penelope-dream` | `DigestFeed` (orchestrator, `scheduler/digest.rs`) |
| `Connector` | `penelope-mcp-host` | `ProcessConnector` |
| `SwitchHost` | `penelope-ops` | `SystemHost` |
| `Provider`, `TokenSource`, `QuotaSink` | `penelope-llm` | fournisseurs OpenRouter, compatibles OpenAI, Codex ; jetons et quota Codex (ops) |
| `Transport` | `penelope-mcp` | stdio, HTTP |
| `Directories`, `ProcessHost`, `SecretStore`, `Sandbox`, `ServiceManager`, `PowerManager` | `penelope-platform` | backends macOS et génériques |
| `BotTransport` | `penelope-telegram` | HTTP, simulé |
| `Clock`, `UsageWatcher` | `penelope-kernel` | horloge système ou de test ; alerte de budget |

### Contextes plutôt que daemon

Les crates sorties du daemon ne reçoivent pas `&Daemon` mais un contexte qui porte ce
qu'elles en lisaient : `penelope_conversation::compaction::Context` (services, providers,
bus, état), `penelope_dream::Context` (services, providers, embeddings),
`penelope_orchestrator::Context` (services, providers, `Handle`, bus, état des runs,
embeddings, `AgentServices`, `Admin` optionnel). Le daemon les compose ; leurs tests les
montent sur `Services::for_tests` et `MockProviders`, sans daemon.

## La frontière canal

Le cœur ne nomme pas Telegram. La règle est mécanique (`penelope-archtest`,
`freeze.rs`) :

- 22 crates sont déclarées agnostiques (`CHANNEL_AGNOSTIC_CRATES` : tout le socle, les
  crates métier sauf `penelope-telegram`, et le cœur jusqu'au daemon) ; 5 ont le droit de
  nommer le canal (`CHANNEL_CRATES` : `penelope-telegram`, la passerelle, la CLI, les
  évaluations, archtest). Une crate qui n'est d'aucun côté fait échouer
  `every_crate_is_on_one_side_of_the_channel_boundary`.
- Dans une crate agnostique, les lignes de code (hors commentaires et tests) ne citent le
  motif `CHANNEL_PATTERN` (`telegram` en toute casse, `tg_…`, `chat_id`, `topic_id`,
  `callback_data`, `find_by_topic`) que dans la mesure de `[channel.allowed]` : 31 fichiers,
  249 mentions en tout, bornes qui ne peuvent que descendre. Quatre entrées sont
  permanentes et le disent en commentaire : `store/migrations.rs` (l'historique SQL est
  immuable), `kernel/config.rs` (le schéma nomme ses canaux), `observe/redact.rs` (forme
  des jetons), `app/bus.rs` (`Origin::Telegram`, jusqu'à T37).
- `penelope-app`, `penelope-executor` et `penelope-daemon` ne dépendent pas de
  `penelope-telegram` ; seule `penelope-cli` dépend de la passerelle
  (`GATEWAY_DEPENDENTS`).

Ce qui traverse la frontière passe par cinq ports : `ChannelDelivery` (livraison d'un
tour, carte de rafale et ses seuils, nom d'une conversation, destination d'une
planification), `Messenger` (envois au propriétaire), `OwnerChannel` (élicitation MCP,
dont la vérification du formulaire), `Cards` (gabarits, liens profonds, commandes) et
`Gateway` (démarrage). Sans canal branché, les messages du cœur disent « canal » là où ils
disaient « Telegram ».

## Le journal, source unique

La conversation vit dans la table `events`, chaînée par hachage comme le reste du journal
d'audit, sous des événements `conv.*` (`penelope-context/src/journal/`) : `conv.system`,
`conv.user`, `conv.context`, `conv.assistant`, `conv.tool_result`, et ceux qui modifient
la surface vue par le modèle, `conv.summary` (remplace une plage), `conv.rewind` (coupe),
`conv.fork` (hérite du préfixe d'une autre session), `conv.import` (scelle l'historique
d'avant la V1, migration `0021_history_seal`). Les tentatives échouées (`conv.attempt`)
sont journalisées hors surface : elles ne repartent jamais dans un prompt.

- **Lecture.** Ce que le modèle reçoit est dérivé du journal par un pliage pur
  (`penelope_context::derive`, sans base ni horloge), repris sur les seuls événements
  nouveaux (`read/cache.rs`). Il n'y a pas d'autre source : les caches ne servent qu'aux
  autres lecteurs (plein texte, outils d'historique, préparation des résumés) et au
  repli d'une session que le journal ne sait pas redonner (décision 0017).
- **Écriture.** Toute écriture de la conversation est un événement `conv.*`, commité
  seul, puis sa projection dans la seconde transaction du même thread écrivain
  (`HistoryStore::journaled`, `penelope-context/src/store/dual.rs`) ; un fork et
  l'archive d'un retour arrière sont projetés en entier depuis leur héritage
  (`projector::project_in`). `HistoryStore` prend le journal à sa construction : il n'y a
  plus de chemin d'écriture sans événement.
- **Caches.** `messages`, `messages_fts`, `message_context`, `lcm_nodes`, `lcm_edges`,
  `prompt_snapshots` et `projections_session` : seule `penelope-context` les écrit, ce que
  vérifie `only_the_context_crate_writes_the_conversation_caches` (`caches.rs`). Le
  projecteur rattrape à l'ouverture d'une session ce qu'une écriture n'a pas posé,
  `penelope history verify` compare journal et caches, `penelope history reindex` refait
  les caches depuis le journal.
- **Plus d'état de conversation en `kv`** : le message d'un tour déjà écrit se lit au
  journal par la clé du tour, le préfixe retenu dans le dernier `conv.system`.
- **Pour la boucle.** Le vocabulaire qu'elle écrit (bornes de tour, provenance,
  tentatives, tuiles du préfixe) est dans `penelope_kernel::journal` ; elle remet ses
  tentatives au port `AttemptSink` et ne voit aucun type du moteur de contexte, ce que
  vérifie `the_agent_crate_reaches_no_context_type_through_its_ports` (`reach.rs`).

## Les règles d'architecture

`cargo test -p penelope-archtest` les vérifie toutes ; leurs messages d'erreur disent quoi
faire.

### Dépendances

- Une règle par crate du socle et du cœur (`dependency_rules`, `lib.rs`) : store,
  kernel, observe, platform, llm, context, hitl, telegram, app, mcp-host, vault, ops,
  agent, dream, executor, conversation, orchestrator. Une dépendance hors de la liste
  échoue (`ca_3_1_dependency_rules_hold`). Les crates sans règle : mcp, memory, skills,
  tools, workflow, daemon, gateway-telegram, cli, evals, archtest.
- Un test nommé par interdit qui compte : `the_agent_crate_sees_neither_context_memory_channel_nor_daemon`,
  `the_executor_crate_sees_neither_the_daemon_nor_the_agent_loop`,
  `the_conversation_crate_sees_neither_daemon_loop_nor_channel`,
  `the_orchestrator_crate_sees_neither_the_daemon_nor_the_channel`,
  `the_ops_crate_depends_neither_on_the_daemon_nor_on_the_mcp_host`,
  `the_app_crate_sees_neither_the_daemon_nor_the_channel`,
  `the_daemon_does_not_depend_on_the_channel_library`, `only_the_cli_depends_on_the_gateway`,
  et leurs voisins pour vault, dream, mcp-host.
- Aucun cycle ; aucun chemin littéral, appel shell, signal Unix ni API Trousseau hors de
  `penelope-platform` ; aucune dépendance propre à un OS ailleurs ; `#![forbid(unsafe_code)]`
  dans chaque point d'entrée, aucune crate exemptée.

### Le gel

Les nombres de `crates/penelope-archtest/budget.toml` ne montent jamais :
`UPDATE_BUDGET=1 cargo test -p penelope-archtest` les abaisse, et
`scripts/check-budget.sh` refuse en CI toute remontée sans le trailer
`Dérogation-budget: #N` (décision 0015).

| Règle | Ce qu'elle tient | Valeur à la 1.0.0-alpha.13 |
|---|---|---|
| R1 | plafond d'un fichier source, tests inline compris | 1 000 lignes |
| R2 | plafond d'un fichier de tests | 1 500 lignes |
| R3 | liste de référence des fichiers au-dessus, qui ne peut que rétrécir | 23 fichiers, dont 2 au daemon (`tool_jobs.rs`, `engine.rs`) |
| R4 | plafond de lignes de `penelope-daemon/src` | 14 986 |
| R5 | liste blanche des modules du daemon ; `impl Daemon` dans quatre fichiers | 21 modules ; `runtime`, `engine`, `runner`, `supervisor` |
| R6 | occurrences du type `Daemon` par fichier du daemon | 65, dans 16 fichiers |
| R7 | `#[allow(clippy::too_many_lines)]` comptés, seuil du lint à 200 lignes | 22 |
| R8 | critères d'acceptation `ca_*` qui ne disparaissent pas | 77 |
| frontière canal | voir plus haut | 31 fichiers, 249 mentions |

Deux règles de la spécification ne sont pas posées : R9 (couverture par crate à cliquet,
annoncée dans `ratchet.rs`) et R10 (test de composition réelle par surface). R11 est
tenue par la suite `scenarios` de `penelope-evals` (scénarios de session rejouables sans
clé).

## Ce qui reste

- **Découpage sous 800 lignes** : `engine.rs` (1 091 lignes) et `supervisor.rs` (930) sont
  presque entièrement des blocs `impl Daemon`, que R6 réserve à quatre fichiers ;
  les couper demande d'abord de convertir des méthodes en fonctions sur `&Services` ou
  sur un port (T33), pas un déplacement.
- **T32** : resserrer les listes du gel, maintenant que T30 est fait.
- **Journal** : l'archive d'un `/rewind` n'est pas encore un fork par référence (elle
  n'a pas de journal à elle) ; un retour arrière qui coupe dans le préfixe scellé empêche
  la mère de se replier (décision 0017, écarts restants).
- **Après la V1** : T33 (ports `TurnIntake`, `SessionModels`, `Transcriber`, pour tester
  la passerelle sans daemon), T34 (registre d'outils natifs enfichable, au lieu des appels
  `Orchestrator::schedule_*` depuis l'exécuteur), T37 (`Origin::Channel` et liaison de
  session générique : la plupart des 249 mentions restantes du canal y sont).
- **Graphe cargo** : `penelope-agent` atteint encore `penelope-context` par
  `penelope-app`, qui porte `Services` et son moteur de contexte ; la règle `reach.rs`
  garantit qu'aucun type ne passe, pas que l'arête disparaisse.
