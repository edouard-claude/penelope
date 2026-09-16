# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 16 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **1110 tests verts**, tous hors réseau.
- `cargo clippy --workspace --all-targets -- -D warnings` : propre.
- `cargo deny check` : propre (avis, interdits, licences, sources).
- `cargo fmt --all --check` : propre.
- CI GitHub Actions (format, lint, tests, binaire, dépendances) et workflow de release
  sur tag `vX.Y.Z` avec binaire universel macOS.
- 65 tests d'acceptation nommés `ca_<section>_<n>_<nom>`, couvrant 14 sections du PRD,
  indexés dans [ca-matrix.md](ca-matrix.md), qui est généré depuis les sources.

## Étapes du §21

| # | Étape | État | Tests |
|---|---|---|---|
| 1 | `store` + `kernel` + `observe` | fait | 95 kernel, 13 store, 28 observe |
| 2 | `llm` + boucle d'agent | fait, `cli chat` non branché | 82 llm, 36 daemon |
| 3 | `context` (tuiles, ancres, niveaux 0 à 4, LCM) | fait | 73 |
| 4 | `telegram` (transport, rendu, gabarits, CTA, formulaires) | bibliothèque faite, boucle non lancée | 85 |
| 5 | `hitl` + bac à sable + `tools` | fait | 16 hitl, 64 tools, 54 platform |
| 6 | `mcp` (négociation, transports, primitives, OAuth, registre, supervision) | fait, superviseur lancé par le daemon ; OAuth non branché | 96 + 19 conformité + 13 daemon |
| 7 | `memory` + `skills` | fait | 97 memory, 12 skills |
| 8 | `workflow` + déclencheurs + workflows livrés | moteur fait, ordonnanceur non lancé | 74 |
| 9 | Routage par complexité, budgets, images, STT | fait côté bibliothèque ; l'ingestion Telegram qui les alimenterait ne tourne pas | inclus en llm |
| 10 | `resilience`, `upgrade`, `backup`, suites live, `ab-hermes` | résilience et sauvegarde faites ; `upgrade`, suites live et A/B non faits | 10 resilience |

## Suites du §20.1

| Suite | Réseau | État |
|---|---|---|
| `unit` | non | verte |
| `arch` | non | verte |
| `ctx-safety` | non | verte |
| `mem-learning` | non | verte |
| `mcp-conformance` | non | verte, 19 tests sur la matrice versions × transports |
| `telegram` | non | verte |
| `hitl` | non | verte |
| `workflow` | non | verte |
| `hot-reload` | non | verte, 14 tests |
| `resilience` | non | verte, 10 tests |
| `security` | non | verte, 11 tests |
| `ctx-recall`, `mem-longitudinal`, `live-openrouter`, `live-telegram`, `ab-hermes` | oui | **non écrites** : elles exigent un modèle réel, un bot réel et l'instance Hermes |

## Ce qui reste à faire

### Branché depuis la 0.1.0

- **Conversation** : boucle d'agent sur l'historique persistant, prompt T0 à T4 (âme,
  skills, instantanés mémoire, rappel), alias collant et classifieur de complexité, repli
  de modèle sur panne, streaming.
- **Exécuteur d'outils natifs** : fichiers, shell sous bac à sable, git, HTTP (contenu
  encadré comme non fiable), mémoire (écriture dans le vault), intentions, planification,
  historique, artefacts, skills, workflows (description, contrôle, écriture).
- **Approbations** : suspension du tour, reprise par un tour `resume` qui exécute l'appel
  approuvé sans redemander, refus transmis au modèle, fenêtres « session » et « toujours ».
- **Pool de runners**, bus d'événements, attente des issues sans perte.
- **Passerelle Telegram** : long polling persistant et dédupliqué, commandes, cartes
  d'approbation à boutons (double confirmation des actions destructives, refus motivé),
  brouillons `sendMessageDraft`, file d'envoi durable avec repli en texte brut, envoi de
  fichiers. Conforme à la Bot API 10.3 réelle (`draft_id` entier, `rich_message.markdown`).
- **RPC** : `chat.send`, `chat.stream`, `chat.stop`, `session.switch`, `secret.set`,
  `model.route_test`, `quiet`, `tail` ; `approve` et `deny` relancent le tour.
- **CLI** : `penelope chat` (ponctuel ou interactif), `penelope secret set`.
- **Catalogue de modèles** chargé au démarrage puis toutes les 6 h ; `model list` montre
  les alias et cherche dans le catalogue.

### Depuis la 0.2.1 (0.2.2 et 0.2.3)

