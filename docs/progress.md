# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 18 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **1465 tests verts** hors réseau ; les suites réseau sont écrites et se lancent à la demande.
- `cargo clippy --workspace --all-targets -- -D warnings` : propre.
- `cargo deny check` : propre (avis, interdits, licences, sources).
- `cargo fmt --all --check` : propre.
- CI GitHub Actions (format, lint, tests, binaire, dépendances) et workflow de release
  sur tag `vX.Y.Z` avec binaire universel macOS.
- 71 tests d'acceptation nommés `ca_<section>_<n>_<nom>`, couvrant 14 sections du PRD,
  indexés dans [ca-matrix.md](ca-matrix.md), qui est généré depuis les sources.

## Étapes du §21

| # | Étape | État | Tests |
|---|---|---|---|
| 1 | `store` + `kernel` + `observe` | fait | 95 kernel, 13 store, 28 observe |
| 2 | `llm` + boucle d'agent | fait, `penelope chat` et Telegram branchés | 82 llm, 36 daemon |
| 3 | `context` (tuiles, ancres, niveaux 0 à 4, LCM) | fait | 73 |
| 4 | `telegram` (transport, rendu, gabarits, CTA, formulaires) | fait, passerelle lancée par le daemon | 85 |
| 5 | `hitl` + bac à sable + `tools` | fait | 16 hitl, 64 tools, 54 platform |
| 6 | `mcp` (négociation, transports, primitives, OAuth, registre, supervision) | fait, superviseur et OAuth branchés | 96 + 19 conformité + 13 daemon |
| 7 | `memory` + `skills` | fait | 97 memory, 12 skills |
| 8 | `workflow` + déclencheurs + workflows livrés | fait, ordonnanceur et pilote des runs lancés | 74 |
| 9 | Routage par complexité, budgets, images, STT | fait, alimenté par Telegram (vocaux, photos, documents) | inclus en llm |
| 10 | `resilience`, `upgrade`, `backup`, suites live, `ab-hermes` | fait ; suites live et A/B écrites, pas encore lancées (clés, bot de test et instance Hermes requis) | 11 resilience, 6 upgrade |

## Suites du §20.1

| Suite | Réseau | État |
|---|---|---|
| `unit` | non | verte |
| `arch` | non | verte |
| `ctx-safety` | non | verte |
| `mem-learning` | non | verte |
| `mem-bench` | non | verte, banc d'essai du rêve sur 5 conversations, modèle simulé, rapport joint aux releases |
| `mcp-conformance` | non | verte, 19 tests sur la matrice versions × transports |
| `telegram` | non | verte |
| `hitl` | non | verte |
| `workflow` | non | verte, dont `ticket-to-deploy` de bout en bout sur mocks avec redémarrage du daemon entre chaque passage (CA 12) |
| `hot-reload` | non | verte, 14 tests |
| `resilience` | non | verte, 10 tests |
| `security` | non | verte, 11 tests |
| `ctx-recall`, `mem-longitudinal`, `mem-bench-live`, `live-openrouter`, `live-telegram`, `ab-hermes` | oui | **écrites, pas encore lancées** : `penelope eval <suite>` avec `OPENROUTER_API_KEY`, un bot de test ou `PENELOPE_AB_HERMES_CMD` |

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
  carte masqués, original conservé (dans `vault/attachments/` depuis 0.9.0), passages indexés en
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

### 0.3.0

- **OAuth des serveurs MCP** (§8.5) : découverte (ressource protégée RFC 9728, puis
  RFC 8414 / OpenID), client CIMD, configuré ou enregistré dynamiquement (indexé par
  issuer), PKCE S256, `resource` systématique, `iss` vérifié (RFC 9207), consentement
  incrémental sur `WWW-Authenticate`. Demande valable 10 minutes et à usage unique ;
  adresse de retour collée dans Telegram ou reçue par le serveur local `127.0.0.1`
  (qui sert aussi le document CIMD). Jetons dans le SecretStore, rafraîchis avant chaque
  connexion, rotation gérée. `mcp.auth`, `penelope mcp auth`, `/mcp auth`, carte
  `mcp_oauth_required`, rappel quotidien des serveurs en attente.
- Transport chiffré exigé : points d'accès OAuth, métadonnées et serveur MCP en HTTPS ou
  en boucle locale exacte ; l'hôte est lu par un analyseur d'URL (les formes
  `localhost.exemple.org` ou `localhost@exemple.org` sont refusées), une URL portant des
  identifiants aussi.
- Correctif : la vérification de la chaîne d'audit signalait un « hash altéré » sur un
  événement intact dès que son payload portait un flottant que `serde_json` ne relit pas
  à l'identique (`123456789.12345679`, par exemple). Le hash est désormais recalculé sur
  le texte canonique stocké, jamais relu puis réécrit ; les chaînes existantes se
  vérifient sans migration.

### 0.3.1

- **Commandes secondaires** : `session.fork` (copie de l'historique dans une nouvelle
  session), `session.rewind` (derniers échanges mis de côté, jamais effacés), `export`
  JSONL (`session`, `run`, `all`), `store.rebuild` (index plein texte, index mémoire,
  audit), `skill.rollback`. `restore` et `eval.run` répondent par la marche à suivre :
  une restauration se fait daemon arrêté (`penelope restore`, base actuelle mise de
  côté) et une suite d'évaluation tourne depuis les sources (`penelope eval <suite>`).
  `/fork`, `/rewind`, `/export` sur Telegram.
- **`penelope upgrade`** (§2.12) : dernière release (les 0.x sont publiées en
  pre-release, la liste est lue plutôt que `releases/latest`), somme SHA-256 vérifiée
  contre `SHA256SUMS`, archive extraite, le nouveau binaire doit annoncer sa version
  avant tout remplacement. L'ancien est gardé en `<binaire>.previous`, le nouveau prend
  sa place par renommage et le daemon redémarre. Chaque démarrage à l'essai est compté
  avant l'ouverture de la base ; sans confirmation de santé 60 s après le premier essai
  (ou au-delà de 5 essais), l'ancien binaire est remis en place et le propriétaire
  prévenu. Un chien de garde quitte un processus bloqué au bout de 90 s pour que ce
  retour s'applique aussi. `--check`, `--tag`, `--force`, `--rollback` (bascule entre
  les deux binaires) ; sans daemon, la CLI fait la même chose elle-même. `/upgrade`,
  `/upgrade install|rollback|v0.3.1`. Un binaire de `target/` est refusé (`make deploy`).
- **`penelope import hermes`** (§7, §8.7) : skills de `~/.hermes/skills/**` (nom
  normalisé en slug, description repliée, annexes copiées, existantes gardées),
  `SOUL.md` et `AGENTS.md` vers le vault (mis de côté pour fusion si le vault a déjà le
  sien), `memories/MEMORY.md` et `USER.md` vers `memoire.md` et `profil.md` (une entrée
  par ligne, uid, origine `owner`, doublons ignorés, secrets et injections refusés),
  `mcp_servers` de `config.yaml` convertis en `mcp.d/<nom>.toml` puis essayés et marqués
  `ok`, `auth_required` ou `failed`. Les secrets rencontrés (valeurs littérales,
  `${VAR}` et `${env:VAR}` du `.env` d'Hermes) partent dans le SecretStore, la
  déclaration ne porte que `${SECRET:…}`. `--dry-run` décrit sans écrire ; un second
  import ne duplique rien ; le rapport part aussi sur Telegram.

### 0.3.2

Les huit issues ouvertes sur le dépôt, corrigées :

- #1 **Recherche d'historique** : `history_grep` déclare `scope` en `session` ou `all`
  et dit quand l'utiliser ; sans résultat dans la session, la réponse suggère `all`.
  `history_expand_query` cherche dans toutes les sessions mot significatif par mot
  significatif (mots vides retirés) et classe par nombre de mots trouvés. Chaque extrait
  porte le titre et la date de sa session. Correctif au passage : les résumés LCM n'étaient
  jamais trouvés par une requête de plusieurs mots.
- #2 **Titres de session** : titre de 3 à 6 mots donné par le modèle rapide après le
  premier échange (`context.auto_title`), jamais par-dessus un titre posé à la main ;
  `/title`, `penelope session title`, méthode `session.title` ; titres et dates dans
  `/sessions`, `penelope session list`, `/switch`, et le message « Nouvelle session »
  complété quand le titre arrive.
- #3 **Documentation** : README, `mcp.md`, `telegram.md` et `workflows.md` remis en accord
  avec le code ; un test échoue si une section « pas encore branché » cite une méthode RPC
  servie. Routine de livraison ci-dessous.
- #4 **Budget atteint** : le message nomme la clé du périmètre atteint
  (`budget.session_usd`, `budget.daily_usd` ou `budget.run_usd`), la dépense et le
  plafond, rappelle `/new` pour une session et que `/compact` ne rembourse rien.
- #5 **Flux coupé** : une erreur réessayable arrivée pendant le flux, avant tout texte ou
  appel d'outil, donne un nouvel essai puis le modèle de repli (même avec OpenRouter) ;
  après du texte, l'échec le dit. Tout tour échoué porte un bouton « Réessayer » qui
  relance la réponse sur le même transcript.
- #6 **Retour OAuth collé sans schéma** : `127.0.0.1:7777/…?code=…&state=…` est reconnu ;
  un texte portant `code=` et `state=` ne part jamais vers le modèle.
- #7 **`make deploy`** : quand le binaire du PATH est celui de `target/release`, la copie
  est sautée et le redémarrage a lieu.
- #8 **Gros résultats d'outils** : budget d'admission plafonné en valeur absolue
  (`min(fenêtre × part, large_payload_tokens)`) ; `http_fetch` rend une page HTML en texte
  lisible, le brut en artefact ; une longue liste `fs_list` part en artefact avec un
  résumé par dossier.

### 0.3.3

- #9 **Sonde `server/discover`** : une réponse qui n'est pas une découverte (ni version, ni
  identité, ni capacités, ou un résultat `isError`) déclenche le handshake historique au
  lieu du mode sans état. Le pont MCP de Xcode (`xcrun mcpbridge`), qui signale ainsi une
  méthode inconnue, négocie en 2025-06-18 et ses outils répondent.

### 0.4.0

- **Frontières d'épisode** (§6.6) : un épisode se clôt sur 2 h d'inactivité, sur trois
  messages consécutifs hors du sujet de l'épisode (similarité lexicale, voir
  [décision 0006](decisions/0006-changement-de-sujet-lexical.md)) ou sur `/new` ; il est
  relu une fois par le rôle `memory_review` : résumé dans le journal du jour, candidats de
  mémoire. Les instantanés T2 (profil, cœur, projets) sont figés par épisode : une
  écriture de profil n'altère plus le préfixe avant l'épisode suivant ou une compaction
  (CA 6 : instantané, frontière automatique).
