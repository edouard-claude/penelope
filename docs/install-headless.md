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

Le répertoire des journaux est créé en `0700` et chaque fichier en `0600`. Les secrets
connus (jeton du bot Telegram, clés d'API) sont masqués sur **toutes** les sorties, stderr
compris ; `penelope doctor` (contrôle `logs_secrets`) cherche un secret resté en clair dans
un journal plus ancien et dit quoi révoquer.

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
| `binary_signature` | Le binaire porte une signature stable, pas ad hoc | `SIGN_IDENTITY="Penelope Dev" make deploy` (section 10) |
| `vault_git` | Le vault est un dépôt git quand l'autocommit est actif | `penelope vault sync` |

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

**Réglages qui s'annulent.** Un réglage qui annulerait sa propre intention est refusé,
nommément : rôle ou palier de routage vers un alias absent, repli vers soi-même,
`budget.alert_ratio` hors de ]0, 1[, adresse privée dans `tools.http_allowlist` alors que
`tools.http_block_private_ips` la bloque. Un réglage qui en rend un autre inutile passe
avec un avertissement : plafond de session au-delà du plafond du jour, alias vers un
provider désactivé, action destructive moins protégée qu'une écriture, déclencheur
planifié pendant `telegram.quiet_hours`. `penelope config validate` et `penelope doctor`
listent ces contradictions, et le daemon les signale au démarrage.

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
flux coupé avant tout texte (429, surcharge)
  même modèle après 2 s ─► modèle de repli
flux coupé après du texte
  échec affiché, bouton « Réessayer »
```

Une erreur qui arrive **pendant** le flux, après la réponse HTTP, est traitée comme une
panne d'avant flux tant que rien n'a été montré (ni texte, ni appel d'outil) : un nouvel
essai du même modèle (après le `Retry-After` s'il est court), puis le modèle de repli, y
compris avec OpenRouter dont le repli ne joue qu'avant le flux. Si du texte est déjà
parti, rien n'est relancé en silence : le message d'échec dit que la réponse a été coupée
et le bouton « 🔁 Réessayer » relance la réponse sur la même conversation.

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
(conversation ou classifieur), `--by upstream`, `--by miss` (ratés de cache par cause) ;
filtres `--session <id>` et `--since AAAA-MM-JJ`. Chaque ligne donne les tokens d'entrée,
en cache et de sortie, et la part servie par le cache. Sur Telegram, `/budget` résume le
jour, la session, la taille du contexte et les requêtes les plus chères ; `/usage`,
`/usage turn`, `/usage model` ou `/usage miss` donnent tokens, cache et coût.

Le budget se règle à côté, en dollars :

```toml
[budget]
daily_usd = 20.0
session_usd = 5.0
alert_ratio = 0.8           # une alerte par périmètre au premier passage de 80 %
turn_checkpoint_usd = 1.0   # « Ce tour a coûté 1,05 $, je continue ? » à chaque dollar
show_turn_cost_usd = 0.5    # coût du tour ajouté à la réponse au-delà
delegate_after_calls = 10   # rappel de regrouper ou de déléguer tous les 10 appels
```

À 80 % d'un plafond (jour, session ou run), une seule notification arrive avec les trois
plus gros postes et leur part de cache ; à 100 %, le tour est suspendu. Un tour qui
enchaîne les appels d'outils paie tout le contexte à chaque appel : au-delà de
`turn_checkpoint_usd`, Pénélope demande si elle continue (▶️ Continuer, ⏹ Arrêter), et
le plafond de 24 appels au modèle compte les reprises après approbation.

**Cache de prompt.** Les fournisseurs facturent beaucoup moins cher un préfixe déjà vu.
Pour le garder, le contexte volatil (heure, rappel mémoire) reste attaché au message qu'il
accompagne, le raisonnement suit toujours les appels d'outil et jamais les réponses
finales, un changement d'instantané mémoire, de skill ou de serveur MCP attend une pause
de 5 min ou une compaction, et le fournisseur amont qui a servi l'appel précédent reste en
tête de `provider.order` pendant 10 min (sauf ordre imposé par
`providers.openrouter.routing`). Chaque raté est expliqué dans `penelope usage --by miss` :
premier appel, pause, préfixe, outils, modèle, historique réécrit, fournisseur amont
différent, ou préfixe intact non servi par le fournisseur.

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
serveur en HTTPS (ou en boucle locale).

Quand un serveur demande une confirmation, un formulaire ou l'ouverture d'un lien, la
carte arrive sur Telegram (Accepter, Refuser, Annuler) ; sans réponse avant
`elicitation_timeout` (10 min par défaut), la demande est annulée. Sans Telegram
configuré, Pénélope n'annonce pas cette capacité. Le sampling reste refusé.

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
revient sur Telegram. L'original, jamais modifié, est rangé dans `vault/attachments/` et
embarqué par la fiche (`![[nom.pdf]]`, propriété `source`) ; l'ingestion s'inscrit dans
`log.md`. Le contenu d'un document est traité comme non fiable : jamais
rappelé automatiquement, toujours encadré quand Pénélope le lit. Une légende commençant
par `/mien` déclare un document rédigé par soi. Toute autre légende est une demande :
« quand expire ce contrat ? » part avec le document.

Pénélope propose au plus cinq faits à retenir ; rien n'entre en mémoire sans « Tout » sur
la carte (ou `penelope approve <id>`). Les autres fichiers sont rangés : un fichier texte
(CSV, JSON, log) devient un artefact lisible par `artifact_read`, un binaire est déposé
dans `<workspace>/telegram/`.

Déposer un fichier dans `vault/inbox/` en SSH fait la même chose : il est ingéré, le
bilan arrive sur Telegram et la boîte est vidée (un format refusé part dans
`inbox/refusés/`). Un PDF scanné, sans couche texte, est lu par l'OCR de macOS
(Vision) : la première fois, le petit lecteur est compilé en quelques secondes, ce qui
demande les outils de développement Xcode (`xcode-select --install`) ; 50 pages au plus.

### Mémoire qui apprend

Après un échange qui en vaut la peine (message un peu long, correction, règle énoncée),
le modèle de l'alias du rôle `memory_review` note au plus cinq candidats : préférence,
correction, décision, fait, écart. Rien n'est écrit dans le profil ni la mémoire à ce
moment-là : les candidats vont dans le journal du jour.

Une conversation Telegram ne se ferme jamais : Pénélope la découpe en **épisodes**. Un
épisode se clôt après 2 h sans message, quand trois messages de suite s'éloignent du
sujet, ou sur `/new`. L'épisode clos est relu une fois en entier : un résumé rejoint le
journal du jour, et ce qui mérite d'être retenu devient des candidats. Le profil et la
mémoire de fond que voit le modèle sont figés pendant un épisode : ce qui est appris
apparaît à l'épisode suivant, sans casser le cache du provider en cours de route.

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
un secret écrit à la main.

**Règles dictées.** Une règle que tu énonces et que Pénélope note avec `mem_note` porte ta
citation exacte : retrouvée dans tes messages récents, elle compte comme venant de toi et
passe la nuit. Notée sans citation (ou avec une citation absente de tes messages), elle
n'est plus rejetée : la consolidation te demande « Tu confirmes cette règle ? » (bouton
Telegram ou `penelope approvals`), et ta réponse la fait promouvoir au rêve suivant. Les
règles rejetées par une version antérieure pour leur seule origine se remettent en file :

```bash
penelope mem retry-rejected
```

**Qualité de ce qui est retenu.** Une entrée est un fait complet : un texte tronqué
(« … »), une phrase incomplète ou un pronom sans sujet est rejeté ; au-delà de 300
caractères, l'entrée est scindée en phrases, et une phrase inexploitable est écartée. Un
état passager (« deal en cours », « propale non lue », « arbitrage ») part dans
`projets.md` avec `expire: AAAA-MM-JJ` (30 jours) et n'est plus injecté après cette date.
Une donnée client, financière ou de sécurité (montant, marge, faille, mot de passe) porte
`sensible: oui` : jamais injectée d'office, toujours trouvable par `mem_search`. Un fait sur
la configuration de Pénélope elle-même n'est pas retenu, `self_status` fait foi. Les deux
annotations se posent aussi à la main et sont relues par `penelope mem reindex`. Quand le
niveau Cœur (`memoire.md`) dépasse `memory.core_budget_tokens`, `DREAMS.md` le signale.

**Historique git du vault.** Avec `memory.vault_git_autocommit` actif (15 min par défaut,
`0s` pour désactiver), le vault devient un dépôt au démarrage (`.gitignore`, commit
initial, identité locale si git n'en a pas). Les éditions faites à la main (éditeur, SSH)
sont commitées à cette période, chaque rêve fait son commit `rêve du AAAA-MM-JJ (d_…) : N
promues`, poussé si `memory.vault_git_remote` est renseigné. `doctor` et le digest
avertissent si le vault reste hors git. Ce que le dernier rêve a changé :

```bash
penelope mem diff --since dream
```

sans `--since`, les changements pas encore commités.

**Accueil.** Sur une instance neuve, le profil ne se remplit qu'au fil des jours. `/accueil`
(ou `penelope onboard`) pose neuf questions, une à la fois, avec des boutons quand c'est
possible : rôle, clients, projets, outils, tutoiement, longueur et langue des réponses, ce
que Pénélope ne doit jamais faire, ce qu'elle doit toujours faire ou éviter. Chaque
question est écrite dans `accueil/accueil-AAAA-MM-JJ.md` avant d'être posée et sa réponse se range
dessous : la séance se reprend après une pause. À la fin, un récapitulatif montre ce qui
change dans `profil.md` (directives Toujours, Jamais, Préférer, Éviter) et `memoire.md` ;
rien n'est écrit sans validation, et chaque entrée garde sa provenance vers sa question.
`/accueil limites` (ou `profil`, `outils`, `style`) ne rejoue qu'une partie et remplace
l'ancienne réponse. Le premier message d'une instance au profil vide propose l'accueil.

**Audit.** `/audit` ou `penelope mem audit` note la mémoire sur 100 avec un barème fixe
(v2, le lint du wiki compte dans la qualité) sur cinq axes : connaissance du propriétaire, portée (serveurs MCP, sources,
projets), savoir-faire, autonomie, qualité (provenance, liens morts, concepts à définir,
contradictions, vecteurs). Chaque axe dit pourquoi et propose une seule prochaine action ;
l'audit est gardé dans `audits/audit-AAAA-MM-JJ.md` avec l'écart depuis le précédent, et le
digest du lundi en donne le score.

**Wiki de concepts.** Le résumé d'un document reçu nomme aussi ses concepts (personne,
client, projet, terme métier) : chacun crée ou complète `concepts/<slug>.md` (définition,
alias, sources), dédoublonné par les mots puis par le sens. La fiche source reçoit une
section `## Concepts` avec ses liens `[[slug]]`, les lignes de `memoire.md` et `projets.md`
qui citent le concept aussi, `concepts/_a-definir.md` liste les termes employés sans
définition (repris par le digest du matin) et `index.md` sert de point d'entrée, y compris
dans un éditeur de wiki Markdown. L'outil `mem_neighbors` parcourt ce graphe.

**Ce qui est indexé.** Toutes les entrées `- …` des fichiers Markdown du vault et les
passages des fiches `sources/`. Ne le sont pas, à dessein : `inbox/` (en attente
d'ingestion), `accueil/`, `audits/`, `archive/`, `attachments/` (originaux, indexés par
leur fiche), `index.md`, `log.md`, `concepts/_a-definir.md`, les fichiers cachés. Tout le reste qui échappe à l'index (un PDF déposé à la main, un
texte sans entrées, des lignes sans uid) est nommé par `penelope vault check`,
`penelope doctor` et le journal du daemon ; une recherche mémoire vide le rappelle au
modèle, qui dit « je ne trouve rien dans ce que j'ai indexé » plutôt que « cela n'existe
pas ».

