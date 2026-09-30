# Pénélope

[![CI](https://github.com/edouard-claude/penelope/actions/workflows/ci.yml/badge.svg)](https://github.com/edouard-claude/penelope/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/edouard-claude/penelope?include_prereleases&sort=semver&label=version)](https://github.com/edouard-claude/penelope/releases)
[![Rust](https://img.shields.io/badge/rust-2024-000?logo=rust)](https://www.rust-lang.org)
[![Plateforme](https://img.shields.io/badge/plateforme-macOS%20arm64-000?logo=apple)](docs/install-headless.md)
[![Documentation](https://img.shields.io/badge/docs-index-informational)](docs/README.md)

Un agent personnel qui tourne en permanence sur un Mac sans écran. On lui parle depuis
Telegram ou en SSH. Elle garde ce qu'elle apprend dans des fichiers Markdown que l'on
peut relire et corriger à la main, appelle des serveurs MCP, exécute des plans qui
survivent à un redémarrage, et demande l'accord de son propriétaire avant tout ce qui
engage. Le modèle peut tourner sur le Mac lui-même.

Rust, 27 crates, `#![forbid(unsafe_code)]` dans chacun. La suite de tests
fonctionne hors réseau externe.

## Pourquoi celle-ci

Il existe des agents personnels bien plus connus, avec dix fois plus de connecteurs.
Pénélope ne joue pas sur ce terrain : un seul canal (Telegram), une seule plateforme
(macOS), un seul propriétaire. Elle traite en revanche sérieusement ce qui casse au bout
de six mois d'usage réel, et qui ne se voit pas dans une démo de cinq minutes.

### La mémoire est un objet versionné, pas un fichier de notes

Ce que l'agent apprend ne s'écrit jamais directement en mémoire. Chaque relecture
d'épisode produit au plus cinq candidats typés (fait, préférence, décision, correction,
écart) posés dans le journal du jour. La nuit, à 3 h 30, une passe les juge sur cinq
critères (durable, utile, précis, introuvable ailleurs, endossé) : le modèle argumente,
le code décide. Un souvenir promu peut en remplacer un autre (`supersede`, `replace`,
`retire`), pas seulement s'empiler ; chaque opération garde sa pré-image dans
`mem_history` et le vault est commité dans git.

Le propriétaire garde le dernier mot. Un remplacement qui vise son profil, ou une entrée
qu'il a écrite, devient une carte (Remplacer, Exception, Ignorer) au lieu de passer en
silence ; la substitution d'une valeur par une autre (tu contre vous, une langue, une
devise, un jour) compte comme une contradiction. « Retiens que… » reste sa parole même
reformulé : une phrase de son message du même tour endosse le candidat. Ses réponses
jugent ensuite chaque souvenir servi : un « non, … » compte une contradiction, un message
ordinaire un succès ; une entrée contestée n'est plus servie d'office et lui est soumise
une fois.

Le vault est un wiki Markdown valide à tout instant : frontmatter YAML, identifiants de
bloc `^uid`, wikilinks. On le lit et on le corrige en SSH pendant que l'agent tourne,
l'écriture est optimiste et rejoue l'opération ligne à ligne si le fichier a bougé. Les
outils de fichiers y écrivent par le préfixe `vault:`, qui n'ouvre que lui ; une note de
forme vault écrite ailleurs est signalée. Sept niveaux, de l'instruction permanente à
l'épisodique, avec la provenance de chaque entrée : ce qui vient d'un document ingéré est
marqué comme non fiable. Les clés et les jetons repérés dans un candidat partent au
magasin de secrets, la mémoire ne garde que `${SECRET:nom}`.

Le rappel ne dépend pas d'un modèle : recherche déterministe bornée à 150 ms au début du
tour, recherche sémantique quand la phrase montre une intention de rappel, embeddings
calculés en fond avec repli lexical si le budget de 1,5 s est dépassé.

### Le contexte est découpé, et le préfixe ne bouge pas

Le prompt est bâti en cinq tuiles, de l'identité (T0) au volatil de fin de prompt (T4).
Le préfixe T0 à T2 est identique octet pour octet d'un tour à l'autre : ce n'est pas une
intention, c'est un test (`ca_5_3_prefix_is_byte_identical_across_turns`). Un souvenir,
une skill ou un serveur MCP ajoutés en pleine conversation n'y entrent qu'au prochain
cache froid ; d'ici là, leur différence part **en fin de prompt**, une seule fois, dans un
bloc `<mise-a-jour>` du message suivant. La liste d'outils suit la même règle : gelée
entre deux frontières, un outil découvert entre-temps s'appelle par `tool_call`.

Cinq niveaux de compaction, dont un seul appelle un modèle ; le résumé peut relire le
préfixe de la conversation au prix du cache plutôt que tout au prix fort
(`context.compaction_on_prefix`). `context.max_prompt_tokens` borne le contexte
indépendamment de la fenêtre annoncée par le modèle, la compaction de fond se déclenche
sur le prompt réellement facturé au dernier appel, et une réserve budgétaire empêche le
plafond du jour de bloquer les résumés. Chaque raté de cache est attribué à une cause (la
tuile du prompt qui a bougé, quand c'est le préfixe) et visible par
`penelope usage --by miss`. Le prompt système envoyé est gardé sous son empreinte :
`penelope audit show --turn <id>` rend ce que le modèle avait sous les yeux, et dit ce
qu'il ne peut pas reconstituer.

### Une session longue ne se perd pas

Le journal d'événements est chaîné par hachage : une altération est détectée, et une
erreur de lecture fait refuser l'écriture plutôt que forger un maillon. Tout effet passe
par un ledger avant exécution ; un effet dont l'issue est incertaine devient une
question posée au propriétaire, jamais une relance automatique. Un run de workflow écrit
son étape courante, ses sorties et son journal dans la même transaction, et reprend à
cette étape après un redémarrage. Le tour interrompu est remis en file une seule fois.

La machine aussi est prise en compte. Un tour, un job ou un run tiennent l'anti-veille ;
une sortie de veille est remarquée, une passe de santé revérifie le canal et relance les
serveurs MCP dégradés, puis les créneaux manqués partent une fois, en disant leur retard.
Une planification qui échoue en série n'alerte qu'au premier échec, quand le motif change
et à des paliers, jamais 24 fois par jour. Chaque arrêt dit qui l'a demandé et pourquoi,
et le démarrage suivant cite l'arrêt précédent, propre ou non.

Le [flux runtime](docs/runtime-events.md) permet à un observateur local authentifié de
suivre ces événements en direct et de les rejouer depuis un identifiant durable. Il
est désactivé sans consommateur configuré : ce WebSocket de lecture seule n'est pas
une API pour poser des questions à Pénélope ou lui envoyer des commandes.

### Une autorisation a des bornes

Répondre « Toujours » ne donne pas un blanc-seing sur un outil : la règle créée est
bornée par un motif d'arguments, famille de commande pour `shell_exec`, répertoire pour
une écriture, remote et branche pour un `git_push`, hôte pour une requête HTTP, clé pour
un changement de configuration. Les motifs refusent tout enchaînement (`;`, `&&`, `|`,
substitution, redirection). Une règle est visible et révocable, la première décision
gagne, et le contenu rapporté par un outil est une donnée, jamais une instruction.

Une ligne de commande sans motif possible peut être décrite par un juge (un modèle
auxiliaire, sous des planchers déterministes) avant d'aller sur la carte ; ses décisions,
carte ou pas, peuvent être gardées dans un jeu local, opt-in, exportable en JSONL pour
évaluer un autre juge plus tard.

### Un plan, pas un formulaire

Un travail qui écrit du code ne se lance pas par un formulaire : on en parle, Pénélope
propose un plan (but, pas, paramètres, brief) dans le sujet de la conversation, on le
corrige, et « Vas-y » porte l'empreinte de la révision montrée. Un clic périmé est
refusé, un double clic ne lance qu'un run.

Le plan approuvé devient un run durable : une phase par pas, chacune dans un agent à
contexte neuf, le modèle choisi par phase, des cartes d'OK après la spécification, les
tests et le code, une revue contradictoire bornée à deux reprises. Un plan qui écrit du
code est ensuite livré en dev : PR sur le forgeur (GitHub ou GitLab, lus dans
`.penelope/delivery.toml` et dans le dépôt, rien n'est supposé), CI du commit poussé, E2E
depuis l'extérieur ; une seule PR par run, même après un arrêt brutal. Puis un bilan
vérifié et un gate humain : « Proposer la PR prod », « Re-vérifier », « Refuser ». Le clic
n'est pas cru sur parole, un bilan périmé refait PR, CI et E2E. Pénélope ne fusionne pas
et ne déploie rien.

### Les modèles : l'inférence locale d'abord

Un Mac Apple Silicon peut servir le modèle lui-même : `mlx_lm.server` (MLX d'Apple) en
LaunchAgent, installé par `penelope local install <modèle>`, visé par un alias
`local:<modèle>`. Rien ne sort de la machine, un appel compte 0 $, et `doctor` surveille
chaque endpoint (`local.<endpoint>`) : joignable, modèles servis, fenêtre, part de
l'entrée relue du cache. Plusieurs serveurs coexistent (`providers.extra`) : le texte
d'un côté, la voix de l'autre. Mesuré sur un MacBook Air M2 avec Qwen3-1.7B : premier
jeton de 8,9 à 13,6 s à froid pour 4 420 jetons, 0,86 s au tour suivant avec le cache. Un
modèle local homonyme d'un modèle du cloud a sa propre entrée au catalogue et son propre
prix. C'est la direction du projet : macOS, Apple Silicon ; un portage Linux n'est pas
prévu (issues #196 à #202, fermées le 29/09/2026 comme non planifiées).

Les deux autres portes : OpenRouter (clé d'API, coût facturé à l'appel), et le backend
Codex d'un abonnement ChatGPT, qui ne facture rien à l'appel mais consomme le quota du
plan. L'abonnement ne sert que les tours ouverts par le propriétaire : rêve, veille,
compaction, workflows et autres travaux de fond repassent par OpenRouter, sans un mot.
Cet usage est toléré par OpenAI, jamais garanti par contrat, et Pénélope le dit là où ça
compte ([décision 0010](docs/decisions/0010-fournisseur-codex-oauth.md)).

Les replis s'enchaînent d'un fournisseur à l'autre, joués par la boucle : un serveur
local arrêté cède la main à OpenRouter, et l'inverse. Un appel d'outil qu'un petit modèle
rend en texte est relu comme un appel.

### Le coût est mesuré, pas estimé

Le coût enregistré est celui facturé par le fournisseur, pas une multiplication de
tokens par un tarif de catalogue ; un appel local est compté en jetons et facturé 0 $. Il
se lit par session, par tour, par modèle, par jour, par rôle, par fournisseur amont et
par cause de raté de cache. Plafonds jour, session et run, alerte à 80 %, point de
contrôle au-delà d'un dollar dans un même tour, délégation à un sous-agent après dix
appels d'outils.

### Un travail long ne bloque pas la conversation

`shell_exec` et `sub_agent_spawn` acceptent `background: true` : l'appel rend la main tout
de suite, la commande continue hors du tour, et son résultat revient seul dans la
conversation, même si le tour d'origine est clos depuis longtemps, même si la session
était fermée entre-temps. Pendant ce temps un message reste traité sans attendre, `/stop`
coupe le job et son groupe de processus, et deux plafonds empêchent d'en accumuler. Un job
est un effet comme un autre : planifié dans le ledger avant de partir, jamais relancé tout
seul après un redémarrage.

Ce qu'elle exécute se voit : une bulle par tour sur Telegram, modifiée en place, une
ligne par outil avec son argument principal, les appels identiques groupés (×4), ✅ ❌ 🚫 ⏹
(`telegram.tool_trace` : `off`, `compact`, `full`). Une photo trop lourde pour le
fournisseur est réduite avant l'envoi ; une photo qu'il refuse quand même est retirée de
la copie envoyée, le tour le dit, et la session n'en souffre plus.

### Elle sait ce qu'elle est

Sa documentation est compilée dans son binaire : elle la cherche, la lit par section, et
cite le lien GitHub au tag de la version qui tourne. `self_status` lui rend sa version,
son modèle du tour, sa configuration effective, ses coûts, sa file, l'état de la machine
et l'inventaire de ses outils, workflows, skills, serveurs MCP et limites. Une carte de
l'environnement, dressée au démarrage et chaque heure, jamais dans un tour, relève la
puce et la mémoire, tous les exécutables du PATH avec leur source et leur version, les
applications, les MCP exposés par des applications (Safari 27, le pont MCP de Xcode) et
les serveurs d'inférence locaux ; `env_explore` y cherche par besoin, et une capacité non
branchée est proposée une fois, jamais imposée. Les tables de référence de la
documentation (outils et clés de configuration) sont générées depuis le code, et un test
refuse une section « limites » qui décrirait comme manquant quelque chose de livré.

## Comparaison

Les projets les plus proches sont OpenClaw et Hermes. Une comparaison mécanisme par
mécanisme, relevée le 18 septembre 2026 et non revérifiée depuis, est dans
[docs/comparaison.md](docs/comparaison.md), avec ce que les autres font mieux.

## Ce qu'elle ne fait pas

Version 1.0 : la V1 est sur `main` depuis le 27 septembre 2026 et publiée en release
(plus en pre-release depuis la 1.0.0). Une seule instance réelle en service.

- macOS seulement. Les backends Linux et Windows compilent et renvoient `Unsupported` ;
  un portage Linux n'est pas prévu (issues #196 à #202, fermées le 29 septembre 2026).
- Telegram seulement, en long polling. Le mode webhook n'est pas servi.
- Un seul propriétaire par instance, tout autre expéditeur est refusé.
- Les requêtes `sampling/createMessage` d'un serveur MCP sont refusées.
- L'OCR ne se déclenche que si un PDF n'a aucune couche texte : un document mixte garde
  ses pages scannées illisibles.
- La recherche vectorielle est exhaustive, à revoir au-delà de 200 000 entrées.
- La signature minisign des releases est implémentée mais la clé n'est pas créée : seule
  la somme SHA-256 est vérifiée aujourd'hui.
- Les suites qui parlent à de vrais services ne tournent pas en CI et pas à chaque lot.
  Elles ont tourné à la main : `live_openrouter` 5/5 et `ctx_recall` le 26 septembre 2026,
  `mem_longitudinal` le même jour à 71 % pour un seuil de 85 % (défaut de qualité de la
  mémoire, corrigé depuis par les 1.0.8 et 1.0.11 ; le rejeu n'est pas consigné ici),
  `live-local` verte le 29 septembre contre un vrai `mlx_lm.server`.
- Le gate de production ne fusionne pas la PR et ne déploie rien ; seule la CI du forgeur
  est lue, pas celle d'un autre système.
- Le bac à sable n'existe que sur macOS : la suite entière tourne aussi sur Linux (la CI
  la rejoue sur `ubuntu-latest`), mais les tests de Seatbelt, de launchd et du trousseau
  ne tournent que sur `macos-14`.
- Les manques et les écarts trouvés en revue sont suivis dans les
  [issues](https://github.com/edouard-claude/penelope/issues) du dépôt, lisibles par
  tout le monde.

## Démarrer

```bash
cargo build --release
```

```bash
./target/release/penelope paths
```

L'installation complète sur un Mac sans écran (service `launchd`, secrets, Telegram,
inférence locale, veille, signature) est décrite dans
[docs/install-headless.md](docs/install-headless.md) ; l'index des commandes est dans
[docs/README.md](docs/README.md#commandes-en-ligne).

Toutes les commandes acceptent `--home <répertoire>` (ou `PENELOPE_HOME`) pour déplacer
l'intégralité de l'état : c'est ce qui rend les bacs à sable et les tests possibles sans
toucher au vrai profil.

## Comment un message est traité

Du message Telegram à la mémoire, dans l'ordre. Chaque case est un crate ou un module
nommé dans [docs/architecture.md](docs/architecture.md).

```
 propriétaire ── Telegram ──► passerelle     commandes, cartes, bulle de trace des outils
                                 │
                                 ▼
                              daemon         file des tours, RPC locale, superviseur :
                                 │           ordonnanceur (planifications, rêve à 3 h 30),
                                 │           inventaire horaire, jobs d'outils
                                 ▼
          ┌──────────── construction du prompt (penelope-context) ──────────────┐
  stable, │ T0 identité et règles · T1 skills, méta-outils, MCP, machine        │
  en cache│ T2 AGENTS.md, instantanés mémoire                                   │
          │ T3 historique : résumés puis fin verbatim                           │
  volatil │ T4 date, rappel mémoire (150 ms), notes, <mise-a-jour>              │
          └───────────────────────────────┬────────────────────────────────────┘
                                          ▼
                    boucle d'agent ──► modèle de l'alias : local (MLX), OpenRouter
                                          │           ou Codex ; replis joués en chaîne
                                          ▼
                   politique d'approbation : auto, carte Telegram, juge ; règle bornée
                                          │
                                       exécuteur
                    ┌─────────────────────┼─────────────────────┐
              outils natifs             skills              serveurs MCP
        fs_*, shell_exec, http_fetch  skill_load,        paresseux : tool_search,
        mem_*, env_explore, vault:    à la demande       tool_describe, tool_call
                    └─────────────────────┼─────────────────────┘
                                          ▼
          ┌───────────────────────────────┼───────────────────────────────┐
          ▼                               ▼                               ▼
  journal d'événements            ledger d'effets                 vault (wiki Markdown)
  source unique, chaîné           planifié avant l'appel,         journal du jour ─► rêve
  par hachage                     jamais rejoué deux fois         à 3 h 30 ─► mémoire
                                                                  ─► rappel au tour suivant
```

Le prompt part au modèle ; chaque appel d'outil passe par la politique d'approbation
avant l'exécuteur ; tout ce qui s'est passé est un événement du journal, chaque effet une
ligne du ledger, et ce qui mérite d'être retenu un candidat du journal du jour, jugé la
nuit, rappelé au tour suivant.

## Comment c'est fait

27 crates, dépendances orientées, aucune dépendance circulaire. Les règles sont vérifiées
par des tests (`cargo test -p penelope-archtest`), pas par la discipline. Le graphe exact,
les ports et les chiffres sont dans [docs/architecture.md](docs/architecture.md) ; en gros,
de haut en bas :

```
 penelope-cli                      le binaire : CLI, client RPC, composition
   ▼
 penelope-evals                    suites déterministes, scénarios rejouables
   ▼
 penelope-gateway-telegram ──────► penelope-telegram (client Bot API)
   ▼
 penelope-daemon                   composition, moteur des tours, RPC
   ▼
 orchestrator, ops, mcp-host
   ▼
 agent, conversation, dream, executor, vault
   ▼
 penelope-app                      Services, ports, bus des tours
   ▼
 workflow, tools, context, memory, mcp, llm, hitl, skills
   ▼
 penelope-kernel ──► penelope-store              socle : store, observe, platform
```

- `penelope-store` ne dépend de rien et réexporte `rusqlite` : aucune crate métier ne
  connaît le pilote SQL (seule la CLI ouvre la base elle-même, en lecture seule, pour
  `penelope approvals stats` et `penelope dataset export`).
- `penelope-platform` isole tout ce qui est spécifique à un OS. Un chemin littéral, un
  appel shell, un signal Unix ou une API Keychain ailleurs fait échouer le test
  d'architecture.
- La boucle d'agent ne voit ni la conversation ni l'exécuteur, l'exécuteur ne voit ni la
  boucle ni l'orchestrateur : ils se parlent par les ports de `penelope-app`. Le cœur
  ne nomme pas Telegram au-delà d'un relevé qui ne peut que descendre : le canal passe par
  la passerelle, au-dessus du daemon.
- La dette est gelée par `crates/penelope-archtest/budget.toml` : aucun fichier au-delà de
  1 000 lignes, le daemon plafonné, les critères d'acceptation figés ; ses nombres ne
  montent jamais.
- Le client MCP, le client Bot API, le validateur JSON Schema, la recherche vectorielle
  et la surveillance de fichiers sont écrits ici : aucun
  framework d'agent, aucune bibliothèque C ajoutée.
- Chaque écart assumé par rapport à la spécification est un fichier de
  [docs/decisions/](docs/decisions/), avec son contexte, ses conséquences et parfois son
  point de bascule.

## Tests

```bash
cargo test --workspace
```

La suite, y compris les tests du WebSocket local, n'a besoin d'aucun secret en CI.
Les suites nommées sont des filtres sur cette même commande, ce qui évite
qu'un chemin de test diverge de l'autre.

```bash
cargo test -p penelope-evals --test mcp_conformance
```

Chaque commande Telegram, outil natif et méthode RPC est exercé par au moins un scénario
de session rejouable sans clé (`crates/penelope-evals/scenarios/`) ; une surface nouvelle
sans scénario fait échouer `penelope-archtest`.

```bash
cargo test -p penelope-evals --test scenarios
```

La matrice des critères d'acceptation, [docs/ca-matrix.md](docs/ca-matrix.md), est
**générée** depuis les sources : tout test nommé `ca_<section>_<n>_<nom>` y entre
automatiquement.

Les suites qui parlent à de vrais services (`live-openrouter`, `live-telegram`,
`live-local`, `ctx-recall`, `mem-longitudinal`, `ab-hermes`) sont ignorées par la CI et se
lancent depuis le dépôt avec leurs variables d'environnement.

La chaîne d'outils est épinglée dans `rust-toolchain.toml` : le lint local rend
exactement le même verdict que la CI.

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo deny check
```

## Documentation

- [docs/README.md](docs/README.md) : index, par besoin, par commande et par fichier.
- [docs/install-headless.md](docs/install-headless.md) : installation, configuration,
  référence des clés et des outils natifs.
- [docs/context.md](docs/context.md) : compression du contexte, seuils, budgets, cache.
- [docs/mcp.md](docs/mcp.md) : versions, transports, OAuth, registre paresseux.
- [docs/workflows.md](docs/workflows.md) : schéma complet, cycle de vie d'un run, plan en
  phases, livraison et gate de production.
- [docs/telegram.md](docs/telegram.md) : commandes, gabarits, rendu, trace des outils.
- [docs/runtime-events.md](docs/runtime-events.md) : flux runtime local, replay et démonstration Pathlayer.
- [docs/comparaison.md](docs/comparaison.md) : Pénélope face à OpenClaw et Hermes,
  instantané du 18 septembre 2026.
- [docs/ca-matrix.md](docs/ca-matrix.md) : critères d'acceptation et tests qui les
  couvrent.
- [docs/architecture.md](docs/architecture.md) : crates, couches, ports, frontière canal,
  règles d'architecture et gel.
- [docs/progress.md](docs/progress.md) : notes de chaque version de la V1 ; celles de la
  0.17 sont archivées dans [docs/progress-0.17.md](docs/progress-0.17.md).

Un test vérifie que cet index cite chaque page, qu'aucun lien n'est mort, et que les
tables de référence correspondent au code.

## Principes qui ne se négocient pas

1. Rien d'irréversible sans accord explicite, et l'accord est traçable.
2. Un effet incertain ne se relance jamais tout seul : il devient une question.
3. Le savoir durable est un fichier Markdown que l'on peut lire et corriger en SSH.
4. Les données rapportées par un outil ne sont jamais des instructions.
5. Le chemin d'écriture de la mémoire est une frontière de sécurité, pas un cache.