- **Workflows** (§12) : attente `mcp_task` (tâche MCP longue suivie dans `mcp_tasks`,
  sondée par `tasks/get` puis `tasks/result`, survit au redémarrage) ; saisie
  `form:<id>` des étapes `user` (JSON Schema dans `settings.forms`, un champ par écran sur
  Telegram, récapitulatif, saisie validée dans `stepOutput.input`) ; sous-groupes
  (on ne quitte une boucle que par une transition taguée, vérifié au chargement, sortie
  tracée `workflow.subgroup_exited`). Un sous-workflow travaille dans l'espace de son
  parent, que le nettoyage ne supprime plus avant lui.
- **`ticket-to-deploy` exécutable** et testé de bout en bout (CA 12) : dépôt git local,
  tracker et forge MCP simulés, approbations et choix par le mock Telegram, daemon
  reconstruit entre chaque passage ; un seul push, une seule PR, un seul commentaire, un
  seul déploiement. Corrections trouvées par le test : chemins `steps.resolve_repo.data.*`,
  étape `create_pr` ajoutée (PRD, étape 8), `deploy-generic` lancé dans le dépôt cloné,
  tests et lint détectés (`make`, `cargo`, `npm`, `go`, voir
  [décision 0007](decisions/0007-deploiement-par-makefile.md)).
- **Signature minisign** (§2.12) : `penelope upgrade` vérifie `SHA256SUMS.minisig` dès
  qu'une clé publique est connue (`upgrade.minisign_pubkey`, ou intégrée au binaire de
  release par la variable de dépôt `MINISIGN_PUBLIC_KEY`) et refuse alors une release non
  signée. Le workflow de release signe si le secret `MINISIGN_SECRET_KEY` existe et échoue
  si la clé publique est posée sans lui. `upgrade.base_url` est honoré.
- **OCR des PDF scannés** : un PDF sans couche texte est lu par Vision (PDFKit + Vision,
  programme Swift compilé une fois dans le cache), 50 pages au plus.
- **Suites réseau** (§20.1) écrites : `live-openrouter` (streaming, outils,
  raisonnement, image, catalogue), `live-telegram` (envoi, édition, réaction, document,
  découpe), `ctx-recall` (faits retrouvés après compaction), `mem-longitudinal` (14 jours
  simulés, score ≥ 85 %, aucune promotion non fiable), `ab-hermes` (30 tâches vérifiables,
  rapport Markdown, critère réussite ≥ Hermes et coût ≤ Hermes).

### 0.5.0

- #10 **Une seule session écrit dans un chat** : celle qui a le focus, choisie par `/new`,
  `/fork`, `/switch` ou le menu `/sessions` (liaison explicite, migration `0005` qui ne
  garde qu'une liaison par chat ou sujet). Une session quittée finit son tour en cours
  sans rien écrire : réponse, messages, fichiers et approbations sont mis de côté derrière
  une seule notification silencieuse (« 📬 2 réponses et 1 approbation en attente dans
  « Titre » »), dont le bouton « Basculer » envoie tout dans l'ordre. Ses tours encore en
  file sont annulés, la sélection des tours ignore les sessions fermées, et
  `/close [session]` ou `penelope session close <session>` arrête une session et vide sa
  file.
- #12 **Élicitation MCP sur Telegram** : confirmation (Accepter, Refuser, Annuler),
  formulaire généré depuis `requestedSchema` (enums titrés `oneOf` et `anyOf` compris),
  lien en mode URL (domaine, adresse entière, alerte Punycode, fin signalée par
  `notifications/elicitation/complete`), `elicitation_timeout` par serveur (10 min) au-delà
  duquel la demande est annulée et la carte le dit. Le délai de l'appel d'outil est
  suspendu tant que le serveur attend le propriétaire ; le flux SSE du transport HTTP est
  lu au fil de l'eau pour que la demande arrive avant la fin de l'appel. En 2026-07-28,
  `input_required` relance l'appel avec `inputResponses` et `requestState` ; une erreur
  −32042 attend la fin des liens puis retente. Le résultat de l'outil dit au modèle qui a
  répondu. L'élicitation n'est annoncée que si Telegram est configuré, le sampling jamais ;
  les métadonnées `_meta` de 2026-07-28 portent les clés préfixées
  `io.modelcontextprotocol/clientCapabilities`, `clientInfo` et `logLevel`.
- #13 **Détecteur d'injection** : la règle `new_persona` ne se déclenche plus sur
  « without new instructions » ni sur une formule d'erreur ordinaire ; l'alerte dit
  qu'elle vient du détecteur local de Pénélope et cite le motif et l'extrait en cause.
- #14 **`/sessions` cliquable** : un bouton par session (▶️ focus, ⏳ tour en cours ou en
  attente, heure de dernière activité pour la plus récente), sous-menu Basculer, Forker,
  Renommer, Fermer, douze par page, fermées masquées par défaut ; `/switch` accepte un
  préfixe unique ou un titre.

### 0.6.0

Coûts : la journée qui a atteint le plafond de 20 $ en évitait environ la moitié.

- #20 **Alerte de budget** : au premier passage de `budget.alert_ratio` (80 %), une seule
  notification par périmètre (jour, session, run), avec les trois plus gros postes, leur
  coût et leur part de cache ; relever le plafond réarme l'alerte. `/usage` sur Telegram et
  `penelope usage` donnent tokens d'entrée, en cache et de sortie et la part de cache.
- #18 **Plafond de prompt** : `context.max_prompt_tokens` (120 000) borne le seuil de
  compaction, le budget des résultats d'outils et la queue verbatim, quelle que soit la
  fenêtre du modèle ; sur GLM-5.3 (1,3 M), la compaction de fond part vers 103 k au lieu
  de 917 k. `/budget` et `self_status` donnent la taille du contexte au dernier appel.
- #19 **Boucles d'outils** : règle de harnais (regrouper les commandes, déléguer une
  investigation à `sub_agent_spawn`), rappel tous les `budget.delegate_after_calls`
  appels (10), point de contrôle « Ce tour a coûté 1,05 $, je continue ? » à chaque
  `budget.turn_checkpoint_usd` (1 $), plafond de 24 appels compté sur tout le tour
  (reprises après approbation comprises), coût du tour ajouté à la réponse au-delà de
  `budget.show_turn_cost_usd` (0,50 $).
- #17 **Cache de prompt** ([décision 0008](decisions/0008-cache-de-prompt.md)) : contexte
  volatil figé avec son message (migration `0006`), raisonnement toujours joint aux appels
  d'outil et jamais aux réponses finales, préfixe T0 à T2 modifié différé jusqu'à un cache
  froid ou une compaction, fournisseur amont collant pendant 10 min (`provider.order`,
  replis permis), empreinte de chaque requête et cause de chaque raté
  (`penelope usage --by miss`). Test : chaque requête reprend la précédente octet pour
  octet, dans un tour et d'un tour à l'autre (CA 5).

### 0.7.0

Mémoire « second cerveau » et configuration.

- #11 **Embeddings calculés** : vecteurs des entrées mémoire, des intentions et des outils
  MCP, rattrapés en fond après chaque tour, réindexation ou inscription d'outils, mis en
  cache par contenu ; au tour, le vecteur du message (1,5 s au plus) alimente le rappel et
  les intentions, `mem_search` et `tool_search` croisent mots et sens. Défaut
  `openrouter:openai/text-embedding-3-small`, sans serveur local (l'ancien défaut bascule
  au chargement) ; `doctor` vérifie l'alias, `self_status` dit si la recherche est
  hybride, `penelope mem reindex --embeddings` recalcule tout.
- #21 **Accueil** : `/accueil` et `penelope onboard`, neuf questions écrites dans
  `accueil/AAAA-MM-JJ.md` avant d'être posées, reprise après pause, récapitulatif validé
  avant d'écrire `profil.md` et `memoire.md` (provenance vers la question), parties
  rejouables avec remplacement, proposé au premier message d'un profil vide.
- #23 **Audit sur 100** : `/audit` et `penelope mem audit`, barème v1 sur cinq axes, une
  prochaine action par axe, historique `audits/`, score au digest du lundi.
- #22 **Wiki de concepts** : pages `concepts/<slug>.md` tirées des documents ingérés,
  dédoublonnées par les mots puis le sens, liens `[[slug]]` depuis les sources et la
  mémoire, `concepts/_a-definir.md` au digest, `index.md`, outil `mem_neighbors`.
- #15 **Contenu hors index nommé** : inventaire du vault (`penelope vault check`,
  `doctor`, journal à chaque changement), exclusions documentées, recherche vide qui
  rappelle le périmètre et les fichiers hors index, règle du harnais « pas de résultat »
  n'est pas « n'existe pas ».
- #16 **Réglages qui s'annulent** : inventaire des paires contradictoires
  (`penelope_kernel::coherence`), refus nommé à `config_set`, avertissement sinon, audit
  au démarrage et dans `doctor` (déclencheurs pendant les heures calmes compris),
  `penelope config validate`.

### 0.8.0

Qualité de la mémoire, historique du vault, secrets et signature.

- #24 **Règles dictées retenues** : `mem_note` accepte la citation exacte du
  propriétaire, vérifiée dans ses messages depuis la dernière réponse ; sans citation, une
  préférence, correction ou décision n'est plus rejetée mais demandée au propriétaire
  (approbation « Tu confirmes cette règle ? »), promue au rêve suivant après
  confirmation. `penelope mem retry-rejected` remet en file les règles rejetées pour leur
  seule origine (migration 0007 pour les passes existantes).
- #25 **Porte de qualité** : texte tronqué, phrase incomplète ou sujet absent rejetés,
  entrées de plus de 300 caractères scindées en phrases, états passagers envoyés dans
  `projets.md` avec `expire`, données client, financières ou de sécurité marquées
  `sensible` (table `mem_flags`, migration 0008) et jamais injectées d'office, faits sur
  la configuration de Pénélope refusés ; dépassement du budget Cœur signalé dans
  `DREAMS.md`.
- #26 **Aucun secret dans les journaux** : stderr rédigé comme les fichiers, jeton du bot
  retiré des erreurs de transport, journaux en `0700`/`0600`, contrôle `logs_secrets` de
  `doctor` sur les journaux existants.
- #27 **Vault sous git** : dépôt créé au démarrage quand l'autocommit est actif, commit
  périodique, commit par rêve, avertissement `doctor` et digest, `penelope mem diff
  [--since dream]`.
- #28 **Signature macOS stable** : `make build`/`make sign` avec `SIGN_IDENTITY`,
  re-signature à l'upgrade (`upgrade.codesign_identity`), type de signature dans `doctor`,
  workflow CI qui vérifie l'exigence désignée entre deux builds.

### 0.9.0

