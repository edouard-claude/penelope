# Installation headless (macOS)

Cible : un MacBook Pro M1 branché au secteur, couvercle fermé, sans écran ni clavier,
joignable en SSH. Pénélope y tourne comme LaunchAgent de l'utilisateur, jamais en root.

## 1. Prérequis

- macOS 13 ou plus récent.
- Rust stable (édition 2024).
- Connexion à distance activée : Réglages Système → Général → Partage → Connexion à
  distance. C'est le seul moyen d'atteindre la machine une fois le couvercle fermé.

## 2. Compiler et installer

```bash
cargo build --release
```

```bash
sudo cp target/release/penelope /usr/local/bin/penelope
```

```bash
penelope install
```

`install` écrit `~/Library/LaunchAgents/com.penelope.daemon.plist` puis charge le service.
Rien n'est écrit hors du profil de l'utilisateur.

Vérifier les répertoires effectifs avant toute autre chose :

```bash
penelope paths
```

Par défaut, sur macOS :

| Rôle | Chemin |
|---|---|
| Données (base, vault, skills, workflows, gabarits, `mcp.d`, artefacts, sauvegardes) | `~/Library/Application Support/Penelope` |
| Configuration | `~/Library/Application Support/Penelope/config/config.toml` |
| État volatil (socket RPC, verrous, workspaces de run, PID des serveurs MCP) | `~/Library/Application Support/Penelope/state` |
| Journaux | `~/Library/Logs/Penelope` |
| Cache | `~/Library/Caches/Penelope` |

`install` inscrit dans le service le PATH du terminal qui la lance, complété des
emplacements usuels (Homebrew, `~/.local/bin`, Docker, nvm) : sans cela, `launchd` ne
fournit que les répertoires système, et le daemon ne trouverait ni `npx`, ni `uvx`, ni
`docker`. Après avoir installé un nouvel outil ailleurs, réinstaller le service :

```bash
penelope uninstall && penelope install
```

`PENELOPE_HOME=/un/répertoire` (ou `--home`) déplace **tout** l'ensemble d'un coup. Utile
pour un bac à sable, une seconde instance ou un test : aucun chemin n'est codé en dur
ailleurs que dans `penelope-platform`, et un test d'architecture l'impose.

## 3. Diagnostic

```bash
penelope doctor
```

Chaque contrôle en échec rend une commande de correction. Les contrôles propres à macOS :

| Contrôle | Ce qu'il vérifie | Correction usuelle |
|---|---|---|
| `macos.sleep` | La machine ne s'endort pas au milieu d'un run | `sudo pmset -a sleep 0 disablesleep 1` |
| `macos.filevault` | Un redémarrage sans intervention est possible | `sudo fdesetup authrestart` au moment du reboot |
| `macos.remote_login` | SSH est ouvert | activer la Connexion à distance |
| `macos.sandbox` | Le bac à sable Seatbelt est utilisable | réinstaller les outils de ligne de commande |
| `macos.keychain` | Le trousseau répond sans invite graphique | déverrouiller le trousseau de session |
| `macos.power_source` | La machine est sur secteur | rebrancher |

FileVault mérite une décision explicite. Activé, il protège les secrets au repos mais
impose `fdesetup authrestart` pour qu'un redémarrage reparte sans mot de passe tapé à
l'écran. Désactivé, la machine redémarre seule mais son disque est lisible par quiconque
l'ouvre. `doctor` signale la situation, il ne tranche pas à votre place.

## 4. Secrets

Les secrets vivent dans le trousseau macOS, pilotés par `/usr/bin/security` appelé comme
exécutable, jamais via un shell. Seul l'index des **noms** est stocké en clair :
`dump-keychain` exigerait un déverrouillage interactif, impossible sans écran.

Les secrets attendus au minimum, sous ces noms exacts :

- `openrouter_api_key` : une clé créée sur https://openrouter.ai/keys ;
- `telegram_bot_token` : le jeton **du bot**, à ne pas confondre avec ton identifiant
  Telegram (`owner.telegram_user_id`, qui dit seulement qui a le droit de parler au bot).

Pour obtenir le jeton du bot : dans Telegram, écrire à `@BotFather`, envoyer `/newbot`,
choisir un nom puis un identifiant qui finit par `bot`. BotFather répond avec un jeton de
la forme `123456789:AAH…` : c'est lui qu'il faut enregistrer.