- **OpenRouter aligné sur sa documentation** : coût facturé (`usage.cost`, BYOK compris)
  plutôt qu'estimé ; `session_id` pour le routage collant et le cache ; replis de modèle
  confiés à OpenRouter (`models`), visibles (`llm.fallback_used`) ; erreurs typées
  (`error_type`, en-tête `Retry-After` honoré une fois) ; refus (`refusal`) et provider
  amont conservés ; annulation qui coupe la connexion même pendant un silence ; en-têtes
  `X-OpenRouter-Title` et `X-OpenRouter-Categories` ; préférences de provider complètes
  (`zdr`, `sort`, `only`, `ignore`, `quantizations`).
- **Réponses vides** : raisonnement renvoyé pendant les enchaînements d'outils,
  contexte volatil dans le dernier message utilisateur, une relance puis un diagnostic
  (fin brute, provider amont, budget de sortie mangé par le raisonnement).
- **Routage lisible** : `model list` et `/models` montrent classifieur, étages et replis ;
  `/model` répond par des boutons qui épinglent un modèle sur la session
  (`penelope session model`) ; `/model auto on|off` ; l'alias `low` ne colle plus à une
  session ; le classifieur
  réduit son raisonnement et demande une sortie structurée quand le modèle le permet.
- **Coûts attribués** : chaque appel porte sa requête d'origine, son rôle, sa génération
  et son provider amont ; `penelope usage --by session|turn|model|day|role|upstream`,
  `/budget`.
- **Shell** : réseau autorisé par défaut (`sandbox.shell_network`), agent SSH et
  emplacements de configuration transmis ; `Makefile` (`make deploy`).
- **Vocaux Telegram** : téléchargement, transcription par le rôle `stt` (OpenRouter ou
  serveur local OpenAI-compatible comme whisper.cpp), citation puis tour normal.
- **Pénélope connaît son état** : outil `self_status` (modèle du tour, routage,
  configuration sans secret, coûts, file, machine avec batterie, disque, mémoire, charge) ;
  `config_set` sous approbation, double et systématique pour les réglages sensibles.

### 0.2.4

- **Superviseur MCP** : serveurs de `mcp.d/` chargés au démarrage et à chaud, découverte
  des outils quand ils sont inconnus, démarrage paresseux au premier appel, arrêt des
  inactifs, reprise à backoff puis panne déclarée, repli `initialize` pour les serveurs
  que la sonde 2026 déroute. Profils de bac à sable par serveur (`full` sur autorisation
  explicite), secrets injectés dans l'environnement du serveur seulement, requêtes du
  serveur traitées (`roots/list`, `ping` ; sampling refusé, elicitation déclinée),
  `list_changed` suivi. `tool_policy` et `tool_risk` appliqués, outils `eager_schemas`
  donnés directement au modèle. Méthodes `mcp.*` (sauf `mcp.auth`), `penelope mcp`,
  `/mcp`, contrôles `doctor`, état dans `self_status`. Validé contre un vrai serveur
  mcp-go (37 outils) sous bac à sable.

### 0.2.5

