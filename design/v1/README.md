# Pénélope V1 : charte de la refonte

Rédigée le 23 septembre 2026 sur la 0.17.58 (`main`, commit `fbbf906`, plus un lot #205 non
commité dans l'arbre de travail). Ce dossier `design/` est suivi par git, il n'est ni embarqué
dans le binaire (`crates/penelope-daemon/build.rs` n'inclut que `docs/`) ni parcouru par le
test `docs` ni par `penelope-archtest`. Le dossier `spec/` est ignoré par git (`.gitignore:2`)
et le PRD qu'il contient date du premier jour : il ne fait plus foi. Ce qui fait foi : le code,
`docs/progress.md`, `docs/decisions/`, `docs/*.md`, les issues GitHub, et ce dossier.

Ce fichier est la charte. Les quatre spécifications qu'il réconcilie sont à côté, chacune
avec ses mesures, ses preuves `fichier:ligne`, ses tâches et ses risques :

| Fichier | Sujet | Tâches |
|---|---|---|
| [source-de-verite.md](source-de-verite.md) | le journal d'événements devient l'unique source de la conversation | 23 |
| [boucle-et-outils.md](boucle-et-outils.md) | la boucle d'agent et le pipeline d'outils en étapes typées | 27 |
| [decoupage-daemon.md](decoupage-daemon.md) | cartographie du couplage, frontière canal/cœur, extraction du daemon en crates | 35 (+ 3 différées) |
| [gel-et-outillage.md](gel-et-outillage.md) | règles mécaniques de gel, branche `v1`, filets, bascule | 19 |
| [contrat-fonctionnel.md](contrat-fonctionnel.md) | ce que la V1 doit conserver à l'identique | inventaire |

## 1. Les arbitrages du propriétaire

1. **La 0.17 est assumée « dirty » et reste en service.** Elle ne reçoit plus que des
   corrections. La V1 est refaite dans une branche parallèle, avec l'exigence d'ingénierie
   la plus haute, sans compromis.
2. **Une seule source de vérité.** La conversation vit dans le journal d'événements ; tout le
   reste est dérivé et reconstructible. C'est le modèle de DeepSeek Harness, et c'est le
   principe que Pénélope applique déjà à sa mémoire (« le vault est la vérité, l'index est
   reconstructible ») sans l'appliquer à sa conversation.
3. **La dette est gelée dès maintenant, mécaniquement**, sur `main`, avant toute refonte :
   aucun fichier ne grossit au-delà d'un plafond, aucun module nouveau n'entre dans le
   daemon, et une liste de référence des fichiers en dépassement ne peut que rétrécir.
4. **L'architecture hexagonale déjà implicite devient explicite et tenue par des règles**,
   pas par la discipline : le noyau est le domaine, les traits sont les ports, les crates
   sont les adaptateurs, la composition est en un seul endroit.
5. **Un énorme chantier découpé en petites tâches**, chacune livrable seule, chacune un lot
   (code, section de `docs/progress.md`, bump), la V0 continuant de tourner.

## 2. Ce que la V1 garde et ce qu'elle change

Ce que les trois harnais de référence (DeepSeek Harness, OpenClaw, Hermes) n'ont pas et que
la V1 garde intact : le ledger d'effets avec l'état `unknown` qui pose une question au lieu de
relancer, la file de tours durable à bail et battement, le bornage des règles « Toujours » à
la famille de commandes, les classes de risque et les planchers déterministes, le coût mesuré
plutôt qu'estimé, `send_unknown` pour un appel modèle coupé avant les en-têtes, la boucle
arrêtée qui répond quand même, l'admission de groupe des résultats d'outils, la cause de raté
du cache, le vault Markdown avec consolidation nocturne.

Ce que la V1 change, en quatre chantiers :

| Chantier | Aujourd'hui | Cible |
|---|---|---|
| Source de vérité | `events` (chaîne de hachage, contrôle) et `messages` + `lcm_nodes` + `message_context` (contenu) ne se connaissent pas ; le prompt système n'est nulle part | événements `conv.*` versionnés dans `events`, avec opération de surface ; historique dérivé par un pliage pur ; tables de messages = caches reconstructibles (`history reindex`, `history verify`) |
| Boucle d'agent | `agent.rs` : 2 146 lignes de code dans un fichier, politique = suite de `if`, steering caché dans une lecture, cartes en JSON non typé | pipeline nommé en étapes typées, gardes monotones, approbation fail-closed, steering explicite, jobs durables, tentatives hors historique |
| Structure | `penelope-daemon` : 78 532 lignes, 63 modules, 42 fichiers nomment `Daemon`, 7 fichiers de plus de 3 000 lignes | daemon réduit à la composition et à la boucle (≈ 9 200 lignes), 10 crates extraites, aucun fichier au-dessus du plafond |
| Filets | 1 775 tests, 71 critères d'acceptation, aucune couverture mesurée, aucun scénario rejouable, suites réseau jamais lancées | scénarios de session rejouables sans clé, test de composition réelle par surface visible, couverture par crate à cliquet, fixture de base 0.17, contrat RPC doré |

### 2.1 Le contrat fonctionnel

`contrat-fonctionnel.md` recense ce que la V1 doit exposer à l'identique : 320 capacités
(conversation 52, contexte 23, mémoire 37, HITL 17, outils 20, MCP 19, LLM 18, Telegram 35,
workflows 15, planification 11, observabilité 16, exploitation 24, sécurité 17, budgets 10,
architecture 6), dont 274 couvertes par un test nommé (85,6 %), 43 partiellement, 3 sans
aucun test (rappel OAuth quotidien, `model.route_test`, tir manqué rattrapé). Contrats
publics : 103 méthodes RPC, 25 commandes CLI, 50 commandes Telegram, 26 gabarits, 59 actions
de boutons, 57 outils natifs (16 noyau, 39 à la demande, 2 réservés aux workflows), 3
méta-outils MCP, 222 clés de configuration (26 sans effet), 75 événements du journal, 6 du
flux runtime, 55 tables et 17 migrations, 14 métriques, 18 suites. Les dix décisions
existantes restent valables ; quatre sont celles que la V1 peut toucher, chacune avec la
garantie et le test à conserver (`contrat-fonctionnel.md` §4) : 0001 (le noyau dépend du
store), 0004 (IPC par socket Unix), 0005 (surveillance par scrutation) et 0008 (cache de
prompt), cette dernière fixée par neuf tests que la nouvelle source de vérité doit garder
verts.

Trois incohérences de documentation relevées au passage, à ramasser dans le lot A : la table
des décisions de `docs/progress.md` s'arrête à 0008 alors que `docs/README.md` en liste dix ;
le noyau d'outils compte 16 entrées dans le code contre 17 dans la documentation ;
`telegram.max_fragments` est marquée « sans effet » dans la référence alors qu'elle est
branchée depuis la 0.17.27.

## 3. Architecture cible

### 3.1 Couches

```
                      penelope-cli  (compose : daemon + passerelle)
                              │
                penelope-gateway-telegram  (adaptateur pilotant, 16 200 l.)
                              │
   ┌──────────────────── penelope-daemon (≈ 9 200 l.) ───────────────────┐
   │ runtime (Daemon, Hooks, recover, status) · engine/ (admission)      │
   │ runner · supervisor · rpc/ · runtime_events · suites e2e            │
   └──────────────────────────────┬───────────────────────────────────────┘
                                  │
      penelope-orchestrator (workflows, ordonnanceur)   penelope-ops (doctor, upgrade,
              │            │              │             backup, hermes, codex, purge)
      penelope-agent   penelope-executor  penelope-dream            │
      (boucle, pipeline)  (outils natifs)  (consolidation, ingest)  │
              │                │               │                    │
      penelope-conversation (transcript, compaction, prompt)        │
              │                │               │                    │
      penelope-vault (mémoire, épisodes, concepts, notes)   penelope-mcp-host
              │                                                     │
   ┌──────────┴─────────────────────────────────────────────────────┴────┐
   │ penelope-app : Services, ports, bus, tâches supervisées, élicitation │
   └──────────────────────────────┬───────────────────────────────────────┘
                                  │
   kernel · store · platform · observe · llm · context · memory · mcp · skills · tools · hitl · telegram · workflow
```

Deux règles de lecture : une flèche descend toujours (aucun cycle, vérifié par
`penelope-archtest`), et une crate ne connaît une crate voisine que par un port de
`penelope-app`.

### 3.2 Réconciliation entre les deux spécifications de structure

`decoupage-daemon.md` propose neuf crates à partir du couplage mesuré ;
`boucle-et-outils.md` demande une crate de boucle qui ne dépende pas de `penelope-context`
(le prompt lui arrive rendu) et se teste sans base ni daemon. Les deux sont compatibles à une
condition, retenue ici : la crate `penelope-agent` du découpage est scindée en deux.

- `penelope-agent` : la boucle et le pipeline d'outils (les 27 modules de
  `boucle-et-outils.md` §4.1), dépendances `kernel`, `llm`, `tools`, `hitl`, `observe`,
  et `penelope-app` pour les ports. Jamais `context`, `memory`, `telegram`.
- `penelope-conversation` : `SessionConversation`, `build_turn_prompt`, la compaction de
  fond et son état, les titres, l'alerte de budget. Dépendances `context`, `penelope-vault`,
  `penelope-app`. C'est l'implémentation du port `Conversation` que la boucle consomme.

Tout le reste de la carte du découpage est repris tel quel, y compris les deux décisions
qu'il défend : la passerelle Telegram **au-dessus** du daemon (un adaptateur pilotant, composé
par `penelope-cli`, plutôt qu'un port de 90 méthodes), et `penelope-app` **en bas** avec
`Services` et tous les ports.

### 3.3 Ports

Existants et conservés : `TurnSink`, `Conversation`, `Compactor`, `ToolExecutor`,
`Messenger`, `McpGateway`, `Orchestrator`, `ChannelDelivery`, `OwnerChannel`, `Connector`,
`Admin`, `SwitchHost`, `Provider`, `TokenSource`, `QuotaSink`, `Transport`, `Directories`,
`ProcessHost`, `SecretStore`, `Sandbox`, `ServiceManager`, `PowerManager`, `BotTransport`,
`Clock`, `UsageWatcher`.

À créer (`decoupage-daemon.md` §2.2) : `kv_*` sur `Services` (une seule famille au lieu de
deux), `ProviderSource`, `Supervision`, `Handle`, `McpAdmin`, `Messenger` reçu explicitement,
`Orchestrator` étendu, `DigestInputs`, `Gateway`. À créer pour la boucle
(`boucle-et-outils.md` §3) : `Inbox`, `AttemptSink`, `Judge`, `JobRunner`, `Approver`,
`RequestShaper`, `SessionModes`, `PromptSnapshots`, `CacheAudit`.

### 3.4 Plafonds

- Sur `main`, dès le gel : **1 000 lignes** par fichier source, tests inline compris, avec
  la liste de référence des 42 fichiers en dépassement qui ne peut que rétrécir ; 1 500 pour
  un fichier de tests ; `penelope-daemon` plafonné à 80 000 lignes ; liste blanche des 64
  modules du daemon ; budget d'occurrences de `Daemon` par fichier (42 fichiers, cible 4) ;
  `clippy::too_many_lines` à 200 avec les 25 exceptions existantes comptées.
- Sur `v1`, cible de sortie : **800 lignes** par fichier, tests en fichiers frères
  (`<module>/tests.rs`), aucun fichier en dépassement, `penelope-daemon` sous 10 000 lignes,
  seuls `runtime`, `engine/*`, `runner`, `supervisor`, `rpc/*` nomment `Daemon`.

Les deux valeurs viennent de la mesure : 80 % des fichiers tiennent déjà sous 1 000 lignes,
21 des 42 fichiers en dépassement passent sous ce plafond rien qu'en déplaçant leurs tests ;
DeepSeek Harness tient 855 de ses 937 fichiers hôtes sous 500 lignes. Le cliquet est celui
d'OpenClaw (`config/max-lines-baseline.txt`, « this list may only shrink »).

### 3.5 Frontière canal / cœur

Le cœur ne doit jamais nommer Telegram. Mesure (`decoupage-daemon.md` §1.3) : hors
`telegram.rs` et `screens.rs`, 279 occurrences des motifs `telegram`, `tg_`, `chat_id`,
`topic_id`, `callback_data` dans 29 modules du daemon (doctor 47, scheduler 42, supervisor 25,
rpc 24, purge 23), et 162 dans les autres crates (kernel 94, dont `session.rs` 39 et
`config.rs` 32 ; `store/migrations.rs` 31). Une soixantaine sont de la logique de canal, pas
des noms : `Services` porte les gabarits et les actions de `penelope_telegram`, la fusion des
rafales vit dans `conversation.rs` et `runner.rs`, les cibles de planification dans
`executor.rs` et `scheduler.rs`, `chat_session_for` dans `engine.rs`. La règle R8 du découpage
transpose la garde d'OpenClaw (`scripts/check-channel-agnostic-boundaries.mts`) : motifs sur
lignes de code, hors tests et commentaires, crates déclarées agnostiques (`kernel`, `app`,
`agent`, `executor`, `vault`, `dream`, `daemon`), liste blanche par fichier à cliquet,
entrées permanentes justifiées (`migrations.rs`, `config.rs`, `redact.rs`, `bus.rs` pour
`Origin::Telegram` tant que `Origin::Channel` n'existe pas). Trouvaille annexe à ramasser dans
le lot A : `penelope-workflow` déclare `penelope-telegram` (`Cargo.toml:27`) sans s'en servir.

## 4. La source unique de vérité, en une page

Détail et preuves dans `source-de-verite.md`.

- Les cinq contenus vus par le modèle deviennent des événements `conv.system`, `conv.user`,
  `conv.assistant`, `conv.tool_result`, `conv.context` dans la table `events` existante,
  chaînés par hachage comme aujourd'hui, avec un numéro de format `v` et une opération de
  surface : `append`, ou `replace(from, to)` d'une plage.
- Les résumés (`conv.summary`), le niveau 1 (corps externalisé), le retour arrière
  (`conv.rewind`, opération `cut`) et le fork (`conv.fork`, opération `inherit`) sont des
  événements qui **remplacent ou coupent une plage de la surface** ; le verbatim reste dans
  le journal, lisible, et la chaîne de hachage ne bouge pas. Le drapeau `messages.compacted`
  muté en place disparaît.
- Les tentatives échouées (`conv.attempt`, issue #206) sont journalisées hors surface : elles
  ne repartent jamais dans un prompt.
- L'historique envoyé au modèle est **dérivé** du journal par une fonction pure `derive`,
  sans base ; `messages`, `messages_fts`, `message_context`, `lcm_nodes`, `prompt_snapshots`
  deviennent des caches alimentés par un projecteur et reconstruits par `penelope history
  reindex`, vérifiés par `penelope history verify` (miroirs de `penelope mem reindex`).
- La décision 0008 (cache de prompt : rien ne bouge avant le dernier message) tient par
  construction : une dérivation append-only ne réécrit rien, et un `replace` n'est écrit qu'à
  une frontière où le cache est de toute façon perdu. `ca_5_4_each_request_extends_the_previous_one`
  reste le test de référence, exécuté sous les deux modes pendant la bascule.
- Migration en quatre phases expand-contract, la V0 restant le chemin de production jusqu'à
  la troisième : double écriture ; comparaison journal contre tables (`history verify`) à
  zéro divergence ; bascule de lecture par une clé `history.source` avec assertion V0 = V1 à
  chaque requête ; retrait du chemin d'écriture direct, verrouillé par une règle
  d'architecture.
- L'historique existant ne reçoit pas de chaîne de hachage rétroactive : chaque session est
  **scellée** par un événement `conv.import` qui porte l'empreinte de son préfixe ; le préfixe
  reste lu dans les tables V0, tout ce qui suit vient du journal.

Ce qui n'est pas repris de DeepSeek Harness, et pourquoi, est écrit dans
`source-de-verite.md` §2.2 : pas de `tool/call` séparé (le ledger d'effets le couvre), pas de
`step/start`, pas de flux brut du fournisseur dans chaque message, pas de fichier JSONL par
session.

## 5. La boucle d'agent, en une page

Détail dans `boucle-et-outils.md`.

- Un tour est un pipeline nommé : résolution des appels en attente, gardes de tour (annulation,
  budget, plafond d'appels, palier de coût), assemblage avec réclamation explicite de l'inbox,
  appel modèle avec plan de tentatives **pur** (`RetryPlan`) et enregistrement des tentatives,
  règlement (usage, instantané de prompt, cause de raté, réponse finale ou appels d'outils).
- Un appel d'outil traverse onze étapes typées : normalisation, description, gardes monotones
  (liste blanche, décision antérieure, garde de boucle, précheck), politique en couches
  nommées (`VerdictLayer`), juge (#203, seulement si `Ask`, `shell_exec`, sans motif possible,
  non destructif), approbation fail-closed, mode d'exécution dérivé de `ToolSpec`, ledger,
  exécution synchrone ou job (#204), post-traitements monotones, enregistrement.
- Les waterfalls de DeepSeek deviennent des traits à implémentation unique et des chaînes
  fixes testées par un test d'ordre : pas d'enregistrement dynamique.
- Le steering devient explicite : `Inbox::claim(Checkpoint)` avec `BeforeModelCall`
  (l'absorption sort de `request_messages`) et `BetweenCalls` (un message arrivé pendant un
  lot d'outils n'attend plus la fin du lot ; les appels non démarrés reçoivent « Non
  exécuté ») ; les notes du harnais deviennent des `Injection` à politique de persistance
  déclarée, placées en queue pour ne pas casser le préfixe.
- Le PTC (`run_code`) est hors V1, avec une couture posée (`CallContext { parent, root }`
  et refus d'une approbation en appel imbriqué) et une décision qui dit pourquoi.

## 6. Le gel et l'outillage, en une page

Détail dans `gel-et-outillage.md`. Onze règles, toutes mécaniques, toutes dans
`penelope-archtest` alimenté par un fichier `budget.toml` dont les nombres ne montent jamais
(`UPDATE_BUDGET=1` les abaisse, un script de CI refuse toute remontée sans le trailer
`Dérogation-budget: #N`) : plafond par fichier (R1), par fichier de tests (R2), liste de
référence décroissante (R3, trois cas d'échec : nouveau dépassement, hausse, entrée périmée),
plafond par crate (R4), liste blanche des modules du daemon (R5), budget d'occurrences de
`Daemon` (R6), fonctions de plus de 200 lignes comptées (R7), critères d'acceptation qui ne
disparaissent jamais (R8), couverture par crate à cliquet vers le haut (R9), test de
composition réelle pour chaque commande, outil et méthode RPC (R10), scénario rejouable sans
clé pour tout changement visible (R11).

La branche `v1` :

- créée depuis `main` **après** le gel et les filets, sur le dernier tag `0.17.x` ;
- une ligne dans `ci.yml:9` (`branches: [main, v1]`) ; le job `livraison` est déjà réservé à
  `main` ;
- versions `1.0.0-alpha.N`, un bump par lot, **jamais taguées** ; même `docs/progress.md`
  avec un bloc « Version 1 » en tête ;
- trois gardes avant tout bump : `upgrade.rs:137` doit ignorer un suffixe de version (sinon
  un tag `v1.0.0-alpha.1` posé par erreur serait installé par toutes les instances 0.17),
  `release.yml` refuse `v1*` sans la variable de dépôt `V1_RELEASES`, le test `docs.rs:530`
  apprend les suffixes ;
- synchronisation `main` → `v1` par fusion après chaque release 0.17.x et au moins une fois
  par jour, script pour les seize lignes de version, une fusion en conflit depuis plus de
  24 h bloque tout autre lot ; `main` ne prend que des corrections ;
- bascule quand tout est vérifiable sans jugement : CI verte, `budget.toml` vide de tout
  dépassement, les 71 critères présents, migration testée depuis une vraie base 0.17 puis
  essai réel par `1.0.0-rc.1` installée explicitement sur le poste, suites réseau au niveau
  de la ligne de base, test `docs` vert. Borne : six semaines ; au-delà, on arrête et on
  replanifie.

## 7. Les filets, avant de découper

Un jalon unique, sur `main`, qui fusionne ce que trois spécifications demandent séparément :

1. un test Telegram simulé de bout en bout sur l'API publique seulement (message → réponse →
   ligne de `tg_outbox` → effet `completed` dans le ledger → redémarrage → même état) ;
2. un test d'approbation de bout en bout (carte, approuver, refuser, « toujours ») ;
3. un contrat RPC doré, une forme JSON par méthode de `penelope_kernel::api::method` ;
4. une fixture de base 0.17 réelle commitée, et un test qui applique toutes les migrations
   dessus (`migrations.rs:1010` ne rejoue aujourd'hui que `0001`) ; un `config.toml` portant
   chaque clé, relu sans avertissement ;
5. les scénarios de session rejouables sans clé (format `scenario`, `model`, `expected`,
   jetons de normalisation, modes rejeu / `UPDATE_SCENARIOS` / `RECORD_SCENARIO`), douze
   scénarios initiaux : tour simple, lectures parallèles, niveau 1 et groupe, compaction puis
   prolongation, session froide, dépassement prouvé, flux coupé, réponse vide, messages
   fusionnés, fork, rewind, purge, crash en deux vies ;
6. la ligne de base des six suites réseau, lancées une fois avec les clés du propriétaire ;
7. le test `docs` corrigé pour lire les sources récursivement (`docs.rs:444` lit
   `penelope-daemon/src` à plat : le premier sous-dossier le casserait).

## 8. La série de lots

Chaque lettre est une épopée GitHub ; chaque tâche des spécifications devient une issue
quand son lot démarre, pas avant (les tâches vieillissent mal). Les flèches sont des
dépendances ; ce qui est sur une même ligne se fait en parallèle.

```
 sur main
 ├─ A  gel de la dette          gel T1..T7, T11, T15 ; découpage T35 (frontière canal), T38   ← cette semaine
 ├─ B  filets                   gel T8..T10, T12, T17..T19 ; noyau T0 ; découpage T04
 │
 ├─ C  branche v1               gel T13, T14, T16
 │
 sur v1  (main → v1 fusionné après chaque release)
 ├─ D  décrocher Daemon          découpage T05..T10, T36 (le cœur ne nomme plus le canal)  ┐
 ├─ E  journal, phases 1 et 2    noyau T1..T12               ├─ en parallèle
 ├─ F  boucle en modules         boucle T01..T08 ; découpage T14, T16, T17 ┘
 │
 ├─ G  fichiers géants           découpage T11..T13, T15, T18..T20
 ├─ H  crates feuilles           découpage T21 app, T22 vault, T25 mcp-host, T28 ops, T29 gateway
 ├─ I  journal, phases 3 et 4    noyau T13..T22   (avant d'extraire la conversation)
 ├─ J  crates du cœur            boucle T09..T11 ; découpage T23 (agent + conversation), T24, T26, T27
 ├─ K  boucle, fils indépendants steering T12..T14 ; tentatives T15..T16 ; jobs T17..T20 ; juge T21..T23 ; T24..T27
 │
 └─ L  clôture et bascule        découpage T30..T32 ; gel §4.5 ; décisions ; docs/architecture.md
```

Ordre justifié par les collisions : le journal (E, I) et l'extraction de la conversation (J)
touchent les mêmes fichiers (`conversation.rs`, `compaction.rs`, `context/`) ; la bascule de
lecture se fait donc **avant** que ces fichiers changent de crate. Les feuilles (H) sortent
tôt parce qu'elles ne dépendent de rien de ce qui bouge. Le découpage de `workflow.rs` (G)
se séquence avec les issues #185 et #191 à #193, qui vivent dans le même fichier.

Comptes : 105 tâches dans les quatre spécifications, environ 90 après fusion des
recouvrements (tentatives : noyau T9 et boucle T15 à T16 ; scénarios : noyau T0 et gel T18 ;
règles d'architecture : gel T1 à T4 et découpage T01 à T03 ; éclatement d'`agent.rs` : boucle
T02 et découpage T14). Tailles : une grande majorité de S et de M ; les L sont l'éclatement de
`telegram.rs`, les crates `app`, `agent`, `orchestrator`, `gateway`, la bascule de lecture du
journal, les jobs et le juge.

## 9. Décisions à écrire dans `docs/decisions/`

Les numéros 0011 (prompt système journalisé, #205) et 0012 (jobs d'outils durables, #204)
ont été pris par des lots livrés sur `main` pendant la rédaction ; 0015 (gel de la 0.17 et
branche `v1`) est écrite. Restent à écrire, dans l'ordre de leur lot :

- 0013 : découpage de `penelope-daemon` en crates, passerelle au-dessus du daemon.
- 0014 : la boucle d'agent est un pipeline d'étapes typées.
- 0016 : le PTC (`run_code`) hors V1, avec sa couture.
- 0017 : le journal d'événements est la source unique de la conversation (au lot E).

## 10. Arbitrages rendus par le propriétaire (23 septembre 2026)

1. **Scellement.** Chaque session existante est scellée par un événement `conv.import` qui
   porte l'empreinte de son préfixe ; aucun message n'est recopié dans le journal avec une
   date de chaîne qui ne dirait rien de la date du contenu.
2. **`conv.system` porte le texte entier du préfixe**, comme DeepSeek Harness : le journal
   se suffit pour rejouer une requête, sans dépendre d'une autre table. Le coût est borné,
   un événement par changement de préfixe et par session, jamais un par tour.
3. **Purge d'un parent et forks : accepté.** Un fork pointe vers le préfixe de sa mère au
   lieu de le recopier ; purger la mère le lui retire, et `penelope purge` prévient avant
   d'agir : « cette session a deux forks, ils perdront leur début ».
4. **Plafonds : 1 000 puis 800.** Sur `main`, dès le gel, 1 000 lignes par fichier tests
   compris avec la liste de référence qui ne peut que rétrécir ; sur `v1`, 800 lignes avec
   les tests en fichiers frères, comme cible de sortie.
5. **Deux crates** : `penelope-agent` (boucle pure, sans base) et `penelope-conversation`
   (transcript, compaction, prompt), §3.2.
6. **Le juge d'approbation (#203) suit le modèle de Hermès**, sous les planchers
   déterministes, mode `explain` par défaut, et ne se construit que si la mesure préalable
   sur l'instance (cartes « sans famille » sur trente jours) le justifie.

## 11. Concurrence avec le travail en cours

Trois autres sessions travaillent sur le dépôt. Le lot #205 est non commité dans l'arbre de
travail (21 fichiers modifiés, `prompt_snapshot.rs`, `audit.rs`, décision 0011) et bouge
pendant la rédaction ; l'issue #207 (`dream::vault_check`) touche `dream.rs` ; #185 et #191 à
#193 touchent `workflow.rs`. Règles : un lot = une version (la serrure de `Cargo.toml`) ;
les déplacements de code sont des commits sans changement de corps, séparés des commits qui
changent une signature ; les tests sous `cfg(target_os = "macos")` (17 sites) ne sont compilés
que par la CI macOS, toute signature qui change se relit à la main.