La valeur n'est jamais un argument : elle resterait dans l'historique du shell et serait
visible dans `ps`. La commande la demande, sans l'afficher :

```bash
penelope secret set openrouter_api_key
```

Coller la valeur à l'invite, puis Entrée. En SSH, c'est la bonne méthode : `pbpaste`
lirait le presse-papiers **de la machine distante**. En local, une redirection marche
aussi :

```bash
pbpaste | penelope secret set openrouter_api_key
```

La commande fonctionne sans daemon, donc avant le tout premier démarrage. Elle affiche le
nom, le backend et la longueur de la valeur, jamais la valeur elle-même. Un daemon déjà
lancé prend une nouvelle clé en compte au tour suivant ; le jeton Telegram, lui, demande
`penelope restart`.

```bash
penelope secret list
```

```bash
penelope secret backend
```

Le Trousseau est commun à tous les `PENELOPE_HOME` d'un même utilisateur. Pour une
instance d'essai qui ne doit pas y toucher, forcer le fichier chiffré :
`PENELOPE_SECRETS=file` avec `PENELOPE_PASSPHRASE` ou `PENELOPE_MASTER_KEY_FILE`.

Un fichier `mcp.d/*.toml` référence un secret par `${SECRET:nom}` dans ses en-têtes ou son
environnement : la valeur n'apparaît jamais dans la configuration ni dans un journal.

## 5. Configuration

```bash
penelope config validate
```

Hors daemon, donc utilisable avant le premier démarrage et en CI. Le fichier de référence
commenté est au §19 du PRD.

**Le propriétaire d'abord.** Tant que `owner.telegram_user_id` vaut 0, la configuration
est invalide (un bot sans propriétaire serait ouvert à tous), et toute autre modification
est refusée. C'est donc le premier réglage à poser :

```bash
penelope config set owner.telegram_user_id 123456789
```

L'identifiant s'obtient en écrivant à `@userinfobot` sur Telegram. Les autres réglages à
poser d'emblée :

```toml
[owner]
telegram_user_id = 123456789
timezone = "Indian/Reunion"

[telegram]
mode = "polling"

[budget]
daily_usd = 20.0
session_usd = 5.0
```

Une modification est publiée à chaud, comme une génération immuable : les tours déjà
commencés gardent l'instantané qu'ils ont lu, les suivants prennent la nouvelle.

```bash
penelope config set context.compaction_threshold 0.66
```

```bash
penelope config status
```

`config status` montre quels sous-systèmes ont pris la génération, et lesquels demandent
un redémarrage.

## 6. Modèles

Pénélope ne connaît jamais un modèle par son nom brut, seulement par **alias**. Trois
étages, du plus concret au plus abstrait :

```
 rôle (qui a besoin d'un modèle)   alias (un nom stable)   modèle réel (chez un provider)
 ───────────────────────────────   ─────────────────────   ─────────────────────────────────────
 chat_default ───────────────────► main ─────────────────► openrouter:deepseek/deepseek-v4-pro
 classifier, memory_review ──────► fast ─────────────────► openrouter:deepseek/deepseek-v4-flash
 code ───────────────────────────► reasoning ────────────► openrouter:z-ai/glm-5.2
 compaction ─────────────────────► summarizer ───────────► openrouter:deepseek/deepseek-v4-flash
 image_describe ─────────────────► vision ───────────────► openrouter:google/gemini-3.1-flash-image
 image_generate ─────────────────► image ────────────────► openrouter:google/gemini-3.1-flash-image
```

Changer de modèle, c'est donc changer **une** ligne : l'alias. Les workflows, les skills et
les rôles n'ont pas à bouger. Le format est `provider:identifiant`, l'identifiant étant
celui affiché sur openrouter.ai :

```bash
penelope model set main openrouter:anthropic/claude-sonnet-4.5
```

La modification est écrite dans `config.toml` puis publiée à chaud ; `penelope config
status` montre que chaque sous-système a pris la nouvelle génération.

En fichier, les trois tables correspondantes :

```toml
[models.aliases]
main = "openrouter:anthropic/claude-sonnet-4.5"
fast = "openrouter:deepseek/deepseek-v4-flash"
reasoning = "openrouter:z-ai/glm-5.2"

[models.roles]
chat_default = "main"
code = "reasoning"

[models.routing]
classifier = true      # un petit modèle estime la complexité de chaque demande
low = "fast"           # demande simple
medium = "main"        # demande ordinaire
high = "reasoning"     # demande difficile
sticky = true          # une session garde son modèle jusqu'à la prochaine compaction
fallback = { main = ["fast"], reasoning = ["main"] }
```