- **Ordonnanceur** : cron (fuseau du propriétaire, tir unique `once` pour les rappels
  datés), intervalle, `mcp_poll` (outils en lecture seulement, amorçage sans tir,
  déduplication, coalescence des notifications), `watch_file`, `event` (fenêtre
  d'événements bornée, historique ignoré). Cibles `notify` (sans modèle), `prompt` (tour
  déclencheur dans la conversation d'origine) et `workflow`. Tir manqué rattrapé une fois.
  `schedule.add` et `schedule.run_now`, `penelope schedule add|run`, `/schedules`.
- **Intentions** : une intention armée revient dans le contexte du message qui la
  réveille, tire une fois par tour même rejoué ; une intention datée est redirigée vers
  un déclencheur.

### 0.2.6

- **Compaction de niveau 3** : quand la projection d'un tour atteint le seuil moins la
  marge, le rôle `compaction` résume en tâche de fond les messages que la queue verbatim
  ne garde pas. Résumé structuré validé (sortie JSON stricte quand le modèle la supporte,
  sections tronquées à 4 000 caractères, résumé vide refusé), ancres extraites du texte
  complet, messages utilisateur verbatim. La re-compaction **prolonge** le nœud précédent
  (couverture et tokens source additionnés) au lieu d'en empiler un second.
- Lots explicites quand la fenêtre du résumeur est trop petite (jamais d'abandon), gros
  messages échantillonnés tête et queue pour le résumeur.
- Publication à la frontière de tour : immédiate si la session est au repos, sinon mise
  de côté (persistée) et publiée à la fin du tour. Un travail périmé est refusé.
- Cooldown persisté 60 s, 300 s, 900 s ; `/compact`, `penelope session compact` et
  `session.compact` le lèvent. Un dépassement de fenêtre prouvé par le provider déclenche
  une compaction immédiate puis une seule relance de la requête.
- Coût attribué au tour déclencheur (rôle `compaction`), événements `context.compacted`
  et `context.compaction_failed`, métrique `penelope_compactions_total`.

### 0.2.7

- **Photos** (§10.4, §14.4) : montrées au modèle de la session s'il lit les images, sinon
  décrites par le rôle `image_describe` et jointes en texte ; album regroupé sur 1,5 s ;
  un modèle sans vision (repli, changement de modèle) reçoit une mention à la place des
  images de l'historique.
- **Ingestion de documents** (§6.13) : PDF (`lopdf`, décompression bornée par page,
  panique rattrapée), DOCX (`zip`), HTML, Markdown, texte ; fiche
  `vault/sources/<slug>.md` d'origine `untrusted` (sauf `/mien`), secrets et numéros de
  carte masqués, original conservé sous `{data}/media/documents`, passages indexés en
  type `source`, exclus du rappel automatique et encadrés à la lecture (`mem_search`,
  `mem_get`). Même contenu reçu deux fois : fiche reprise. La réindexation garde la
  provenance déclarée par la fiche.
- **Résumé et propositions** : rôle `memory_review`, au plus cinq faits passés au filtre
  d'écriture de la mémoire, approbation `memory_proposal` (carte « Tout » / « Rien »,
  `penelope approve`) ; un fait accepté rejoint `notes.md` avec le document en
  provenance, une seule fois.
- **Boîte de dépôt** `vault/inbox/` scrutée à côté de l'ordonnanceur ; autres pièces
  jointes rangées en artefact (texte) ou dans `<workspace>/telegram/` (binaire).
- Version minimale de Rust portée à 1.88 (exigée par `lopdf` et `zip`), lints clippy
  associés appliqués.

### 0.2.8

- **Moteur de workflows** (§12.7) : un pilote par run actif, étape courante exécutée,
  transition choisie, `advance` transactionnel. Étapes `agent` (session du run, outils de
  workflow, relances bornées jusqu'à `step_done()`), `sub_agent` (contexte neuf, outils
  en lecture par défaut, `outputSchema` validé en deux tentatives), `shell` et `tool`
  (ledger d'effets : rejoués après un crash, jamais ré-exécutés ; politique et
  approbations comme en conversation), `user` (question à boutons, saisie texte),
  `parallel` (concurrence bornée, agrégat `success`/`partial`/`failure`), `workflow`
  (profondeur 3), `wait` (délai, événement, cron, `timeoutMs`), `verify` (contrôles puis
  vérificateur, statuts des critères mis à jour). `retry` avec backoff, bornes
  d'itérations, de budget (recalculé depuis l'usage du run) et de durée.
- Admission `hold` (file), `coalesce`, `drop`, `parallel` ; paramètres vérifiés
  (obligatoires, défauts, inconnus refusés) ; workspace éphémère `{state}/runs/<run>`
  nettoyé après la rétention, ou persistant.
- Contrôle `pause`, `resume`, `cancel`, `retry-step`, `skip-step`, `goto` ; `skip-step` et
  `goto` refusés à l'outil de l'agent. Une décision d'approbation réveille le run.
- Carte de progression Telegram éditée sur place ; `/run`, `/resume`, `wf.run`,
  `penelope wf run`, `penelope wf control … answer`. L'ordonnanceur démarre les
  workflows ciblés.
- **Sous-agents** (`sub_agent_spawn`) et **génération d'images** (`image_generate`,
  modalités `image` + `text`, images `data:` enregistrées puis envoyées).
- Bac à sable macOS : les chemins autorisés sont aussi déclarés sous leur forme résolue
  (`/var` → `/private/var`, `/tmp`), faute de quoi l'écriture était refusée dans un
  workspace atteint par un lien symbolique.

### 0.2.9

- **Revue de fond** (§6.6) après les échanges substantiels : rôle `memory_review`, au plus
  `memory.review_max_candidates` candidats typés, filtre d'écriture, origine `owner` pour
  ce que le propriétaire a dit, importance 8 pour une correction, ligne dans le journal du
  jour.
