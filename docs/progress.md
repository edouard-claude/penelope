# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 17 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **1311 tests verts** hors réseau ; les suites réseau sont écrites et se lancent à la demande.
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

### 0.16.1

Verrou de session à jeton de clôture (#43), écrivain à l'épreuve des paniques (#44),
mutations de configuration sérialisées (#45), purge RGPD et rétention (#46), journal
d'audit sans faux positif (#47), listes de frontmatter lues correctement (#48), rafales de
messages regroupées (#49), nouvelles tentatives avant le flux (#50), flux muet coupé sur
son inactivité (#51), résultats d'outils parallèles admis en groupe (#52), comptage et
fenêtre des modèles locaux (#53), émulation d'outils retirée (#54), projection qui ne relit
plus ce qui est résumé (#55).

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
- **Écrivain à l'épreuve des paniques** : une panique dans une closure d'écriture annule sa
  transaction (rien de commité), est journalisée en `error`, comptée
  (`penelope_store_writer_panics_total`, contrôle `doctor` « Écrivain de la base ») et
  renvoyée au demandeur (`WriterPanic`, qui nomme la panique) au lieu de tuer le thread
  écrivain. Un daemon qui lit mais n'écrit plus, sans alerte, n'est plus possible.

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