### Qui répond à un message

```
classifier = true (défaut)
  message ─► classifieur (alias fast) ─┬─ simple    ─► low    (fast)
                                       ├─ ordinaire ─► medium (main)
                                       └─ difficile ─► high   (reasoning)
classifier = false
  message ─────────────────────────────────────────► main
panne avant le premier jeton
  main ─► fast ; reasoning ─► main       (fallback, fait par OpenRouter)
```

Conséquence directe : `penelope model set main …` ne suffit pas à tout faire passer par
ce modèle tant que le classifieur est actif, puisque `fast` et `reasoning` gardent leurs
valeurs. Deux façons de décider soi-même :

```bash
penelope config set models.routing.classifier false
```

(tout passe par `main` ; sur Telegram : `/model auto off`), ou garder l'adaptatif en
réglant chaque étage :

```bash
penelope model set reasoning openrouter:z-ai/glm-5.3
```

`penelope model list` (ou `/models` sur Telegram) affiche le routage en vigueur, alias et
modèles réels compris.

### Choisir le modèle d'une session

Sur Telegram, `/model` répond par l'état de la session (épinglée, ou automatique avec le
modèle du dernier message) et un bouton par modèle de conversation, plus « Automatique ».
Un clic épingle l'alias sur la session : tous ses messages l'utilisent, sans classifieur,
jusqu'au retour à « Automatique ». Les autres sessions ne sont pas touchées. En texte,
`/model reasoning` épingle et `/model auto` rend la main au routeur ; `/model main
openrouter:<id>` change, lui, ce que vise l'alias partout. En ligne de commande :

```bash
penelope session model reasoning
```

(`penelope session model` seul affiche l'état, `auto` revient à l'automatique, `--session`
vise une autre session que la courante). Une photo ou une demande d'image passe toujours
par `vision` ou `image`, même sur une session épinglée.

`sticky` compte plus qu'il n'y paraît : changer de modèle en pleine session casserait le
cache du provider et ferait payer tout le contexte une seconde fois. Seul le petit modèle
(`low`) ne colle jamais : un « bonjour » n'enferme pas la session sur `fast`, le message
suivant est reclassé. Un repli fait par OpenRouter est journalisé
(`llm.fallback_used`) et le modèle qui a réellement répondu est celui enregistré.

### OpenRouter

Chaque appel porte l'identifiant de session (`session_id`) : OpenRouter garde la session
sur le même provider amont, ce qui garde le cache de préfixe chaud, et regroupe les appels
dans ses journaux (onglet Sessions). Préférences de provider, envoyées seulement si elles
diffèrent du défaut :

```toml
[providers.openrouter.routing]
data_collection = "deny"   # uniquement des providers qui ne conservent pas les données
zdr = false                # true : uniquement des endpoints à rétention nulle
sort = ""                  # "price", "throughput" ou "latency"
ignore = []                # providers à éviter, par exemple ["deepinfra"]
quantizations = []         # par exemple ["fp8", "bf16"]
order = []                 # à éviter : un ordre imposé désactive le routage collant
```

### Coûts

Chaque appel au modèle est enregistré avec le coût **facturé** annoncé par OpenRouter
(`usage.cost`), les tokens (dont cache et raisonnement), le provider amont, et la requête
d'origine : une reprise après approbation compte pour la demande initiale, et le
classifieur est compté avec elle.

```bash
penelope usage
```

Par défaut, les sessions les plus chères, avec leur titre ou leur premier message. Autres
regroupements : `--by turn` (requêtes), `--by model`, `--by day`, `--by role`
(conversation ou classifieur), `--by upstream` ; filtres `--session <id>` et
`--since AAAA-MM-JJ`. Sur Telegram, `/budget` résume le jour, la session et ses requêtes
les plus chères ; `/budget sessions`, `/budget requêtes`, `/budget modèles` détaillent.

Le budget se règle à côté, en dollars :

```toml
[budget]
daily_usd = 20.0
session_usd = 5.0
alert_ratio = 0.8
```

`model set` signale un identifiant absent du catalogue (`known: false`) quand le catalogue
est chargé ; sinon, une faute de frappe ne se verra qu'au premier appel.

### Shell et bac à sable