- **Consolidation nocturne** (§6.8) : verrou, `dream_runs` (Light → REM → Deep), groupes
  dédoublonnés, réflexions dans `DREAMS.md`, portes déterministes (non fiable et système
  exclus avant tout prompt), contradictions en questions, opérations du rôle `compaction`
  validées (uids, pratiques, prédicats, plafond de retrait, conflit d'édition manuelle,
  filtre) puis appliquées ligne par ligne (`add_entry`, `replace_entry`, `retire_entry`,
  `add_exception`, `record_ecart`, `update_exception`, `link`, `create_entity`), pré-images
  dans `mem_history`, index mis à jour, commit git du vault. `update_default` reste une
  proposition. `--dry-run` n'écrit rien. Écarts périmés expirés.
- **Digest du matin** et crons système `dreaming_cron`, `digest_cron`.
- Méthodes `mem.history`, `mem.restore`, `mem.reindex`, `mem.forget`, `mem.candidates`,
  `mem.dream`, `mem.learned`, `vault.sync`, `vault.check` ; `penelope mem …`,
  `penelope vault …` ; `/dream`, `/appris`, `/pratique`.
- Correctif : une pratique réécrite perdait son défaut à la relecture (puce manquante).

### Encore à brancher

1. **OAuth des serveurs MCP** : flux `paste_back` depuis Telegram, `mcp.auth`.
2. **Frontières d'épisode** (§6.6) : clôture sur inactivité ou changement de sujet,
   ingestion du transcript de l'épisode ; aujourd'hui la revue se fait tour par tour.

### Méthodes RPC déclarées mais non servies

Un test (`penelope-daemon`, `rpc.rs`) fixe cette liste : elle ne peut pas s'allonger en
silence.

```
session.fork  session.rewind
mcp.auth
skill.rollback
import.hermes  export  restore  store.rebuild  eval.run  upgrade
```

### Autres manques

- Workflows : sémantique des sous-groupes (échappement par transition taguée), saisie
  `form:<schema>` des étapes `user` et attente `mcp_task` non implémentées ; le scénario
  `ticket-to-deploy` de bout en bout contre des mocks (CA 12) reste à écrire.
- Pas d'OCR : un PDF scanné sans couche texte est signalé, pas lu. L'extraction PDF de
  `lopdf` ignore la mise en page (colonnes, tableaux) et les polices sans table Unicode.
- `penelope import hermes` (§20.2, point 3) : non implémenté.
- `penelope upgrade` : non implémenté.
- Les captures d'écran de [telegram.md](telegram.md) sont des maquettes ASCII.

## Décisions

Les écarts assumés par rapport à un « DEVRAIT » du PRD sont documentés un par un :

| # | Décision | Raison courte |
|---|---|---|
| [0001](decisions/0001-kernel-depend-de-store.md) | `penelope-kernel` dépend de `penelope-store` | Le noyau **est** la couche durable ; `store` est une infrastructure, pas un crate métier |
| [0002](decisions/0002-pas-de-sqlite-vec.md) | Pas de `sqlite-vec` | Recherche exhaustive en Rust : même sémantique, une dépendance C de moins |
| [0003](decisions/0003-validateur-json-schema-local.md) | Validateur JSON Schema maison | `$ref` borné (contenu non fiable) et annotations exposées au moteur de formulaires |
| [0004](decisions/0004-ipc-tokio-unix-socket.md) | `tokio::net::UnixListener` direct | Seul macOS est livré ; permissions `0600` explicites |
| [0005](decisions/0005-watcher-par-scrutation.md) | Surveillance par scrutation | La resynchronisation périodique est déjà le mécanisme de vérité, et elle est testable |

## Deux failles corrigées en écrivant la suite `security`

Dignes d'être notées, parce qu'elles montrent à quoi la suite sert :

- `http::check_url` acceptait `http://[::1]/`. `Url::host_str()` rend une IPv6 **entre
  crochets**, et `"[::1]".parse::<IpAddr>()` échoue : la boucle locale v6 passait donc à
  travers le filtre SSRF. Les crochets sont maintenant retirés avant l'analyse.
- `fs::resolve` acceptait `~/Library/Preferences`. Le tilde n'étant pas développé, le
  chemin était joint au workspace et créait silencieusement un répertoire nommé `~` au
  lieu de refuser. Un chemin commençant par `~` est désormais rejeté avec un message
  explicite.

Par ailleurs, `penelope-tools/src/shell.rs` existait mais n'était pas déclaré comme
module : il n'était donc ni compilé, ni testé, ni vu par clippy. Corrigé.