**Wiki Markdown.** Le vault reste un wiki Markdown valide à tout moment, même édité à la
main pendant que Pénélope écrit :

- chaque note porte des propriétés YAML : `type` (`journal`, `source`, `concept`, `profil`,
  `memoire`, `accueil`, `audit`, `revue`…), `created` et `updated` en `AAAA-MM-JJ`,
  `aliases` et `tags` en listes, `date` pour le journal ;
- chaque entrée se termine par son identifiant de bloc (`- texte ^01J9…`), que vise un
  wikilink `[[memoire#^01J9…]]` ; les autres annotations restent en commentaires ;
- un nom de fichier est unique dans tout le vault (sinon le wikilink porte le chemin) ;
  accueil et audits s'appellent `accueil-AAAA-MM-JJ` et `audit-AAAA-MM-JJ` ;
- `log.md` reçoit, en ajout seul, une ligne `## [AAAA-MM-JJ] <op> | <titre>` par ingestion,
  rêve, accueil et lint :

```bash
grep "^## \[" log.md
```

Avant d'écrire, Pénélope relit le fichier : une édition faite entre-temps est gardée et
l'opération réappliquée ligne à ligne ; si la ligne visée a elle-même changé, l'opération
est reportée. Les dossiers cachés (configuration d'éditeur) ne sont jamais touchés. Le rêve
passe le lint et l'écrit dans `DREAMS.md` et `log.md` ; à la demande :

```bash
penelope vault lint
```

liens non résolus, notes orphelines et impasses, alias et noms en double, identifiants de
bloc invalides ou dupliqués, propriétés mal typées, puis les entrées expirées et les
contradictions à trancher (proposées, jamais corrigées en silence). Le digest du matin
cite les entrées du rêve (`[[memoire#^…]]`) et le journal de la veille. Un vault d'une
version antérieure est converti au premier démarrage (et par `penelope mem reindex`) :
uid en identifiants de bloc, `alias` en `aliases`, noms d'accueil et d'audit (wikilinks
réécrits), originaux déplacés dans `attachments/`, propriétés posées ; les uid et leur
provenance ne changent pas. La skill livrée `wiki-markdown` donne ces règles au modèle.

**Recherche par le sens.** Le rappel mémoire, `mem_search`, le déclenchement des
intentions et `tool_search` croisent les mots et les vecteurs : une entrée sur
« l'automobile » répond à une question sur « la voiture ». Les vecteurs viennent de
l'alias `embedding`, par défaut `openrouter:openai/text-embedding-3-small` (quelques
centimes par million de tokens, aucun serveur local), et sont calculés en fond après
chaque tour, chaque réindexation et chaque inscription d'outils MCP, puis mis en cache par
contenu. Au tour, le vecteur du message a 1,5 s ; au-delà, ou si l'alias ne répond pas, la
recherche reste lexicale pour ce tour. `penelope doctor` vérifie l'alias, `self_status` dit
si la recherche est hybride ou lexicale seule et combien de vecteurs sont calculés. Après
un changement de modèle :