`shell_exec` tourne sous Seatbelt : écriture limitée au workspace et au répertoire
temporaire, réseau autorisé (`sandbox.shell_network`, vrai par défaut ; sans réseau,
`gh auth status` croit le jeton invalide faute de pouvoir le vérifier). L'environnement
reste filtré : `PATH`, `HOME`, la langue, l'agent SSH et les emplacements de configuration,
jamais de jeton. Sur une machine dédiée à Pénélope, le bac à sable peut être levé pour le
shell :

```bash
penelope config set sandbox.default_profile full
```

Les approbations restent en place : c'est la carte `shell_exec` (« Toujours » compris) qui
décide.

### Messages vocaux

Un vocal (ou un fichier audio) envoyé sur Telegram est téléchargé, transcrit par le modèle
du rôle `stt`, montré en citation, puis traité comme un message tapé. Le coût de la
transcription est compté avec le rôle `stt`. Deux façons de transcrire.

Par OpenRouter, sans rien installer :

```bash
penelope model set stt openrouter:openai/whisper-large-v3
```

En local, avec whisper.cpp (accéléré par Metal sur Apple Silicon ; `ffmpeg` convertit
les vocaux Opus de Telegram) :

```bash
brew install whisper-cpp ffmpeg
```

```bash
mkdir -p ~/models && curl -L -o ~/models/ggml-large-v3-turbo-q5_0.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin
```

```bash
whisper-server -m ~/models/ggml-large-v3-turbo-q5_0.bin --host 127.0.0.1 --port 8080 --inference-path /v1/audio/transcriptions --convert -l fr
```

Le serveur doit tourner en permanence (un LaunchAgent comme celui de Pénélope, ou une
session `tmux`). Puis activer le provider local, qui vise `http://127.0.0.1:8080/v1` par
défaut ; l'alias `stt` livré (`openai_compat:whisper-default`) le désigne déjà :

```bash
penelope config set providers.local.enabled true
```

Sans provider local actif, un vocal reçoit une réponse qui dit quoi configurer, au lieu
d'être envoyé ailleurs.

### Ce que Pénélope sait d'elle-même

L'outil `self_status` lui donne son état complet : version et durée de fonctionnement,
modèle qui répond au tour et routage, configuration effective (secrets désignés par leur
nom, jamais par leur valeur), coûts du jour et de la session, file de travail, chemins, et
la machine : batterie et alimentation, disque, mémoire, charge, démarrage, système. Elle
l'appelle d'elle-même dès qu'on lui pose une question sur elle ou sur l'ordinateur.

Elle peut aussi changer un réglage à ta demande avec `config_set`, appliqué à chaud et
toujours soumis à approbation. Le bac à sable, les providers, Telegram, les outils et les
politiques exigent une double confirmation à chaque fois : une règle « Toujours » ne les
couvre pas. Les secrets et l'identifiant du propriétaire sont refusés : ils ne se règlent
qu'en ligne de commande.

### Serveurs MCP

Chaque serveur se déclare dans un fichier de `mcp.d` (`penelope paths` donne le
répertoire), un serveur par fichier :

```toml
# mcp.d/redmine.toml
command = "~/go/bin/redmine-mcp"
timeout = "30s"

[env]
REDMINE_URL = "https://projets.exemple.fr"
REDMINE_API_KEY = "${SECRET:redmine_api_key}"

[tool_policy]
delete_issue = "deny"
```

Le daemon lit `mcp.d` au démarrage puis dès qu'un fichier change (quelques secondes). Un
serveur dont les outils sont inconnus est lancé une fois pour les lister ; ensuite, il ne
démarre qu'au premier appel et s'arrête après `idle_timeout` d'inactivité. Une panne est
retentée à intervalle croissant (1 s à 5 min) ; après 8 échecs, le serveur passe en panne
jusqu'à `penelope mcp restart`. Un serveur qui ne comprend pas la sonde de protocole 2026
est rappelé en `initialize`, sans intervention.

