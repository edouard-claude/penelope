# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 23 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **1733 tests verts** hors réseau externe ; les suites réseau sont écrites et se lancent à la demande.
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
| 6 | `mcp` (négociation, transports, primitives, OAuth, registre, supervision) | fait, superviseur et OAuth branchés | 98 + 19 conformité + 16 daemon |
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
(#114) ; plus de `null` dans les bulles Telegram (#115) ; un appel aux arguments invalides
ne coûte plus de carte d'approbation (#117).

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
  avec sa dernière ligne d'erreur ; une écriture refusée (« Broken pipe », serveur mort
  avant de lire) attend un instant sa fin pour la citer. `logs` ajoute la fin du processus et dit une sortie
  vide au lieu de rendre `[]` ; `last_error` de `mcp show` et le résultat de `mcp test` le
  reprennent.
- **Plus de `null` dans les bulles** (#115) : une valeur JSON montrée passe par `shown`
  (chaîne sans guillemets, nombre tel quel, absence en « ? ») ; quinze sites corrigés dans
  les écrans, les commandes et `doctor`. Un test refuse toute valeur JSON brute passée à
  `format!` dans le code Telegram et parcourt les écrans usuels sans y trouver `null`.
- **Valider avant de demander** (#117) : en production, un `workflow_start` dont `params`
  était une chaîne remplie du balisage d'appel du modèle (`<arg_key>…`) a coûté deux
  cartes pour rien, la validation n'intervenant qu'à l'exécution. `ToolExecutor::precheck`
  vérifie désormais chaque appel avant la politique et l'approbation (balisage
  d'appel nommé, outil connu, schéma natif ou MCP, par `tool_call` compris) ; un refus
  revient au modèle avec les paramètres attendus (#110). Les appels refusés comptent pour
  la garde de boucle par outil (`observe_invalid`) : le deuxième avertit, le troisième
  arrête le tour, même si les arguments changent. L'étape `tool` d'un workflow fait la
  même vérification avant sa carte.

### 0.17.6

La carte d'approbation dit d'abord ce que Pénélope cherche à faire, puis la commande
telle qu'elle sera exécutée (#116).

- **La carte d'approbation dit ce que Pénélope cherche à faire** (#116) : chaque outil
  natif qui peut demander l'approbation (et `tool_call`) accepte `pourquoi`, une phrase
  pour le propriétaire, que la règle du harnais demande ; à défaut, la carte reprend le
  message du propriétaire qui a lancé le tour, jamais la raison de la politique.
  `tool_approval` devient intention, action (la commande exacte en bloc de code, sinon
  l'outil et ses valeurs sur une ligne), puis une ligne de qualificatifs (réseau, sortie
  complète, répertoire, serveur MCP, classe, politique) ; l'alerte réseau tient en quatre
  mots et « Toujours » dit sa portée (« Toujours pour « gh pr » (réseau) »). `gh` et `go`
  rejoignent les familles de commandes en deux mots. L'intention est retirée des
  arguments envoyés à un serveur MCP et de l'empreinte de la garde de boucle ; le champ
  n'a pas de description dans les schémas (la règle l'explique une fois) : 2 859 tokens de
  schémas au premier tour, sous le plafond de 3 000 de #104. La règle du
  harnais qui conseillait d'enchaîner les lectures (`&&`) dit désormais qu'une lecture
  simple passe sans demande et qu'enchaînée elle demande (#111).

### 0.17.7

Les skills ne se rechargent plus chaque minute (#118), et la mémoire injectée d'office
suit le sujet de travail de la session (#119).

- **Les skills ne se rechargent plus chaque minute** (#118) : `reload_skills` réécrivait
  les skills livrées à chaque passage, ce qui déplaçait leur date, et l'empreinte du
  dossier était bâtie sur les dates : chaque rechargement provoquait le suivant (851 par
  jour en production). Une skill livrée n'est réécrite que si son contenu diffère,
  l'empreinte hache le contenu (la date seulement au-delà d'un Mio), et celle gardée est
  calculée après le rechargement (`skills_tick`). Un contenu inchangé laisse le préfixe du
  prompt intact. Les ratés de cache « outils » ne venaient pas de là (les skills sont dans
  le préfixe, dont le changement se classe « préfixe ») mais des outils à la demande qui
  rejoignent ou quittent la liste d'une session (#104).
- **La mémoire d'office suit le sujet de la session** (#119) : dans un sujet Telegram
  créé pour LinkedIn, tout Fidelatoo était injecté à chaque tour. Une session porte
  désormais un sujet de travail (`session_project`) : choisi (`/projet`, `penelope session
  project`, `session.project`) ou déduit, quand l'instantané de l'épisode se fige, du nom
  du sujet Telegram (lu dans les messages du forum), du titre ou du message, parmi les
  projets connus du vault (annotation `projet`, sections de `projets.md`). L'instantané T2
  garde le profil, les entrées sans projet et celles du sujet ; les autres restent au
  rappel (exclues de `injected_uids`) et à `mem_search`. Une session sans sujet ne reçoit
  que les entrées sans projet. `/sessions` affiche 📁, `self_status` donne `project`.

### 0.17.8

Une planification qui n'envoie rien est un échec signalé, et l'état qu'elle a consommé est
remis (#120).

- **Une planification muette est un échec, et son état est remis** (#120) : la veille de
  8 h 33 avait produit son édition, consommé `seen.json` et rien envoyé, comptée réussie.
  Un prompt planifié déclare son `livrable` (`message`, `fichier:<chemin>`, `run`) et son
  `etat` (fichier consommé) dans sa cible. Avant le tour, l'état est gardé (2 Mio au plus,
  dans un workspace) ; après, `trigger_outcome_of` vérifie le livrable : sans lui,
  l'exécution est enregistrée en échec (« exécutée sans livrable : … ») et prévenue comme
  celles de #39, et l'état est remis. Le digest du matin liste les planifications en
  échec. Sans livrable déclaré, rien ne change.

### 0.17.9

Pendant un tour, Pénélope donne des signes de vie : l'indicateur d'activité vit aussi
longtemps que le tour, dans le bon sujet (#121).

- **Des signes de vie pendant un tour** (#121) : `sendChatAction` n'était envoyé qu'une
  fois, au premier brouillon, sans `message_thread_id` et jamais dans un groupe. Chaque
  tour d'une session au premier plan a désormais son indicateur (`start_activity`),
  renvoyé toutes les quatre secondes tant que le bus le dit actif, dans son sujet, arrêté
  à la fin du tour ; un signe part dès la mise en file du message. L'action suit l'outil
  en cours (`upload_document`, `record_voice`, `upload_photo`, sinon `typing`). Les appels
  sont jetables (hors file durable, erreurs ignorées). Le brouillon privé montre l'outil
  et son argument principal (commande, chemin, requête).

### 0.17.10

Un serveur MCP qui lit ses propres identifiants garde son bac à sable et retrouve le
trousseau, et un trousseau fermé ne se fait plus passer pour un secret absent (#122).

- **Le trousseau, par serveur, sans tout rouvrir** (#122) : depuis #89, `mailbridge` ne
  lisait plus ses mots de passe, et le trousseau fermé répondait « introuvable » (`secret
  not found in keyring`), ce qui envoyait vérifier une configuration juste.
  `sandbox.allow_keychain_for` ouvre le trousseau (`mach-lookup` sur
  `com.apple.SecurityServer`) aux seuls serveurs stdio nommés ; lectures refusées,
  écritures et sockets restent celles du profil, `allow_full_for` n'est pas nécessaire.
  Le réglage prend effet au prochain appel : un serveur lancé repart sous le profil en
  vigueur. `penelope mcp list` (colonne « trousseau »), `mcp show` (`keychain`) et
  `doctor` disent quels serveurs le joignent. Un serveur confiné qui échoue en parlant du
  trousseau voit son erreur suivie du bac à sable et du réglage, ou de la voie par
  l'environnement (`${SECRET:nom}` dans `env`). `docs/mcp.md` gagne une section « Bac à
  sable ».

### 0.17.11

Une lecture précédée de `cd <dépôt> &&` ne demande plus rien (#123), et chaque
planification dit où elle livre et se déplace sans être recréée (#124).

- **Un `cd` en tête n'est plus un enchaînement** (#123) : le modèle écrit
  `cd <dépôt> && grep …`, et le critère d'enchaînement de #111 faisait demander chacune de
  ces lectures. Un `cd <répertoire> &&` seul en tête, vers un répertoire d'un workspace,
  sans variable, substitution, tilde ni joker, est relevé en `cwd` avant toute décision
  (`normalise_call`) : garde de boucle, classement, carte, « Toujours » et exécution
  portent sur la vraie commande. Hors workspace, suivi d'un `;`, d'un `|`, d'un second
  `&&`, d'une redirection ou d'une substitution, la ligne reste composée et demandée.
  Même forme dans les étapes de workflow et par `tool_call`. La description de
  `shell_exec` dit au modèle d'utiliser `cwd` plutôt que ce préfixe.

- **Où livre une planification, et la déplacer** (#124) : la veille du matin était bien
  partie (#120, constat corrigé), mais dans la conversation privée où elle était née,
  alors que le propriétaire regardait un sujet de groupe ; rien ne le disait, et la
  déplacer obligeait à la recréer. `schedule list`, `/schedules` et `schedule_list`
  portent sa `destination` en mots (titre du groupe et nom du sujet retenus au passage,
  « (par défaut) » sans choix). `penelope schedule move <id> --chat … --topic …|--private`,
  `/schedules ici <id>`, le bouton 📍 et l'outil `schedule_move` (méthode
  `schedule.move`) changent `target.origin` sans toucher à l'identifiant, aux exécutions
  ni à l'état, vers le propriétaire ou une conversation de `telegram.allowed_chats`
  seulement. Le digest du matin liste ce qui part dans la journée, avec l'heure locale et
  l'endroit.

### 0.17.12

Pénélope sait pointer un élément dans une image, plus seulement la décrire (#125).

- **Décrire, lire ou pointer** (#125) : pour vérifier une pagination iOS, une session a
  passé 161 appels à deviner des coordonnées, le bouton visé étant une image sans
  libellé, absente de l'arbre d'accessibilité ; la seule voie de vision décrivait, en
  prose française. Le module `vision` porte trois tâches : `describe` (consigne
  inchangée, photo reçue), `read` (texte recopié tel quel, dans sa langue) et `locate`.
  L'outil à la demande `image_inspect` les pose sur une photo reçue (son chemin reste
  dans le message) ou une capture du workspace. En `locate`, le rôle `image_locate`
  (défaut : l'alias de `image_describe`) reçoit la taille de l'image et le repère
  attendu, sans langue imposée ; la réponse est rendue brute, avec la taille et
  `points` en pixels de l'image (lus dans `(x, y)`, `[x1, y1, x2, y2]`, `"x"/"y"`,
  `<point>`), ramenés depuis les millièmes si `models.locate_frame = "per_mille"`,
  signalés hors de l'image. Le résultat est encadré comme donnée non fiable. Les rôles
  d'image ne comptent plus parmi ceux qui appellent des outils : un modèle de pointage
  sans tool calling peut servir l'alias `vision`.

### 0.17.13

Les appels d'outils MCP en protocole 2026-07-28 aboutissent (#126).

- **`Mcp-Name` reprend le corps** (#126) : depuis l'implémentation initiale (5aceb59),
  toute requête Streamable HTTP partait avec `Mcp-Method` et `Mcp-Name: penelope` en dur,
  quelle que soit la version, et jamais avec `MCP-Protocol-Version`. Un serveur 2026-07-28
  conforme (ClickUp) refuse le désaccord en 400 `-32020` : il se connectait et listait ses
  outils (sans `params.name`), mais aucun appel n'aboutissait. La suite de conformance,
  montée sur un transport en mémoire, ne produisait aucune requête HTTP, donc aucun
  en-tête. Désormais, les en-têtes d'une requête 2026 sont tirés de son corps
  (`MCP-Protocol-Version` du `_meta`, `Mcp-Method`, `Mcp-Name` = `params.name` pour
  `tools/call` et `prompts/get`, `params.uri` pour `resources/read`, encodé
  `=?base64?…?=` hors ASCII) ; une requête d'avant 2026 n'en porte aucun, mais porte à
  partir de 2025-06-18 la version négociée par `initialize`. Une erreur JSON-RPC dans
  une réponse 4xx garde son code (une version refusée à `initialize` en HTTP repart
  enfin sur la meilleure version annoncée). `mcp test` appelle un outil en lecture sans
  argument et échoue sur une faute de protocole, pas sur un refus de l'outil ; un
  `-32020` en conversation passe le serveur en `degraded` et dit au modèle de ne pas
  réessayer. Stdio, OAuth et négociation inchangés ; un serveur 2025 qui refuse la
  version 2026 de la sonde retombe toujours sur `initialize`. Reste ouvert :
  `x-mcp-header` (`Mcp-Param-*`).

### 0.17.14

Une nuit de consolidation ratée ne passe plus en silence, et un flux muet est repris
(#127).

- **La nuit ratée se dit, le lot muet se reprend** (#127) : la passe du 18 au 19
  septembre est morte après 1 h 09 sur « Upstream idle timeout exceeded », sans reprise
  ni message (comportement d'origine : `tokio::spawn` puis `tracing::warn!`). Le lot en
  erreur passagère (flux muet, 5xx, 429, 240 s sans réponse complète) est repris sur
  place après `memory.dream_retry_wait` puis le double : les lots déjà faits ne sont pas
  refaits, et rien n'est encore écrit à ce stade. Un candidat est marqué promu dès son
  entrée écrite (et non plus en fin de passe) : une passe arrêtée plus loin ne le
  repromeut pas. Une nuit ratée écrit un événement `memory.dream_failed` et une ligne
  datée dans `DREAMS.md`, prévient le propriétaire (à la première nuit, puis quand la
  raison change) et le digest le dit. La fenêtre de la passe suivante part toujours de
  la dernière passe réussie.

### 0.17.15

Un point de vision douteux n'est plus servi, le repère se déduit, et la documentation
embarquée enseigne la méthode (#128).

- **Repère déduit, points contrôlés, méthode enseignée** (#128) : l'alias `vision` sur
  UI-TARS rendait des millièmes pendant que `models.locate_frame` valait `pixels` ;
  chaque point servi était faux d'un facteur d'échelle (154 messages, 7,21 $). En cause
  aussi, la documentation de #125 (c3b3c1e), qui donnait UI-TARS pour un modèle à
  pixels. `models.locate_frame` vaut désormais `auto` : repère déduit de la famille
  (UI-TARS et Qwen3-VL en millièmes, Qwen2-VL et Qwen2.5-VL en pixels) et des valeurs
  (au-delà de 1000 : pas des millièmes ; au-delà de l'image sans dépasser 1000 : des
  millièmes). Un désaccord entre ces indices et un repère déclaré, ou un point hors de
  l'image, fait refuser les points (`refused`, avec le repère et la commande) au lieu de
  les servir ; `model_frame` dit comment la réponse a été lue. Une section « Travailler
  sur une interface » enseigne l'ordre (identifiant d'abord, coordonnées en dernier), le
  repère, la conversion pour un tap et la règle des deux taps ; la description de
  `image_inspect` y renvoie. `config set` accepte une entrée nouvelle dans les tables à
  clés libres (`models.roles.image_locate` sur une configuration d'avant #125). La
  confrontation à l'arbre d'accessibilité reste à l'agent : Pénélope ne le détient pas,
  la méthode le lui dit.

### 0.17.16

Une carte d'approbation part toujours, même quand la commande porte `{{…}}` (#129).

- **Une valeur n'est jamais relue comme une variable** (#129) : deux cartes n'ont jamais
  atteint Telegram, la commande contenant `{{.Name}}` d'un `docker compose ps --format`.
  Le moteur de gabarits cherchait les variables manquantes dans le texte déjà rempli, et
  substituait variable par variable (une valeur portant `{{autre}}` était remplie à son
  tour) ; #116 insère la commande telle quelle sur la carte, d'où la régression. Les
  variables se cherchent désormais dans le gabarit, et la substitution se fait en un
  seul passage (moteur des gabarits et rendu HTML du démon). Une vraie variable
  manquante reste refusée ; une carte d'approbation qui ne se rend pas part en texte
  brut, avec « Approuver » et « Refuser », et un événement `telegram.card_degraded`. Les
  rappels de #97 passent par la même carte. Une demande restée bloquée se renvoie par
  `/approvals`.

### 0.17.17

Un tour ne panique plus sur une commande en échec de 40 à 60 lignes, et une panique dit
où elle a eu lieu (#130). Une session qui ne se résume plus est compactée sans modèle au
troisième échec, et se voit (#131). Un identifiant d'artefact n'est plus pris pour un
numéro de carte (#132).

- **Le résumé d'une sortie courte ne découpe plus à l'envers** (#130) : un tour repris
  après « Toujours » est mort sur « slice index starts at 30 but ends at 11 ». La
  commande (un script Python par heredoc) n'y était pour rien : elle a échoué en écrivant
  41 lignes, et le résumé générique d'une commande en échec (#32, 6785191) prenait le
  milieu `lines[30..len - 30]`, à l'envers de 31 à 59 lignes. Sous 60 lignes, la sortie
  part désormais entière. `parse_classification` gagne la même garde (une `}` avant la
  première `{`). Une panique rattrapée porte maintenant son emplacement dans le code
  (crochet de panique), dans le message au propriétaire et l'événement
  `daemon.task_panicked`. Les formes de commande sans test (multi-lignes, heredoc,
  guillemet non fermé, un mot, vide, préfixe `cd`) traversent classement, motif et
  carte ; une commande multi-lignes reste sans motif, donc sans règle (#67, #111). Le
  tour n'est pas remis en file tout seul : son appel a pu s'exécuter, le relancer le
  rejouerait ; le propriétaire reçoit la raison avec « Réessayer ».

- **Le résumeur qui ne répond plus** (#131) : la session « Fidelatoo » (548 messages)
  a vu trois résumés expirer à 180 s, et grossissait sans que personne le sache. Pas une
  régression : délai fixe et modèle lent. Le délai se proportionne au lot (120 s plus
  une seconde par millier de tokens, 420 s au plus). Un échec passager est relancé une
  fois sur un lot trois fois plus court ; puis l'alias de repli déclaré pour le résumeur
  (`models.routing.fallback`), une fois, dans la réserve `budget.compaction_reserve_usd`
  (un modèle au prix inconnu n'est pas essayé). Au troisième échec de suite, compaction
  sans modèle : un nœud franc (derniers messages du propriétaire et ancres gardés, le
  reste relisible), un message au propriétaire avec le coût par tour, un événement
  `context.compaction_mechanical`. `/status` et le digest disent la session en échec.
  Seuils (#18, #40), validation du résumé et niveau 4 d'urgence inchangés ; pendant les
  échecs, rien n'est publié et le préfixe ne bouge pas.

- **Un identifiant n'est pas une carte** (#132) : une note citant
  `command-output:38228-1743576040856618` était refusée pour « numéro de carte » (seize
  chiffres après un tiret, Luhn vrai une fois sur dix), et l'agent tronquait l'identifiant
  à l'aveugle. Pas une régression : motif d'origine. Le filtre d'écriture (mémoire,
  notes, titres, contrôle du vault) écarte un nombre collé à un identifiant (`:`, `-`,
  `_`, `/`, `=`, `#`, `@` ou une lettre), sauf si le mot collé nomme une carte ; longueurs
  et Luhn inchangés. Le refus cite le fragment masqué (« …6467 », « sk-0… »). Le
  masquage des journaux et des événements (#26) ne change pas : il masque toujours tout
  nombre qui passe les deux tests. Les cartes ne se rangent toujours pas, elles se
  refusent (#37).

### 0.17.18

Une veille planifiée n'arrive plus deux fois (#133), et une clé recopiée par l'agent ne
part plus en clair dans une carte d'approbation (#134).

- **La réponse finale est le livrable, une fois** (#133) : la veille du 19 est arrivée
  deux fois, la skill l'envoyant par `send_message` et le tour livrant en plus sa réponse
  finale (chemin d'origine, #39) ; #120 comptait l'un ou l'autre sans trancher. Pour un
  tour planifié, la réponse finale part toujours, sauf si l'agent a déjà envoyé le même
  contenu pendant le tour (même texte à la mise en forme près, l'un dans l'autre, ou le
  même corps sous un autre en-tête) : événement `schedule.final_not_repeated`. Le
  livrable est évalué avant, sur ce qui est parti ; un message intermédiaire différent
  s'ajoute ; un `send_message` en échec ne retient rien. `send_message` vise l'origine du
  tour, donc la même cible après `/schedules ici` (#124). La skill `veille-agents-ia` vit
  sur l'instance, pas dans ce dépôt : sa consigne a été réécrite là-bas.

- **Une clé recopiée reste masquée là où elle est stockée** (#134) : une clé lue dans
  `apollo.config.js` et un mot de passe, recopiés dans un script Python, sont partis en
  clair dans une carte d'approbation (#116 affiche la commande exacte) et dans la
  demande stockée. La demande passait pourtant par la rédaction : c'est la détection qui
  ne reconnaissait ni `KEY = "…"`, ni `"password": "…"`, ni une clé sans préfixe connu.
  Elle reconnaît désormais les affectations entre guillemets (`'x-api-key': '…'`,
  `"password": "…"`, `key = …`), les jetons longs et aléatoires (trois familles de
  caractères, entropie élevée, hors chemins, URL et identifiants lisibles), et retient
  comme valeur connue ce qu'elle repère dans un résultat d'outil : recopiée ensuite, la
  valeur est masquée. La file Telegram est rédigée à l'écriture ; la carte dit combien
  de valeurs sont masquées ; `doctor` gagne `stored_secrets` (demandes et file des 30
  derniers jours). Rien de ce qui s'exécute n'est modifié (commande, `fs_write`). La
  consigne de `shell_exec` demande de ne pas recopier un secret lu. Ce qui a déjà fui
  (message Telegram, sauvegardes) est à renouveler.

### 0.17.19

La consolidation ne jette plus quatre appels sur cinq, et se voit pendant qu'elle tourne
(#135).

- **Des lots qui retiennent ce qui a tenu** (#135) : un essai à blanc de deux heures a
  fait 134 appels (53 000 tokens d'entrée chacun) sans un signe. Après une descente
  40 → 20 → 10 → 5 sur sortie coupée (#59), chaque lot repartait à 40 ; et la sortie
  demandée (120 tokens par candidat) était sous ce que la grille de #37 fait écrire.
  Désormais la taille reste sous la plus petite taille coupée de la passe et remonte à
  mi-chemin après un lot qui tient (un modèle qui coupe au-delà de 10 juge 188 groupes en
  au plus 25 appels) ; la sortie est estimée d'après les tokens réellement écrits par
  candidat (350 au départ), dans la limite du modèle, et un lot trop grand est réduit
  avant l'appel. Un événement `memory.dream_batch` par lot, une ligne de journal, et le
  rapport comme le digest disent appels, appels jetés et durée. Une passe arrêtée avant
  ses verdicts ne consomme aucun report (vérifié) : les 88 candidats déjà reportés ne
  risquent rien d'une interruption.

### 0.17.20

Le budget de tokens d'un run compte ce qui est facturé, se relève pour un run seul, et
se dit (#136). `build-verify` atteint `verify` et vérifie le projet, pas Pénélope (#137).

- **Tokens facturés, plafond relevable** (#136) : un run `build-verify` s'est bloqué
  « budget de tokens atteint » à 2,23 M tokens pour 0,48 $ sur 5 $, travail fait ; son
  `Resume` re-bloquait dans la seconde, et relever le plafond de la session n'y pouvait
  rien. Le comptage d'origine (`prompt + completion`) prenait plein pot le préfixe servi
  par le cache. `maxTokens` compte désormais l'entrée hors cache plus la sortie. `wf
  control <run> budget --usd … --tokens …` relève les plafonds du run seul, avec trace
  (`workflow.budget_raised`). La borne atteinte se dit avec ses chiffres et la commande ;
  `resume` sur un run encore au-dessus répond « toujours bloqué » sans changer son état.
  Le plafond en dollars reste la référence ; les workflows livrés gardent 2 M tokens.

- **Le contrat des critères, et les tests du projet** (#137) : le run du constat n'a
  jamais atteint `verify`. Les critères du plan n'avaient pas de `status`, l'agent
  « cochait » par une entrée `{"status": "all_passed"}` hors vocabulaire (un `update`
  sans `id` ne faisait rien, en silence), et `verify` lançait `cargo test` en dur sur un
  dépôt Go. `session_metadata` vérifie désormais le contrat pour `criteria` (`id` unique,
  `text` ou `label`, `status` parmi pending, completed, passed, failed, `pending` par
  défaut, cocher par `update` sur un `id` connu) et refuse le reste avec ce qu'il faut ;
  sa description et les consignes de `build-verify` et `ticket-to-deploy` disent le geste
  exact. `session_metadata` passe sans approbation, comme `session_notes` (sinon chaque
  critère coché demandait une carte). Le plan de `build-verify` déclare le projet
  (`project.dir`, `project.test_command`) ; son `verify` lance ce contrôle
  `project_tests`, sinon la commande déduite du dépôt. Une boucle qui recommence dit
  quels critères la retiennent (`workflow.step`, journal). Les gabarits lisent
  `{{metadata.…}}`, et `criteriaList` montre `label` comme `text`. `review` reste Rust.

### 0.17.21

Une valeur seule remplit un champ liste, dans `mcp edit` comme dans `config set` (#138).

- **Une valeur seule vaut une liste d'un élément** (#138) : `penelope mcp edit mailbridge
  roots "/chemin"` répondait par l'erreur brute du désérialiseur, et seule la forme
  `'["/chemin"]'` passait, sans que rien ne le dise. Pour un champ liste (`args`,
  `scopes`, `roots`, et toute liste de la configuration), une chaîne devient une liste
  d'un élément, jamais découpée sur les espaces ; une liste passe telle quelle ; un autre
  type est refusé en nommant la forme attendue (« `roots` attend une liste : `["a",
  "b"]`, ou une valeur seule »). Même règle pour `config set` (#122) ; ce que `roots`
  autorise ne change pas.

### 0.17.22

Un tour coupé par son plafond d'appels propose de continuer, pas de réessayer (#139).

- **« ▶️ Continuer (24 appels de plus) »** (#139) : un tour arrêté par le plafond de 24
  appels au modèle arrivait avec « ❌ le tour n'a pas convergé… » et « 🔁 Réessayer »,
  qui laissait croire que tout serait refait. Le message dit maintenant que les 24 appels
  sont utilisés, que « Continuer » en redonne 24 en reprenant ce qui est fait (dernier
  résultat compris), donne le coût du tour coupé et rappelle la délégation à un
  sous-agent. Seuls le libellé et le texte changent : même bouton, même `enqueue_retry`,
  même tour de reprise. Une vraie erreur (panne du fournisseur, réponse vide) garde
  « ❌ » et « Réessayer ». La passerelle reconnaît le plafond au préfixe
  `CALLS_EXHAUSTED` des deux messages de l'agent : un changement de ce texte côté agent
  doit passer par la constante, sinon le bouton redevient « Réessayer » (test d'agent qui
  vérifie le préfixe).

### 0.17.23

Consolidation : une tête de lot bavarde ne réduit plus toute la passe à des lots d'un
candidat (#140, suite de #135).

- **Les coupures enseignent l'estimation de sortie** (#140) : `OutputBudget::observe`
  n'apprenait que des lots qui tenaient, et les lots faciles tiraient l'estimation à ~191
  tokens par candidat ; chaque lot rejoué recevait une demande juste sous ce que le
  modèle écrivait (9 932 pour 9 958, 4 966 pour 4 979, 2 483 pour 2 489). Une coupure
  prouve maintenant une borne basse : si l'estimation par candidat a fixé la demande,
  elle ne redescend plus sous `tokens / candidats` ; si c'est le plancher (2 000, ou un
  lot d'un candidat), il double, dans la limite du modèle. Un lot d'un candidat qui tient
  relève le plancher à ce qu'il a écrit, marge comprise.
- **Un candidat seul coupé est repris une fois, sortie doublée** (#140) : il était gardé
  tronqué dès la première coupure. La réponse tronquée n'est gardée qu'après la reprise,
  avec un avertissement, et l'appel est compté jeté.
- **La taille des lots accuse la bonne cause** (#140) : la coupure à 2 posait le plafond à
  2, et `BatchSizer::ok` rendait `min(c − 1)` = 1 pour toute la passe (121 lots d'un
  candidat). Un rejeu qui ne tient qu'à un candidat accuse ce candidat : seule la
  première coupure compte. Deux fois de suite, la plus petite coupure au-dessus de 2
  compte. Une coupure à 2 ou moins n'accuse jamais la taille. La remontée se fait à
  mi-chemin entre la taille accusée et la plus grande taille qui a tenu depuis, jamais
  au-dessus : pas de retour à `dream_batch` (#135).
- **Une passe de lots d'un candidat s'arrête** (#140) : au-delà de huit lots, si plus de
  la moitié n'ont tenu qu'à un candidat, la passe s'arrête avec un avertissement ; les
  candidats non jugés restent en attente sans consommer de report (#59). Les reprises
  d'erreur passagère (#127) comptent en appels, pas en lots.
- Rejeu de la passe du 19 septembre avec un modèle simulé (250 tokens par candidat
  facile, 2 400 pour un épineux) : 53 appels avant, moins de 20 maintenant, chaque
  candidat jugé. Le mock de fournisseur sait rendre une sortie mesurée et coupée
  (`Scripted::Written`).

### 0.17.24

Un `&` entre guillemets n'est plus un enchaînement : les règles « Toujours » et les
lectures se décident sur un seul découpage de ligne (#141).

- **Un seul lexer de ligne de commande** (#141) : `arg_pattern`, `command_matches`,
  `declared_allow`, `rule_note` et `is_read_command` cherchaient chacun `; & | ( ) $ …`
  dans le **texte brut**, guillemets compris. Une URL de requête (`glab api --hostname h
  "projects?membership=true&per_page=100"`) passait donc pour une commande composée :
  neuf cartes en neuf minutes, huit « Toujours » cliqués, zéro règle créée, zéro règle
  appliquée. `penelope_hitl::cmdline` découpe désormais la ligne une fois (mots,
  apostrophes littérales, guillemets doubles littéraux sauf `$`, `` ` `` et `\`) et les
  cinq appelants s'y branchent : ce qu'un « Toujours » écrit est ce qui s'applique
  ensuite. Le lexer rend des tokens, jamais des offsets (#130).
- **Reste composé** (#67 ne se rouvre pas) : un opérateur ou une redirection hors
  guillemets, une substitution (`$`, `` ` ``) ou un échappement même entre guillemets
  doubles, un saut de ligne, une négation `!` en tête, des guillemets non fermés.
  `cargo test; rm -rf ~`, `ls | sh`, `cat x > y`, `echo "$(rm -rf ~)"` redemandent comme
  avant.
- **Affectations d'environnement en tête** (#141) : `GITLAB_HOST=h glab api "…"` a pour
  famille `glab` et `TZ=UTC date` est une lecture ; une variable qui détourne
  l'interpréteur (`PATH`, `HOME`, `IFS`, `ENV`, `BASH_ENV`, `DYLD_*`, `LD_*`,
  `GIT_CONFIG*`, `GIT_SSH_COMMAND`, `NODE_OPTIONS`, `PYTHONSTARTUP`, `PERL5OPT`,
  `RUBYOPT`…) laisse la ligne composée : `PATH=/tmp ls` n'est jamais couvert par une
  règle `ls`.
- **La carte dit quand « Toujours » ne réglera rien** (#141) : sur une commande sans
  famille, le bouton devient « ✅ Autoriser (pas de règle possible) » et la ligne de
  qualificatifs porte « aucune règle possible : commande composée ». Le propriétaire ne
  clique plus dans le vide.
- Les règles déjà en base ne bougent pas : celles tirées d'une affectation
  (`GITLAB_HOST=…`) restent signalées inutiles par `rule_note`, à retirer d'un bouton
  dans `/policies`. Une famille créée est désormais vérifiée à l'écriture : si elle ne se
  relit pas comme elle a été écrite, aucune règle n'est créée (régression de #111).
- **Un tube vers une lecture pure n'est plus un enchaînement** (#141, second constat du
  20/09 : neuf cartes en cinq minutes, huit « Toujours » cliqués, zéro règle, sur des
  `glab api … | jq -r '…'`). Ce qui agit est la première étape ; les suivantes ne peuvent
  que lire. La famille est celle de la tête (une règle `glab` couvre `glab api …` comme
  `glab api … | jq …`), et une lecture qui traverse un tel tube reste une lecture.
  Acceptées : `jq` (sans `-f`, `--rawfile`, `--slurpfile`), `cat` sans fichier, `grep`,
  `egrep`, `fgrep`, `rg` sans `--pre`, `head`, `tail`, `cut`, `sort` sans `-o`, `wc`,
  `uniq`, `tr`, `nl`, `rev`, `column`. `| sh`, `| xargs`, `| tee`, `| python`, `| sed`,
  `||`, une redirection ou une substitution restent composés.
- Onze tests nouveaux : le lexer forme par forme, le tube dans les deux sens, la famille
  créée puis appliquée à la commande suivante (le même découpage écrit la règle et la
  reconnaît, régression de #111), la famille déclarée avec réseau, les lectures entre
  guillemets, et la carte qui annonce l'absence de règle.
- Reste ouvert, hors de ce lot : `gh api`/`glab api` en GET, `glab repo|mr list|view`,
  `gh pr list|view`, `git ls-remote` et `git fetch` en classe lecture **avec** réseau
  (commentaire de #111), et une règle sans réseau qui couvrirait un appel `network: true`
  quand `sandbox.shell_network` est déjà ouvert.

### 0.17.25

Fournisseur `codex` : les modèles d'un abonnement ChatGPT, à côté d'OpenRouter (#142).

- **Un troisième fournisseur** (#142) : `ProviderSet` a un emplacement `codex`, et un
  modèle `codex:` n'est **jamais** servi par un autre — avant, un préfixe inconnu partait
  en silence chez OpenRouter, identifiant complet en nom de modèle. La liste des préfixes
  vit désormais dans `penelope-kernel` (`PROVIDER_PREFIXES`), lue par le découpage
  (`provider_of`, `strip_provider`) **et** par la validation de configuration : un alias
  `codx:gpt-6` est refusé en nommant le préfixe, là où `x-ai/grok-4:free` reste une
  variante OpenRouter.
- **Dialecte Responses** (#142) : `CodexProvider` parle l'API Responses en flux — items
  typés en entrée (`message`, `function_call`, `function_call_output`, `reasoning`),
  outils à plat, `store: false`, `include: ["reasoning.encrypted_content"]`,
  `prompt_cache_key` = session. Le backend est sans état : le raisonnement chiffré est
  réinjecté au tour suivant, sinon il est perdu sans erreur visible. Les événements
  `response.*` rendent les mêmes `StreamChunk` que `chat/completions` (accumulateur
  distinct, transport et `SseDecoder` communs, issues #51 et #80 inchangées) ; un appel
  d'outil vient de `response.output_item.done` avec le `call_id` du serveur (#54), et une
  fermeture sans `response.completed` est une coupure, pas une fin.
- **Connexion par code d'appareil** (#142) : `penelope model auth codex` (et `/model auth
  codex`) affiche une adresse et un code, attend la validation, range les jetons dans le
  magasin de secrets sous `codex.oauth` — jamais `~/.codex/auth.json`. Le rafraîchissement
  est sérialisé et la rotation écrite avant tout usage : le `refresh_token` est à usage
  unique, deux rafraîchissements concurrents déconnecteraient le compte pour de bon. Un
  échec permanent (`refresh_token_reused`, `refresh_token_expired`, 401) marque la
  connexion morte, prévient le propriétaire une fois et laisse le repli OpenRouter jouer.
  Les trois jetons sont masqués dans les journaux à l'obtention **et** à chaque rotation
  (#26).
- **Identité assumée** (#142) : `originator`, `User-Agent` et `x-codex-installation-id`
  sont identiques sur `/responses` et sur `/models` — une identité incohérente vaut des
  heures de « servers overloaded » chez un client tiers. Tout est en configuration
  (`providers.codex.originator`, `client_version`) : une liste blanche qui change se
  rattrape sans recompiler, et un 403 nomme la cause probable.
- **Erreurs et quota** (#142) : 429 `usage_limit_reached` → `RateLimited` avec l'heure de
  retour, jamais rejoué ; `usage_not_included` → `PaymentRequired` ;
  `context_length_exceeded` → `ContextLength` ; 401 → un rafraîchissement et **un** rejeu.
  Les jauges `x-codex-*` et l'événement `codex.rate_limits` sont lus à chaque réponse, et
  le fournisseur se met en retrait au-delà de `quota_stop_ratio` plutôt que d'aller
  chercher un refus.
- Livré derrière `providers.codex.enabled = false` : sans compte connecté, rien ne change.
  Le catalogue `/models` du plan est rafraîchi comme celui d'OpenRouter (`upsert`, jamais
  `replace`), avec repli sur la liste embarquée.
- **L'abonnement ne sert que les tours du propriétaire** (#142, décision 2) : une garde
  unique, posée juste avant le choix du fournisseur. Un message Telegram ou CLI et les
  sous-agents de ce tour passent par `codex:` ; planification à cible `prompt`, rêve,
  veille, compaction, relecture d'épisode, consolidation, classifieur, embeddings,
  transcription, synthèse vocale, titre automatique et runs de workflow se replient sur le
  modèle OpenRouter de l'alias, sans carte ni bruit, avec l'événement
  `llm.codex_scope_fallback`. Le sous-agent hérite du périmètre de son tour
  (`spawn_sub_agent` reçoit désormais l'origine). `penelope model set` refuse un alias de
  rôle de fond (`classifier`, `compaction`, `memory_review`, `embedding`, `stt`, `tts`) en
  disant pourquoi, et un préfixe mal écrit (`codx:`) est refusé au lieu de partir chez
  OpenRouter. Un seul compte à la fois : une seconde connexion demande d'abord
  `--logout`.
- **Le quota du plan remplace le budget en dollars** (#142, décision 3) : un appel par
  abonnement coûte 0 $, et les lignes d'usage le disent (`provider = codex`,
  `cost_usd = 0`, `estimated = false` — le coût est **connu**). Les jauges `x-codex-*` et
  l'événement `codex.rate_limits` sont rangés en `kv` à chaque réponse, affichés dans
  `/budget`, `penelope model list` et `self_status` (`primary 42 % · retour 18:05`). Une
  alerte par fenêtre à `quota_alert_ratio`, un retrait à `quota_stop_ratio` : le
  fournisseur répond `RateLimited` avant l'appel, le routeur se replie, et le message
  distingue un quota d'une panne (#139). Les plafonds jour, session et run ne comptent
  rien pour ce fournisseur, et la documentation le dit.
- **`doctor` dit tout** (#142, lot 3) : `provider.codex` (connecté, plan, compte,
  fraîcheur du jeton, dernier rafraîchissement), `provider.codex.identity` — un
  avertissement permanent sur l'identité empruntée, toléré mais jamais garanti —,
  `provider.codex.scope` (un alias de rôle de fond qui l'aurait contournée),
  `provider.codex.quota`, le secret `codex.oauth` dans la boucle des secrets attendus, et
  `chatgpt.com` et `auth.openai.com` dans les hôtes joignables quand le fournisseur est
  actif.
- ADR [0010](decisions/0010-fournisseur-codex-oauth.md) : les trois décisions, le statut
  « toléré, jamais garanti », et la sortie écrite d'avance (clé d'API sur `openai_compat`).
  Section « Codex » de `install-headless.md`, ligne de comparaison au README.

### 0.17.26

Les confirmations MCP reviennent là où la conversation vit, et les avis sans session ont
un foyer (#143).

- **La carte d'élicitation suit la session** (#143) : elle partait dans le chat privé du
  propriétaire, en appel direct. Depuis que la conversation vit dans un groupe à sujets,
  personne ne la voyait : six demandes Redmine les 19 et 20/09, cinq annulées au bout de
  dix minutes, « write aborted », rien d'écrit. La conversation de l'appel d'outil est
  désormais posée le temps de l'appel (`Broker::scope`, un garde qui tombe avec l'appel),
  et la carte y revient — chat et sujet —, par la file `tg_outbox` comme les cartes
  d'approbation (#101). Sans session, elle va au foyer.
- **Un rappel, puis une relance** (#143) : à mi-délai, une ligne au même endroit
  (« la confirmation attend toujours, 5 min restantes ») ; à l'annulation, le message dit
  que rien n'a été écrit et porte un bouton « 🔄 Relancer » qui remet la demande dans la
  session. Trois demandes identiques cliquées dans le vide coûtaient trois fois dix
  minutes.
- **Le foyer du propriétaire** (#143) : `telegram.home` (`{chat, topic}`) et un seul point
  de résolution, `home_chat()`, remplacent les sept retombées sur le chat privé — alertes
  de budget, rappels d'approbation, digest, veille, cartes OAuth MCP, élicitations sans
  session. `/home` dans un sujet l'y règle, `/home off` revient au privé ; une session
  garde toujours son propre chat et son propre sujet. `penelope doctor` réclame un foyer
  dès que `telegram.allowed_chats` n'est pas vide, et dit lequel est réglé.
- La carte n'a plus d'identifiant de message à l'envoi (la file n'en rend pas) : elle est
  modifiée en place au clic, et l'issue arrive en message dans la même conversation quand
  personne n'a cliqué. `Orchestrator`, `McpGateway::call_tool` et `OwnerChannel::close`
  prennent un paramètre de plus (la conversation, la possibilité de relancer).

### 0.17.27

Le digest du matin tient en une bulle, une contradiction se tranche au bouton, et une
entrée de mémoire porte un fait (#145).

- **Un digest court** (#145) : celui du 20/09 faisait 11 300 caractères en six messages,
  dont la moitié en `[[memoire#^01M2…]]` bruts, avec cinq lignes de journal interne. Le
  digest dit maintenant ce que la nuit a appris (le compte, cinq exemples tronqués à 80
  caractères avec leur fichier), combien de questions attendent, le seul avertissement
  qui demande une action (le Cœur au-delà de `memory.core_budget_tokens`), le nettoyage
  proposé et où lire la suite. Les références promues, les relances de lot, le lint, le
  journal et les secrets rangés restent dans `DREAMS.md` et le journal du vault. Au-delà
  de `telegram.max_fragments`, le texte part en document (`should_send_as_document`,
  jusque-là appelé nulle part).
- **Une question, une carte, trois boutons** (#145) : « je remplace, j'ajoute une
  exception, ou j'ignore ? » n'avait aucune réponse cliquable. Chaque contradiction
  devient une carte `memory_proposal` à part du digest, citant les deux entrées tronquées
  (80 caractères) ; le digest n'en donne que le compte. « Remplacer » retire l'ancienne,
  « Exception » écrit la nouvelle avec son contexte, « Ignorer » écarte le candidat. Sans
  réponse, la carte n'est pas reposée le lendemain : le candidat reste à l'état
  `question` — listé par `penelope mem candidates` — et la question est rangée dans
  `DREAMS.md` sous « Questions sans réponse ».
- **Une contradiction, c'est deux règles opposées sur le même sujet** (#145) : la polarité
  se lisait par sous-chaîne n'importe où dans le texte (un dossier de 3 188 caractères
  contenant « toujours payé » contredisait une phrase sur un JWT), et le sujet commun par
  containment sur le plus petit énoncé, seuil 0,25. Elle se lit désormais en tête de la
  première phrase, sur six mots ; le sujet commun se mesure en Jaccard (union) à 0,4 **et**
  par la similarité d'embedding du voisin (0,80) ; deux énoncés au-delà de la borne d'une
  entrée, ou dont les longueurs sont dans un rapport de plus de trois, ne se comparent
  pas ; un `fait` et un `écart` ne contredisent rien, ils se datent.
- **Une entrée, un fait** (#145) : `mem_remember` refuse au-delà de
  `penelope_memory::quality::MAX_ENTRY_CHARS` (300) avec la consigne de découper, et le
  schéma de l'outil porte la borne ; `mem_note` reste sans borne. Pour les entrées déjà
  écrites, `penelope mem split <uid>` propose un découpage en faits courts, sans les
  données financières personnelles : une carte, jamais une écriture. `penelope doctor`
  liste les entrées actives au-delà de la borne et le Cœur au-delà de son budget.

### 0.17.30

Trois chantiers dans la même release : l'import de skills tierces, la fuite de jetons du
Trousseau, et le test qui bloquait la livraison (#146, #148, #151). Les versions 0.17.28
et 0.17.29 n'ont jamais été publiées — la première faute de tag, la seconde parce que sa
vérification de release est tombée sur le test de #151 ; leurs notes sont ici.

#### Skills tierces (#146)

Les skills d'un dépôt tiers s'installent en une commande, avec leurs fichiers, leur
vocabulaire et leurs dépendances nommées (#146).

- **`penelope skill install <depot>[:skill,…]`** (#146) : le portage manuel de six skills
  officielles Anthropic, le 20/09, a demandé un script ad hoc. La commande télécharge
  l'archive du dépôt en HTTPS (aucun `git`, aucun sous-processus) et copie **le dossier
  entier** de chaque skill dans `{data}/skills/` — un documentaire embarque ses scripts et
  ses schémas de validation, inertes sans leurs fichiers. `owner/repo`, `owner/repo@revision`,
  `:skill,skill` pour choisir, `--force` pour remplacer. Une archive qui remonte hors de son
  dossier, qui porte un lien symbolique, qui dépasse les bornes de taille, ou dont le
  `SKILL.md` ne se charge pas, est refusée **avant la moindre écriture**.
- **Le frontmatter complété, le reste mot pour mot** (#146) : `version: 1.0.0` s'il manque,
  et `allowed_tools` déduit du corps. Sans cette clé une skill a droit à tous les outils :
  pour une skill venue d'ailleurs, ce défaut est trop large. Le corps n'est pas touché, une
  mise à jour du dépôt reste lisible en diff.
- **Le vocabulaire traduit au chargement** (#146) : un corps écrit pour Claude Code parle de
  `Read`, `Bash`, `Grep`. `skill_load` pose devant lui le dossier absolu de la skill et la
  table de correspondance (`Read` → `fs_read`, `Bash` → `shell_exec`, `Glob`/`Grep` →
  `fs_search`, `WebFetch` → `http_fetch`…), sans rien réécrire dans le fichier : un bloc de
  portage collé à la main disparaissait à la mise à jour suivante.
- **Les dépendances nommées, jamais installées** (#146) : `requires: [pip:openpyxl,
  npm:docx, bin:pandoc]` dans le frontmatter. L'installation liste ce qui manque avec la
  commande qui le pose, et `penelope doctor` refait le contrôle (`skills.requirements`) :
  `bin:` par le `PATH`, `pip:` par un `import`, `npm:` par un `require.resolve` avec
  `NODE_PATH` réglé sur `npm root -g`. Un nom de paquet qui pourrait s'échapper dans une de
  ces lignes est refusé, pas échappé.
- Deux réglages de fond au passage : le répartiteur RPC met son futur sur le tas (il porte
  l'état de toutes les méthodes servies, et sa pile débordait au test dès qu'une branche
  s'ajoutait), et la dorsale de déflation de `zip` est nommée là où elle doit l'être.
  `zip` active `flate2` **sans moteur** : jusqu'ici la déflation ne compilait que par
  l'unification des features avec `lopdf`, et `cargo build -p penelope-skills` échouait
  seul. Chaque crate qui dépend de `zip` déclare donc aussi `flate2` en `rust_backend`
  (`miniz_oxide`, en Rust, MIT) ; les dorsales que `zip` propose tireraient `zlib-rs`, sous
  licence Zlib, que `cargo deny` refuse.

#### Trousseau et fuite de jetons (#148)

Un secret de plusieurs kilo-octets entre dans le Trousseau, et un échec d'écriture ne
recopie plus ce qu'il refusait (#148).

- **La fuite** (#148) : `security -i` n'accepte que 4 096 octets par ligne. Le `Grant`
  Codex (#142), ≈ 4 Ko, soit ≈ 8 Ko en hexadécimal, dépassait la limite ; `security`
  relisait la fin de la ligne comme des commandes et en recopiait des morceaux dans sa
  sortie d'erreur, que Pénélope envoyait au propriétaire : cinq messages Telegram, les
  trois jetons en clair, rien de stocké donc rien à révoquer.
- **Stockage en morceaux** : au-delà de la borne, la valeur est écrite en plusieurs items
  (`penelope.<nom>#0`, `#1`, …) derrière un item de tête qui dit combien ; `get`
  réassemble, `delete` efface tout, `list` ne montre que le nom logique, et un secret court
  garde la forme d'avant (lecture compatible, rien à migrer). Une réécriture plus courte ne
  laisse pas de morceau derrière elle.
- **Plus jamais la sortie de `security`** : le message d'échec ne porte que le code de
  retour et la marche à suivre. Un test avec un faux `security` qui recopie son entrée
  vérifie qu'aucun fragment de la valeur, ni de son hexadécimal, n'atteint le message.
- **Le rédacteur voit l'hexadécimal** : une suite hexadécimale de plus de 64 caractères est
  masquée, quelle que soit sa classe de caractères (les règles d'entropie en exigeaient
  trois, l'hexadécimal n'en a que deux). Une empreinte SHA-256, 64 caractères exactement,
  reste lisible. `doctor stored_secrets` en hérite, et le `Grant` est enregistré comme
  secret connu **sous ses deux formes** avant la première tentative d'écriture.
- **Rien d'ouvert derrière** : si le rangement échoue après une connexion réussie, les
  jetons sont révoqués côté OpenAI et le message le dit. Un message d'échec de plus de 500
  caractères est tronqué avant l'envoi, avec renvoi au journal.
- **Le passage d'entretien repasse le rédacteur** sur les lignes déjà en file, une fois :
  les cinq lignes du 20/09 n'attendent pas les quatre-vingt-dix jours de rétention.
- `penelope doctor` écrit, relit et efface un secret de 8 Ko (`secret_roundtrip`), et
  `providers.codex.client_version` passe à `0.149.0` (le catalogue en dépend).

#### Livraison débloquée (#151)

La vérification de release de la `v0.17.29` échouait sur un seul test, alors que la CI de
`main` était verte sur le même commit.

- **Le test mesurait un état de processus** : `purge::messages_already_queued_are_redacted_again`
  (ajouté par #148) comptait les lignes réécrites par `reredact_outbox`. Le rédacteur de
  secrets est global au binaire de test ; `a_real_stdio_server_runs_under_the_sandbox`,
  réservé à macOS, lui apprend des valeurs en lançant un vrai serveur MCP, et une ligne
  ordinaire se retrouvait réécrite elle aussi : 2 au lieu de 1. Le test vérifie désormais
  **ses** deux lignes, et ses attentes passent par `redact` elles aussi — quoi que le
  rédacteur ait appris, une ligne en file vaut exactement ce qu'il en fait. Vérifié en
  simulant la pollution : l'ancienne forme échoue, la nouvelle passe.
- **La CI de `main` lance la suite complète sur macOS**, comme la vérification de release.
  Elle ne lançait que deux tests filtrés : une release pouvait donc découvrir rouge ce que
  `main` avait dit vert, et les tests réservés à macOS ne tournaient jamais dans le même
  processus que les autres — précisément la condition qui fait apparaître ce genre de
  couplage.

#### Livraison automatique (#147)

Le tag et la release ne se posaient pas tout seuls : trois lots fermés le 20/09 sont
restés plus d'une heure en `0.17.23`, puis deux sessions ont livré en parallèle.

- **La CI pose le tag** : job `livraison` de `ci.yml`, après les trois suites, sur `main`
  seulement, `concurrency: livraison` (un seul à la fois, dans l'ordre des pushes). Il lit
  la version du workspace ; si `v<version>` n'existe pas, il la pose et appelle
  `release.yml` par `workflow_dispatch` — un tag posé par `GITHUB_TOKEN` ne déclenche pas
  `on: push: tags`. Un push qui ne change pas la version ne fait rien, un tag présent n'est
  jamais déplacé.
- **Le bump sans friction** : `make bump V=0.17.31` (ou `scripts/bump.sh`) vérifie que
  `docs/progress.md` a la section, réécrit les seize lignes de `Cargo.toml`, met le
  `Cargo.lock` à jour et commite « Version 0.17.31 ». Refuse sur un arbre sale, sur une
  version déjà posée, ou si la section manque.
- **Le test `docs` vérifie les deux sens** : la section la plus haute de `progress.md` doit
  être la version du workspace. Une section écrite sans bump — ce qui est arrivé trois fois
  le 20/09 — fait maintenant échouer la CI, avec la commande à passer.
- **La règle est dans le dépôt** : `CLAUDE.md` à la racine (un lot = code + section +
  version ; le tag est à la CI ; un conflit sur `Cargo.toml` au rebase est la serrure entre
  deux sessions), et une case de plus dans le gabarit de pull request.
- Pas fait, et assumé : le contrôle `doctor` « tag sans release depuis 30 min ». Il
  demanderait au daemon d'interroger la liste des tags GitHub, pour une panne dont la cause
  — un tag posé sans release — disparaît avec ce lot ; l'état se lit dans l'onglet Actions.

#### Formulaires Telegram par sujet (#149)

La carte de confirmation MCP arrivait dans le bon sujet depuis #143 ; ce qui la suivait,
non. Le 20/09, « Accepter » sur une carte Redmine du sujet 3 a fait partir l'erreur de
validation dans Général, et « re test », tapé dans un autre sujet, a été pris pour la
réponse au formulaire — avalé, jamais arrivé à sa session.

- **La clé porte le sujet** : `tg.form.{chat}.{sujet}` (le chat seul en privé). Une clé
  d'avant est reprise une dernière fois puis réécrite par sujet, rien à migrer à la main.
- **Toutes les phrases du flux** — invite de champ, erreur de validation, récapitulatif,
  issue, « aucun formulaire en cours » — partent dans le sujet du formulaire, que la
  charge transporte. Onze envois sans sujet corrigés, sur les trois chemins (élicitation
  MCP, paramètres de workflow, écran de prompt MCP).
- **La capture du texte entrant est bornée au sujet** du formulaire : un message tapé
  ailleurs va à sa session. Deux formulaires peuvent vivre en parallèle dans deux sujets.
- L'erreur de validation dit maintenant où répondre (« réponds **ici**, ou ✖️ Abandonner »).
- `penelope doctor` (`telegram_forms`) signale un formulaire ouvert depuis plus d'une heure,
  avec son sujet : il retient le texte tapé là.

### 0.17.31

#### Les listes `&&` ont des familles (#150)

Le 20/09, le propriétaire colle deux liens YouTube dans « Mindset ». La skill de
transcription lance trois commandes sur une ligne ; la carte propose
`✅ Autoriser (pas de règle possible)`, qu'il lit comme le nouveau « Toujours » ; la
deuxième vidéo redemande exactement la même chose. Le ledger note `Toujours`, la table
des règles ne reçoit rien.

Ce n'était pas un cas isolé : sur l'instance, 383 des 549 appels `shell_exec` portaient
un `&&` (70 %), 129 des 166 cartes portaient sur une ligne collée, et 107 clics
« Toujours » avaient produit 27 règles, dont 20 mortes (`cd`, `echo`, `export`, `set`).

- **Le découpage sait rendre une liste** : `a && b && c` où chaque étape est nommable est
  une liste d'étapes, pas une ligne composée. `&&` est le seul opérateur admis — `;` et
  `||` lancent la suite quoi qu'il arrive (`cargo test; rm -rf ~`, #67), une substitution
  ou une redirection change ce qui est lancé.
- **Une règle par famille** : « Toujours » sur la ligne YouTube écrit
  `{"command": {"$cmd_prefix": "yt-dlp"}, "network": true}` ; la vidéo suivante passe sans
  carte. Trois familles au plus d'un seul clic, et le bouton les nomme toutes avant :
  `♾️ Toujours pour « yt-dlp », « ffmpeg » (réseau)`.
- **Couverture par étape** : la ligne n'est autorisée que si **chaque** étape l'est.
  `yt-dlp … && curl …` redemande tant que `curl` n'est pas couvert.
  `tools.shell_allow` et `shell_allow_network` suivent la même règle.
- **Ce qui ne demande aucune règle** : une lecture (`ls -la tmp/x*`), et un mot-clé qui ne
  fait que régler le shell (`cd`, `export`, `set`). C'est d'où venaient les vingt règles
  mortes ; `cd /x && cargo test` règle désormais `cargo test`, jamais `cd`.
- **Ce qui reste composé, en plus** : une étape qui ne nomme pas ce qu'elle lance —
  `sh -c`, `sudo`, `env`, `xargs`, `timeout`, `python3 -c`, `node -e`. Sans cela, admettre
  `&&` aurait laissé `a && sh -c "…"` créer une règle de famille `sh`, qui aurait couvert
  n'importe quel code.
- **Le bouton honnête n'est plus un bouton** : quand aucune règle n'est possible, rien ne
  prend la place de « Toujours ». La carte dit ce qui l'empêche et par où sortir : « pas de
  règle possible (`;`) — demande-lui une commande par appel ». Et `rule_created` ne vaut
  `always` que si une règle a été écrite.
- **Une commande par appel** : la description de `shell_exec` le demande, dans le budget de
  schémas de #104 (vérifié : 3 000 jetons). `penelope doctor` (`shell_lines`) donne la part
  de lignes collées sur sept jours et signale au-delà d'une sur cinq composées ; la forme de
  chaque ligne (`simple`, `liste`, `composee`) est journalée, jamais la commande.
- **Les skills installées reçoivent la consigne** au chargement, sans réécriture de leur
  fichier : les exemples de blocs shell qui collent des commandes sont comptés et la note
  de portage demande des appels séparés.
- Un seul endroit pour classer une lecture : `is_read_command` a rejoint le découpage dans
  `penelope-hitl::cmdline`, comme #141 l'avait fait pour le lexer. Deux listes de
  programmes de lecture auraient divergé.

### 0.17.32

#### Les mises à jour revenaient en arrière : boucle du rédacteur (#153)

Sur l'instance, **0.17.30 puis 0.17.31 sont revenues en arrière toutes seules** ; le Mac
est resté en 0.17.27, sans le Trousseau de #148, les formulaires de #149 ni la livraison
de #147. Le mécanisme de #36 a fait exactement son travail : cinq essais, retour au
binaire précédent, message au propriétaire.

- **La cause** : `redact()` s'appelait elle-même sur les segments autour des références
  `${SECRET:nom}`. Un texte portant `${SECRET:` **sans** référence complète derrière
  passait le `contains`, la recherche ne trouvait rien, et la fonction repartait sur le
  **même** texte : récursion infinie. Six lignes de ce genre dormaient dans `tg_outbox`,
  écrites par Pénélope elle-même en expliquant la syntaxe. Défaut latent depuis #37.
- **Le déclencheur, c'était #148** : `reredact_outbox` repassait toute la file au
  rédacteur au premier démarrage, dans une seule transaction, sur le thread écrivain. La
  première ligne orpheline faisait déborder sa pile, le processus était **abattu**
  (`abort`, pas une panique : le filet de #44 ne le voit pas), avant d'avoir écrit une
  ligne de journal.
- **Le rédacteur est linéaire** : un découpage sur les références trouvées, chaque segment
  rédigé par une fonction qui ne se rappelle pas. Une référence orpheline est un texte
  comme un autre. Les références complètes traversent toujours intactes (#37).
- **La passe est hors transaction** : lecture, rédaction dans la tâche courante, écriture
  par paquets de cent. Un paquet qui échoue est journalisé et n'arrête pas la passe ; le
  drapeau n'est posé qu'à la fin.
- **Le thread écrivain a 8 Mio de pile**, explicitement : il porte des travaux de
  maintenance qui traversent des tables entières, et un débordement y abat tout.
- **La carte de retour arrière dit pourquoi** : la dernière ligne parlante de
  `daemon.err.log` du binaire à l'essai (ici « stack overflow »), rédigée avant l'envoi.
  Elle disait seulement « n'a pas démarré correctement ».
- **`penelope doctor` (`redactor`)** passe neuf formes connues — références complètes,
  orphelines, hexadécimal, JSON — avec un délai de cinq secondes : une boucle est vue
  avant d'atteindre un démarrage.

Reproduit hors suite de tests (un débordement de pile abat le processus, il ne se
rattrape pas) : sonde sur les six lignes, `fatal runtime error: stack overflow` sur
l'ancien code, sept lignes rendues sur le nouveau.

### 0.17.33

#### La consolidation ne pense plus à la place d'écrire (#152)

Nuit du 20 au 21/09 : passe en échec, 224 candidats examinés, zéro promu. Sur un **seul**
candidat, avec 8 000 tokens de sortie autorisés, le modèle a dépensé les 8 000 en
raisonnement et n'a rendu aucune opération. Deuxième nuit sur trois à échouer.

- **La cause** : `lightest_effort` prenait le plus faible niveau **de la liste déclarée**.
  Pour `deepseek-v4-flash`, OpenRouter annonce `["xhigh","high"]` avec
  `mandatory: false` : la consolidation partait donc en `high`. Le raisonnement n'était
  pas obligatoire, rien ne permettait de le couper par ce chemin.
- **Raisonnement facultatif = coupé**, quelle que soit la liste ; obligatoire = le plus
  faible déclaré. Et `none` n'est plus envoyé comme un effort : c'est
  `reasoning: {enabled: false, exclude: true}`, que le fournisseur entend vraiment.
- **Le plafond porte les deux** quand le modèle impose de réfléchir : `max_tokens` compte
  le raisonnement chez OpenRouter, le budget de sortie est donc doublé dans ce cas, et
  laissé tel quel quand le raisonnement est coupé.
- **« Raisonnement plein » n'est pas « coupé »** : sortie utile vide et budget parti en
  réflexion, ce n'est pas une réponse trop longue. L'échelle de lots de #135 ne s'applique
  plus (réduire le lot n'y change rien) ; le premier cas est dit, le second bascule sur
  l'alias de repli (`memoire`) pour le reste de la passe, et si le repli s'affame aussi,
  la passe s'arrête en le disant, candidats non jugés reportés.
- **Le lot affamé ne compte plus comme jugé** : l'événement `memory.dream_batch` porte
  `reasoning`, `reasoning_starved` et `judged`. Il passait pour un lot abouti.
- `penelope doctor` (`reasoning_effort`) annonce l'effort qui partira pour le rôle
  `compaction` et la part de raisonnement observée sur sept jours ; au-delà de 50 %, c'est
  une alerte. `penelope model set` prévient quand un alias de consolidation ou de relecture
  reçoit un modèle qui impose de réfléchir — un avertissement, pas un refus.

### 0.17.34

#### La consolidation garde son raisonnement, mais le budgète (#152)

La 0.17.33 avait traité la même panne en **éteignant** le raisonnement. Décision du
propriétaire, prise après cette livraison : le garder et le faire bien. Cette version
remplace donc l'approche de la précédente, et traite la moitié qui manquait — une passe
qui jette son travail à la première coupure.

Nuit du 20 au 21/09 : passe en échec, 224 candidats examinés, zéro promu. Sur un **seul**
candidat, avec 8 000 tokens de sortie autorisés, le modèle a dépensé les 8 000 en
raisonnement et n'a rendu aucune opération. Deuxième nuit sur trois à échouer. Relancée à
la main au matin, la passe a écrit sept lots (126 candidats), puis le Mac est passé sur
batterie, un lot est resté sans réponse, et **tout a été jeté** : 226 candidats en attente,
zéro promu, aucun message.

**La cause**

`lightest_effort` prenait le plus faible niveau **de la liste déclarée**. Pour
`deepseek-v4-flash`, OpenRouter annonce `["xhigh","high"]` avec `mandatory: false` : la
consolidation partait en `high`. Le raisonnement était facultatif, mais rien ne permettait
de le couper par ce chemin. Le contournement évident (passer à un modèle qui déclare
`low`) a été vérifié et ne change rien : `effort: low` n'est pas honoré, la part de
raisonnement reste entre 85 et 100 %.

**Le raisonnement est gardé, et dimensionné**

Le tri d'un candidat (règle ou fait, durable ou passager, contradiction ou exception)
gagne à être réfléchi : la nuit du 19/09, à 73 % de raisonnement, a produit 113
promotions de bonne qualité. Ce qui cassait, c'est que la réflexion mangeait le budget
prévu pour la réponse, et que la passe prenait ça pour une sortie trop longue.

- **Deux budgets** : `reasoning.max_tokens` pour la réflexion (8 000 au départ, plafond
  `memory.consolidation_reasoning_tokens`), et `max_tokens` qui vaut la somme des deux ;
  OpenRouter compte le raisonnement dans le plafond de sortie.
- **« Raisonnement plein » relève le budget de réflexion** et rejoue le même lot, au lieu
  de réduire le lot : réduire le travail ne réduit pas la réflexion, et l'échelle de #135
  descendait jusqu'à l'échec. Au plafond atteint deux fois, repli sur l'alias suivant pour
  le reste de la nuit.
- **La sortie utile seule dimensionne les lots** (`completion − reasoning`) : `OutputBudget`
  apprenait le raisonnement comme si c'était du JSON et dérivait. L'échelle de lots de
  #135 et #140 raisonne désormais sur cette sortie utile, pas sur le plafond envoyé, qui
  porte les deux budgets.
- **Éteindre reste possible** : `memory.consolidation_reasoning = "off"` envoie
  `reasoning: {enabled: false, exclude: true}`, qu'un modèle entend vraiment ; en
  `effort: "none"`, plusieurs continuent de réfléchir.
- **Un modèle inconnu du catalogue est supposé réfléchir** : ne rien envoyer « dans le
  doute » est précisément ce qui a laissé `deepseek-v4-flash` dépenser tout son budget en
  réflexion. Le budget est inoffensif pour un modèle qui ne réfléchit pas.

**Un lot est une unité de travail complète**

- Opérations appliquées au vault, candidats marqués, état de la passe enregistré, **puis**
  le lot suivant. Le snapshot du vault est relu entre deux lots : les entrées écrites sont
  connues du lot suivant, et la garde anti-doublon vaut donc aussi entre deux passes.
- Une passe interrompue est close en `interrupted` avec le compte de ses lots ; la
  suivante reprend sur les candidats restants et le dit. Rejouer ne peut rien dédoubler.
- **Contrat de #135 remplacé** : « une passe qui échoue n'écrit rien » devient « une passe
  qui échoue garde ce qu'elle a écrit, et le dit ». Ce qui reste de #135 : elle ne consomme
  aucun report pour les lots qu'elle n'a pas jugés. Le message d'échec et `DREAMS.md`
  annoncent le nombre d'entrées gardées.
- Une coupure réseau ou une machine endormie n'est plus un abandon : le lot est rejoué au
  retour, avec une attente de cinq minutes, dans la limite du temps de la nuit. Le tri est
  étroit : notre propre délai et les erreurs de connexion locales, jamais « timeout » tout
  court. Un `Upstream idle timeout` est une erreur du fournisseur (#127), qui se reprend
  en deux minutes ; la confondre avec une machine endormie faisait attendre cinq minutes
  pour rien.
- Une passe lancée à la main qui échoue émet `memory.dream_failed` et prévient au foyer,
  comme une passe planifiée. Le 21/09, le propriétaire a dû demander pour l'apprendre.

**Ce qui se voit**

- `penelope doctor` : `reasoning_effort` annonce ce qui partira et la part de raisonnement
  sur sept jours ; `dream_power` prévient si la machine est sur batterie à l'heure du rêve.
- `penelope model set` prévient quand un alias d'extraction reçoit un modèle qui impose de
  réfléchir. Un avertissement, pas un refus.
- L'événement `memory.dream_batch` porte `reasoning`, `reasoning_starved` et `judged` : un
  lot affamé ne passe plus pour un lot jugé.
- Le digest et `DREAMS.md` distinguent trois cas : « consolidation coupée » (sortie trop
  longue), « raisonnement plein » (budget parti en réflexion), « réseau coupé ou machine
  endormie, lot rejoué ».

### 0.17.35

#### Pénélope ne connaissait pas sa machine (#156)

Le 20/09, le propriétaire colle un lien GitHub. Deux `http_fetch` sur `api.github.com`,
refusés faute de User-Agent, puis un `curl … | python3` cassé par un `===` que zsh
interprète, avant qu'il ne demande « pourquoi tu n'utilises pas `gh` ? ça devrait être ton
réflexe ». `gh` était installé **et connecté**. Même scène avec `glab`. Elle ne s'améliorait
que parce qu'on la corrigeait en séance, et la correction dictée dormait trois fois dans
`mem_candidates`, les deux passes de nuit ayant échoué (#152).

- **La cause** : rien, dans le message système, ne parlait de la machine. Ni les binaires,
  ni leur état de connexion. `doctor` vérifiait bien `git`, `npx`, `docker` pour lui-même ;
  ce résultat n'atteignait jamais le modèle. La ligne « Machine : MBP M1 » venait du profil,
  donc de ce que le propriétaire avait tapé à l'accueil, pas d'une détection.
- **Une passe d'inventaire** au démarrage, toutes les heures et à chaque `doctor` : `which`
  sur dix-sept binaires connus plus `tools.inventory_extra`, la version par une commande
  courte, et pour les forges l'état de connexion (`gh auth status`, `glab auth status`).
  Sonde à trois secondes, entrée fermée — une commande qui réclame une saisie meurt au
  délai ; l'option qui afficherait un jeton n'est jamais passée et ce qui est gardé traverse
  le rédacteur.
- **Une ligne dans le message système**, après les outils : ce qui est installé, ce qui est
  connecté et à quel compte, ce qui manque. **Ni version, ni date, ni chemin** : elle est en
  T1, dans le préfixe mis en cache (#104, décision 0008), et un `brew upgrade gh` ne doit
  pas la changer. Une déconnexion, elle, la change une fois.
- **Le routage n'est émis que pour ce qui est utilisable** : GitHub → `gh api`, GitLab →
  `glab`, YouTube → `yt-dlp`, conteneurs → `docker`, et jamais `curl` ni `http_fetch` vers
  une forge dont le client est connecté. Une forge installée mais **déconnectée** ne produit
  aucune règle : envoyer le modèle sur `glab` non connecté rend « not logged in », et il
  repart sur `http_fetch`.
- **`http_fetch` vers une forge connectée** porte une remarque (« `gh` est installé et
  connecté : préfère `gh api …` »), comme celle du réseau coupé de #106. Elle ne bloque
  rien : la lecture a déjà eu lieu, et `http_fetch` reste bon pour une page publique.
- **`self_status`** (section `machine`, champ `inventory`) porte les versions ; **`doctor`**
  donne une ligne pour les présents, une pour les absents, et une par forge installée mais
  déconnectée. Une skill dont un `bin:` requis manque est annotée dans `skill_search` et
  `skill_load` ; rien n'est installé à l'insu du propriétaire (#146 tient).
- **L'accueil `outils`** propose l'inventaire détecté au lieu d'une page blanche ; ce que le
  propriétaire déclare reste prioritaire sur ce qui est détecté.

Non traité, et pourquoi : la règle `profil.md:38` (« pour installer un MCP, demander les
URLs **au lieu d'utiliser gh** ») vit dans la mémoire de l'instance, pas dans ce dépôt ; sa
reformulation revient à la consolidation, réparée par #152. Le réflexe ajouté ici ne la
contredit pas — il porte sur la lecture d'une forge, pas sur la déclaration d'un serveur MCP.

### 0.17.36

#### Un secret long était écrit juste et relu faux (#157)

`penelope doctor` sur l'instance, le 21/09 : « aller-retour de 8 Ko — relecture différente :
8192 octets écrits, **42 relus** », avec pour correction proposée de déverrouiller le
Trousseau. Le Trousseau n'était pas verrouillé : il répondait, et il rendait autre chose.

- **La cause** : la tête d'un secret découpé (#148) portait `\x01penelope-chunks:v1:N`. Ce
  premier octet est un caractère de contrôle, et `security -w` n'imprime un mot de passe en
  clair **que s'il est imprimable** : il rendait la tête de 21 octets en 42 caractères
  d'hexadécimal. `count` n'y reconnaissait plus sa marque, et `get` concluait « item unique
  d'une version antérieure » — il rendait l'hexadécimal **comme valeur du secret**. Tout
  secret assez long pour être découpé était donc illisible : le `Grant` Codex de #142 aurait
  rendu 401 au premier appel.
- **La marque devient imprimable** (`penelope-chunks:v1:`). L'ancienne est encore **lue**,
  jamais écrite : les secrets déjà posés n'ont pas à migrer. Une valeur qui commencerait par
  la marque n'est pas confondue pour autant, `set` la force sur le chemin découpé.
- **`read_item` décode l'hexadécimal**, et seulement s'il redonne une tête de morceaux : une
  empreinte ou une clé brute sont des secrets entièrement hexadécimaux parfaitement
  ordinaires, les décoder rendrait faux ce qui était juste.
- **Une tête illisible n'est plus une valeur** : quand des morceaux existent, `get` rend une
  erreur explicite. Mieux vaut un secret qui manque qu'un jeton faux qui échouera plus tard,
  ailleurs, sans rapport apparent.
- **`doctor` ne se trompe plus de coupable** : « relecture refusée » (le magasin ne répond
  pas) reste « déverrouiller le Trousseau » ; « relecture altérée » (il répond autre chose)
  dit la longueur relue, si elle ressemble à de l'hexadécimal, et que le Trousseau n'est pas
  en cause.

**Ce que ce lot corrige vraiment, c'est le test.** Les tests de #148 passaient parce que le
faux `security` rendait la valeur telle quelle ; seul le vrai hexadécimalise. Le double de
test mentait sur le point qui comptait. Il rend maintenant `-w` comme le vrai — en clair si
imprimable, en hexadécimal sinon — et c'est lui qui tient les trois cas : un `Grant` de 8 Ko
qui revient identique, un secret posé avec l'ancienne marque qui reste lisible, une tête
abîmée qui devient une erreur.

Rappel pour ce dossier : `crates/penelope-platform/src/backend/macos.rs` n'est pas compilé
sous Linux. `cargo check -p penelope-platform --target aarch64-apple-darwin --all-targets`
est la seule relecture possible depuis le conteneur, et elle a servi.

### 0.17.37

#### La consolidation tournait à vide, en silence (#152)

Passe du 21/09 en 0.17.34 puis 0.17.36 : un lot écrit, puis des cycles muets de 240 s
d'appel tué et 300 s d'attente, pendant une heure, sans un événement ni une ligne de
journal, `stats` figés. Régression du lot précédent, livrée par moi.

- **La cause** : `LLM_TIMEOUT = 240 s` enveloppait l'appel entier. Avec 8 000 à 16 000
  tokens de raisonnement autorisés à ~25 tokens/s, un lot demande cinq à onze minutes :
  tout appel qui réfléchissait vraiment était tué à quatre. Et `network_stall` comptait
  **notre propre délai** comme une coupure réseau, donc la reprise attendait 300 s et
  rejouait, tant qu'il restait du temps avant le verrou de deux heures.
- **Le délai suit le budget** (`call_timeout`) : `(R + sortie) / débit prudent`, borné à
  [4 min, 15 min] et par le temps restant de la nuit. Le délai d'inactivité du fournisseur
  (`stream_idle_timeout`, #51) continue de couvrir le flux muet : l'un borne le silence,
  l'autre la durée totale.
- **Notre délai n'est pas le réseau** : une coupure se prouve par une sonde TCP vers le
  fournisseur. Sinon c'est « appel trop long pour son budget », repris avec l'attente
  ordinaire.
- **Trois tentatives par lot**, toutes causes confondues : au-delà, le lot est reporté et
  la passe continue. Une nuit entière sur un seul lot ne vaut pas mieux qu'un échec.
- **Chaque tentative laisse une trace** : événement `memory.dream_retry`, ligne de journal,
  et `save_stats`. Les avertissements restaient en mémoire jusqu'au premier lot écrit ;
  c'est pourquoi la dérive a été invisible une heure durant.

Deux tests de 0.17.34 encodaient le mauvais comportement : celui de la coupure réseau
injectait notre propre message de délai, et validait donc la confusion qui a causé la
panne. Un double de test qui ment sur le point qui compte ne prouve rien — deuxième fois
dans ce dossier après le faux `security` de #157.

#### La base n'est plus accusée sur la parole d'un seul lecteur (#158)

Trois `doctor` d'affilée : « malformed inverted index for FTS5 », sur une table différente
à chaque fois, avec pour seule correction `penelope restore --latest` — onze heures de
conversations perdues si la consigne était suivie, et l'option n'existe pas. Le fichier
était intègre : sept connexions neuves le relisaient sans rien trouver.

- **Une connexion neuve tranche** : quand un lecteur du pool accuse la base, le verdict lui
  est redemandé (même idée que `backup_to`, #77). La connexion qui a menti est fermée, le
  pool en rouvre une.
- **Trois verdicts au lieu d'un** : fichier intègre et lecteur fautif (« redémarrer »,
  jamais de restauration) ; index dérivé confirmé abîmé (`penelope store rebuild`) ; vraie
  atteinte aux données (critique, et la consigne nomme la sauvegarde et ce qu'elle ferait
  perdre).
- **Toutes les lignes d'un `PRAGMA`** sont lues : n'en lire qu'une laissait une corruption
  de données se cacher derrière une ligne d'index.
- **Un index dérivé n'empêche plus le démarrage** : `Store::open` le reconstruit puis
  revérifie, et émet `store.fts_rebuilt`. Refuser, c'était mettre le daemon en panne pour
  ce qui se rebâtit en une commande, et déclencher le retour arrière de #153.

Reste ouvert sur #152 : reprise idempotente lot par lot, troisième cas du digest, ligne
`doctor` sur le budget de raisonnement envoyé.

### 0.17.38

#### Un `{{workdir}}` avec une espace cassait tout workflow sur macOS (#154)

Run `apnl-task-to-pr` bloqué en quinze secondes sur `fatal: Too many arguments`.
`{{workdir}}` vaut `/Users/edouard/Library/Application Support/Penelope/…` : le répertoire
de données de macOS contient une espace. Substituée telle quelle dans
`git clone … {{workdir}}/repo`, la valeur donne deux arguments. Juste sur Linux
(`~/.local/share/penelope`), faux sur tout Mac par défaut — le workflow livré compris.

- **Le moteur cite**, pas l'auteur : toute valeur substituée dans une `command` d'étape
  `shell` part entre apostrophes, apostrophes internes échappées. Un gabarit qui cite déjà
  n'est pas cité deux fois. Les workflows existants restent valides sans réécriture.
- `prompt`, `cwd` et `args` ne passent par aucun shell : rien n'y est cité.
- `quote: false` par étape, pour le gabarit rare qui met une **liste d'arguments** dans une
  variable, où citer ferait un seul argument de plusieurs.

**Et le run n'est plus annoncé sans avoir été regardé.** La carte disait « ⛔ bloqué » à
05:58:32 ; à 05:59:06 la réponse du tour annonçait « 🚀 Lancé — en cours » avec un tableau
d'étapes. `workflow_start` rendait `running` avant que la première étape ne tourne. Il
attend désormais le premier verdict (au plus cinq secondes) et joint une remarque quand le
run est `blocked` ou `failed`. Cette consigne vit dans le résultat, pas dans la description
de l'outil : celle-ci entre dans le préfixe mis en cache, et le contrôle de budget de #104
refusait l'ajout. Une consigne qui n'apparaît qu'au moment utile ne coûte rien.

#### `/stop tout` disait « Rien à arrêter » avec un run bloqué ouvert (#155)

Seuls les runs `running` étaient regardés. Un run `blocked` — celui qui *paraît* en cours,
carte « ⛔ bloqué » et message « en cours » — n'était ni touché ni nommé.

- Tous les runs ouverts du chat sont pris : les `running` mis en pause, les autres
  **nommés** avec leur état. Jamais annulés à la place du propriétaire, puisqu'un run
  annulé ne se reprend pas.
- « Rien à arrêter » n'est plus possible quand un run est ouvert ; `/stop` sans `tout`
  nomme ce qui continue.
- `/stop tout` vide aussi les sessions de **sous-agents** dont le parent est dans ce chat :
  sans `tg_chat_id`, elles étaient sautées.
- La phrase qui promettait de mettre l'ingestion en pause est retirée : elle n'était pas
  tenue. Pas de promesse sans code.

La construction du message sort du transport Telegram (`StopReport::render`), donc se
vérifie : deux tests, dont la reproduction du cas du 21/09.

### 0.17.39

#### Consolidation : la reprise est idempotente, et prouvée (#152)

Dernier lot de #152. Les points de robustesse demandés le 21/09 sont maintenant tous
couverts, et celui qui manquait d'une preuve en a une.

- **Rejouer un lot déjà écrit n'ajoute rien deux fois.** La garde « un texte déjà en
  mémoire ne s'ajoute pas » part de l'instantané du vault, relu à chaque lot depuis
  0.17.34 : elle tient donc aussi **entre deux passes**, y compris après une passe tuée au
  milieu d'une écriture. Un test le montre de bout en bout : un texte promu, puis une
  seconde passe où le modèle propose le même texte — une seule entrée dans le vault, et le
  rejeu est dit (« ＝ déjà en mémoire ») au lieu d'être silencieux.
- Un candidat promu ne revient pas dans la file : la passe suivante ne reprend que les
  candidats encore en attente.

**État des exigences de l'issue**, vérifié plutôt que supposé : écriture lot par lot,
marqueur `interrupted` et reprise (0.17.34) ; défaut `auto`, budget de raisonnement séparé,
`OutputBudget` sur la sortie utile (0.17.34) ; délai dérivé du budget, notre délai distingué
d'une coupure réseau, plafond de trois tentatives, trace par tentative (0.17.37) ; échec
d'une passe manuelle annoncé au foyer, ligne `doctor` sur le budget envoyé, alerte batterie
(0.17.34) ; les trois cas — sortie coupée, raisonnement plein, coupure réseau et lot rejoué
— sortent dans les avertissements du rapport.

### 0.17.40

#### Les trois points laissés de côté, faits (#155, #158)

Ils étaient écrits dans les issues fermées plutôt que réalisés. Les voici.

- **L'ingestion est réellement interruptible** (#155). `ingest.rs` lançait son appel au
  modèle avec un `CancelToken::new()` que personne ne tenait : `/stop tout` promettait de
  la mettre en pause et ne touchait rien. J'avais retiré la promesse — honnête, mais pas
  mieux. Le bus porte désormais un registre des ingestions par session (`start_ingest`,
  `end_ingest`, `cancel_ingests`), le jeton traverse `ingest` → `summarise` →
  `chat_stream`, `/stop` les compte et `/stop tout` les annule. La phrase est de retour,
  tenue cette fois.
- **Les boutons** (#155). La machinerie existait : l'écran des runs porte ⏸, ▶️ et ⏹ par
  run, l'arrêt sous confirmation. Quand `/stop tout` laisse des runs ouverts, cet écran
  suit la réponse. « Laisser », c'est ne pas cliquer — la commande ne décide pas à la
  place du propriétaire.
- **SQLite à jour** (#158). `rusqlite` 0.32 → 0.37, SQLite 3.46.0 → ≥ 3.50, la raison
  écrite dans `Cargo.toml` : 3.46.1 a corrigé un faux positif de l'`integrity-check` FTS5,
  c'est-à-dire **la source** du verdict que la 0.17.37 apprenait à ne pas croire sur
  parole. Les deux bouts sont maintenant traités. Aucun changement de code n'a été
  nécessaire ; un test verrouille la version embarquée.

Un appelant de `ingest` vivait dans les tests de `penelope-evals`, que
`cargo check -p penelope-daemon` ne compile pas : seule la suite complète l'a vu. Même
leçon que pour `macos.rs` plus tôt — une vérification partielle laisse croire que tout
compile.

### 0.17.41

#### La 0.17.40 n'a pas été publiée : deux vérifications manquantes

Le job `livraison` a été sauté, `cargo deny` et le job macOS étant rouges. Rien à reprendre
dans le code de la 0.17.40 ; ce sont deux angles morts de la vérification locale.

- **Un test `#[cfg(target_os = "macos")]` appelait `ingest` à six arguments**
  (`ingest.rs:979`). Sous Linux il n'est compilé par rien : ni `cargo test --workspace`, ni
  `cargo clippy --all-targets`, ni la ligne `--target aarch64-apple-darwin` de `CLAUDE.md`,
  qui ne couvre que `penelope-platform`. Les crates qui embarquent SQLite ne se compilent
  pas en croisé ici, faute de SDK C : l'arité des dix-sept appels a été vérifiée
  statiquement, et les cinq blocs macOS de `penelope-daemon` relus un par un.
- **`foldhash` est sous licence Zlib** et entre par `hashbrown` → `hashlink` → rusqlite 0.37.
  C'est une conséquence directe de la montée SQLite de la 0.17.40. Zlib est permissive, sans
  clause de réciprocité : elle entre dans l'autorisation avec sa raison écrite, la règle du
  fichier étant inchangée.

Deux lints réels ont été corrigés au passage (`FAKE_PY` inutilisée hors macOS,
un `if` imbriqué dans le bloc Linux de `rss_mb`). La CI ne les voyait pas — elle ne lance
clippy que sur macOS — mais ils faisaient échouer la commande que `CLAUDE.md` prescrit
avant de pousser.

### 0.17.42

#### `penelope mcp auth slack` : ce qui manquait (#159)

La commande échouait sur « aucun moyen d'enregistrer le client », sans dire la suite.
Trois manques, tous visibles sur `mcp.slack.com`, aucun spécifique à Slack.

- **`mcp.callback_host`**, `127.0.0.1` par défaut, `localhost` accepté, rien d'autre : une
  valeur qui ne soit pas de bouclage ferait partir le code d'autorisation sur le réseau.
  Slack n'enregistre que `localhost` dans les URL de rappel d'une app, et l'URL envoyée
  doit être **exactement** celle enregistrée. `doctor` affiche l'URL effective.
- **Portées par défaut.** La ressource protégée n'était lue que pour
  `authorization_servers` ; ses `scopes_supported` (RFC 9728) servent désormais de dernier
  recours quand la déclaration et le défi 401 sont muets. Slack ne met pas de `scope` dans
  son défi et refuse une demande sans portée. La liste demandée est journalisée.
- **Le message dit la suite** : `choose_registration` reçoit le nom du serveur et nomme la
  commande, `penelope mcp edit <srv> client_id <id>`, avec le renvoi à `docs/mcp.md`.

`docs/mcp.md` § « Client pré-enregistré : Slack » donne le manifeste et l'ordre des
commandes. Il insiste sur un point que l'issue laissait implicite : **les portées demandées
doivent être celles du manifeste, ou un sous-ensemble**. Le repli sur `scopes_supported`
demanderait plus que ce que l'app déclare, et Slack refuserait. Pénélope ne peut pas
connaître les portées d'une app tierce, donc c'est à la déclaration du serveur de trancher ;
la documentation le dit plutôt que le code ne le devine.

L'app Slack reste nécessaire, Slack n'offrant ni CIMD ni enregistrement dynamique et
n'ouvrant le MCP qu'aux apps internes ou publiées. Elle ne sert que de porteur d'identité :
le jeton obtenu est un jeton **utilisateur** (`xoxp-`), et Pénélope ne voit que ce que voit
le compte qui a autorisé. Le bot user du manifeste n'est jamais utilisé.

Reste ouvert dans l'issue : le `client_secret` optionnel, inutile tant que l'app active
PKCE, et l'audit des deux entrées à jeton neuf de #155.

### 0.17.43

#### Les workspaces changent réellement au prochain appel (#163)

`config_set sandbox.workspaces` disait « dès le prochain appel », mais l'exécuteur gardait
les racines copiées au début du tour. Les résolutions de chemins, les `cwd`, les commandes
`cd … && …` et `image_inspect` relisent désormais la génération vivante. Les racines propres
à un workflow restent stables et une liste volontairement restreinte de sous-agent n'est
jamais élargie.

Le résultat de `config_set` distingue maintenant l'appel suivant, le tour suivant et le
redémarrage. La référence des clés documente ces moments ; les refus de chemin citent les
racines réellement utilisées. Trois tests couvrent le changement dans le même tour, le tour
suivant, le message rendu et le maintien du cloisonnement.

### 0.17.44

#### `build-verify` reprend la commande et les preuves du build (#167)

Le build conserve dans `session_metadata.verification` la commande effectivement validée,
ses prérequis non secrets et les références de preuves TDD, PR et CI. Le contrôle
`project_tests` reprend cette commande par `shell_exec` avec la politique d'approbation et
le bac à sable du builder. Le vérificateur reçoit l'objectif, les critères, les résultats,
les règles du dépôt et un passage de relais borné et rédigé ; il examine lui-même les
preuves. Une preuve PR, CI ou test vert liée à un autre commit est refusée avant son
jugement ; un test rouge peut documenter l'état antérieur.

Le retour en build distingue prérequis absent, preuve manquante ou périmée, test rouge et
critère non satisfait. Cinq tests couvrent la commande actualisée, l'outil absent et
son approbation, le test rouge, le SHA périmé et les clauses conditionnelles du dépôt.
Les budgets et plafonds du workflow ne changent pas.

### 0.17.45

#### Les approbations de workflow reviennent dans leur sujet Telegram (#165)

La session technique d'un run n'a pas de coordonnées Telegram. Au clic, le bot consultait
seulement cette session : les confirmations d'approbation partaient dans Général alors
que la carte était dans le sujet. La destination de chaque carte est maintenant persistée
et relue au clic. Si elle manque, l'origine enregistrée du run puis la session servent de
repli, sans mélanger le chat d'une source avec le sujet d'une autre.

Autorisation, refus, « Déjà tranché », seconde confirmation destructive et budget suivent
la même résolution. Les raisons de refus sont isolées par sujet ; une carte d'effet
incertain renvoyée après redémarrage retrouve aussi l'origine du run. Deux tests couvrent
le clic après redémarrage, deux sujets du même groupe, les décisions répétées et les
cartes de budget et d'effet incertain.

### 0.17.46

#### Les workspaces suivent la casse réelle du volume (#164)

Les racines existantes de `sandbox.workspaces` sont canonicalisées au chargement et à
l'enregistrement. `config_set` rend la valeur enregistrée et avertit quand elle diffère
de la demande ou quand le répertoire n'existe pas. La garde des fichiers compare les
chemins réels, y compris pour un fichier à créer ; un lien symbolique sortant reste refusé.

Le `cwd` du shell, le relèvement de `cd … && …` et les règles `$path_prefix` utilisent
la même résolution. Les tests exercent un alias de casse selon le système de fichiers,
un volume sensible à la casse, la persistance de la configuration et un lien sortant.

### 0.17.47

#### Flux d'observation runtime et relais Pathlayer (#162)

Le journal d'audit émet désormais chaque ligne commitée sur un WebSocket local, dans
l'ordre de ses identifiants SQLite. L'abonnement précède le replay, et une reconnexion
avec `after_id` ou une perte de tampon reprend au journal. Chaque consommateur a son
jeton du magasin de secrets et son filtre de types ; le serveur est désactivé sans
consommateur et refuse un bind hors de `127.0.0.1`. Le flux exporte une projection
rédigée et bornée, sans les empreintes d'audit ni commande d'actuation.

L'exécuteur commun journalise les appels natifs, MCP et shell, y compris dans les
workflows, avec arguments, résultat, durée et coût estimé. Le ledger LLM émet les
tokens et coûts ; les stores émettent le cycle de vie des sessions et approbations ;
l'ordonnanceur émet les déclenchements. Les événements préexistants complètent les
tours, runs, étapes, intents et erreurs. Les tests vérifient l'ordre sous concurrence,
le replay filtré, le refus sans jeton et la rédaction. Une session locale a exécuté
quatre lectures réelles ; le relais de 48 lignes les a transmis au `HttpIngestAdapter`
de Pathlayer, qui a détecté la boucle à 0,50, 0,75 puis 0,88. Le schéma réserve
`actuation: null` sans accepter de commande de retour en V1.

### 0.17.48

#### Un tour absorbe les messages suivants de sa session (#161)

À la réclamation, les messages textuels en attente de la même session et de la même
origine sont liés au tour porteur sans perdre leur ID ni leur clé de déduplication. Le
transcript écrit chaque message séparément, dans l'ordre et à son heure de réception ;
la reprise après incident n'en écrit aucun deux fois. Les reprises d'approbation, les
déclencheurs et les autres origines gardent leur priorité et leur propre tour.

Avant chaque nouvel appel au modèle, le tour absorbe les messages arrivés pendant les
outils et signale ce qui a déjà été exécuté. La réponse Telegram vise le dernier
message absorbé. Les seuils de rafale ouvrent la carte de choix, même pendant un tour ;
`/stop` annule le porteur et toutes ses lignes absorbées. `turn.merged` indique le nombre
et le moment de l'absorption. Les tests couvrent la réclamation, la projection, la
reprise, la déduplication, les bornes, la livraison et l'annulation. Un message avec
photo conserve son traitement visuel séparé.

### 0.17.49

#### `git_clone` valide sa source et retrouve les clones existants (#160)

`owner/repo` devient une URL HTTPS GitHub avant tout appel Git ; les chemins locaux
relatifs ou absolus sont refusés avec les formes admises. Le résultat rend l'URL
réellement utilisée. Un appel invalide est rejeté avant la carte d'approbation, y
compris via `tool_call`. Avant de cloner, l'outil cherche dans les workspaces autorisés
une origine équivalente, y compris SSH/HTTPS sur GitHub, et retourne son chemin.

La migration révoque les anciennes règles « Toujours » de `git_clone` sans motif.
Les nouvelles règles sont bornées au schéma et à l'hôte ; `file://` ne crée pas de
règle durable. Le schéma de l'outil donne des exemples. Les tests couvrent les URLs,
les chemins refusés avant lancement, la réutilisation d'un clone, un vrai clone local
explicite, la migration et la portée des règles.

### 0.17.50

#### Les clients MCP OAuth confidentiels gardent leur secret hors configuration (#174)

Une déclaration MCP peut référencer un `client_secret` par `${SECRET:nom}` ; toute valeur
en clair et toute référence autre que le magasin de secrets sont refusées. La valeur est
résolue seulement lors de l'échange du code et du rafraîchissement, puis enregistrée dans
le rédacteur avant l'appel réseau. Les demandes et autorisations persistées ne conservent
que la référence.

Pénélope lit `token_endpoint_auth_methods_supported`, prend en charge
`client_secret_basic` et `client_secret_post`, et conserve le client public PKCE par
défaut. Les tests simulent les deux méthodes, l'échange, le rafraîchissement et vérifient
que la valeur du secret n'est pas persistée.

### 0.17.51

#### La taille des workspaces de runs vivants devient visible (#177)

L'audit du 22 septembre a attribué les 11 Go de `state/runs` à un run en pause :
son `target/debug` Cargo, et non un échec de la rétention des runs terminés.
Une passe quotidienne mesure désormais les espaces des runs `running`, `paused` et
`blocked`. Dès 1 Gio, le journal et l'événement `workflow.workspace_large` indiquent
le run, son état, son chemin et sa taille. Les liens symboliques ne sont pas suivis ;
aucun run vivant n'est effacé. Les tests vérifient la sélection des runs en pause,
la borne du parcours et l'absence de suppression.

### 0.17.52

#### Deux sessions distinctes avant une proposition de procédure (#178)

La consolidation ne propose plus une `ProcedureCandidate` après deux succès dans
la même session, même à des dates différentes. Elle exige deux occurrences réussies
et deux sessions traçables distinctes. Ce critère évite de compter deux fois une
seule trajectoire ; il reste un indicateur de tâches indépendantes, pas leur preuve.
Le test couvre aussi les candidats sans session.

### 0.17.53

#### La compaction relève les indices explicites absents du contexte final (#179)

L'événement `context.compacted` compte désormais les actions marquées `TODO:`,
`À faire:` ou `- [ ]` et les identifiants des messages utilisateur présents dans le
transcript échantillonné du lot. Le contrôle les compare au contexte final, après
ajout mécanique des ancres et des citations verbatim. Il conserve au plus trois
exemples de 80 caractères par catégorie dans l'événement ; la métrique agrégée ne
porte que les nombres.
Le résultat du résumeur, y compris le repli sans modèle, ne change pas. Ce contrôle
textuel reste indicatif : une paraphrase peut être signalée comme manquante.

### 0.17.54

#### `doctor` reconnaît un hôte GitLab connecté et conseille la bonne formule (#182)

`glab auth status` peut sortir en erreur quand `gitlab.com` est déconnecté alors qu'une
autre instance GitLab est authentifiée. L'inventaire retient maintenant la ligne de
connexion positive de cette instance ; il peut proposer `glab` au modèle. Une sortie
sans cette preuve reste déconnectée. Quand l'exécutable `rg` manque dans le `PATH` du
daemon, la correction proposée est `brew install ripgrep`, nom réel de la formule.
Deux tests de régression couvrent ces cas. Les autres alertes `doctor` de cette machine
ont été traitées dans sa configuration, sa skill LinkedIn et l'index de son vault.

### 0.17.55

#### `mem split` refuse les propositions vides ou trompeuses (#184)

Le découpage lit seulement les puces de faits, accepte les listes numérotées et sépare
une longue puce aux fins de phrase plutôt que de la perdre entièrement. Un garde-fou
local écarte les mentions de salaire, d'IBAN, de SIREN/SIRET et les montants en euros.
Si le modèle ne
répond pas, ne fournit aucun fait exploitable ou résume une longue entrée en un seul
fait, il reçoit un second essai dans le même délai total de 120 secondes. Un nouvel
échec est expliqué et ne crée pas de carte d'approbation trompeuse. Cinq tests de
régression couvrent ces cas ; aucune entrée du vault n'est modifiée par cette commande
avant l'approbation du propriétaire.

### 0.17.56

#### Un plan révisable précède les workflows Telegram (#186)

`/run <workflow>` et le bouton du catalogue ouvrent une conversation de plan, avec ou
sans paramètres fournis. L'orchestrateur propose un but et des étapes typées via
`workflow_plan` ; une correction crée une nouvelle version, un retour arrière conserve
l'historique. Le plan et son gate sont écrits durablement dans le store. La carte dans
le même sujet permet « Vas-y » ; le clic approuve la version affichée et rejette un
bouton périmé. Aucune exécution n'est déclenchée par cette tranche : l'état approuvé
sera consommé par T3 de #185. `workflow_start` ne peut plus lancer directement depuis
une conversation Telegram. La CLI et les runs techniques gardent leur moteur actuel.

### Routine de livraison

Le tag et la release sont posés par la CI (job `livraison` de `ci.yml`, issue #147) :
rien à taguer à la main. Dans le lot, avant de pousser :

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
5. `make bump V=x.y.z` : la section de `progress.md` est vérifiée, les seize lignes de
   `Cargo.toml` et le `Cargo.lock` sont réécrits, le commit « Version x.y.z » est posé.
   Une section de version sans bump fait échouer la CI (le test `docs` vérifie les deux
   sens depuis #147).
6. Pousser. La CI rejoue les suites, pose le tag `vx.y.z` et appelle `release.yml` ; la
   release apparaît une douzaine de minutes plus tard. Un push qui ne change pas la
   version ne pose rien.

Deux sessions qui livrent en même temps se heurtent au conflit sur `Cargo.toml` : c'est
la serrure, prendre le numéro suivant.

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