```bash
penelope mem reindex --embeddings
```

Une configuration qui gardait l'ancien défaut (`openai_compat:embeddings-default`, serveur
local désactivé) bascule au chargement sur le défaut OpenRouter, avec un avertissement
dans le journal.

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

### Retrouver une conversation

Chaque session reçoit un titre de quelques mots après son premier échange (réglage
`context.auto_title`, modèle `fast`) ; `/title` ou `penelope session title <session>
<titre>` le remplace, et un titre posé à la main n'est jamais écrasé. `/sessions` et
`penelope session list` affichent titres et dates. Sur Telegram, `/sessions` rend un
bouton par session : un clic lie le chat à cette session, « ⋯ » propose de la forker, de
la renommer ou de la fermer, et les sessions fermées n'apparaissent qu'à la demande.
`/switch` accepte un identifiant, un préfixe unique ou un titre ; `/close` ou
`penelope session close <session>` arrête une session et vide sa file.

Une seule session écrit dans un chat : celle qui a le focus (la dernière choisie par
`/new`, `/fork`, `/switch` ou le menu). Une session quittée finit son tour en cours, mais
sa réponse, ses messages et ses approbations sont mis de côté derrière une seule
notification « 📬 2 réponses en attente dans « Titre » » ; son bouton « Basculer »
revient sur la session et envoie tout dans l'ordre. Ses messages encore en file sont
abandonnés. Pour retrouver un sujet d'une session
fermée, Pénélope cherche dans toutes les sessions (`history_grep` en `scope: all`, ou
`history_expand_query` avec la question en phrase) et cite le titre et la date de la
session d'où vient chaque extrait.