Le vault devient un wiki Markdown valide à tout moment (#29).

- **Format** : propriétés YAML sur chaque note (`type`, `created`, `updated`, `aliases` et
  `tags` en listes, valeurs citées au besoin), uid en identifiant de bloc final (`^uid`,
  visé par `[[note#^uid]]`), encadrés reconnus, noms de fichiers uniques dans tout le vault
  (`accueil-AAAA-MM-JJ`, `audit-AAAA-MM-JJ`, wikilink par chemin si ambigu), originaux dans
  `attachments/` embarqués par leur fiche, `log.md` en ajout seul, skill livrée
  `wiki-markdown` (les skills sont désormais chargées au démarrage).
- **Lint** : résolveur de wikilinks (nom, chemin, puis alias), liens non résolus, blocs
  absents, orphelines, impasses, alias et noms en double, identifiants de bloc invalides
  ou dupliqués, propriétés ; entrées expirées et contradictions proposées ; rapport dans
  `DREAMS.md`, `log.md` et l'audit (barème v2) ; `penelope vault lint`.
- **Coexistence** : écriture optimiste (relecture juste avant d'écrire, opération
  réappliquée, reportée si la ligne visée a changé), écriture atomique, dossiers cachés
  intacts, renommage avec réécriture des wikilinks, `.gitignore` sans nom de produit.
- **Journal et digest** : candidats et épisodes reliés aux concepts cités et aux sources
  de la session, digest avec les entrées du rêve et le journal de la veille.
- **Migration** d'un vault antérieur au premier démarrage et à `mem reindex`, uid et
  provenance conservés.

### 0.10.0

Telegram cliquable et boucles d'outils qui répondent.

- #30 **Écrans de commandes** : plus aucun « Usage : » ni affichage brut. Chaque commande
  sans argument ouvre un écran (un bouton par élément, message redessiné en place,
  pagination, confirmation des gestes risqués) ; paramètres de workflow et arguments de
  prompts MCP par formulaire ; `/help` par familles ; `/status` et `/doctor` renvoient vers
  l'écran de chaque alerte ; boutons `copy_text` et liens profonds
  `t.me/<bot>?start=<écran>` depuis le digest ; `/p` exécute un prompt MCP ; `/note` écrit
  dans le journal ; `/logs` lit le journal JSON. Test de couverture sur tout le catalogue.
- #31 **Boucle d'outil arrêtée** : le dernier résultat réel et une note d'arrêt restent
  dans la conversation, un appel sans outil (`tool_choice: none`) explique l'erreur exacte
  et propose deux ou trois suites en boutons, qui arrivent dans la session comme des
  messages ; repli lisible si cet appel échoue ; le rapport technique reste dans les
  événements et les journaux.

### 0.11.0

Sessions de travail longues (#32).

- **Budget par session** : `penelope session budget` et `/budget session <montant>`,
  plafond stocké avec la session, affiché par `/budget` et `/status`, recopié par `/fork`.
  Au plafond, carte « continuer ? » (+5 $, +20 $, Arrêter) : la session du propriétaire
  reprend son tour suspendu, le jour reste un arrêt ferme relevable pour la journée, un
  run reprend après relèvement. Une demande en attente n'est plus dupliquée à chaque
  tentative.
- **Sorties de tests filtrées** : `shell_exec` reconnaît cargo, go, npm/pnpm/yarn, Jest,
  Vitest, pytest et `make test`, ne rend que résumé et échecs, et garde la sortie complète
  en artefact ; même règle (tête, queue, lignes d'erreur) pour toute commande en échec à
  longue sortie ; `output: "full"` pour la sortie brute.
- **Notes de travail** : outil `session_notes`, fichier `notes/<titre>-<id>.md` à sections
  fixes, bloc borné en fin de prompt à chaque tour, rappel après délégation ou
  compaction, copie au fork, proposition à `/new` sur un sujet proche, décisions relevées
  par le rêve.

### 0.12.0

Mise à jour à distance d'une installation source (#33).

- `/upgrade install` sur un binaire de compilation affiche une carte de bascule vers les
  releases ; `/upgrade` le signale dès l'annonce ; `penelope upgrade --switch` en ligne de
  commande.
- Préconditions vérifiées avant toute action : identité de signature utilisable depuis le
  daemon (essai réel), `upgrade.install_dir` inscriptible, LaunchAgent qui lance ce binaire
  et fichier modifiable.
- Bascule : release vérifiée, binaire installé et re-signé, binaire de compilation gardé
  comme précédent, `ProgramArguments` réécrit (original sauvegardé), service rechargé par un
  processus détaché ; sans santé confirmée, fichier d'origine restauré et ancien binaire
  relancé.
- `penelope doctor` : mode d'installation et programme lancé par le service ; `make deploy`
  revient aux sources.

### 0.13.0

Connaissance de soi (#34) et workflows lancés par la conversation (#35).

- **Inventaire** : `self_status` sections `workflows` (rôle, paramètres requis et
  facultatifs, machine), `skills`, `tools` (classe de risque), `mcp`, `commands`,
  `schedules`, `install`, `limits`, ou `inventory` pour tout ; index des workflows dans le
  prompt (T1, borné à 2 000 caractères).
- **Documentation embarquée** : `README.md` et `docs/` compilés dans le binaire ; outil
  `self_docs` (`list`, `search`, `read` paginé, `limits`), chaque résultat avec le lien
  GitHub de la section au tag de la version. Règle du harnais : le dépôt fait foi, consulter
  `self_status` puis `self_docs` avant d'écrire un workflow, une skill ou un réglage, citer
  la section. Un brouillon refusé par `workflow_author` renvoie à la section concernée.
- **Conversation vers workflow** : règle du harnais (paramètres complétés avec les outils,
  questions limitées à ce qui manque, lancement proposé) ; `workflow_start` accepte
  `brief`, enregistré avec le run, placé avant la consigne de la première étape `agent` ou
  `sub_agent`, variable `{{brief}}`, affiché sur la carte de progression.
- **Carte de lancement** Telegram : workflow, paramètres, brief, « ▶️ Lancer » ou
  « ⏸ Pas encore » (refus motivé transmis au modèle), sans « Toujours ».
- **`/run <workflow>` sans paramètres** : demande transmise à la conversation, formulaire
  à un bouton. Formulaire, questions, approbations et sous-agents d'un run restent dans le
  chat et le sujet d'origine.
- **`ticket-to-deploy` indépendant du tracker et de la forge** : lecture du ticket, demande
  de fusion et commentaires passent par `tool_search` / `tool_call` (Redmine ou ClickUp,
  GitHub ou GitLab) ; paramètre facultatif `tracker` ; la réponse à « quel dépôt ? »
  relance la résolution au lieu de laisser `checkout` sans dépôt.

### 0.13.1

Mise à jour à distance qui laissait le service déchargé (#36).

- **Cause** : le rechargement lancé par le daemon enchaînait `launchctl bootout` et
  `bootstrap` ; `bootout` rend la main avant la fin de l'arrêt du daemon, `bootstrap`
  échouait (« 5: Input/output error ») et le service restait déchargé, sans retour arrière
  possible puisque le nouveau binaire ne démarrait jamais.
- **Chemin stable** : le service lance toujours le même fichier (`upgrade.install_dir`) ;
  `/upgrade install` et `make deploy` le remplacent, le fichier de service ne change plus.
  `make deploy` migre une dernière fois un service qui lance encore `target/release`,
  depuis le shell.
- **Relais launchd** (`com.penelope.daemon.reloader`) : job éphémère hors du job du daemon,
  qui attend l'arrêt réel, vérifie et retente le `bootstrap`, remet le fichier de service
  d'origine si launchd le refuse, puis garde-fou : nouveau binaire jamais démarré en deux
  minutes, binaire précédent remis au même chemin et service relancé. Armé aussi après une
  mise à jour ordinaire.
- `penelope doctor` : service qui lance un binaire de compilation, mise à jour installée
  depuis plus de cinq minutes sans démarrage.
- Test contre le vrai launchd en CI macOS (`PENELOPE_LAUNCHD_TESTS=1`) : un faux daemon
  lent à s'arrêter charge le relais depuis son propre job ; le service repart sur le chemin
  stable, ou revient à l'ancien binaire si le nouveau ne démarre pas. Sans le correctif, le
  test reproduit l'échec de production.

### 0.14.0

Rêve nocturne trié par une grille explicite (#37).

- **Grille de tri** : faits, préférences, décisions et corrections ne passent plus par des
  comptages (`fact_min_recalls`, `fact_min_importance`, `preference_min_sessions`, gardés
  mais ignorés) ni par une importance auto-attribuée. Le modèle juge cinq critères
  (durable, utile, précis, introuvable ailleurs, endossé) avec une justification ; le code
  décide : mémoire durable, journal ou ignoré. Le tri est écrit dans `DREAMS.md`. Plus de
  confirmation manuelle pour une règle notée sans citation.
- **Mettre à jour plutôt qu'empiler** : chaque candidat arrive avec ses souvenirs proches
  (embeddings, sinon recherche lexicale). Opérations `replace_entry`, `supersede_entry`
  (ancienne entrée retirée, `remplace: <uid>` et `depuis` sur la nouvelle, provenance
  liée) et `noop`. Un texte déjà en mémoire n'est jamais ajouté deux fois ; une
  contradiction non tranchée reste une question.
- **Validité temporelle** : `depuis` sur chaque entrée écrite ; états passagers dans
  `projets.md`, section « États en cours », avec `expire` (14 jours par défaut, 90 au
  plus), retirés tout seuls après expiration.
- **Secrets** : clés, jetons et mots de passe des candidats (relecture, épisodes,
  `mem_note`) rangés dans le magasin de secrets sous un nom tiré du contexte ; la mémoire
  ne garde que `${SECRET:nom}`, que la détection de secrets et la redaction laissent
  passer. Clés Stripe reconnues.
- **Retour d'usage** : rappels automatiques et `mem_search` comptés ; une entrée de
  `memoire.md` ou `projets.md` jamais rappelée depuis 60 jours est proposée au retrait
  dans le digest.
- **`sensible`** n'empêche plus l'injection : un marqueur, le vault étant privé.
- **Banc d'essai** : suites `mem-bench` (modèle simulé, en CI, seuils exacts) et
  `mem-bench-live` (vrai modèle) ; précision, rappel, journal, faux souvenirs, périmés,
  doublons, fuites de secrets, réponses aux questions ; rapport `banc-memoire.md` joint
  aux releases. Il a trouvé deux défauts, corrigés : une référence `${SECRET:…}` prise
  pour un secret, et un marqueur de masquage pris pour un mot de passe.

### 0.15.0

Documentation vérifiée par la CI (#38), planifications qui ne meurent plus en silence (#39),
compaction de fond sur la taille réelle du contexte (#40).

- **Index `docs/README.md`** : par besoin, par fichier (rôle et sections), décisions ; le
  README racine y renvoie et `self_docs list` donne le rôle de chaque page.
- **Test `docs`** (remplace `docs_freshness`) : index complet, aucun lien relatif mort
  (ancres comprises), catalogue Telegram et `telegram.md` identiques dans les deux sens,
  références générées des clés de configuration (tirées des commentaires de `config.rs`,
  chaque clé documentée) et des outils natifs dans `install-headless.md`, section de
  version dans `progress.md`, sections de limites sans méthode, commande ni outil livrés.
  Modèle de pull request avec la case documentation.
- **Planifications** : chaque exécution d'un prompt ouvre sa session, titrée d'après la
  planification, et répond dans le chat ou le sujet d'origine ; `session_id` devient la
  référence `origin_session` (migration 0009). Un tour planifié annulé, en échec ou au
  budget atteint envoie une alerte avec « Relancer maintenant » et « Voir la
  planification » ; `runs` et `last_run` ne comptent qu'une exécution menée à terme,
  sinon `last_error`, visible dans `/schedules` et `doctor`. Un déclenchement manuel
  n'est plus dédoublonné avec le passage prévu. Une planification identique à une
  planification active est signalée à la création.
- **Compaction de fond** (#40) : déclenchée aussi par le prompt réellement facturé au
  dernier appel, pas seulement par l'estimation locale ; session froide (pause au-delà de
  la durée du cache) ou premier tour d'un fork au-delà du seuil résumés avant l'appel au
  modèle ; événements `context.compaction_requested`, `context.compaction_skipped` (avec
  la raison) et `context.compacted`, en INFO dans les journaux. Les plafonds de session et
  de run n'arrêtent plus les résumés ; au plafond du jour, une réserve
  `budget.compaction_reserve_usd` (0,50 $). `/status`, `/budget` et `self_status` donnent
  la taille réelle du contexte, le seuil de fond et la dernière compaction.

### 0.16.0

Réponses vocales (#41).

- **Synthèse locale** : méthode `speak` des providers (`POST /audio/speech`), rôle et alias
  `tts` (Voxtral TTS de Mistral servi par mlx-audio, voix `fr_female`), jamais replié sur
  le modèle de conversation ; section de configuration `[voice]` (`tts_voice`,
  `max_chars`, `reply_in_kind`).
- **Outil `send_voice`** (lecture, sans approbation) : texte rendu lisible (sans Markdown,
  liens, code ni emojis, symboles dits en toutes lettres), découpé en phrases, synthétisé,
  assemblé, converti en OGG/Opus par `ffmpeg`, envoyé par `sendVoice` en réponse au message
  d'origine ; texte trop long renvoyé au modèle pour un résumé vocal ; synthèse impossible,
  la réponse part en texte avec la raison. Usage `tts` et événement `voice.sent` avec la
  durée.
- Règle du harnais : vocal seulement sur demande explicite ou après un vocal si
  `voice.reply_in_kind` ; `doctor` (ffmpeg et phrase d'essai) ; inventaire `install`.

### 0.16.1

Verrou de session à jeton de clôture (#43), écrivain à l'épreuve des paniques (#44),
mutations de configuration sérialisées (#45), purge RGPD et rétention (#46), journal
d'audit sans faux positif (#47), listes de frontmatter lues correctement (#48), rafales de
messages regroupées (#49), nouvelles tentatives avant le flux (#50), flux muet coupé sur
son inactivité (#51), résultats d'outils parallèles admis en groupe (#52), comptage et
fenêtre des modèles locaux (#53), émulation d'outils retirée (#54), projection qui ne relit
plus ce qui est résumé (#55), délai d'étape qui borne l'étape, pas le run (#56), `/stop`
qui interrompt outils et sous-agents (#57), pratiques rappelées en conversation (#58),
consolidation nocturne par lots (#59), état d'un candidat décidé après l'écriture (#60),
décisions des notes de travail récoltées (#61), retour d'usage juste (#62), skills relues
sans redémarrage (#63), redirections HTTP revérifiées (#64), `shell_exec` qui ne laisse ni
processus ni mémoire derrière lui (#65), liens symboliques bornés au workspace (#66),
« Toujours » borné à l'appel (#67), bac à sable qui ferme les secrets (#68), boucle
Telegram qui ne bloque plus (#69), brouillons qui ne retardent plus la réponse (#70),
échecs de commande dits (#71), export d'une session inconnue refusé (#72), boutons
acquittés tout de suite (#73), latence du premier jeton réduite (#74).

- **Jeton de clôture** : `heartbeat` et `finish` n'écrivent que si le bail est encore au
  runner qui l'a réclamé (`WHERE resource = ? AND holder = ?`). Un runner évincé reçoit
  `LeaseLost`, abandonne son tour, ne livre rien et ne touche pas au bail de son
  successeur.
- **Bail expiré n'est pas runner mort** : les runners vivants du processus sont tenus en
  mémoire ; un bail expiré parce que l'écrivain était figé (sauvegarde, réindexation, veille
  du Mac) n'est pas repris. Seule la disparition du processus libère, comme avant, par
  expiration puis `recover_on_boot`.
- **Réglages** : `runners.heartbeat` doit valoir au plus la moitié de `runners.lease_ttl`,
  refusé par `penelope config validate` et par la table de cohérence.
- **Mutations de configuration sérialisées** : `ConfigStore::mutate` et
  `reload_from_disk` prennent le même verrou (lecture de l'instantané, écriture du fichier,
  publication), et le fichier temporaire d'`atomic_write` porte un nom unique. 800 mutations
  concurrentes donnent 800 générations et aucun refus « No such file or directory » ; le
  fichier sur disque porte toujours la dernière génération.
- **Purge RGPD** : `penelope session purge`, `/purge` et la méthode `session.purge`
  effacent le contenu d'une session (messages et index plein texte, contexte figé, résumés,
  artefacts et leurs fichiers, requêtes au modèle, payloads des tours et des updates
  Telegram, candidats de mémoire), avec confirmation. Le journal d'événements garde ses
  lignes et leurs hachages : `audit-verify` reste vert, `audit.purge` dit ce qui a été fait.
- **Rétention** : une passe par jour efface les tours terminés, les requêtes au modèle
  abouties, les payloads d'updates Telegram et les clés de travail au-delà de
  `retention.days` (90), les pré-images de la mémoire au-delà de
  `retention.memory_history_days` (30). Le payload d'un update Telegram est vidé dès son
  traitement : seul son identifiant sert encore, à la déduplication. `kv` date ses clés
  (migration `0010_retention`).
- **Journal d'audit** : une erreur de lecture pendant `append` fait échouer l'écriture au
  lieu de forger un maillon chaîné sur GENESIS ou un `seq` en doublon, indiscernables d'une
  altération. Un doublon `(session_id, seq)` est refusé par la base (migration
  `0011_events_seq_unique`), un payload illisible est dit au lieu d'être remplacé par
  `null`, et écrire les métadonnées d'une session inconnue échoue au lieu de réussir sans
  rien écrire.
- **Frontmatter** : une liste en ligne (`aliases: ["Le Crew, coworking", Crew]`) est
  découpée sur les virgules hors guillemets. Un alias contenant une virgule ne devient plus
  deux alias faux avec un guillemet résiduel : le lien qui le vise se résout, le lint ne
  signale plus d'alias fantôme.
- **Rafales de messages** : les morceaux d'un même envoi (un long texte collé, découpé par
  Telegram en messages de 4 096 caractères) forment un seul tour, recollés dans l'ordre,
  dans la fenêtre `telegram.text_group_window_ms` (3 s). Au-delà de
  `telegram.burst_messages` (5) ou `telegram.burst_chars` (20 000), une carte demande quoi
  en faire (un seul document, ingérer sans répondre, un par un, tout annuler) et aucun tour
  ne démarre avant le choix. `/stop` vide désormais la file de la session en plus d'arrêter
  le tour en cours, et le dit ; `/stop tout` vide aussi les autres sessions du chat et met
  les runs en pause.
- **Incident passager côté fournisseur** : une erreur avant le flux (5xx, délai de
  connexion, limite de débit sans `Retry-After`) est réessayée sur le même modèle avec une
  attente qui double (1 s, 2 s, 4 s, `providers.openrouter.request_retries`, 3), puis les
  replis d'alias jouent **aussi** avec OpenRouter, dont le repli côté serveur ne sert à
  rien quand c'est OpenRouter lui-même qui est injoignable. Chaque nouvelle tentative est
  tracée (`llm.retried`), l'arrêt demandé interrompt l'attente, et l'échec final dit
  combien de fois on a essayé et combien de temps on a attendu.
- **Flux muet** : un fournisseur qui se tait après ses en-têtes est coupé au bout de
  `providers.openrouter.stream_idle_timeout` (120 s, même clé pour `providers.local`), avec
  une erreur réessayable qui déclenche la relance et le repli. Tout octet reçu, commentaire
  SSE compris, remet le compteur à zéro ; le délai global passe à 30 minutes, donc une
  longue réponse qui progresse n'est plus coupée, et le client OpenAI-compatible a
  désormais un délai de connexion.
- **Résultats d'outils parallèles** : le budget d'admission (§5.4 niveau 1, 25 k tokens)
  s'applique au **groupe** d'appels d'une itération, plus seulement à un résultat isolé.
  Cinq `fs_read` de 20 k tokens entraient entiers (100 k tokens dans le canonique, renvoyés
  à chaque appel) ; ils sont maintenant répartis sous le budget, les petits gardés entiers,
  les gros externalisés en artefacts relisibles. L'admission reste idempotente.
- **Modèles locaux** : la requête demande `stream_options.include_usage`, sans quoi vLLM,
  llama.cpp, LM Studio et mlx_lm ne renvoient jamais `usage` en streaming (comptage à zéro,
  compaction sur la taille réelle jamais déclenchée). La fenêtre de contexte vient de
  `GET /models` quand l'endpoint la donne (`context_length`, `max_model_len`, `n_ctx`),
  sinon de `providers.local.context_window` (32 768) : un modèle local à 128 k n'est plus
  compacté vers 22 900 tokens.
- **Émulation d'outils retirée** ([décision 0009](decisions/0009-pas-d-emulation-d-outils.md)) :
  le code du §10.1 n'avait aucun appelant et, branché tel quel, aurait donné deux fois la
  même clé d'idempotence à deux appels identiques. Un modèle dont le catalogue dit qu'il
  n'appelle pas d'outils est désormais refusé pour un alias qui sert un rôle à outils
  (`penelope model set` dit pourquoi, `doctor` signale une configuration déjà en place) ;
  les rôles de service (`stt`, `tts`, `embeddings`, `classifier`, `summarizer`, `titler`,
  `vision`) l'acceptent toujours. Le parseur JSON tolérant reste, sous `json_scan`.
- **Projection** : la requête se construit à partir de la couverture des résumés actifs
  (`seq > covered_to`) au lieu de relire et désérialiser toute la table `messages` pour
  jeter ce que la compaction vient d'en retirer, et la queue du transcript se lit en
  `ORDER BY seq DESC LIMIT 64`. Sur une session de 4 500 messages (9 Mo), deux lectures
  complètes par itération (17 ms chacune) deviennent une lecture de ce qui est projeté
  (2 ms).
- **Délai d'une étape de workflow** : `CancelToken::child()` crée un vrai jeton enfant
  (annuler le parent annule l'enfant, jamais l'inverse), et chaque étape, chaque enfant
  d'un `parallel` et chaque vérification d'un `verify` reçoit le sien. Une étape qui dépasse
  `timeoutMs` enregistre son résultat `timeout` et suit sa transition, au lieu d'annuler le
  run, de perdre son résultat et d'être rejouée à chaque passage du pilote jusqu'au plafond
  de budget.
- **`/stop` arrête vraiment** : le jeton du tour descend jusqu'aux outils
  (`execute_cancellable`). Un `shell_exec` en cours est interrompu en moins de deux
  secondes, son groupe de processus terminé (`SIGTERM` puis `SIGKILL`), sans orphelin ; un
  sous-agent reçoit un jeton enfant du tour parent, donc `/stop` l'arrête aussi, et un
  sous-agent qui s'arrête tout seul ne touche pas au parent. L'attente entre deux
  tentatives d'une étape de workflow écoute la pause au lieu de dormir jusqu'à 300 s.
- **Pratiques branchées** : la voie 1 reçoit enfin les pratiques du vault (relues quand un
  fichier change) et un contexte réel (type de tâche déduit du message, projet actif du
  workspace de la session). Une règle défaisable est rappelée avec son défaut et ses
  exceptions **satisfaites**, jamais ses écarts observés : ceux-ci, comme les exceptions,
  sont désormais exclus du rappel automatique (ils restent trouvables par `mem_search`), et
  `reindex` leur donne leur vrai type (`exception`, `ecart`) au lieu de `fait`. Le facteur
  « projet actif » du classement fonctionne enfin en conversation.
- **Consolidation nocturne** : les candidats partent par lots (`memory.dream_batch`, 40),
  la fenêtre de sortie est dimensionnée au lot, une réponse coupée est détectée et rejouée
  avec un lot deux fois plus petit (la cause est nommée dans le rapport), les souvenirs
  proches tiennent en un seul appel d'embeddings, et un candidat reporté trois nuits de
  suite est rejeté avec sa raison. Un lot de 150 candidats ne se reportait plus jamais,
  nuit après nuit, sans que rien ne soit promu.
- **État d'un candidat** : `promoted` n'est plus posé avant l'écriture. Une opération
  refusée par la validation (uid inconnu, pratique inconnue, plafond de retrait), une
  écriture en échec ou un candidat pour lequel le modèle n'a rien proposé laissent le
  candidat en attente, avec sa raison dans le rapport : la règle n'est plus perdue en
  silence, et elle est retentée la nuit suivante (jusqu'à trois reports, #59).
- **Décisions des notes de travail** : elles étaient enregistrées avec le genre de session
  `notes`, que la provenance refuse, puis marquées consommées : aucune ne devenait candidat,
  sans un mot. Elles portent maintenant le genre de leur session d'origine, une session qui
  n'autorise pas de candidat (planifiée, sous-agent) est ignorée sans marquer la décision,
  le marqueur « récoltée » ne se pose qu'après enregistrement, et le plafond de cinq par
  tour ne s'applique plus à la récolte d'une journée.
- **Retour d'usage** : une entrée servie d'office dans l'instantané (Profil, Cœur,
  Projets) n'est plus comptée « jamais rappelée », donc plus proposée au retrait alors
  qu'elle est dans chaque prompt ; le signal porte sur ce qui n'est injecté que sur
  déclenchement (niveau Cure, et entrées laissées hors budget). La voie 1 ne répète plus
  dans T4 une entrée déjà présente dans T2.
- **Skills déposées en SSH** : la passe d'entretien compare une empreinte des dossiers de
  skills (chemins, tailles, dates) et les relit dès qu'elle change, sans redémarrage ni
  dépendance nouvelle ; `penelope skill reload` (méthode `skill.reload`) force la relecture
  tout de suite et rend la génération et le nombre de skills.
- **`http_fetch`** : le client ne suit plus les redirections lui-même, si bien que la
  revérification annoncée « à chaque saut » (liste blanche, adresses privées) s'exécute
  enfin : une page publique qui redirige vers `127.0.0.1` ou `169.254.169.254` est refusée
  au lieu de revenir dans le transcript. Le corps est lu par morceaux et coupé à
  `max_bytes` sans passer en mémoire, et une taille annoncée au-delà de vingt fois la
  limite est refusée avant lecture.
- **`shell_exec`** : au dépassement du délai, le groupe de processus est terminé (`SIGTERM`
  puis `SIGKILL`) au lieu de survivre jusqu'au redémarrage de la machine pendant que le
  modèle relance la commande ; les sorties sont lues en continu sous plafond (tête et
  queue), si bien qu'une commande bavarde ne fait plus monter la mémoire du daemon de la
  taille de sa sortie ; chaque appel porte une étiquette PID, ramassée au démarrage suivant
  si le daemon a été tué.
- **Outils de fichiers et liens symboliques** : `resolve` vérifie désormais le chemin
  demandé **et** sa forme réelle (plus long préfixe existant canonicalisé). Un lien déposé
  dans le workspace n'ouvre plus le reste du disque à `fs_read`, `fs_write`, `fs_edit`,
  `fs_list`, `fs_search` ni au `cwd` de `shell_exec` et des outils git, y compris pour un
  fichier qui n'existe pas encore sous le lien ; un lien interne au workspace, et un
  workspace qui est lui-même un lien, restent acceptés.
- **Approbation « Toujours »** : la règle créée est bornée au contexte de l'appel grâce à
  un motif d'arguments (nouvel opérateur de préfixe) : famille de commandes pour
  `shell_exec`, répertoire pour `fs_write` et `fs_edit`, remote et branche pour `git_push`,
  hôte pour `http_fetch`, clé pour `config_set`. Un « Toujours » accordé à `cargo test` ne
  rend plus automatique `rm -rf target`, et `/policies` affiche la portée de chaque règle.
  Les règles des outils MCP sont inchangées.
- **Bac à sable** : `sandbox.deny_read` (liste livrée : `~/.ssh`, `~/.aws`, `~/.gnupg`,
  `~/.config/gh`, `~/.netrc`, `~/.kube`, base, secrets, `mcp.d`, configuration, état) est
  rendu en `(deny file-read* …)` **après** les autorisations, et le trousseau est fermé
  (`deny mach-lookup com.apple.SecurityServer`) : une commande ne relit plus les clés par
  `security find-generic-password -w`. `doctor` signale un profil sans refus effectif. Le
  réseau du shell reste ouvert par défaut (`sandbox.shell_network`), pour ne pas casser
  `gh`, `git push` et `npm` sans prévenir : le fermer est un choix documenté.
- **Approbation « Toujours »** : les motifs d'arguments sont écrits pour ne pas se
  contourner (revue de sécurité) : la famille de commandes refuse tout enchaînement
  (`;`, `&&`, `|`, `$(…)`, redirection) et s'arrête à une frontière de mot, le répertoire
  est comparé sur le chemin normalisé (`..` résolu), et l'URL sur l'origine exacte, jamais
  sur un préfixe de texte.
- **Boucle des updates Telegram** : un vocal (téléchargement puis transcription, jusqu'à
  trois minutes), une photo, `/export` et `/audit` sont traités dans une tâche à part. `/stop`,
  un clic de carte ou un message d'une autre conversation ne font plus la queue derrière
  eux ; la déduplication par `update_id` garantit qu'un rejeu ne double pas le tour.
- **Brouillons Telegram** : les aperçus (brouillon, réaction, « écrit… ») ont leur propre
  seau de cadence, séparé de celui des messages, et un seul brouillon est en vol à la fois,
  le suivant portant le dernier texte. Une réponse de dix secondes arrivait quatre secondes
  après la fin de la génération, une de trente secondes treize secondes après ; elle part
  maintenant dès qu'elle est prête, et le brouillon en vol est abandonné à ce moment-là. Un
  429 sur un aperçu ne retarde plus les messages.
- **Jamais de silence côté Telegram** : un update dont le traitement échoue est signalé au
  propriétaire dans la conversation où il l'a envoyé, en réponse au message fautif, avec la
  raison ; l'erreur ne part plus seulement dans le journal pendant que l'offset avance.
- **`/export` d'une session inconnue** : l'identifiant passe par la résolution commune
  (identifiant, préfixe ou titre) et une session inconnue répond « aucune session ne
  correspond », au lieu d'un fichier JSONL vide. La méthode RPC `session.export` suit la
  même règle, donc `penelope export session <id>` aussi.
- **Boutons d'écran** : le clic est acquitté avant l'opération, qui se poursuit en tâche de
  fond et livre son résultat dans la conversation (carte redessinée, ou message). Une
  vérification de mise à jour ou un redémarrage de serveur MCP laissait le bouton tourner
  jusqu'à ce que Telegram invalide la requête, et le résultat n'arrivait jamais.
- **Latence avant la réponse** : la classification et le vecteur de rappel mémoire partent
  en parallèle (ils ne dépendent pas l'un de l'autre), et un message manifestement trivial
  (« ok », « merci », salutation) ne passe plus par le classifieur : il répond tout de suite
  avec le modèle par défaut. Deux allers-retours réseau en série avant le premier jeton
  deviennent une seule attente, ou aucune.
- **Écrivain à l'épreuve des paniques** : une panique dans une closure d'écriture annule sa
  transaction (rien de commité), est journalisée en `error`, comptée
  (`penelope_store_writer_panics_total`, contrôle `doctor` « Écrivain de la base ») et
  renvoyée au demandeur (`WriterPanic`, qui nomme la panique) au lieu de tuer le thread
  écrivain. Un daemon qui lit mais n'écrit plus, sans alerte, n'est plus possible.

### 0.17.0

Sauvegarde complète chiffrée et restauration en une commande (#42).

- **`penelope backup --push`** : archive de l'instantané de la base, du vault, des skills,
  des workflows, des gabarits, de `mcp.d` et de `config.toml` (artefacts et médias exclus
  sauf `--media`), **chiffrée** par la phrase de passe `backup_passphrase` (Argon2id puis
  XChaCha20-Poly1305) avant de quitter la machine, poussée dans un dépôt privé avec un
  `MANIFEST.json` lisible qui dit la date, la version, les tailles, la somme SHA-256 et les
  **noms** des secrets à ressaisir, jamais leurs valeurs.
- **Garde-fous** : dépôt public refusé (vérifié par `gh`), archive au-delà de
  `backup.max_push_bytes` refusée avec la marche à suivre, absence de phrase de passe dite
  avant tout travail. Rotation 7 quotidiennes, 4 hebdomadaires, 12 mensuelles.
- **Planification** : une sauvegarde par nuit à `backup.cron` (4 h), échec annoncé sur
  Telegram.
- **`penelope restore-all`** : clone le dépôt (ou lit une archive locale), demande la phrase
  de passe à l'invite, remet base et fichiers en place (l'existant mis de côté), puis dit ce
  qui reste à faire : service, secrets à ressaisir, `penelope doctor`. `--dry-run` liste sans
  rien écrire.
- **Diagnostic** : `doctor` suit l'âge de la dernière sauvegarde (alerte au-delà de 48 h), sa
  taille et sa destination ; `self_status` porte la même information.

### 0.17.1

Effets durables contre une coupure (#75), retour arrière qui relit la configuration de la
version suivante (#76), sauvegarde sans gel (#77), purge et rétention de ce que l'agent a
fait et dit (#78), journée budgétaire locale (#79), flux sans « � » (#80), routage image
strict (#81), modèle collant revu aux frontières (#82), effet incertain tranché pour de bon
(#83), boucles de fond surveillées (#84), lectures parallèles (#85), souvenirs anciens
rappelables (#86), filtrage avant la coupe de la recherche mémoire (#87), serveurs MCP
confinés (#89), profil Seatbelt en argument (#90), socket RPC authentifiée (#91),
descriptions d'outils MCP encadrées et épinglées (#92), `http_fetch` épinglé sur
l'adresse vérifiée (#93), fichiers lus en flux (#94), secret posé sans `argv` (#95).

- **Effets durables contre la machine** (#75) : `dispatching` et `completed` d'un effet non
  idempotent passent par `Store::write_durable`, qui exécute la transaction sous
  `synchronous=FULL` et `fullfsync=ON` puis rétablit `NORMAL`, même après une erreur ou une
  panique. Une coupure secteur entre le départ d'un `git push` et le checkpoint suivant ne
  ramène plus la ligne en `planned` : l'effet devient une question au redémarrage. Environ
  8 ms par transition sur un M2, aucune pour les lectures ni le trafic de fond ; le store
  compte ces commits (`durable_commits`).
- **Retour arrière sûr** (#76) : le chargement de `config.toml` tolère les clés inconnues
  (écrites par une version plus récente, ou fautes de frappe) et les nomme dans `doctor`,
  `config validate` et le journal de démarrage, au lieu de refuser de démarrer ; `config set`
  refuse toujours une clé inexistante. Une mutation n'écrit plus le fichier entier : elle
  édite le document en place (`toml_edit`) et ne touche que les clés modifiées, si bien que
  commentaires, ordre et clés inconnues survivent et que le fichier n'acquiert pas les clés
  nouvelles d'une version. Le premier démarrage écrit un fichier court (ce qui s'écarte des
  défauts), une relecture n'écrit plus rien, et un fichier complet écrit par la 0.17.0 est
  gardé en fixture pour que les versions suivantes le relisent.
- **Sauvegarde sans gel** (#77) : l'instantané `VACUUM INTO` part d'une connexion en
  lecture seule ouverte pour l'occasion au lieu de l'écrivain, qui continue de valider
  pendant la copie ; copie des fichiers, tar, Argon2id, chiffrement, somme et envoi git
  s'exécutent sur un thread bloquant au lieu d'un worker tokio. L'événement `store.backup`
  et `doctor` donnent la durée de l'instantané et de la sauvegarde.
- **Purge et rétention complètes** (#78) : `session.purge` vide aussi les arguments et
  résultats d'outils (`effects`, la ligne et la clé d'idempotence restent, un rejeu est
  toujours reconnu), les messages envoyés sur Telegram pendant la session (`tg_outbox`,
  fenêtre bornée à l'ouverture de la session suivante sur le même chat, comme pour les
  updates), les demandes d'approbation (celles en attente sont annulées), les tâches MCP,
  les paramètres et sorties des runs de la session. La rétention vide au-delà de
  `retention.days` le contenu des effets tranchés (un effet `unknown` garde tout), les
  envois partis, les demandes décidées, les tâches MCP et sorties de workflows terminées.
  `doctor` donne la date de la dernière passe et ce que ces tables gardent.
- **Journée budgétaire locale** (#79) : `usage.day`, `spent_today`, la clé de relèvement
  `budget.daily.<jour>`, `/budget`, `/usage` et `--by day` suivent minuit dans
  `owner.timezone` au lieu de minuit UTC ; à La Réunion, la journée ne commence plus à
  4 h et la consolidation de 3 h 30 n'est plus imputée à la veille. Le fuseau est relu à
  chaque calcul : un changement à chaud vaut pour les lignes suivantes, et `doctor` signale
  les consommations des dernières 48 h comptées dans un autre fuseau. Les lignes des
  versions précédentes gardent leur jour UTC (décalage ponctuel le jour de la mise à jour).
- **Flux sans « � »** (#80) : le décodeur SSE garde les octets d'un caractère coupé par le
  transport (au plus trois) pour le paquet suivant au lieu de décoder chaque paquet seul ;
  un `é` ou un emoji à cheval sur deux segments TCP n'abîme plus ni la réponse, ni le
  transcript, ni les arguments d'outils. Testé à chaque offset d'octet et sur des paquets
  de tailles aléatoires ; des octets réellement invalides restent remplacés.
- **Routage image strict** (#81) : la règle qui envoie tout le tour au modèle d'image ne
  réagit plus à une sous-chaîne (« génère », « illustre ») mais à une demande explicite,
  sur mots entiers : verbe de création suivi d'« une image », « un dessin », « une
  illustration » (ou « an image », « a picture »), jamais en présence d'un mot du logiciel
  (script, test, rapport, fichier, ASCII…), de code ou de chemin. « génère un script »,
  « régénère les tests » et « illustre par un exemple » passent par le classifieur.
- **Collant revu aux frontières** (#82) : `at_boundary` est enfin posé, quand le dernier
  appel de conversation de la session est plus vieux que la durée du cache, ou qu'une
  compaction ou une clôture d'épisode l'a suivi. Le préfixe change de toute façon à ces
  moments-là : le message repasse par le classifieur, monte sur `reasoning` ou en redescend,
  et un reclassement en « simple » retire l'ancien collant au lieu de le laisser revenir.
  `/model` dit quand le dernier message a été reclassé à une frontière ; alias épinglé et
  `/model auto off` ne sont pas concernés.
- **Effet incertain tranché pour de bon** (#83) : au redémarrage, une seule demande
  `effect_unknown` par effet (plus une de plus par redémarrage), portant l'appel qui l'a
  lancé ; la passerelle Telegram la pousse au propriétaire dès qu'elle est prête, sans
  attendre `/approvals`. Sa carte propose « C'est fait », « Relancer », « Ignorer » ; la
  décision passe au ledger (`resolve_unknown`, enfin branché) avant la reprise du tour, qui
  rejoue le résultat, relance une fois ou dit au modèle que l'appel reste tel quel. Aucune
  règle n'en naît : `decide_approval` refuse toute règle pour une demande sans arguments
  (budget, effet incertain), et ces cartes n'affichent plus « Toujours » ni « Pour cette
  session ». En ligne de commande : `penelope approve <id> --effect done|retry`.
- **Boucles de fond surveillées** (#84) : runners, ordonnanceur, pilote de workflows,
  maintenance, catalogue, rappel OAuth, entretien MCP et boucles Telegram passent par
  `spawn_supervised` : une panique est journalisée avec le nom de la boucle, comptée,
  versée au journal (`daemon.task_panicked`) et suivie d'une relance (1 s, 2 s… 5 min),
  interrompue par l'arrêt. Un tour qui panique échoue proprement (runner vivant, battement
  du bail arrêté, verrou de session rendu, échec livré). `status` donne les runners vivants,
  `doctor` les boucles relancées dans l'heure ou finies.
- **Lectures parallèles** (#85) : `resolve_pending` décide d'abord de chaque appel dans
  l'ordre (liste blanche, décision prise, boucles, politique), puis exécute. Les lectures
  pures consécutives (liste fermée : fichiers, git en lecture, mémoire, historique,
  artefacts, catalogues) partent ensemble, par quatre ; écritures, actions externes,
  appels approuvés et lectures dont l'ordre compte (`return_value` puis `step_done`,
  `ask_user`, `send_voice`) restent seuls et forment une barrière. Les résultats sont
  enregistrés dans l'ordre des appels et admis en groupe (#52), un échec n'annule pas les
  autres, `/stop` interrompt tout le lot. Trois lectures de 300 ms : moins de 600 ms au lieu
  de 900.
- **Souvenirs anciens rappelables** (#86) : le seuil du rappel automatique porte sur la
  pertinence (`Scored.relevance`, rang RRF), plus sur le score multiplié par la récence et
  l'importance ; ces facteurs ne font qu'ordonner. Une entrée de niveau Cure d'importance 5
  n'est plus perdue après 7 jours : à 30, 90 et 180 jours elle est injectée quand elle est
  la seule réponse, après une récente équivalente. `memory.half_life_days` est enfin lu
  (à chaud) et passe à 180 jours ; un `config.toml` écrit en entier par une version
  antérieure porte encore `30.0`, à relever par `penelope config set
  memory.half_life_days 180`. Le retrait n'est proposé qu'aux entrées apparues au
  moins dix fois dans les résultats sans être retenues (`mem_signals.seen`, migration
  `0013_memory_seen`).
- **Filtrer avant de couper** (#87) : niveau, type, projet, slug, épisodique, passages de
  documents (`source`) et parties de pratiques entrent dans les deux requêtes de
  `MemoryIndex::search` ; la coupe aux 200 premiers porte sur des candidats admissibles et
  les vecteurs écartés ne sont plus décodés. Mille passages ingérés plus proches de la
  question ne chassent plus un souvenir de `notes.md` du rappel automatique ; `mem_search`
  explicite garde sa portée. La décision 0002 dit enfin ce que fait le code.
- **Serveurs MCP confinés pour de bon** (#89) : le profil d'un serveur stdio imposé
  (`mcp-stdio`, `workspace-write`, `readonly`) reçoit `sandbox.deny_read`, comme le shell :
  `~/.ssh`, `penelope.db`, `secrets.enc`, `mcp.d`, configuration et état ne se lisent plus
  depuis un paquet tiers ; son répertoire de données et ses racines restent lisibles. Le
  trousseau est fermé à tout profil imposé, même avec `deny_read = []`. `doctor` signale un
  serveur confiné sans lecture refusée.
- **Profil Seatbelt en argument** (#90) : `sandbox-exec -p <profil>` au lieu d'un fichier
  `.sb` dans `$TMPDIR/penelope-sandbox`, dossier que les profils `workspace-write` et
  `mcp-stdio` ouvrent en écriture : un processus confiné ne peut plus réécrire le profil du
  suivant avant sa lecture. Plus aucun fichier écrit (le champ `cleanup`, jamais lu, a
  disparu) ; le daemon efface au démarrage le dossier laissé par les versions précédentes.
  Vérifié sur la machine par un test ignoré par défaut (`-- --ignored`).
- **Socket RPC authentifiée** (#91) : le daemon tire un jeton de session à chaque démarrage
  (`{state}/rpc.token`, `0600`, écrit avant que la socket n'apparaisse) ; toute requête
  sans ce jeton, ou avec un autre, reçoit `unauthorized` et n'exécute rien (comparaison à
  temps constant). La CLI le joint à chaque appel ; il n'apparaît ni dans les journaux ni
  dans `doctor`. En défense en profondeur, les profils Seatbelt à réseau ouvert refusent
  les sockets Unix (`(deny network-outbound (remote unix-socket))`) hors `mDNSResponder` et
  l'agent SSH : un serveur MCP ne joint plus la socket du daemon ni celle de Docker.
  Vérifié sur la machine par un test ignoré par défaut.
- **Outils MCP : descriptions encadrées, empreintes épinglées** (#92) : `tool_search` et
  `tool_describe` rendent descriptions et schémas encadrés comme contenu non fiable (alerte
  du détecteur local comprise) ; un schéma exposé d'office dit son serveur et perd sa
  description si elle porte une consigne ; la carte d'approbation d'un outil signalé
  l'annonce. `mcp_tools` garde l'empreinte (description, schéma, annotations) et la date de
  première vue (migration `0014_mcp_tool_fingerprint`) : un changement silencieux après
  `tools/list_changed` révoque les « Toujours » de l'outil, journalise `mcp.tool_changed` et
  prévient le propriétaire ; une description suspecte journalise `mcp_tool_suspicious`.
- **`http_fetch` épinglé sur l'adresse vérifiée** (#93) : chaque saut résout le nom une
  seule fois (`resolve_checked`), refuse s'il voit une adresse privée, puis se connecte par
  un client épinglé sur ces adresses (`resolve_to_addrs`) : reqwest ne résout plus une
  seconde fois, un DNS à TTL nul ne peut plus basculer vers la boucle locale ou les
  métadonnées entre le contrôle et la connexion. SNI, `Host` et certificat restent sur le
  nom. Le garde (`AddressGuard`) est injectable : les tests rejouent un rebinding, une
  redirection vers un nom qui rebinde et le repli sur une seconde adresse vérifiée.
- **Fichiers lus en flux** (#94) : `fs_read` saute `offset` lignes sans les garder, prend
  `limit` lignes, regarde s'il en reste, et ne compte le total que sous 8 Mio ; `fs_search`
  lit ligne à ligne et ignore (en les nommant) les fichiers de plus de 32 Mio ; une ligne
  est tronquée à 64 Kio, un saut au-delà de 64 Mio est refusé avec la marche à suivre.
  Mesuré sur un M2 : 50 lignes d'un journal de 512 Mio en 0,7 ms et 7,6 Mio de mémoire
  résidente, contre 944 ms et 607 Mio.
- **Secret posé sans `argv`** (#95) : `KeychainStore::set` ne passe plus la valeur en
  argument de `security add-generic-password -w`, lisible par `ps` depuis tout processus du
  même utilisateur pendant l'écriture ; la commande part sur l'entrée standard de
  `security -i`, valeur en hexadécimal (`-X`), sans rien à échapper. Une erreur du
  Trousseau reste lisible, sans la valeur. Aller-retour réel dans le Trousseau : test
  ignoré par défaut, à lancer à la main (il écrit un secret d'essai).

### 0.17.2

Message court traité tout de suite (#96), rappels d'approbation envoyés (#97), document
Telegram détaché (#98), CLI qui ne pend plus devant un daemon muet (#99), Ctrl-C qui
arrête le tour (#100), file d'envoi Telegram dans l'ordre et échecs dits (#101), suite
verte sur Linux et CI en deux jobs (#102), journaux corrélés par tour et métriques
lisibles (#103), outils natifs à la demande : 20 définitions par appel au lieu de 52
(#104), usage mesuré dans le classement de la mémoire (#105), réseau du shell fermé par
défaut et accordé par appel (#106), compression de contexte expliquée sur une page
vérifiée contre le code (#107).

**À la mise à jour.** Une configuration sans `sandbox.shell_network` perd le réseau du
shell : une commande le demande désormais (`network: true`) et l'approbation le dit ; pour
le rouvrir à tout, `penelope config set sandbox.shell_network true`. La migration 0015 remet
à zéro les compteurs de rappels de la mémoire.

- **Un message court part tout de suite** (#96) : la fenêtre de regroupement ne s'ouvre
  plus pour chaque message. Un morceau à la limite de Telegram (4 000 caractères ou plus)
  ou un message transféré ouvre ou prolonge une rafale (`telegram.text_group_window_ms`,
  2 s au lieu de 3) ; un message court tapé seul crée son tour aussitôt ; un message court
  pendant une rafale en est la fin probable et la ferme après 300 ms de silence. Un
  « merci » ne paie plus 3 s d'attente avant le modèle.
- **Rappels d'approbation** (#97) : `due_reminders`, écrit et testé mais jamais appelé, est
  branché dans la maintenance. Une demande sans réponse est rappelée à T+1 h puis T+6 h
  (« ⏰ Rappel 1/2 », carte et boutons neufs, dans la conversation d'origine), plus rien
  ensuite ni après une décision ; `reminded_at` est enfin écrit.
- **Document détaché** (#98) : le téléchargement d'un document (jusqu'à 20 Mo) quitte la
  boucle des updates, comme vocaux et photos depuis #69 : `/stop`, les boutons et le
  message suivant n'attendent plus ; l'échec reste dit en réponse au document.
- **CLI qui ne pend plus** (#99) : une réponse du daemon est attendue 15 s au plus
  (`--timeout`, 0 pour sans limite ; les méthodes longues par nature attendent sans limite
  sauf `--timeout` explicite), puis la commande sort avec le code 7 (`DAEMON_UNRESPONSIVE`)
  et la marche à suivre. `doctor` rend ses contrôles locaux (binaire, configuration) même
  quand le daemon est absent ou muet, et le nomme en tête comme contrôle critique.
- **Ctrl-C arrête le tour** (#100) : dans `penelope chat`, Ctrl-C appelle `chat.stop`
  (qui vide aussi la file de la session) et sort avec 130 ; un second Ctrl-C quitte sans
  attendre. Côté daemon, un client de flux qui ferme sa connexion (terminal fermé, SSH
  coupé) fait annuler son tour, qu'il tourne ou attende encore : plus d'outil ni d'appel
  facturé pour une réponse que personne ne lira. `tail` et les tours Telegram ne sont pas
  concernés.
- **File d'envoi dans l'ordre, échecs dits** (#101) : un envoi qui attend sa nouvelle
  tentative retient les suivants du même chat, dans le passage et d'un passage à l'autre
  (les autres chats passent) ; une erreur de transport isolée est reprise tout de suite par
  `Bot::call`. Un refus définitif est dit dans le chat par une note en texte brut, jamais
  suivie d'une autre si elle échoue à son tour ; `/status` et `status` comptent les
  messages non envoyés (`outbox_failed`).
- **Suite verte sur Linux, CI en deux jobs** (#102) : hors macOS, `Services::for_tests`
  lance les commandes sans profil imposé (le bac à sable n'existe pas, l'échec fermé
  refusait tout) : le condensé de tests, le ledger des étapes shell et l'e2e
  `ticket-to-deploy` y tournent. Le vrai serveur MCP sous Seatbelt est marqué macOS, le test
  de `probe_version` n'utilise plus `sleep` (GNU répond à `--version`). La CI rejoue la
  suite entière sur `ubuntu-latest` et garde sur `macos-14` le format, clippy, les tests de
  la plateforme (Seatbelt réellement appliqué compris), le serveur MCP sous bac à sable,
  launchd et le binaire de release.
- **Journaux corrélés** (#103) : `runner::process` entre dans un span `turn` (tour,
  session, genre), le pilote de workflows dans un span `run`, la maintenance dans un span
  `maintenance` ; avec `with_current_span`, chaque ligne JSON porte le tour ou le run qui
  l'a causée. `penelope logs --turn <id>` (ou `--session`) relit les journaux du jour et de
  la veille sans daemon. Sous launchd (`PENELOPE_SERVICE=1`), plus de copie sur stderr ;
  `observability.log_level` est enfin lu. Le registre de métriques a un lecteur :
  méthode RPC `metrics` et `penelope metrics` (texte Prometheus), avec les compteurs de
  tours par issue et leur durée, d'appels d'outils par outil, et les jauges d'approbations,
  d'effets incertains et de mémoire.
  Les deux tests restés rouges sur Linux sont corrigés : l'arrêt du groupe de processus
  passe par `kill -s SIG -- -<pgid>` (le `kill` de procps-ng lisait `-<pgid>` comme une
  option, le groupe d'une commande hors délai survivait), et le test d'instantané garde sa
  preuve (des écritures aboutissent pendant la copie) avec un plafond de latence adapté au
  disque d'un runner partagé.
- **Outils natifs à la demande** (#104) : en conversation, chaque appel ne décrit plus que
  le noyau de 17 outils d'usage courant et les trois méta-outils, 20 définitions et
  2 592 tokens de schémas au premier tour (contre 52 et 5 970). Les 35 outils rares
  (planification, `git_*`, `skill_*`, `intent_*`, gestion des workflows, `history_expand*`,
  `mem_remember/get/neighbors/forget`, `config_set`, `self_docs`, `session_*`,
  `send_file`, `send_voice`, `image_generate`) sont nommés dans le message système ;
  `tool_search` les trouve par racines de mots (« planifier un rappel » donne
  `schedule_create`), `tool_describe` rend leur schéma, `tool_call` les appelle par le même
  chemin qu'un appel direct (nom effectif, classe de risque, politique : même carte
  d'approbation). Un outil décrit ou appelé rejoint la liste de la session au tour suivant
  et la quitte après dix tours sans usage (`session.tools.<id>`, purgé avec les clés
  éphémères). Workflows et sous-agents gardent leur liste complète. L'évaluation en
  direct du choix d'outil (`live_openrouter`) demande une clé et le réseau : non rejouée.
- **L'usage pèse sur le rappel** (#105) : un souvenir servi en conversation (rappel
  automatique ou `mem_search`) compte comme rappelé, et comme utile seulement si la
  réponse reprend un de ses mots distinctifs absents de la question (deux pour un souvenir
  long) ; un tour rejoué après approbation ne compte pas deux fois. Le score gagne un
  facteur `usage_factor` borné à [0,85 ; 1,2] : gain sur les rappels utiles et les succès,
  perte sur la part de rappels inutiles (à partir de cinq) et les contradictions. La
  pertinence seule garde le seuil du rappel automatique. La grille de consolidation voit
  l'usage de chaque souvenir proche comme preuve, le placement reste calculé des critères
  (test : mêmes verdicts, compteurs à 0 ou 20, même tri). `penelope mem signals <uid>`
  (méthode `mem.signals`) lit les compteurs et le facteur. Migration 0015 : rappels et
  rappels utiles repartent de zéro, ils comptaient tout souvenir servi comme utile.
- **Réseau du shell accordé par appel** (#106) : `sandbox.shell_network` passe à `false`
  par défaut (une configuration qui porte `true` le garde). `shell_exec` prend
  `network: true` : l'appel devient `external`, la raison de la carte commence par « accès
  réseau demandé » et Telegram ajoute une alerte 🌐. « Toujours » enregistre la famille de
  commandes **et** le réseau ; une règle sur `shell_exec` qui ne nomme pas le réseau ne le
  donne jamais (`PolicyRule::matches`), `/policies` affiche « avec réseau ». Un échec sans
  réseau qui y ressemble (résolution, connexion, `curl`, `git push`, `gh`, installation de
  paquets) porte `NETWORK_OFF_NOTE` et `network: false`. Les étapes `shell` gagnent
  `network` (schéma, aperçu « · réseau ») ; les workflows livrés le déclarent pour le
  clone, le déploiement, la vérification et le retour arrière. `doctor` :
  `sandbox.shell_network`. Test sous Seatbelt (CI macOS) : `nc -z` vers un port local
  échoue sans réseau avec la note, passe avec `network: true`.
- **La compression de contexte sur une page** (#107) : `docs/context.md` suit une session
  du premier tour à la cinquième compaction (ce qui part au modèle, niveaux 0 à 4, queue
  verbatim, gabarit à neuf sections, mise à jour du même résumé, échecs, attente, réserve
  de budget, cache, événements). Sa table par fenêtre (8 k à 1 M) est générée depuis
  `CompactionParams`, et le test `docs` refuse une clé inconnue ou une valeur citée
  (`` `clé` = `valeur` ``) qui n'est plus le défaut ; l'index la cite. Le code suit la page
  sur deux points : sans seuil propre au modèle, le seuil descend jusqu'à laisser la place
  d'un groupe de résultats d'outils entier et de la réserve de réponse (65 % sur 32 k,
  63 % sur 8 k, 70 % inchangé dès 128 k), et la queue verbatim ne dépasse jamais le quart
  du seuil de fond (sur 8 k, 10 k de queue interdisaient tout résumé). La documentation de
  `context.model_thresholds` disait « sans effet » : elle était lue depuis longtemps.

### 0.17.3

Un « ok » qui accepte une proposition de Pénélope est relu comme la décision qu'il prend
(#108), le digest du matin dit ce que la nuit a écarté et pourquoi, nuits blanches
comprises (#109), et `tool_call` ne perd plus ses arguments : une erreur d'arguments dit
quoi envoyer, un nom inconnu rend les noms proches (#110).

- **Un « ok » relit la décision qu'il prend** (#108) : un accord court (« ok », « go »,
  « oui », « vas-y », « tu peux publier », 👍 ; ni remerciement, ni question, ni réserve)
  qui suit une proposition de Pénélope (choix proposés, « je propose… », ou une dernière
  ligne qui demande « je l'ouvre ? », « tu valides ? ») déclenche la revue avec la
  proposition du tour précédent comme matière. Le prompt de revue dit qu'une décision
  acceptée d'un mot se formule depuis la proposition, et ne pousse plus vers la liste
  vide. Un appel au rôle `memory_review` par tour retenu, au plus
  `memory.review_max_candidates` candidats, comme avant.
- **Le digest dit aussi ce qui est écarté** (#109) : trois chiffres (candidats examinés,
  promus, écartés, et reportés s'il y en a), les motifs de rejet regroupés par famille
  (`rejection_family` : ce qui précède la précision du motif ; cinq familles, le reste
  additionné) et le renvoi au rêve de la nuit dans `DREAMS.md`, qui gagne une section
  « Motifs d'écart » et s'écrit désormais même quand tout a été écarté avant la grille.
  Deux nuits consécutives ou plus à zéro promue ajoutent « ⚠️ N nuits de suite sans rien
  retenir » avec le motif dominant, ou le constat qu'aucun candidat n'a été noté. Le
  digest reprend `DreamReport::render_brief` (sans la liste des rejets) : cinquante
  rejets tiennent sous la limite d'un message Telegram.
- **Une erreur d'arguments dit quoi envoyer** (#110) : la cause racine était `tool_call`,
  qui déclarait `args` en objet sans propriétés ; les fournisseurs qui contraignent la
  génération le vidaient, huit appels de suite arrivaient avec `{}`. `args` autorise
  maintenant des propriétés libres (`additionalProperties: true`, exemple dans la
  description), `args_json` accepte les mêmes arguments en chaîne, et seul `name` est
  requis. Un refus (natif ou MCP, sans aller au serveur) devient
  `ToolError::BadArguments` avec les paramètres attendus (`expected_args` : requis
  d'abord, type, valeurs, description, 1 500 caractères au plus) ; un `tool_call` vide
  vers un outil à paramètres requis le dit (« perdus en route ») ; un nom inconnu devient
  `NoSuchTool` avec les noms proches (`close_names`, natifs et MCP). La garde de boucle
  reste le filet. Au passage, par `tool_call`, la politique, la carte et la règle
  « Toujours » portaient sur l'enveloppe : un « Toujours » sur `shell_exec` appelé ainsi
  couvrait tout le shell ; elles portent désormais sur les arguments de l'outil visé.

### 0.17.4

Un groupe Telegram à sujets s'ouvre par son identifiant, et le propriétaire y parle même
en administrateur anonyme (#113).

- **Un groupe à sujets s'ouvre par son identifiant** (#113) : `telegram.allowed_chats`
  liste les conversations de groupe autorisées ; hors de la liste, un message est ignoré
  en silence (`Incoming::ForeignChat`), mais journalisé avec l'identifiant, le type et le
  titre, et gardé (vingt au plus) pour la ligne « Conversations Telegram » de
  `penelope doctor`. Dans un groupe listé, un message de `GroupAnonymousBot`
  (`1087968824`) dont `sender_chat` est le groupe lui-même vaut propriétaire, et lui
  seul ; un tiers reste refusé. La liste est relue à chaque update, donc s'applique à
  chaud. `telegram.allow_groups` n'ouvre plus rien seul ; `doctor` le signale s'il est
  vrai sans identifiant. Chaque sujet porte sa session, deux sujets travaillent en
  parallèle.

### 0.17.5

Les lectures du shell passent sans demande, avec un mode d'approbation par session et des
familles autorisées d'avance (#111) ; quitter une session ne vide plus sa file, elle
travaille en fond (#112) ; un serveur MCP stdio mort dit son code et sa durée de vie
(#114) ; plus de `null` dans les bulles Telegram (#115).

- **Les lectures du shell ne demandent plus rien** (#111) : `is_read_command` classe une
  ligne de lecture (programme connu appelé par son nom, sans enchaînement, redirection,
  substitution, variable, échappement ni option qui écrive ou lance autre chose : `find
  -exec`, `sort -o`, `git -c`, `git diff --output`…) ; `shell_exec` y prend la classe
  `read`, idempotente, et part sans demande. Mode par session (`session.mode`, `/mode`,
  `penelope session mode`, défaut `tools.approval_mode` = `reads`) : `ask` redemande même
  une lecture du shell et passe outre les règles, `auto` laisse passer ce que la classe
  demanderait sauf le destructif, une politique imposée par un serveur MCP et toute
  commande que `may_destroy` ne peut juger sûre. `tools.shell_allow` et
  `tools.shell_allow_network` autorisent des familles d'avance. Une commande composée
  n'a plus de famille : « Toujours » l'autorise une fois sans créer de règle sur `cd`.
  `penelope policies` et `/policies` marquent les règles inutiles (`rule_note`).
- **Quitter une session ne vide plus sa file** (#112) : `bind_chat` n'annule plus les
  tours de la session qui perd le fil ; ils s'exécutent en fond et leurs sorties sont
  retenues (`hold`) puis délivrées au retour, comme la réponse du tour en vol. L'avis de
  bascule dit « continue en fond (N tours en file) », `/sessions` affiche `⏳N`. `/new` sur
  une session qui a une file demande d'abord : « Garder l'ancienne en fond » (la nouvelle
  prend le fil, l'ancienne reste active) ou « Fermer (N tours perdus) ». Un bouton de
  commande porte désormais ses arguments (`command_button_with`). Une session en fond
  au-delà de son plafond s'arrête seule.
- **Un serveur MCP stdio mort dit comment** (#114) : quand sa sortie standard se ferme,
  le transport attend le processus (deux secondes au plus) et garde `ExitInfo` (code ou
  signal nommé) et sa durée de vie ; une requête en attente échoue sur « le serveur s'est
  arrêté : sorti avec le code 1 après 40 ms, sans rien écrire sur sa sortie d'erreur » ou
  avec sa dernière ligne d'erreur. `logs` ajoute la fin du processus et dit une sortie
  vide au lieu de rendre `[]` ; `last_error` de `mcp show` et le résultat de `mcp test` le
  reprennent.
- **Plus de `null` dans les bulles** (#115) : une valeur JSON montrée passe par `shown`
  (chaîne sans guillemets, nombre tel quel, absence en « ? ») ; quinze sites corrigés dans
  les écrans, les commandes et `doctor`. Un test refuse toute valeur JSON brute passée à
  `format!` dans le code Telegram et parcourt les écrans usuels sans y trouver `null`.

### Routine de livraison

Avant chaque tag :

1. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`, `cargo deny check`.
2. `progress.md` : nouvelle section de version, compte de tests, tableau du §21.
3. Relire **toutes** les sections « Limites actuelles » et « pas encore branché » de
   `README.md` et de `docs/` (`mcp.md`, `telegram.md`, `workflows.md`,
   `install-headless.md`), pas seulement celle-ci. Le test `docs` attrape une méthode
   RPC, une commande ou un outil livrés présentés comme absents, pas le reste.
4. `install-headless.md` pour tout ce qui change l'usage ; `UPDATE_DOCS=1 cargo test -p
   penelope-evals --test docs` si une clé de configuration ou un outil a changé (le test
   `docs` vérifie aussi l'index `docs/README.md`, les liens et ancres, les commandes
   Telegram et la section de version) ; `UPDATE_CA_MATRIX=1` si un test `ca_*` a été
   ajouté. Le modèle de pull request reprend ces cases.
5. CI verte sur `main`, puis tag annoncé, release suivie jusqu'aux artefacts.

### Encore à brancher

Rien de ce que décrit le PRD n'est déclaré sans implémentation. Restent à **exécuter**,
avec des ressources réelles : les suites réseau, `ab-hermes` contre l'instance Hermes, et
`penelope import hermes` sur cette instance (§20.2, points 3 et 4).

### Méthodes RPC déclarées mais non servies

Aucune. Un test (`penelope-daemon`, `rpc.rs`) fixe cette liste vide : une méthode
déclarée sans être servie le fait échouer.

### Autres manques

- L'OCR ne tourne que sur macOS (Vision) et s'active seulement quand un PDF n'a aucune
  couche texte ; un PDF mixte garde ses pages scannées illisibles. L'extraction de `lopdf`
  ignore la mise en page (colonnes, tableaux) et les polices sans table Unicode.
- `.penelope/deploy.toml` n'est pas encore interprété : `deploy-generic` passe par les
  cibles `make` du dépôt ([décision 0007](decisions/0007-deploiement-par-makefile.md)).
- Telegram : seul le long polling est servi (`telegram.mode = "webhook"` non).
- `penelope import hermes` n'a pas encore tourné sur l'instance réelle (§20.2, point 3) :
  le format suit la documentation d'Hermes, l'importeur reste tolérant. Les filtres
  `tools` et les réglages TLS d'un serveur Hermes n'ont pas d'équivalent (signalés).
- Signature des releases : le code est prêt, la clé reste à créer et à poser dans le
  dépôt (`docs/install-headless.md`, section « Mise à jour ») ; d'ici là, seule la somme
  SHA-256 est vérifiée.
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
| [0006](decisions/0006-changement-de-sujet-lexical.md) | Changement de sujet mesuré sans modèle | Aucun appel de plus par message ; une fausse frontière ne perd rien |
| [0007](decisions/0007-deploiement-par-makefile.md) | `deploy-generic` par cibles `make` | Pas de commande arbitraire lue dans le dépôt ; convention lisible en SSH |
| [0008](decisions/0008-cache-de-prompt.md) | Rien ne bouge avant le dernier message | Un préfixe relu coûte une fraction du prix d'entrée ; chaque raté est mesuré |

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