Le modèle voit une ligne par serveur (« redmine : 12 outils ») et passe par `tool_search`,
`tool_describe` et `tool_call`. Avec `eager_schemas = true`, les outils d'un petit
serveur critique lui sont donnés directement. Le risque de chaque outil vient de ses
annotations (lecture, écriture, destructif, externe), corrigeable par `tool_risk` ; la
politique par classe (`mcp.policy`) décide de l'approbation, et `tool_policy` l'impose
outil par outil (un `deny` l'emporte toujours).

Chaque serveur stdio tourne sous le profil `mcp-stdio` : écriture limitée à
`mcp-data/<nom>` et au répertoire temporaire, réseau autorisé, secrets injectés dans son
environnement seulement. Si un serveur doit écrire ailleurs (un cache dans son dossier de
configuration, par exemple), son journal le montre et `penelope mcp list` le signale ; le
profil `full` se donne alors serveur par serveur, dans la déclaration
(`sandbox_profile = "full"`) et en l'autorisant :

```bash
penelope config set sandbox.allow_full_for '["mailbridge"]'
```

Administration, en ligne de commande comme sur Telegram (`/mcp`, `/mcp redmine`,
`/mcp restart|logs|test redmine`) :

```bash
penelope mcp list
```

`show <nom>` détaille état, déclaration et outils ; `test <nom>` (ou `--file x.toml`)
essaie une connexion à blanc ; `logs <nom>` donne le stderr du serveur ; `add <fichier>`,
`edit <nom> <champ> <valeur>`, `enable`, `disable` et `rm` modifient `mcp.d` ; `penelope
doctor` signale les serveurs en panne et les secrets manquants.

Un serveur HTTP protégé par OAuth passe en « autorisation requise » : Pénélope envoie
le lien sur Telegram (une fois par jour au plus), ou `/mcp auth <nom>` le redemande.
Après l'autorisation, le navigateur du téléphone tombe sur une page `127.0.0.1` en
erreur : copier l'adresse complète de la barre et la coller dans la conversation suffit
(valable 10 minutes, une seule fois). Par un tunnel SSH `-L 8765:127.0.0.1:8765`, le
retour est reçu directement. Sans Telegram :

```bash
penelope mcp auth <nom>
```

puis `penelope mcp auth <nom> --callback '<adresse collée>'`. Les jetons sont rangés
dans le SecretStore et rafraîchis avant chaque connexion ; ils ne partent que vers un
serveur en HTTPS (ou en boucle locale). Le sampling reste refusé et les formulaires
d'elicitation déclinés.

### Venir d'Hermes

Une instance Hermes se reprend en une commande, d'abord à blanc :

```bash
penelope import hermes --dry-run
```

Skills, `SOUL.md`, `AGENTS.md`, mémoire (`MEMORY.md`, `USER.md`) et serveurs MCP de
`config.yaml` sont listés avec ce qui serait fait ; rien n'est écrit. Sans `--dry-run`,
chaque serveur importé est essayé et marqué `ok`, `auth_required` (`penelope mcp auth
<nom>`) ou `failed`, et le rapport part aussi sur Telegram. Les secrets trouvés dans la
configuration ou le `.env` d'Hermes vont dans le trousseau, jamais dans `mcp.d`. Rien
d'existant n'est écrasé : un `SOUL.md` déjà présent reste en place et la version Hermes
est mise de côté pour fusion. `--path` pointe une autre racine que `~/.hermes`.

### Rappels et tâches planifiées

L'ordonnanceur passe toutes les dix secondes. « Rappelle-moi vendredi à 9 h d'appeler
Paul » devient un déclencheur `cron` à tir unique (`once`) dont la cible `notify` envoie
le message tel quel, sans appel au modèle ; « chaque lundi à 8 h, fais le point sur mes
tickets » garde le `cron` sans `once` avec une cible `prompt`, qui fait travailler
Pénélope à l'heure dite dans la conversation d'origine. Un rappel manqué pendant un
arrêt part une fois au redémarrage.

Les autres déclencheurs : `interval` (toutes les N minutes), `mcp_poll` (un outil MCP en
lecture interrogé à intervalle ; seuls les éléments nouveaux déclenchent, le premier
passage ne fait que mémoriser l'existant), `watch_file` (un fichier modifié) et `event`
(un événement du journal, `run.done` par exemple). La création passe par une approbation.

Sur Telegram, `/schedules` liste les déclencheurs et `/schedules pause|resume|rm|run <id>`
les gère. En ligne de commande :

```bash
penelope schedule list
```

Une « intention » est l'autre mémoire prospective : « quand on reparle du déploiement,
rappelle-moi le changelog » reste armée et revient dans le contexte du premier message
qui en parle (trois fois au plus, une fois par jour au plus).

### Photos et documents

Une photo part dans la conversation. Si le modèle de la session lit les images, il la
voit ; sinon le modèle de l'alias `vision` la décrit (texte visible recopié) et la
description rejoint le message. Plusieurs photos envoyées d'un coup forment un seul
message.

Un document PDF, DOCX, HTML, Markdown ou texte est ingéré : son texte devient une fiche
`vault/sources/<nom>.md`, découpée en passages que `mem_search` retrouve, et un résumé
revient sur Telegram. Le contenu d'un document est traité comme non fiable : jamais
rappelé automatiquement, toujours encadré quand Pénélope le lit. Une légende commençant
par `/mien` déclare un document rédigé par soi. Toute autre légende est une demande :
« quand expire ce contrat ? » part avec le document.

Pénélope propose au plus cinq faits à retenir ; rien n'entre en mémoire sans « Tout » sur
la carte (ou `penelope approve <id>`). Les autres fichiers sont rangés : un fichier texte
(CSV, JSON, log) devient un artefact lisible par `artifact_read`, un binaire est déposé
dans `<workspace>/telegram/`.

Déposer un fichier dans `vault/inbox/` en SSH fait la même chose : il est ingéré, le
bilan arrive sur Telegram et la boîte est vidée (un format refusé part dans
`inbox/refusés/`). Un PDF scanné sans couche texte n'est pas lu : il n'y a pas d'OCR.

### Mémoire qui apprend

Après un échange qui en vaut la peine (message un peu long, correction, règle énoncée),
le modèle de l'alias du rôle `memory_review` note au plus cinq candidats : préférence,
correction, décision, fait, écart. Rien n'est écrit dans le profil ni la mémoire à ce
moment-là : les candidats vont dans le journal du jour.

Chaque nuit (03:30, `memory.dreaming_cron`), la consolidation les passe à des règles
fixes : une préférence doit venir de toi et être formulée comme une règle (« toujours »,
« désormais ») ou revenir dans deux sessions, un fait doit être important ou rappelé,
un écart doit se répéter sur plusieurs jours, un contenu non fiable n'est jamais retenu.
Ce qui passe est confié au modèle, qui propose des modifications ligne par ligne,
vérifiées avant écriture ; une contradiction avec ce qui est déjà retenu devient une
question, un changement de défaut une proposition. Le digest du matin (08:00) résume la
nuit, les demandes en attente, les runs et la dépense de la veille.

```bash
penelope mem dream --dry-run
```

`/dream` lance une passe, `/appris 7` liste ce qui a été retenu, `/pratique <slug>`
affiche une pratique. Chaque modification garde son état antérieur :

```bash
penelope mem history --file profil.md
```

puis `penelope mem restore <id>`. `penelope vault check` signale un frontmatter cassé ou
un secret écrit à la main ; si le vault est un dépôt git, chaque passe fait un commit
`dream: AAAA-MM-JJ` (poussé si `memory.vault_git_remote` est renseigné).

### Workflows

Un workflow enchaîne des étapes : agent (Pénélope travaille jusqu'à `step_done()`),
sous-agent (contexte neuf, sortie JSON validée), commande shell, outil, question à
boutons, étapes en parallèle, sous-workflow, attente (délai, événement, cron) et
vérification des critères. Chaque run a son répertoire de travail et sa carte sur
Telegram, mise à jour à chaque étape.

```bash
penelope wf list
```

```bash
penelope wf run build-verify --param objectif="corriger le calcul de TVA"
```

Sur Telegram : `/wf`, `/run build-verify objectif=…`, `/runs`, `/resume <run>`. Une
question arrive avec ses boutons ; si l'étape attend une précision, le message suivant
la donne. Un outil soumis à approbation (écriture, push) envoie sa carte comme en
conversation, et le run reprend après la décision. Un arrêt du daemon reprend chaque run
à son étape courante sans refaire une commande ou un appel déjà passés.

Les runs se pilotent aussi en ligne de commande :

```bash
penelope wf control <run> pause
```

(`resume`, `cancel`, `retry-step`, `skip-step`, `goto:<étape>`, ou `answer --choice …
--input …`). Un run s'arrête en « bloqué » quand ses itérations, son budget ou sa durée
sont épuisés, ou quand aucune transition ne convient ; `/resume` le relance. Les
workflows se déposent dans `{data}/workflows/<id>.workflow.json` (validés au
chargement) ; le répertoire d'un run éphémère est effacé 7 jours après sa fin.

L'outil `image_generate` produit une image avec l'alias `image` et l'envoie sur
Telegram.

### Longues conversations

Quand une conversation approche le seuil de sa fenêtre (70 % par défaut, moins une marge
de 10 points), Pénélope fait résumer les anciens échanges en tâche de fond par l'alias du
rôle `compaction` (`summarizer`). La conversation ne s'arrête pas : le résumé est publié
à la fin du tour en cours. Les derniers échanges restent mot pour mot, les identifiants
(chemins, tickets, SHA, URLs) sont conservés tels quels, et un résumé existant est mis à
jour plutôt que refait. Rien n'est effacé : les échanges résumés restent consultables par
`history_grep` et `history_expand`.

`/compact` sur Telegram force un résumé tout de suite. En ligne de commande :

```bash
penelope session compact
```

Un résumé raté attend 1 min, puis 5, puis 15 avant un nouvel essai de fond ; `/compact`
lève cette attente. Si le provider refuse une requête trop longue, Pénélope résume une
fois et relance la même demande. Le coût apparaît sous le rôle `compaction` de
`/budget rôles` ; un budget atteint suspend les résumés de fond, pas `/compact`.

## 7. Premier essai en CLI

Dans un premier terminal, le daemon au premier plan (les journaux s'affichent) :

```bash
penelope daemon
```

Dans un second terminal, une question ponctuelle :

```bash
penelope chat "Bonjour, qui es-tu ?"
```

Ou une conversation interactive, avec approbation des outils sur place :

```bash
penelope chat
```

Dans ce mode, `/new` ouvre une nouvelle session, `/stop` arrête la génération, Ctrl-D
quitte. Quand un outil demande une autorisation (écrire un fichier, lancer une commande),
la CLI pose la question et affiche la suite du tour une fois la décision prise.

`penelope model list` affiche d'abord tes alias, puis la taille du catalogue OpenRouter,
chargé par le daemon au démarrage. Pour chercher dedans :

```bash
penelope model list --filter glm
```

## 8. Démarrer et surveiller

```bash
penelope start
```

```bash
penelope status
```

```bash
penelope stop
```

`penelope daemon` lance le processus au premier plan : c'est la forme utile pour déboguer
en SSH, puisque les journaux partent alors sur le terminal.

## 9. Sauvegarde et audit

```bash
penelope backup
```

La sauvegarde est cohérente même pendant l'écriture : elle passe par `VACUUM INTO`,
exécuté sur le fil écrivain hors transaction, et atterrit dans `backups/`.

```bash
penelope audit-verify
```

Recalcule la chaîne de hachage du journal d'événements et nomme le premier maillon rompu
s'il y en a un. Une purge RGPD conserve le hachage d'origine : purger n'invalide pas la
chaîne.

## 10. Mise à jour

Depuis les releases GitHub :

```bash
penelope upgrade
```

L'archive est vérifiée (somme SHA-256), le binaire courant gardé en
`<binaire>.previous`, puis le daemon redémarre sur la nouvelle version. S'il ne confirme
pas sa santé dans la minute, l'ancien binaire revient tout seul et Telegram le signale.
`penelope upgrade --check` indique seulement la dernière version, `--tag v0.3.1` en
choisit une, `--rollback` revient au binaire précédent. Le répertoire du binaire doit
être inscriptible par l'utilisateur du service ; sinon, ou pour une version pas encore
publiée, depuis le dépôt cloné sur la machine :

```bash
make deploy
```

`deploy` enchaîne `git pull --ff-only`, `cargo build --release`, remplace le binaire que
trouve le PATH (sudo seulement si son répertoire n'est pas inscriptible) et lance
`penelope restart`. `make update` s'arrête après la compilation, `make clean` libère les
Go de `target/`.

Au démarrage suivant, la reprise (§17) s'exécute : les tours interrompus sont remis en
file, les runs repartent à leur étape courante, les effets restés en vol deviennent des
questions plutôt que des relances. `penelope approvals` montre ce qui attend une réponse.

## 11. Ce qui n'est pas encore branché

Conversation (CLI et Telegram), approbations, catalogue de modèles, vocaux et serveurs
MCP, rappels et déclencheurs, résumé des longues conversations, photos et documents,
workflows, génération d'images, consolidation nocturne de la mémoire, autorisation
OAuth des serveurs MCP, mise à jour du binaire et import d'Hermes fonctionnent. Voir [progress.md](progress.md) pour
l'état exact.