### Gros résultats d'outils

Une page web lue par `http_fetch` arrive en texte lisible (titres, listes, liens) ; le
HTML d'origine reste relisible en artefact. Au-delà de `context.large_payload_tokens`
(25 k par défaut), un résultat d'outil part en artefact avec un aperçu du début et de la
fin, quelle que soit la fenêtre du modèle : un modèle à un million de tokens ne garde pas
un résultat de 175 k entier. Une longue liste de fichiers (`fs_list` récursif) est
résumée par dossier, la liste complète en artefact.

### Longues conversations

Quand une conversation approche le seuil de sa fenêtre (70 % par défaut, moins une marge
de 10 points), Pénélope fait résumer les anciens échanges en tâche de fond par l'alias du
rôle `compaction` (`summarizer`). Le seuil est aussi plafonné en valeur absolue par
`context.max_prompt_tokens` (120 000 par défaut, `0` pour s'en passer) : sur un modèle à
1,3 M de tokens, la compaction part vers 103 k au lieu de 917 k. Une fenêtre immense sert
à ne jamais échouer, pas à renvoyer 500 k tokens à chaque appel ; relever le plafond garde
plus de conversation mot pour mot, au prix de chaque appel. `/budget` et `self_status`
donnent la taille du contexte au dernier appel et le seuil de compaction. La conversation ne s'arrête pas : le résumé est publié
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
choisit une, `--rollback` revient au binaire précédent.

Signature : quand les releases sont signées, `penelope upgrade` vérifie `SHA256SUMS`
avec minisign avant de faire confiance aux sommes. La clé publique vient de
`upgrade.minisign_pubkey`, sinon de celle intégrée au binaire de release ; dès qu'une clé
est connue, une release non signée est refusée. Pour signer les releases du dépôt, une
fois :

```bash
minisign -G -W -p minisign.pub -s minisign.key
```

```bash
gh secret set MINISIGN_SECRET_KEY < minisign.key
```

```bash
gh variable set MINISIGN_PUBLIC_KEY --body "$(tail -n 1 minisign.pub)"
```

puis ranger `minisign.key` hors de la machine. Les binaires publiés ensuite portent la
clé publique et n'acceptent plus que des releases signées. Le répertoire du binaire doit
être inscriptible par l'utilisateur du service ; sinon, ou pour une version pas encore
publiée, depuis le dépôt cloné sur la machine :

```bash
make deploy
```

`deploy` enchaîne `git pull --ff-only`, `cargo build --release`, remplace le binaire que
trouve le PATH (sudo seulement si son répertoire n'est pas inscriptible) et lance
`penelope restart`. `make update` s'arrête après la compilation, `make clean` libère les
Go de `target/`.

### Signature locale

Un binaire compilé n'a qu'une signature ad hoc dont l'exigence désignée change à chaque
build : macOS redemande alors l'accès au Trousseau après chaque `make deploy`. Signé avec
un certificat stable et un identifiant fixe, un « Toujours autoriser » donné une fois
reste valable. Une fois :

1. Trousseau d'accès > Assistant de certification > Créer un certificat : nom « Penelope
   Dev », type d'identité « Racine auto-signée », type de certificat « Signature de code ».
2. Dans `~/.zshrc` :

   ```bash
   export SIGN_IDENTITY="Penelope Dev"
   ```

3. Premier `make deploy` **depuis une session graphique** (pas en SSH, où `codesign` ne
   peut pas demander l'accès à la clé) : « Toujours autoriser » pour `codesign`, puis pour
   Pénélope au premier accès au Trousseau.

`make build` signe alors le binaire (`make sign` seul le re-signe), avec l'identifiant
`io.github.edouard-claude.penelope` (`SIGN_IDENTIFIER` pour un autre). Pour
`penelope upgrade`, `upgrade.codesign_identity = "Penelope Dev"` re-signe le binaire
téléchargé avant la bascule ; sans ce réglage, l'upgrade prévient que l'autorisation sera
redemandée. `penelope doctor` affiche le type de signature du binaire en cours. Le
workflow CI « Signature macOS » vérifie, avec une identité jetable, que deux builds signés
gardent la même exigence désignée.

Au démarrage suivant, la reprise (§17) s'exécute : les tours interrompus sont remis en
file, les runs repartent à leur étape courante, les effets restés en vol deviennent des
questions plutôt que des relances. `penelope approvals` montre ce qui attend une réponse.

## 11. Ce qui n'est pas encore branché

Tout ce que décrit ce guide fonctionne. Restent : le mode webhook de Telegram,
l'interprétation de `.penelope/deploy.toml` (le déploiement passe par les cibles `make`),
et l'OCR des pages scannées d'un PDF qui a aussi du texte. Voir
[progress.md](progress.md) pour l'état exact.
