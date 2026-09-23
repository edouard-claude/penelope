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

**Ce qui est rédigé, et où.** Journaux, événements, demandes d'approbation stockées et
textes de la file Telegram passent par la même rédaction : secrets enregistrés, formats
connus (clés de fournisseurs, bearer, JWT, clé privée), affectations (`password = …`,
`"password": "…"`, `'x-api-key': '…'`, `KEY = "…"`), numéros de carte, jetons longs et
aléatoires, et toute valeur que l'agent a lue ainsi plus tôt (un fichier de configuration,
une sortie de commande) puis recopiée ailleurs. La carte d'approbation montre la commande
exacte, valeurs masquées (« 🔒 2 valeur(s) masquée(s) ») ; la commande **exécutée**, un
`fs_write` ou tout ce qui s'exécute ne sont jamais modifiés. La consigne de `shell_exec`
demande de ne pas recopier un secret lu dans une commande, mais de le lire dans son fichier
ou une variable d'environnement. `penelope doctor` (contrôle `stored_secrets`) cherche un
secret en clair dans les demandes et la file des 30 derniers jours : écrit avant cette
rédaction, il est à considérer comme exposé.

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

Chaque contrôle en échec rend une commande de correction. `doctor` vérifie d'abord ce qui
ne dépend pas du daemon (binaire, fichier de configuration) puis lui demande ses propres
contrôles : un daemon arrêté ou figé devient le premier contrôle en échec du rapport, pas
une commande qui pend. Toute commande attend au plus 15 s une réponse du daemon (sauf les
commandes longues par nature : `chat`, `session compact`, `mem dream`, `backup`, `upgrade`…),
puis sort avec le code 7 et la marche à suivre ; `--timeout 60` attend plus, `--timeout 0`
sans limite. Les contrôles propres à macOS :

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
exécutable, jamais via un shell. La valeur d'un secret ne lui est jamais passée en
argument : elle part sur son entrée standard (`security -i`, valeur en hexadécimal), si
bien qu'aucun `ps` ne la voit passer, même pendant l'écriture. Seul l'index des **noms**
est stocké en clair : `dump-keychain` exigerait un déverrouillage interactif, impossible
sans écran.

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

**Les secrets longs.** `security`, le binaire du Trousseau, n'accepte que 4 096 octets par
commande. Au-delà, la fin était relue comme des commandes et `security` en recopiait des
morceaux dans sa sortie d'erreur : c'est ainsi qu'un `Grant` Codex de 4 Ko est parti en
clair sur Telegram le 20/09 (#148). Une valeur trop longue est désormais écrite en
plusieurs items (`penelope.<nom>#0`, `#1`, …) avec un item de tête qui dit combien ;
`penelope secret list` ne montre que le nom logique, `get` réassemble et `rm` efface tout.
Les secrets courts gardent leur forme d'avant : rien à migrer. Une écriture refusée ne
recopie **jamais** la sortie de `security`, seulement son code de retour.

`penelope doctor` écrit, relit et efface un secret de 8 Ko (`secret_roundtrip`) : un
Trousseau verrouillé se voit avant qu'un vrai secret le rencontre.

**Si un secret est parti en clair.** `penelope doctor` le signale (`stored_secrets`,
`logs_secrets`). Dans l'ordre : révoquer ou renouveler le secret à la source, supprimer les
messages concernés dans la conversation Telegram, et laisser le passage d'entretien
réécrire les lignes déjà en file — il repasse le rédacteur sur `tg_outbox` une fois, avec
les règles du jour, sans attendre les quatre-vingt-dix jours de rétention.

## 5. Configuration

```bash
penelope config validate
```

Hors daemon, donc utilisable avant le premier démarrage et en CI. Chaque clé, sa valeur
par défaut et son rôle sont dans la [référence des clés](#référence-des-clés).

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

Une modification est publiée comme une génération immuable. Les consommateurs relisent la
valeur au moment indiqué ci-dessous : un tour garde seulement ses choix collants (modèle et
routage déjà choisis), pas les gardes des outils.

| Clés acceptées par `config_set` | Effet | Garantie |
|---|---|---|
| `sandbox.workspaces`, `sandbox.default_profile`, `sandbox.shell_network`, `sandbox.deny_read` | appel suivant | Les chemins, le profil, le réseau et les lectures interdites sont recalculés avant chaque outil. |
| `tools.*`, `models.aliases.*`, `models.roles.*` hors `chat_default`, `budget.*`, autres clés lues par leur sous-système | appel suivant | La génération publiée est lue au prochain appel concerné ; un appel déjà parti n'est pas interrompu. |
| `owner.language`, `models.roles.chat_default`, `models.routing.*` | tour suivant | Le résultat dit `au prochain tour` et précise que les outils du tour gardent l'ancienne valeur. |
| `store.path`, `rpc.socket`, `telegram.token` | redémarrage | Seuls chemins autorisés à demander un redémarrage ; les secrets et l'identité restent refusés par `config_set`. |

`config_set` rend ce moment dans `applied`. Pour `sandbox.workspaces`, le nouvel espace est
donc utilisable par `fs_*`, `git_*`, `shell_exec` et `image_inspect` dès l'appel suivant du
même tour. Un chemin existant est enregistré sous sa forme réelle (casse et liens
symboliques résolus) ; le résultat signale cette correction. Un chemin inexistant reste
accepté avec un avertissement. Le modèle déjà choisi pour le tour ne change jamais au
milieu de sa réponse.

**Le fichier reste le vôtre.** `config.toml` ne porte que les clés qui s'écartent des
valeurs par défaut, et `penelope config set`, `/model` ou un réglage fait depuis Telegram
ne réécrivent que la clé visée : commentaires, ordre et édition faite en SSH sont
conservés, et le fichier n'acquiert pas les clés nouvelles d'une version tant que personne
ne les pose. Une clé que le binaire ne connaît pas (écrite par une version plus récente, ou
faute de frappe) est ignorée et nommée par `penelope config validate`, `penelope doctor`
et le journal de démarrage, jamais fatale : un retour à la version précédente
(`penelope upgrade --rollback`) redémarre donc sur le fichier laissé par la suivante.
`penelope config set` refuse toujours une clé qui n'existe pas.

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

### Référence des clés

Tirée des commentaires de `crates/penelope-kernel/src/config.rs` et des valeurs par
défaut ; le test `docs` échoue si une clé manque ou si la table est périmée. Une clé
« sans effet dans cette version » est acceptée mais pas encore lue.

<!-- reference:config:debut (générée : UPDATE_DOCS=1 cargo test -p penelope-evals --test docs) -->
**[owner]**

| Clé | Défaut | Rôle |
|---|---|---|
| `owner.telegram_user_id` | `0` | Identifiant Telegram du propriétaire : le seul compte auquel le bot répond (0 : canal fermé). |
| `owner.timezone` | `"Indian/Reunion"` | Fuseau horaire du propriétaire : planifications, digest, date du jour. |
| `owner.language` | `"fr"` | Langue des réponses. |

**[telegram]**

| Clé | Défaut | Rôle |
|---|---|---|
| `telegram.token` | `"${SECRET:telegram_bot_token}"` | Jeton du bot, par référence au magasin de secrets. |
| `telegram.mode` | `"polling"` | Réception des messages : `polling` (long polling) ou `webhook` (pas encore servi). |
| `telegram.topics` | `true` | Sujets de forum Telegram. Sans effet dans cette version. |
| `telegram.rich_messages` | `false` | Rendu riche natif de la Bot API plutôt que HTML. |
| `telegram.quiet_hours` | `"22:00-07:00"` | Heures calmes `HH:MM-HH:MM` : les notifications non urgentes attendent la fin de la plage. |
| `telegram.api_base` | `"https://api.telegram.org"` | Adresse de la Bot API. |
| `telegram.poll_timeout_s` | `50` | Attente d'un appel `getUpdates` en long polling, en secondes. |
| `telegram.rate_per_chat_per_s` | `1.0` | Messages envoyés au plus par seconde, par chat. |
| `telegram.text_limit` | `4096` | Taille maximale d'un message. Sans effet dans cette version. |
| `telegram.caption_limit` | `1024` | Taille maximale d'une légende. Sans effet dans cette version. |
| `telegram.max_fragments` | `3` | Fragments au-delà desquels un avis interne (digest du matin, rapport de veille) part en document plutôt qu'en chapelet de messages (issue #145). 0 : jamais. |
| `telegram.draft_interval_ms` | `700` | Intervalle entre deux mises à jour du brouillon de réponse, en millisecondes (300 au moins). |
| `telegram.webhook_url` | `""` | Adresse du webhook. Sans effet dans cette version. |
| `telegram.allow_groups` | `false` | Ancien interrupteur des groupes, sans effet depuis 0.17.4 : un groupe s'ouvre en ajoutant son identifiant à `telegram.allowed_chats`. |
| `telegram.allowed_chats` | `[]` | Conversations de groupe autorisées, par identifiant (`-100…` pour un supergroupe) : le propriétaire y parle, y compris en administrateur anonyme ; un sujet donne une session. `penelope doctor` liste les conversations refusées récemment avec leur identifiant. |
| `telegram.text_group_window_ms` | `2000` | Attente après un morceau qui ressemble à une coupure de Telegram (4 000 caractères ou plus) ou un message transféré, en millisecondes : les morceaux d'un même envoi forment un seul tour. Un message court tapé part tout de suite. 0 : un message, un tour. |
| `telegram.burst_messages` | `5` | Messages regroupés à partir desquels Pénélope demande quoi en faire au lieu de répondre à chacun. 0 : jamais. |
| `telegram.burst_chars` | `20000` | Caractères cumulés à partir desquels elle demande de même. 0 : jamais. |
| `telegram.home.chat` | `0` | Identifiant du chat (un groupe : `-100…`). 0 : le chat privé du propriétaire. |
| `telegram.home.topic` | `0` | Sujet du groupe (`message_thread_id`). 0 : le sujet « Général ». |

**[providers]**

| Clé | Défaut | Rôle |
|---|---|---|
| `providers.openrouter.api_key` | `"${SECRET:openrouter_api_key}"` | Clé d'API, par référence au magasin de secrets. |
| `providers.openrouter.base_url` | `"https://openrouter.ai/api/v1"` | Adresse de l'API OpenRouter. |
| `providers.openrouter.request_retries` | `3` | Nouvelles tentatives sur erreur transitoire **avant** le flux (5xx, délai de connexion, limite de débit) : attente de 1 s, 2 s, 4 s. 0 : aucune. |
| `providers.openrouter.stream_idle_timeout` | `"120s"` | Silence toléré **pendant** un flux : au-delà, le flux est coupé et relancé. Tout octet reçu, commentaire compris, remet le compteur à zéro. |
| `providers.openrouter.catalog_refresh` | `"6h"` | Période de rechargement du catalogue de modèles. |
| `providers.openrouter.referer` | `"https://github.com/edouard-claude/penelope"` | Attribution (`HTTP-Referer`, `X-OpenRouter-Title`, `X-OpenRouter-Categories`). |
| `providers.openrouter.title` | `"Penelope"` | Titre d'attribution (`X-OpenRouter-Title`). |
| `providers.openrouter.categories` | `"personal-agent"` | Catégories d'attribution (`X-OpenRouter-Categories`). |
| `providers.openrouter.routing.allow_fallbacks` | `true` | OpenRouter peut passer à un autre provider du même modèle en cas d'échec. |
| `providers.openrouter.routing.order` | `[]` | Ordre imposé des providers. Attention : il désactive le routage collant, donc le cache de préfixe entre deux tours. |
| `providers.openrouter.routing.data_collection` | `"allow"` | `deny` : uniquement des providers qui ne conservent pas les données. |
| `providers.openrouter.routing.require_parameters` | `false` | Uniquement les providers qui acceptent tous les paramètres de la requête. |
| `providers.openrouter.routing.zdr` | `false` | Uniquement des endpoints à rétention nulle (ZDR). |
| `providers.openrouter.routing.sort` | `""` | `price`, `throughput` ou `latency` ; vide = répartition par défaut d'OpenRouter. |
| `providers.openrouter.routing.only` | `[]` | Providers autorisés, à l'exclusion des autres ; vide : tous. |
| `providers.openrouter.routing.ignore` | `[]` | Providers exclus. |
| `providers.openrouter.routing.quantizations` | `[]` | Quantifications acceptées (`fp8`, `bf16`…) ; vide = toutes. |
| `providers.openrouter.enabled` | `true` | Fournisseur actif. |
| `providers.local.kind` | `"openai_compat"` | Type d'endpoint (`openai_compat`). |
| `providers.local.base_url` | `"http://127.0.0.1:8080/v1"` | Adresse de l'endpoint OpenAI-compatible. |
| `providers.local.api_key` | `""` | Clé éventuelle, par référence au magasin de secrets. |
| `providers.local.enabled` | `false` | Endpoint actif. |
| `providers.local.models` | `[]` | Modèles servis par l'endpoint. |
| `providers.local.stream_idle_timeout` | `"120s"` | Silence toléré pendant un flux, comme pour OpenRouter. |
| `providers.local.context_window` | `32768` | Fenêtre de contexte annoncée pour les modèles servis par cet endpoint, quand `GET /models` ne la donne pas. |
| `providers.codex.enabled` | `false` | Fournisseur actif. Faux tant que le compte n'est pas connecté (`penelope model auth codex`). |
| `providers.codex.base_url` | `"https://chatgpt.com/backend-api/codex"` | Adresse du backend Codex. |
| `providers.codex.issuer` | `"https://auth.openai.com"` | Serveur d'autorisation du compte ChatGPT. |
| `providers.codex.client_id` | `"app_EMoamEEZ73f0CkXaXp7hrann"` | Identifiant du client OAuth, celui de Codex CLI. |
| `providers.codex.originator` | `"codex_cli_rs"` | En-tête `originator` envoyé au backend. Le serveur filtre cette valeur : la changer sans raison donne un 403 sur toutes les requêtes. |
| `providers.codex.client_version` | `"0.149.0"` | Version de client annoncée (`User-Agent`, `?client_version=`). Épinglée, mise à jour à la main quand le backend exige plus récent : le catalogue et certains identifiants de modèle en dépendent, et une version trop ancienne en fait disparaître (issue #148). |
| `providers.codex.stream_idle_timeout` | `"120s"` | Silence toléré pendant un flux, comme pour OpenRouter. |
| `providers.codex.request_retries` | `3` | Nouvelles tentatives sur erreur transitoire avant le flux (5xx, coupure). Un 429 de quota n'est jamais rejoué. |
| `providers.codex.reasoning_summary` | `"auto"` | Résumé de raisonnement demandé (`auto`, `concise`, `detailed`, ou vide). |
| `providers.codex.verbosity` | `"medium"` | Verbosité du texte rendu (`low`, `medium`, `high`). |
| `providers.codex.quota_alert_ratio` | `0.8` | Part de la fenêtre de quota qui déclenche une alerte (0 à 1). |
| `providers.codex.quota_stop_ratio` | `0.95` | Part de la fenêtre de quota au-delà de laquelle le fournisseur se met en retrait et laisse le repli jouer (0 à 1). |
| `providers.codex.models` | `["gpt-6-astra","gpt-5.6-sol","gpt-5.6-terra","gpt-5.6-luna","gpt-5.5","gpt-5.4"]` | Modèles servis, en repli quand `GET /models` ne répond pas. |
| `providers.extra.<nom>.kind` | – | Type d'endpoint (`openai_compat`). |
| `providers.extra.<nom>.base_url` | – | Adresse de l'endpoint OpenAI-compatible. |
| `providers.extra.<nom>.api_key` | – | Clé éventuelle, par référence au magasin de secrets. |
| `providers.extra.<nom>.enabled` | – | Endpoint actif. |
| `providers.extra.<nom>.models` | – | Modèles servis par l'endpoint. |
| `providers.extra.<nom>.stream_idle_timeout` | – | Silence toléré pendant un flux, comme pour OpenRouter. |
| `providers.extra.<nom>.context_window` | – | Fenêtre de contexte annoncée pour les modèles servis par cet endpoint, quand `GET /models` ne la donne pas. |

**[models]**

| Clé | Défaut | Rôle |
|---|---|---|
| `models.aliases.embedding` | `"openrouter:openai/text-embedding-3-small"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.fast` | `"openrouter:deepseek/deepseek-v4-flash"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.image` | `"openrouter:google/gemini-3.1-flash-image"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.main` | `"openrouter:deepseek/deepseek-v4-pro"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.reasoning` | `"openrouter:z-ai/glm-5.2"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.stt` | `"openai_compat:whisper-default"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.summarizer` | `"openrouter:deepseek/deepseek-v4-flash"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.tts` | `"openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.aliases.vision` | `"openrouter:google/gemini-3.1-flash-image"` | Alias de modèle vers un identifiant `fournisseur:modèle` (§10.2). |
| `models.roles.chat_default` | `"main"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.classifier` | `"fast"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.code` | `"reasoning"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.compaction` | `"summarizer"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.embedding` | `"embedding"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.image_describe` | `"vision"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.image_generate` | `"image"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.image_locate` | `"vision"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.memory_review` | `"fast"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.stt` | `"stt"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.roles.tts` | `"tts"` | Rôle vers alias : conversation, classification, compaction, relecture de mémoire, code, images, embeddings, transcription. |
| `models.routing.classifier` | `true` | Classer la complexité d'un message pour choisir l'alias. |
| `models.routing.low` | `"fast"` | Alias d'un message simple. |
| `models.routing.medium` | `"main"` | Alias d'un message moyen. |
| `models.routing.high` | `"reasoning"` | Alias d'un message complexe. |
| `models.routing.sticky` | `true` | Garder l'alias choisi pour la session (sauf l'alias `low`). |
| `models.routing.fallback.main` | `["fast"]` | Alias de repli, dans l'ordre, quand un modèle ne répond pas. |
| `models.routing.fallback.reasoning` | `["main"]` | Alias de repli, dans l'ordre, quand un modèle ne répond pas. |
| `models.locate_frame` | `"auto"` | Repère des coordonnées que rend le modèle du rôle `image_locate` : `auto` (déduit de la famille du modèle et des valeurs rendues), `pixels` (pixels de l'image) ou `per_mille` (0 à 1000 sur chaque axe, comme UI-TARS et Qwen3-VL). `image_inspect` ramène toujours les points en pixels de l'image, et refuse ceux qui ne tiennent pas dans le repère. |

**[budget]**

| Clé | Défaut | Rôle |
|---|---|---|
| `budget.daily_usd` | `20.0` | Plafond de dépense par jour, en dollars. |
| `budget.session_usd` | `5.0` | Plafond de dépense par session, en dollars. |
| `budget.run_usd` | `5.0` | Plafond de dépense par run de workflow, en dollars. |
| `budget.alert_ratio` | `0.8` | Part d'un plafond à partir de laquelle une alerte part. |
| `budget.turn_checkpoint_usd` | `1.0` | Coût d'un tour de conversation à chaque multiple duquel Pénélope demande si elle continue (issue #19). 0 : jamais. |
| `budget.show_turn_cost_usd` | `0.5` | Coût d'un tour au-delà duquel la réponse finale l'indique. 0 : jamais. |
| `budget.delegate_after_calls` | `10` | Nombre d'appels au modèle dans un tour à chaque multiple duquel le résultat d'outil suggère de regrouper les commandes ou de déléguer à un sous-agent. 0 : jamais. |
| `budget.compaction_reserve_usd` | `0.5` | Dépense du jour réservée aux résumés de compaction une fois le plafond du jour atteint, en dollars ; les plafonds de session et de run ne les arrêtent jamais. |

**[context]**

| Clé | Défaut | Rôle |
|---|---|---|
| `context.compaction_threshold` | `0.7` | Part de la fenêtre du modèle à partir de laquelle l'historique est compacté. |
| `context.tail_ratio` | `0.025` | Part de la fenêtre gardée intacte en fin d'historique. |
| `context.tail_min_tokens` | `10000` | Taille minimale de la fin d'historique gardée intacte, en jetons. |
| `context.tail_max_tokens` | `25000` | Taille maximale de la fin d'historique gardée intacte, en jetons. |
| `context.min_tail_user_messages` | `2` | Messages du propriétaire gardés intacts au moins. |
| `context.max_tool_result_share` | `0.25` | Part maximale de la fenêtre qu'un résultat d'outil peut occuper. |
| `context.large_payload_tokens` | `25000` | Taille à partir de laquelle un résultat d'outil est rangé en artefact et résumé, en jetons. |
| `context.max_prompt_tokens` | `120000` | Taille de prompt au-delà de laquelle la compaction se déclenche, quelle que soit la fenêtre du modèle : une limite de coût, pas de fenêtre (issue #18). 0 : aucune. |
| `context.model_thresholds.<nom>` | – | Seuil de compaction propre à un modèle (identifiant avec ou sans provider). Sans entrée, le seuil général s'applique, abaissé sur une fenêtre courte pour laisser la place d'un résultat d'outil et de la réponse. |
| `context.background_compaction_margin` | `0.1` | Marge sous le seuil à partir de laquelle la compaction se prépare en tâche de fond. |
| `context.cooldown_ms` | `[60000,300000,900000]` | Attentes successives après une compaction en échec, en millisecondes. |
| `context.auto_title` | `true` | Titre de 3 à 6 mots donné par le modèle rapide après le premier échange. |

**[memory]**

| Clé | Défaut | Rôle |
|---|---|---|
| `memory.vault_path` | `"{data}/vault"` | Répertoire du vault (`{data}` : répertoire de données). |
| `memory.vault_git_autocommit` | `"15m"` | Période de commit du vault sous git ; `0s` : désactivé. |
| `memory.vault_git_remote` | `""` | Remote git où pousser le vault ; vide : aucun. |
| `memory.profile_budget_tokens` | `600` | Budget du profil injecté (`profil.md`), en jetons. |
| `memory.core_budget_tokens` | `1200` | Budget du niveau Cœur injecté (`memoire.md`), en jetons. |
| `memory.project_budget_tokens` | `800` | Budget des projets injectés (`projets.md`), en jetons. |
| `memory.recall_budget_tokens` | `1000` | Budget du rappel automatique par tour, en jetons. |
| `memory.recall_timeout_ms` | `150` | Temps accordé au rappel automatique avant de répondre sans lui, en millisecondes. |
| `memory.trigger_threshold` | `0.72` | Pertinence minimale d'une entrée pour être rappelée automatiquement : rang de recherche normalisé (1 pour la première d'une liste, 2 pour la première des deux), sans la récence ni l'importance, qui ne font qu'ordonner. |
| `memory.max_injected_per_turn` | `3` | Entrées rappelées automatiquement au plus par tour. |
| `memory.half_life_days` | `180.0` | Demi-vie de la récence dans le classement des souvenirs, en jours : un souvenir ancien passe après un récent équivalent, il reste rappelable. |
| `memory.dedup_cosine` | `0.92` | Similarité cosinus de doublon. Sans effet dans cette version. |
| `memory.dedup_jaccard` | `0.9` | Similarité à partir de laquelle deux candidats sont des doublons. |
| `memory.episode_idle` | `"2h"` | Inactivité qui clôt un épisode. Sans effet dans cette version. |
| `memory.episode_topic_shift` | `0.35` | Écart de sujet qui clôt un épisode. Sans effet dans cette version. |
| `memory.review_max_candidates` | `5` | Candidats notés au plus par relecture d'un échange ; 0 : relecture désactivée. |
| `memory.dream_batch` | `40` | Candidats consolidés par appel au modèle, la nuit : au-delà, la réponse ne tient plus dans la fenêtre de sortie et tout le lot est reporté. |
| `memory.dreaming_cron` | `"30 3 * * *"` | Heure de la consolidation nocturne (cron, fuseau du propriétaire). |
| `memory.dream_retry_wait` | `"2m"` | Attente avant de reprendre un lot de la consolidation après une erreur passagère du modèle (flux muet, 5xx, 429), doublée à la seconde reprise. |
| `memory.consolidation_reasoning` | `"auto"` | Raisonnement du modèle de consolidation : `auto` le garde et le budgète (le tri d'un candidat gagne à être réfléchi), `off` l'éteint pour rendre tout le budget de sortie au JSON. Le défaut est `auto` (issue #152). |
| `memory.consolidation_reasoning_tokens` | `16000` | Plafond du budget de raisonnement d'un appel de consolidation, en jetons. Le budget part plus bas et monte quand le modèle s'y heurte ; `max_tokens` de l'appel vaut ce budget plus la sortie estimée du lot. |
| `memory.digest_cron` | `"0 8 * * *"` | Heure du digest du matin (cron, fuseau du propriétaire). |
| `memory.promotion.ecart_min_occurrences` | `3` | Occurrences minimales d'un écart pour devenir une exception. |
| `memory.promotion.ecart_min_sessions` | `3` | Sessions distinctes minimales d'un écart. |
| `memory.promotion.ecart_min_days` | `2` | Jours distincts minimaux d'un écart. |
| `memory.promotion.fact_min_recalls` | `2` | Ignoré depuis 0.14.0 : faits, préférences, décisions et corrections passent par la grille de tri (issue #37). Gardé pour qu'une configuration existante reste valide. |
| `memory.promotion.fact_min_importance` | `8` | Ignoré depuis 0.14.0 (grille de tri, issue #37). |
| `memory.promotion.preference_min_sessions` | `2` | Ignoré depuis 0.14.0 (grille de tri, issue #37). |
| `memory.promotion.max_retire_ratio` | `0.2` | Part maximale des entrées d'un fichier retirées en une nuit. |
| `memory.promotion.contested_confidence` | `0.5` | Confiance d'une règle contestée. Sans effet dans cette version. |
| `memory.promotion.contested_min_observations` | `4` | Observations minimales d'une règle contestée. Sans effet dans cette version. |
| `memory.intents.cooldown` | `"24h"` | Délai minimal entre deux déclenchements d'une intention. |
| `memory.intents.fire_budget` | `3` | Déclenchements au plus d'une intention. |
| `memory.intents.expiry` | `"90d"` | Durée de vie d'une intention. |
| `memory.intents.max_per_turn` | `3` | Intentions déclenchées au plus par tour. |
| `memory.prune_episodic_days` | `180` | Âge d'élagage du journal. Sans effet dans cette version. |
| `memory.expire_ecart_days` | `90` | Âge au-delà duquel un écart jamais promu est abandonné, en jours. |

**[mcp]**

| Clé | Défaut | Rôle |
|---|---|---|
| `mcp.registry_mode` | `"lazy"` | `lazy` \| `eager`. Sans effet dans cette version. |
| `mcp.max_processes` | `24` | Processus de serveurs MCP actifs au plus. |
| `mcp.default_timeout` | `"30s"` | Délai par défaut d'un appel MCP (réglé par serveur dans `mcp.d`). Sans effet dans cette version. |
| `mcp.oauth_redirect_mode` | `"paste_back"` | Retour OAuth : `paste_back` (adresse collée dans Telegram) ou `public_callback`. |
| `mcp.public_callback_url` | `""` | Adresse publique de retour OAuth en mode `public_callback`. |
| `mcp.cimd_url` | `""` | Adresse du document de métadonnées client OAuth ; vide : enregistrement dynamique. |
| `mcp.preferred_protocol` | `"2026-07-28"` | Version de protocole MCP préférée. Sans effet dans cette version. |
| `mcp.idle_timeout` | `"10m"` | Inactivité d'arrêt d'un serveur (réglée par serveur dans `mcp.d`). Sans effet dans cette version. |
| `mcp.max_concurrency_per_server` | `4` | Appels simultanés au plus par serveur. Sans effet dans cette version. |
| `mcp.sticky_set_max` | `30` | Outils MCP gardés décrits d'un tour à l'autre, au plus. |
| `mcp.schema_max_bytes` | `8192` | Taille maximale d'un schéma d'outil exposé directement au modèle, en octets. |
| `mcp.eager_total_max_bytes` | `65536` | Taille totale des schémas exposés directement au modèle, en octets. |
| `mcp.callback_port` | `7777` | Port local du retour OAuth. |
| `mcp.callback_host` | `"127.0.0.1"` | Hôte local du retour OAuth : `127.0.0.1` ou `localhost`. Slack n'enregistre que `localhost` dans les URL de rappel d'une app (cf. #159). |
| `mcp.policy.read` | `"auto"` | Politique d'un outil MCP en lecture : `auto`, `ask`, `ask_twice` ou `deny`. |
| `mcp.policy.write` | `"ask"` | Politique d'un outil MCP en écriture. |
| `mcp.policy.destructive` | `"ask_twice"` | Politique d'un outil MCP destructif. |
| `mcp.policy.external` | `"ask"` | Politique d'un outil MCP à effet externe. |
| `mcp.policy.unknown` | `"ask"` | Politique d'un outil MCP au risque inconnu. |
| `mcp.restart_backoff_max` | `"5m"` | Attente maximale entre deux redémarrages d'un serveur. Sans effet dans cette version. |
| `mcp.max_failures` | `8` | Échecs consécutifs après lesquels un serveur est mis de côté. |

**[runners]**

| Clé | Défaut | Rôle |
|---|---|---|
| `runners.count` | `4` | Tours traités en parallèle. |
| `runners.lease_ttl` | `"60s"` | Durée du bail d'un tour réclamé ; au-delà, un autre runner le reprend. |
| `runners.heartbeat` | `"15s"` | Période de renouvellement du bail : au plus la moitié de `lease_ttl`, sinon un tour en cours perd son bail. |

**[sandbox]**

| Clé | Défaut | Rôle |
|---|---|---|
| `sandbox.default_profile` | `"workspace-write"` | Profil du bac à sable de `shell_exec` : `read-only`, `workspace-write` ou `full`. |
| `sandbox.allow_full_for` | `[]` | Serveurs MCP autorisés à tourner avec le profil `full` (sans bac à sable). |
| `sandbox.allow_keychain_for` | `[]` | Serveurs MCP stdio qui gardent leur bac à sable mais joignent le trousseau macOS : ceux dont le métier est de lire leurs propres identifiants. Le trousseau reste fermé aux autres, qui y verraient « introuvable » ce qui y est rangé. |
| `sandbox.workspaces` | `[]` | Répertoires de travail des outils de fichiers et du shell, en plus du défaut. |
| `sandbox.shell_network` | `false` | Réseau pour **toutes** les commandes de `shell_exec` et des étapes `shell`. Faux : une commande n'a le réseau que si son appel le demande (`network: true`, carte d'approbation qui le dit, « Toujours » borné à la famille de commandes) ou si son étape de workflow le déclare. Une configuration qui porte `true` le garde. |
| `sandbox.deny_read` | `["~/.ssh","~/.aws","~/.gnupg","~/.config/gh","~/.netrc","~/.kube","~/.docker/config.json","{data}/penelope.db","{data}/secrets.enc","{data}/mcp.d","{config}","{state}"]` | Chemins dont la lecture est refusée aux commandes sous bac à sable, même quand le profil lit le disque : clés, jetons, base de Pénélope, secrets, configuration. `{data}`, `{config}`, `{state}` et `~` sont développés. |

**[observability]**

| Clé | Défaut | Rôle |
|---|---|---|
| `observability.otlp_endpoint` | `""` | Export OpenTelemetry. Sans effet dans cette version. |
| `observability.prometheus` | `"127.0.0.1:9464"` | Adresse d'exposition Prometheus. Sans effet dans cette version : les métriques se lisent par `penelope metrics`. |
| `observability.log_retention_days` | `14` | Durée de conservation des journaux, en jours. |
| `observability.log_level` | `"info"` | Niveau de journalisation du daemon (`info`, `debug`, `warn`…), lu au démarrage ; la variable `PENELOPE_LOG` l'emporte. |
| `observability.runtime_stream_bind` | `"127.0.0.1:9465"` | Écoute WebSocket locale, active seulement si un consommateur est déclaré. |
| `observability.runtime_consumers` | `[]` | Chaque consommateur possède son propre secret et son filtre d'événements. |

**[tools]**

| Clé | Défaut | Rôle |
|---|---|---|
| `tools.shell` | `""` | Shell de `shell_exec`, programme puis arguments (`-c` par défaut) ; vide : shell de la plateforme. |
| `tools.shell_timeout` | `"120s"` | Délai d'une commande `shell_exec`. |
| `tools.http_allowlist` | `[]` | Hôtes autorisés pour `http_fetch` ; vide : tous. |
| `tools.http_block_private_ips` | `true` | Refuser les adresses privées et locales dans `http_fetch`. |
| `tools.loop_detector_repeats` | `3` | Appels identiques qui font arrêter une boucle d'outil. |
| `tools.max_output_bytes` | `262144` | Taille maximale d'une sortie de commande gardée telle quelle, en octets. |
| `tools.approval_mode` | `"reads"` | Mode d'approbation par défaut d'une session : `ask` (demander tout, lectures du shell comprises), `reads` (lectures sans demande, le reste selon la politique), `auto` (tout sans demande sauf le destructif). `/mode` le change pour une session. |
| `tools.shell_allow` | `[]` | Familles de commandes `shell_exec` autorisées d'avance, sans enchaînement : par exemple `cargo test`, `npm run lint`. |
| `tools.shell_allow_network` | `[]` | Familles de commandes autorisées d'avance **avec** le réseau : `git push`, `gh pr`. |
| `tools.inventory_extra` | `[]` | Binaires à ajouter à l'inventaire de la machine (issue #156), en plus de la liste connue : ce que le modèle apprend qu'il peut lancer au lieu de bricoler. |
| `tools.background_after` | `"120s"` | Délai au-delà duquel un appel d'outil se voit **proposer** l'arrière-plan plutôt que d'immobiliser le tour (issue #204). La proposition passe par le texte du résultat : c'est le modèle qui décide, rien n'est détourné d'office. |
| `tools.jobs_per_session` | `3` | Jobs d'outils simultanés au plus pour une session (issue #204). |
| `tools.jobs_total` | `10` | Jobs d'outils simultanés au plus pour tout le daemon (issue #204). |

**[workflows]**

| Clé | Défaut | Rôle |
|---|---|---|
| `workflows.workspace_retention_days` | `7` | Durée de conservation de l'espace de travail d'un run terminé, en jours. |
| `workflows.max_depth` | `3` | Profondeur maximale de sous-workflows. |
| `workflows.default_max_iterations` | `40` | Itérations au plus d'un run sans réglage propre. Sans effet dans cette version. |

**[upgrade]**

| Clé | Défaut | Rôle |
|---|---|---|
| `upgrade.channel` | `"stable"` | Canal de mise à jour. Sans effet dans cette version. |
| `upgrade.base_url` | `""` | Adresse de la liste des releases ; vide : le dépôt GitHub. |
| `upgrade.minisign_pubkey` | `""` | Clé publique minisign des sommes ; vide : celle du binaire de release. |
| `upgrade.health_timeout` | `"60s"` | Délai de confirmation de santé. Sans effet dans cette version. |
| `upgrade.heartbeat_daily` | `true` | Signal de vie quotidien. Sans effet dans cette version. |
| `upgrade.codesign_identity` | `""` | Identité de signature macOS (nom du certificat ou empreinte SHA-1) : le binaire téléchargé est re-signé avec elle avant la bascule (issue #28). Vide : non re-signé. |
| `upgrade.codesign_identifier` | `"io.github.edouard-claude.penelope"` | Identifiant fixe de la signature macOS. |
| `upgrade.install_dir` | `"~/.local/bin"` | Répertoire du binaire de release quand une installation source bascule vers les releases (issue #33). |

**[voice]**

| Clé | Défaut | Rôle |
|---|---|---|
| `voice.tts_voice` | `"fr_female"` | Voix préréglée du modèle de synthèse (rôle `tts`). |
| `voice.max_chars` | `1500` | Longueur maximale d'un texte lu en vocal, en caractères : au-delà, un résumé vocal. |
| `voice.reply_in_kind` | `false` | Répondre en vocal quand le propriétaire vient d'envoyer un vocal. |

**[retention]**

| Clé | Défaut | Rôle |
|---|---|---|
| `retention.days` | `90` | Jours gardés pour les tours terminés, les requêtes au modèle, les updates Telegram et les clés de travail. 0 : rien n'est effacé. |
| `retention.memory_history_days` | `30` | Jours gardés pour les pré-images de la mémoire (`mem_history`). 0 : rien n'est effacé. |

**[backup]**

| Clé | Défaut | Rôle |
|---|---|---|
| `backup.git_remote` | `""` | Dépôt git privé où pousser les sauvegardes chiffrées ; vide : celui du vault. |
| `backup.cron` | `"0 4 * * *"` | Heure de la sauvegarde nocturne (cron à cinq champs) ; vide : aucune. |
| `backup.keep_daily` | `7` | Sauvegardes quotidiennes gardées. |
| `backup.keep_weekly` | `4` | Sauvegardes hebdomadaires gardées. |
| `backup.keep_monthly` | `12` | Sauvegardes mensuelles gardées. |
| `backup.include_media` | `false` | Inclure les artefacts et les médias reçus. Lourd, et reconstructible. |
| `backup.max_push_bytes` | `104857600` | Taille maximale d'une archive poussée, en octets (limite de fichier de GitHub). |
<!-- reference:config:fin -->

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
 image_describe, image_locate ───► vision ───────────────► openrouter:google/gemini-3.1-flash-image
 image_generate ─────────────────► image ────────────────► openrouter:google/gemini-3.1-flash-image
```

Décrire une image et y pointer un élément ne demandent pas le même modèle : le rôle
`image_locate` (outil `image_inspect`, mode `locate`) peut viser un modèle d'interface qui
rend des coordonnées, sans toucher à la description. Son repère (`models.locate_frame`)
vaut `auto` par défaut : déduit de la famille du modèle (UI-TARS et Qwen3-VL rendent des
millièmes, Qwen2-VL et Qwen2.5-VL des pixels) et des valeurs rendues (une valeur qui
dépasse l'image sans dépasser 1000 est un millième). `pixels` ou `per_mille` le
déclarent ; un désaccord avec ces indices fait refuser les points plutôt que de les
servir faux. Voir « Travailler sur une interface ».

```bash
penelope model set pointage openrouter:bytedance/ui-tars-1.5-7b
```

```bash
penelope config set models.roles.image_locate pointage
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
  « ok », « merci », « salut » ────────────────────► main (aucun appel)
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

Un message manifestement trivial (salutation, accusé de réception, moins de sept mots sans
question, sans chemin ni URL) ne passe pas par le classifieur : il répond tout de suite avec
le modèle par défaut. Une demande **explicite** d'image (« génère une image de… », « fais-moi
un dessin de… », « generate an image of… ») part directement sur le modèle d'image ; « génère
un script », « régénère les tests » ou « dessine l'architecture en ASCII » passent, elles,
par le classifieur, et toute autre demande d'image par l'outil `image_generate` du modèle
de conversation. Pour les autres, la classification et le calcul du vecteur de rappel
mémoire partent **en parallèle** : une seule attente avant le premier jeton, pas deux.

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
suivant est reclassé. Le collant tombe aussi là où le cache est froid de toute façon : après
une pause plus longue que la durée du cache (5 min), une compaction du contexte ou un
changement d'épisode, le message repasse par le classifieur. Une session montée sur
`reasoning` pour une question difficile redescend donc au message suivant qui ne l'est pas,
et une session restée sur `main` monte quand la difficulté arrive ; `/model` dit quand le
dernier message a été reclassé ainsi. Un repli fait par OpenRouter est journalisé
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

### Codex : les modèles d'un abonnement ChatGPT

Un abonnement ChatGPT (Plus, Pro, Business) ne donne ni clé d'API ni crédits Platform,
mais il ouvre le backend Codex : les modèles du plan (`gpt-6-astra`, `gpt-5.6-*`), 272 k
de contexte, outils et images, sans facturation à l'appel. Pénélope sait s'y connecter, à
côté d'OpenRouter.

**Ce que c'est, juridiquement.** Cet usage est **toléré** par OpenAI (page « Codex for
Open Source », déclarations publiques sur les clients tiers), **jamais garanti par
contrat**, et révocable du jour au lendemain. Pénélope emprunte l'identité du client Codex
CLI (`originator`, `User-Agent`, identifiant d'installation), parce que le backend filtre
cet en-tête et sert un catalogue qui en dépend ; c'est une usurpation assumée, la même sur
toutes les requêtes — une identité incohérente vaut des heures de « servers overloaded ».
Le repli, si la porte se ferme : une clé d'API sur `openai_compat` vers `api.openai.com`.

**Se connecter** (la machine n'a pas besoin de navigateur) :

```bash
penelope model auth codex          # affiche une adresse et un code à six caractères
penelope model auth codex --status # plan, compte, état du jeton
penelope model auth codex --logout # révoque le jeton et l'oublie
```

Sur Telegram : `/model auth codex`, `/model auth codex status`, `/model auth codex
logout`. Le code vaut quinze minutes ; Pénélope confirme dès qu'il est saisi, puis active
`providers.codex.enabled`. Les jetons vivent dans le magasin de secrets sous `codex.oauth`
— jamais dans `~/.codex/auth.json`, qui appartient à Codex CLI — et sont rafraîchis en
fond, hors tour. Le jeton de rafraîchissement est **à usage unique** : Pénélope sérialise
ses rotations, et un jeton rejoué déconnecte le compte pour de bon (il faut alors
reconnecter).

**Donner un alias**, comme pour tout autre fournisseur :

```bash
penelope model set code codex:gpt-6-astra
```

**Pour quels tours.** L'abonnement ne sert que ce que le propriétaire ouvre lui-même : un
message Telegram ou CLI, et les sous-agents de ce tour. Tout ce qui tourne sans lui —
planification à cible `prompt`, rêve nocturne, veille, résumeur de compaction, relecture
d'épisode, consolidation, classifieur, embeddings, transcription, synthèse vocale, titre
automatique, run de workflow — repasse par le modèle OpenRouter de l'alias, sans carte ni
message, en laissant l'événement `llm.codex_scope_fallback`. C'est la contrepartie de la
tolérance d'OpenAI : un compte, un humain, un usage interactif.

Conséquence pratique : `penelope model set <alias> codex:<modèle>` **refuse** les alias qui
servent un rôle de fond (`classifier`, `compaction`, `memory_review`, `embedding`, `stt`,
`tts`) en disant pourquoi, et `penelope doctor` signale une configuration déjà en place qui
l'aurait contournée. Un seul compte à la fois : se connecter à un autre demande d'abord
`penelope model auth codex --logout`.

**Ce que ça coûte.** Rien à l'appel : les lignes d'usage portent `provider = codex`,
`cost_usd = 0` et `estimated = false` — le coût est connu, il vaut zéro. Les plafonds en
dollars (`budget.daily_usd`, `budget.session_usd`, `budget.run_usd`) ne comptent donc rien
pour ce fournisseur et ne le freinent pas.

La vraie limite est le quota du plan, que le backend annonce à chaque réponse (une fenêtre
de 5 h, une fenêtre hebdomadaire). Pénélope le lit, le range, et l'affiche dans `/budget`,
`penelope model list` et `self_status` (`primary 42 % · retour 18:05`). À
`providers.codex.quota_alert_ratio` (0,8 par défaut) : une alerte, une seule par fenêtre. À
`quota_stop_ratio` (0,95) : Pénélope se met en retrait **avant** l'appel et laisse le repli
OpenRouter jouer, plutôt que d'aller chercher un refus. Un quota atteint n'est pas une
panne : le message dit l'heure de retour.

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
(conversation ou classifieur), `--by upstream`, `--by miss` (ratés de cache par cause, tuile du prompt nommée quand
c'est le préfixe) ;
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
plus gros postes et leur part de cache. À 100 %, une carte demande « 5,02 $ dépensés sur
5 $ dans « Titre » : continuer ? » avec « +5 $ », « +20 $ » et « Arrêter » :

- pour une session ouverte par toi, le tour est suspendu et reprend là où il s'était arrêté
  une fois le plafond relevé ;
- le plafond du jour reste un arrêt ferme ; le relever vaut pour la journée, et la
  demande se renvoie. La journée est celle de `owner.timezone` : minuit local, pas minuit
  UTC, pour le plafond, son relèvement, `/budget`, `/usage` et `--by day`. Une consommation
  de 0 h 30 à La Réunion compte pour ce jour-là, et la consolidation de 3 h 30 aussi ;
- un run de workflow reçoit la même carte, et il reprend après relèvement.

Une session de travail longue a son propre plafond, sans toucher aux autres ni au
garde-fou du jour :

```bash
penelope session budget <session> 20
```

sur Telegram `/budget session 20` (`off` pour revenir à `session_usd`) ; `/budget` et
`/status` l'affichent, et `/fork` le recopie. Un tour qui
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
différent, ou préfixe intact non servi par le fournisseur. Quand c'est le préfixe, la
cause nomme la tuile qui a bougé (`prefixe:T1`, `prefixe:T2`), et `penelope audit show`
rend le prompt de l'appel pour comparer.

`model set` signale un identifiant absent du catalogue (`known: false`) quand le catalogue
est chargé ; sinon, une faute de frappe ne se verra qu'au premier appel.

### Shell et bac à sable

`shell_exec` tourne sous Seatbelt : écriture limitée au workspace et au répertoire
temporaire, **réseau coupé** sauf pour l'appel qui le demande. L'environnement
reste filtré : `PATH`, `HOME`, la langue, l'agent SSH et les emplacements de configuration,
jamais de jeton. Sur une machine dédiée à Pénélope, le bac à sable peut être levé pour le
shell :

```bash
penelope config set sandbox.default_profile full
```

Les approbations restent en place : c'est la carte `shell_exec` (« Toujours » compris) qui
décide.

Sous bac à sable, une commande peut lire le disque : `sandbox.deny_read` ferme ce qui ne la
regarde pas, et le trousseau du système avec (`security find-generic-password` ne répond
plus depuis une commande). La liste livrée couvre `~/.ssh`, `~/.aws`, `~/.gnupg`,
`~/.config/gh`, `~/.netrc`, `~/.kube`, la base de Pénélope, le magasin de secrets, `mcp.d`,
la configuration et l'état. Un workspace situé sous un chemin refusé reste lisible. Les
serveurs MCP stdio confinés (`mcp-stdio`, `workspace-write`, `readonly`) suivent la même
liste et n'ont pas non plus le trousseau (sauf ceux de `sandbox.allow_keychain_for`) : un
paquet tiers ne lit pas ce que `shell_exec` ne lit pas ; son répertoire de données et ses racines restent lisibles pour lui, et
`penelope doctor` signale un serveur confiné qui ne refuserait aucune lecture. Réseau
ouvert ou non, les sockets Unix locales restent fermées aux processus confinés (socket du
daemon, `/var/run/docker.sock`, autres services), sauf la résolution DNS et l'agent SSH
(`SSH_AUTH_SOCK`).

**Réseau accordé par appel.** Une commande qui a besoin du réseau (`git push`, `gh`,
`npm install`, `curl`) le demande dans son appel (`"network": true`) : l'appel devient
une action externe, et la carte d'approbation le dit en toutes lettres (« accès réseau
demandé », 🌐 sur Telegram). « Toujours » l'accorde à la famille de commandes (`git push`
avec réseau), jamais au shell : `curl` ou `python` redemandent, et une règle sans réseau
(antérieure, ou sur l'outil entier) ne le donne pas. Sans réseau, un échec de résolution ou
de connexion, ou d'une commande qui ne vit que du réseau, porte la note « Réseau coupé pour
cette commande » : l'agent relance avec `network: true` au lieu de boucler. Une étape
`shell` de workflow déclare `network: true` dans le workflow, et l'aperçu validé au
lancement la marque « réseau » ; parmi les workflows livrés, le clone, le déploiement, la
vérification et le retour arrière la déclarent, pas les tests ni le lint. `penelope
doctor` dit si le réseau est fermé et combien de règles l'accordent. Pour le rouvrir à
toutes les commandes, comme avant 0.17.2 :

```bash
penelope config set sandbox.shell_network true
```

Une configuration qui porte déjà `shell_network = true` (écrite par une version
antérieure) le garde à la mise à jour ; sans la clé, le réseau est fermé.

**Lectures sans demande, modes et autorisations déclarées.** Une commande qui ne fait que
lire (`ls`, `cat`, `head`, `grep`, `find` sans `-exec` ni `-delete`, `wc`, `git status`,
`git log`, `git diff`…), appelée par son nom, sans enchaînement, redirection,
substitution ni échappement **hors guillemets**, est classée lecture : elle part sans
demande, comme `fs_read`. Tout le reste est une écriture. Chaque session a un mode (`/mode` sur
Telegram, `penelope session mode`, défaut `tools.approval_mode`) : « demander tout »
(`ask`, même une lecture du shell attend ton accord), « lectures sans demande » (`reads`,
le défaut) et « tout sauf le destructif » (`auto` : plus de demande, sauf une commande qui
supprime, écrase, élève ses droits, réécrit git, ou qu'on ne peut juger parce qu'elle
enchaîne). Des familles peuvent être autorisées d'avance, sans clic :

```bash
penelope config set tools.shell_allow '["cargo test", "npm run lint"]'
penelope config set tools.shell_allow_network '["git push", "gh pr"]'
```

Un `cd <répertoire> &&` seul en tête, vers un répertoire d'un workspace et sans
variable, substitution, tilde ni joker, n'est pas un enchaînement : c'est le répertoire de
travail de la commande qui suit. `cd /Users/moi/depot && grep -rn foo src` (avec
`/Users/moi/depot` dans `sandbox.workspaces`) se lit donc comme `grep -rn foo src` dans ce
dépôt : lecture sans demande, carte qui montre la vraie commande et son répertoire, « Toujours » qui règle la famille de cette commande (jamais `cd`), et
famille déclarée d'avance qui vaut aussi derrière le `cd`. Hors des workspaces, ou suivi
d'un autre enchaînement (`;`, `|`, `&&`), d'une redirection ou d'une substitution, la
ligne reste composée. Le modèle a de toute façon le paramètre `cwd` de `shell_exec`.

**Les listes `&&` ont des familles.** `a && b && c`, où chaque étape est une commande
nommable, n'est plus composée : c'est une **liste**, et chacune de ses étapes est jugée
pour elle-même (issue #150). Conséquences :

- « Toujours » écrit **une règle par famille**, trois au plus d'un seul clic, et le bouton
  les nomme toutes avant : `♾️ Toujours pour « yt-dlp », « ffmpeg » (réseau)` ;
- la ligne n'est couverte que si **chaque** étape l'est, par une règle ou parce qu'elle
  lit ; `tools.shell_allow` et `tools.shell_allow_network` suivent la même règle. Une
  seule étape non couverte fait repartir la ligne entière en carte ;
- une étape qui ne fait que lire (`ls -la tmp/x*`) ou que régler le shell (`cd`, `export`,
  `set`) n'a besoin d'aucune règle : c'est ce qui créait vingt règles mortes sur vingt-sept ;
- une étape qui ne nomme pas ce qu'elle lance — `sh -c`, `sudo`, `env`, `xargs`,
  `timeout`, `python3 -c` — laisse la ligne composée : une règle `sh` couvrirait n'importe
  quel code.

`&&` est le seul opérateur admis, et c'est délibéré : `;` et `||` lancent la suite quoi
qu'il arrive (`cargo test; rm -rf ~`).

**Ce qui compte comme enchaînement.** Une ligne est dite *composée* — donc jamais une
lecture, sans famille, et hors de portée d'une règle ou d'une famille déclarée — quand
elle porte, **hors guillemets**, un opérateur (`;`, `&`, `||`, `(`, `)`) ou une
redirection (`>`, `<`) ; une substitution (`$(…)`, `` `…` ``, `$VAR`) ou un échappement
(`\`), même entre guillemets doubles, où ils gardent leur pouvoir dans un shell ; un saut
de ligne ; une négation (`!` en tête) ; une apostrophe ou un guillemet non fermé ; ou une
affectation d'environnement qui détourne ce qui sera exécuté (`PATH=`, `HOME=`, `IFS=`,
`ENV=`, `BASH_ENV=`, `DYLD_*`, `LD_*`, `GIT_CONFIG*`, `GIT_SSH_COMMAND=`, `NODE_OPTIONS=`,
`PYTHONSTARTUP=`, `PERL5OPT=`, `RUBYOPT=`…).

**Un tube vers une lecture pure n'est pas un enchaînement.** `glab api … | jq -r '…'`,
`cat f | grep x | head -20`, `ls | wc -l` : ce qui agit est la première étape, et les
suivantes ne peuvent que lire. La famille est donc celle de la première étape (une règle
`glab` couvre `glab api …` comme `glab api … | jq …`), et une lecture qui traverse un tel
tube reste une lecture. Les étapes acceptées sont `jq` (sans `-f`, `--rawfile`,
`--slurpfile`), `cat` (sans fichier), `grep`, `egrep`, `fgrep`, `rg` (sans `--pre`),
`head`, `tail`, `cut`, `sort` (sans `-o`), `wc`, `uniq`, `tr`, `nl`, `rev`, `column`. Tout
le reste — `| sh`, `| xargs`, `| tee`, `| python`, `| sed`, un `||`, une redirection —
laisse la ligne composée.

Entre guillemets, ces caractères ne sont que des caractères : `glab api --hostname
gitlab.example "projects?membership=true&per_page=100"` est de la famille `glab`, pas un
enchaînement ; `grep -n 'x | y' fichier` et `echo "a & b"` sont des lectures. Une
affectation anodine en tête laisse la famille à son programme : `GITLAB_HOST=example glab
api "…"` est de la famille `glab`, `TZ=UTC date` est une lecture. Le même découpage sert à
créer la règle, à la reconnaître, à lire `tools.shell_allow` et à classer les lectures :
ce qu'un « Toujours » écrit est ce qui s'applique ensuite.

Un « Toujours » sur une commande composée (`ls; pwd`, `a > f`, `$(…)`) l'autorise cette
fois, sans créer de règle. La carte le dit **avant** le clic, et depuis #150 **aucun
bouton ne prend la place de « Toujours »** : une coche verte à cet endroit se lisait comme
un « Toujours » nouvelle formule, et n'autorisait qu'une fois. La ligne de qualificatifs
nomme ce qui l'empêche et la sortie : « pas de règle possible (`;`) — demande-lui une
commande par appel ». Le ledger suit : `rule_created` ne vaut `always` que si une règle a
vraiment été écrite (`penelope approvals`).
`penelope policies` et `/policies` signalent les règles inutiles (famille issue d'une
commande composée ou d'une affectation comme `GITLAB_HOST=…`, lecture déjà libre, jamais
utilisée depuis une semaine), à retirer d'un bouton.

La description de `shell_exec` demande au modèle **une commande par appel** : plusieurs
commandes font plusieurs appels, en parallèle si elles sont indépendantes. `penelope
doctor` (`shell_lines`) donne la part de lignes collées sur sept jours, et signale
au-dessus d'une sur cinq composées. Une skill installée dont les exemples collent des
commandes reçoit la consigne au chargement, sans que son fichier soit réécrit.

Un « Toujours » est **borné à l'appel qu'il autorise**, jamais à l'outil entier :
pour `shell_exec`, à la famille de commandes (`cargo test …`, `git log …`) ; pour `fs_write`
et `fs_edit`, au répertoire du fichier ; pour `git_push`, au couple remote et branche ; pour
`http_fetch`, à l'hôte ; pour `config_set`, à la clé. Une autre commande, un autre
répertoire, un autre hôte redemandent. `/policies` (et `penelope policies`) affiche la
portée de chaque règle et permet de la retirer.

Une suite de tests (`cargo test`, `go test`, `npm|pnpm|yarn test`, Jest, Vitest, `pytest`,
`make test`) ne renvoie au modèle que le résumé et les sections d'échec (nom, assertion,
pile courte) ; une autre commande en échec à longue sortie, sa tête, sa queue et ses
lignes d'erreur. La sortie complète part en artefact, relisible par pages avec
`artifact_read`. `output: "full"` dans l'appel rend la sortie brute : échouer sur 3 tests
sur 1 200 ne fait plus entrer 1 197 lignes de succès dans le contexte.

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

**Répondre en vocal.** Pénélope peut aussi envoyer des vocaux, avec une voix féminine
française générée en local : sur demande (« réponds-moi en vocal », « lis-moi la
veille »), ou en réponse à un vocal si `voice.reply_in_kind` vaut `true`. Jamais pour du
code, un tableau ou une longue réponse : elle envoie alors un résumé vocal et garde le
détail en texte. L'outil `send_voice` retire le Markdown, les liens, les blocs de code et
les emojis (« 8,5 % » devient « 8,5 pour cent »), découpe le texte en phrases, le fait
lire par le modèle du rôle `tts`, convertit l'audio en OGG/Opus avec `ffmpeg` et l'envoie
en message vocal dans la même conversation. Si la synthèse échoue (serveur arrêté, modèle
absent), la réponse part en texte avec « vocal indisponible : <raison> ».

Le modèle livré est Voxtral TTS de Mistral (`mlx-community/Voxtral-4B-TTS-2603-mlx-4bit`,
2,5 Go, voix `fr_female`), servi par mlx-audio sur le même serveur local que la
transcription (`/v1/audio/speech`). Voxtral TTS demande mlx-audio **depuis sa branche
principale** (la release 0.5.4 ne le charge pas) et `mistral-common[audio]` 1.11.7 ou plus
récent ; sinon le serveur répond `unexpected keyword argument 'voice_num_audio_tokens'`.

```bash
pip install "git+https://github.com/Blaizzy/mlx-audio.git" "mistral-common[audio]>=1.11.7"
```

```bash
mlx_audio.server --host 127.0.0.1 --port 8080
```

Le premier appel télécharge le modèle ; ensuite, mettre `HF_HUB_OFFLINE=1` dans
l'environnement du LaunchAgent du serveur pour ne plus contacter Hugging Face. Compter
environ une seconde de calcul par seconde d'audio à chaud sur un M1 Pro. Réglages :
`voice.tts_voice` (voix), `voice.max_chars` (1 500 caractères au plus par vocal),
`voice.reply_in_kind` ; le modèle se change comme les autres :

```bash
penelope model set tts openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit
```

`penelope doctor` vérifie `ffmpeg` et lit une phrase d'essai ; `self_status` (inventaire
`install`) dit si la réponse vocale est disponible et avec quelle voix.

### Ce que Pénélope sait d'elle-même

L'outil `self_status` lui donne son état complet : version et durée de fonctionnement,
modèle qui répond au tour et routage, configuration effective (secrets désignés par leur
nom, jamais par leur valeur), coûts du jour et de la session, file de travail, chemins, et
la machine : batterie et alimentation, disque, mémoire, charge, démarrage, système. Elle
l'appelle d'elle-même dès qu'on lui pose une question sur elle ou sur l'ordinateur.

Le même outil donne son inventaire, par section ou en entier (`inventory`) : workflows
(identifiant, rôle, paramètres requis et facultatifs, s'ils tournent sur cette machine),
skills, outils natifs et leur classe de risque, serveurs MCP et leurs outils, commandes
Telegram, planifications, mode d'installation, limites connues. L'index des workflows
figure aussi dans son prompt, borné à 2 000 caractères.

Sa documentation fait foi : `README.md` et tout `docs/` sont embarqués dans le binaire,
dans la version qui tourne. L'outil `self_docs` les liste (`list`), y cherche (`search`),
lit une section par pages (`read`) et rassemble les limites connues (`limits`) ; chaque
résultat porte le lien GitHub de la section au tag de la version. Avant d'écrire un
workflow, une skill ou un réglage, ou pour répondre sur ses capacités, elle consulte
`self_status` puis `self_docs` et cite la section ; elle dit quand la documentation ne
couvre pas le cas. Un brouillon de workflow refusé par `workflow_author` renvoie à la
section concernée de [workflows.md](workflows.md).

Elle peut aussi changer un réglage à ta demande avec `config_set`, appliqué à chaud et
toujours soumis à approbation. Le bac à sable, les providers, Telegram, les outils et les
politiques exigent une double confirmation à chaque fois : une règle « Toujours » ne les
couvre pas. Les secrets et l'identifiant du propriétaire sont refusés : ils ne se règlent
qu'en ligne de commande.

### Ce que Pénélope sait de sa machine

Savoir qu'un binaire est là change ce qu'elle tente. Sans cette connaissance, un lien
GitHub donnait deux `http_fetch` refusés sur `api.github.com`, puis un
`curl … | python3` cassé par le shell ; `gh` était installé et connecté. Il fallait le
lui dire, à chaque nouveau sujet.

Une passe d'inventaire tourne **au démarrage, toutes les heures et à chaque
`penelope doctor`**. Elle cherche dans le PATH `gh`, `glab`, `git`, `docker`, `yt-dlp`,
`ffmpeg`, `brew`, `node`, `npx`, `python3`, `uvx`, `go`, `cargo`, `jq`, `rg`, `make`,
`ssh`, plus ce que tu ajoutes dans `tools.inventory_extra`. Pour chacun, elle relève sa
version ; pour les forges, son **état de connexion** (`gh auth status`,
`glab auth status`) ; pour Docker, si le démon répond. Une sonde a trois secondes et
l'entrée fermée : une commande qui réclame une saisie meurt au délai. L'option qui
afficherait un jeton n'est jamais passée, et ce qui est gardé traverse le rédacteur.

Le résultat va dans `machine.inventory` et sort par `self_status` (section `machine`,
champ `inventory`, avec les versions) et par `doctor`, qui donne une ligne pour ce qui est
présent, une pour ce qui manque, et une par forge installée mais **déconnectée** — celle-là
ne sert à rien tant qu'elle n'est pas connectée.

Dans le message système, l'inventaire tient en **une ligne**, juste après les outils :

```text
Machine : macOS 27 arm64, bac à sable `workspace-write`, réseau accordé par appel ·
installés et connectés : gh (edouard-claude), glab (gitlab.apnl.tech), docker (joignable) ·
installés : git, ffmpeg, brew, node, jq · absents : yt-dlp
Lance la commande installée plutôt que le réseau brut : GitHub (github.com, api.github.com)
→ `gh api …` ; GitLab → `glab` ; images et conteneurs → `docker`. Jamais `curl` ni
`http_fetch` vers une forge dont le client est connecté […]
```

Deux règles tiennent cette ligne :

- **Ni version, ni date, ni chemin.** Elle est en T1, dans le préfixe mis en cache ; un
  `brew upgrade gh` ne doit pas la changer, sinon le cache tombe à chaque tour. Les
  versions restent dans `self_status`. Une déconnexion, elle, la change une fois.
- **Un réflexe n'est émis que pour ce qui est utilisable.** Une forge installée mais
  déconnectée n'apparaît pas dans les règles de routage : l'envoyer sur `glab` alors que
  `glab` n'est pas connecté rend « not logged in », et elle repart sur `http_fetch`.

En complément, un `http_fetch` vers une forge dont le client est connecté porte une
remarque (« `gh` est installé et connecté : préfère `gh api …` »). Elle ne bloque rien :
la lecture a déjà eu lieu, et `http_fetch` reste le bon outil pour une page publique.

Une skill qui déclare `requires: [bin:yt-dlp]` sur une machine sans `yt-dlp` est annotée
dans `skill_search` et dans `skill_load` (`binaires_manquants`), et `doctor` le dit. Elle
reste listée et rien n'est installé à ton insu : la machine est la tienne.

L'accueil (`penelope onboard outils`) propose désormais l'inventaire détecté comme point
de départ au lieu d'une page blanche. Ce que tu déclares reste prioritaire sur ce qui est
détecté.

### Outils natifs

Ce que le modèle peut appeler sans serveur MCP, avec la classe de risque qui décide de
l'approbation (`read` : sans approbation ; `write`, `external`, `destructive` : selon la
politique). Les outils MCP passent par `tool_search`, `tool_describe` et `tool_call`.

En conversation, chaque appel au modèle ne décrit que le noyau d'usage courant (16 outils)
et ces trois méta-outils : 19 définitions, environ 2 600 tokens de schémas au lieu de
52 définitions et 6 000 tokens. Les outils marqués « à la demande » dans la table sont
seulement nommés dans le message système ; `tool_search` les trouve par ce qu'ils font
(« planifier un rappel » donne `schedule_create`), `tool_describe` donne leur schéma et
`tool_call` les appelle avec la même classe de risque et la même approbation qu'un appel
direct. Un outil décrit ou appelé rejoint la liste de la session dès le tour suivant et
la quitte après dix tours sans usage. Les étapes de workflow et les sous-agents gardent
leur liste complète.

Un appel aux arguments invalides est refusé avant de partir, avec les paramètres
attendus : les requis d'abord, leur type, leurs valeurs permises et leur description,
1 500 caractères au plus (le reste se lit avec `tool_describe`). Un nom d'outil inconnu
rend les noms proches. Un `tool_call` arrivé avec `args` vide alors que l'outil visé a
des paramètres requis le dit comme tel : les arguments ont pu se perdre en route, et
`args_json` (la même chose en chaîne JSON) les fait passer. La garde de boucle reste le
filet : le même appel rejoué à l'identique est arrêté quoi que dise l'erreur. Par
`tool_call`, la politique, la carte d'approbation et « Toujours » portent sur les
arguments de l'outil visé, jamais sur l'enveloppe.

`git_clone` accepte une URL `https://`, `ssh://`, `git://`, `file://`, la forme
`git@hôte:owner/repo.git` ou le raccourci GitHub `owner/repo`. Ce dernier devient
`https://github.com/owner/repo.git` avant le clone ; un chemin local relatif ou absolu
est refusé. Si la même origine existe déjà dans un workspace (SSH et HTTPS GitHub
équivalents), l'outil rend son chemin avec `already: true` et l'URL demandée. Une
approbation « Toujours » est bornée au schéma et à l'hôte ; les anciennes règles
`git_clone` sans motif sont révoquées lors de la mise à jour.

`fs_read` et `fs_search` lisent en flux : 50 lignes d'un journal de 512 Mio se lisent en
moins d'une milliseconde et quelques Mio de mémoire. Au-delà de 8 Mio, `fs_read` ne compte
plus le total des lignes (il le dit) et refuse d'aller chercher une ligne au-delà de 64 Mio
parcourus (`tail -n` par `shell_exec` fait mieux) ; une ligne est tronquée à 64 Kio ;
`fs_search` ignore les fichiers de plus de 32 Mio et les nomme dans `ignorés`.

Quand le modèle demande plusieurs appels d'un coup, les lectures pures consécutives
(fichiers, git en lecture, mémoire, historique, artefacts, catalogues) partent ensemble,
par quatre : cinq `fs_read` ou trois `mem_search` coûtent la durée du plus lent, pas la
somme. Tout le reste (écriture, action externe, appel soumis à approbation, et les
lectures dont l'ordre compte comme `return_value` puis `step_done`) part seul, après les
lectures qui le précèdent et avant celles qui le suivent.
Les résultats sont enregistrés dans l'ordre des appels, un échec n'annule pas les autres,
et `/stop` interrompt tout le lot.

<!-- reference:outils:debut (générée : UPDATE_DOCS=1 cargo test -p penelope-evals --test docs) -->
| Outil | Risque | Rôle |
|---|---|---|
| `artifact_read` | read | Lecture paginée d'un artefact ; le curseur n'avance que des octets renvoyés. |
| `ask_user` | read | Pose une question au propriétaire et attend sa réponse. |
| `config_set` | write | Modifie un réglage de sa propre configuration et indique son moment d'effet. (à la demande) |
| `fs_edit` | write | Remplace une portion exacte d'un fichier. |
| `fs_list` | read | Liste le contenu d'un répertoire du workspace. |
| `fs_read` | read | Lit un fichier du workspace autorisé, avec pagination par lignes. |
| `fs_search` | read | Recherche une expression régulière dans les fichiers du workspace. |
| `fs_write` | write | Écrit un fichier dans le workspace. |
| `git_branch` | write | Crée ou change de branche. (à la demande) |
| `git_clone` | external | Clone un dépôt distant dans le workspace, ou retrouve un clone existant de la même origine. (à la demande) |
| `git_commit` | write | Valide les changements indexés. (à la demande) |
| `git_diff` | read | Diff du dépôt, éventuellement contre une référence. (à la demande) |
| `git_push` | external | Pousse une branche vers le dépôt distant. (à la demande) |
| `git_status` | read | État du dépôt : branche, fichiers modifiés. (à la demande) |
| `history_describe` | read | Manifeste d'un nœud de résumé : tokens, intervalle, enfants. (à la demande) |
| `history_expand` | read | Contenu paginé d'un nœud ou d'un intervalle brut. (à la demande) |
| `history_expand_query` | read | Retrouve, dans toutes les sessions y compris fermées, les passages liés à une question en langage naturel : recherche mot significatif par mot significatif, extraits classés par nombre de mots trouvés, avec le titre et la date de leur session. (à la demande) |
| `history_grep` | read | Recherche plein texte dans les messages bruts et les résumés. |
| `http_fetch` | external | Récupère une URL. |
| `image_generate` | external | Génère une image et la stocke en artefact. (à la demande) |
| `image_inspect` | read | Pose une question au modèle de vision sur une image : photo reçue (son chemin est dans le message) ou capture d'écran du workspace. (à la demande) |
| `intent_cancel` | write | Annule une intention. (à la demande) |
| `intent_create` | write | Arme une intention événementielle : « quand on reparle de X, rappelle-moi Y ». (à la demande) |
| `intent_list` | read | Liste les intentions armées. (à la demande) |
| `job_cancel` | write | Annule un job en cours : le processus et son groupe sont tués, le job passe `cancelled`. (à la demande) |
| `job_list` | read | Jobs d'outils de cette session : ceux qui tournent encore, leur outil et leur âge. (à la demande) |
| `job_status` | read | État d'un job lancé en arrière-plan : `working`, `completed`, `failed` ou `cancelled`, et son résultat s'il est terminé. (à la demande) |
| `job_wait` | read | Attend la fin d'un job, au plus `timeout_ms` (120 s au maximum, 30 s par défaut). (à la demande) |
| `mem_forget` | destructive | Retire une entrée de mémoire. (à la demande) |
| `mem_get` | read | Lit une entrée de mémoire par uid ou par slug. (à la demande) |
| `mem_neighbors` | read | Voisins d'une note dans le graphe du vault : concepts d'une source, sources et entrées de mémoire qui citent un concept (liens `[[slug]]` sortants et entrants). (à la demande) |
| `mem_note` | write | Note une observation dans le journal du jour. |
| `mem_remember` | write | Écrit directement en mémoire. (à la demande) |
| `mem_search` | read | Recherche dans la mémoire curée, dans les documents ingérés (`vault/sources`, passages encadrés comme non fiables, `slug` pour un seul document) et, sur demande explicite, épisodique. |
| `return_value` | read | Renvoie le résultat d'une étape de workflow. (dans un workflow) |
| `schedule_create` | write | Crée un déclencheur planifié, soumis à approbation. (à la demande) |
| `schedule_delete` | write | Supprime un déclencheur planifié. (à la demande) |
| `schedule_list` | read | Liste les déclencheurs planifiés, avec la conversation où chacun livre (`destination`). (à la demande) |
| `schedule_move` | write | Change où livre un déclencheur planifié, sans le recréer (historique gardé) : `to: "here"` vers cette conversation (ce sujet compris), `"private"` vers la conversation privée du propriétaire. (à la demande) |
| `self_docs` | read | Documentation de ta propre version, embarquée dans le binaire : le dépôt edouard-claude/penelope est la source de vérité sur toi. (à la demande) |
| `self_status` | read | État complet de Pénélope et de sa machine : version, modèle qui répond à ce tour et routage, configuration effective (alias, rôles, bac à sable, budgets, Telegram, providers, transcription), coûts du jour et de la session, file de travail, chemins, et machine (batterie, secteur, disque, mémoire, charge, démarrage, système). |
| `send_file` | write | Envoie un fichier au propriétaire. (à la demande) |
| `send_message` | write | Envoie un message au propriétaire. |
| `send_voice` | read | Lit un texte en message vocal (voix féminine locale) dans cette conversation. (à la demande) |
| `session_metadata` | read | Lit ou modifie les métadonnées de session : critères, findings, todos. (à la demande) |
| `session_notes` | read | Notes de travail de la session, qui survivent aux compactions et au fork : objectif, plan, décisions, fichiers touchés, points ouverts, prochaine étape. (à la demande) |
| `shell_exec` | write | Exécute une commande sous bac à sable, avec délai. |
| `skill_load` | read | Charge une skill dans le tour courant. (à la demande) |
| `skill_patch` | write | Propose une modification de skill. (à la demande) |
| `skill_propose` | write | Propose une nouvelle skill. (à la demande) |
| `skill_search` | read | Cherche une skill par mots-clés. (à la demande) |
| `step_done` | read | Déclare l'étape de workflow terminée. (dans un workflow) |
| `sub_agent_spawn` | write | Lance un sub-agent à contexte neuf, outils restreints, retour structuré. |
| `time_now` | read | Date et heure courantes dans le fuseau du propriétaire. |
| `workflow_author` | write | Rédige un workflow (format décrit dans `docs/workflows.md` : lis-le avec `self_docs` avant d'écrire, n'invente aucun type d'étape ni champ). (à la demande) |
| `workflow_control` | write | Contrôle un run : pause, reprise, annulation, relance d'étape. (à la demande) |
| `workflow_describe` | read | Décrit un workflow : étapes, paramètres, budget. (à la demande) |
| `workflow_list` | read | Liste les workflows disponibles. (à la demande) |
| `workflow_plan` | write | Propose ou révise un plan de workflow avant tout lancement. (à la demande) |
| `workflow_start` | write | Lancement direct réservé aux contextes internes et CLI ; depuis Telegram, propose d'abord `workflow_plan` et attends le gate « vas-y ». (à la demande) |
| `workflow_status` | read | État d'un run. (à la demande) |
<!-- reference:outils:fin -->

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

Pour un serveur HTTP dont le client OAuth pré-enregistré est confidentiel, la
configuration ne contient que la référence au secret :

```toml
transport = "http"
url = "https://mcp.exemple.fr/mcp"
client_id = "penelope"
client_secret = "${SECRET:mcp_exemple_client_secret}"
```

La valeur se pose séparément avec
`penelope secret set mcp_exemple_client_secret`. Pénélope accepte
`client_secret_basic` et `client_secret_post` selon les métadonnées du serveur
d'autorisation.

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

Un serveur dont le métier est de lire ses propres identifiants dans le trousseau n'a pas
besoin de `full` : le trousseau fermé lui répond « introuvable » (`secret not found in
keyring`), et Pénélope complète alors son erreur par le réglage qui l'ouvre, lui seul,
sans rien défaire du reste du profil. Le changement vaut au prochain appel, sans relance :

```bash
penelope config set sandbox.allow_keychain_for '["mailbridge"]'
```

Plus propre quand le serveur le permet : lui passer le secret par son environnement
(`${SECRET:nom}` dans `env` de sa déclaration), et laisser le trousseau fermé. Détail dans
[mcp.md](mcp.md), section « Bac à sable ».

Administration, en ligne de commande comme sur Telegram (`/mcp`, `/mcp redmine`,
`/mcp restart|logs|test redmine`) :

```bash
penelope mcp list
```

`show <nom>` détaille état, déclaration et outils ; `test <nom>` (ou `--file x.toml`)
essaie une connexion à blanc, jusqu'à l'appel d'un outil en lecture sans argument ; `logs <nom>` donne le stderr du serveur ; `add <fichier>`,
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

### Déposer une skill

Une skill est un dossier avec un `SKILL.md` dans `{data}/skills/` : il suffit de le
déposer, par exemple par `scp`. La passe d'entretien vérifie le dossier chaque minute et
ne relit les skills que si le contenu d'un fichier a changé (pas sa seule date), sans
redémarrage ; la skill est alors visible par `skill_search`,
`skill_load` et l'index des capacités. Pour ne pas attendre :

```bash
penelope skill reload
```

Un `SKILL.md` invalide est ignoré, sans effacer les autres, et signalé par
`penelope doctor`.

### Importer des skills d'un dépôt

Le format `agentskills.io` est celui des skills publiées ailleurs : un `SKILL.md` amont se
charge tel quel. Pour ne pas le faire à la main :

```bash
penelope skill install anthropics/skills:docx,xlsx,pptx,pdf
penelope skill install anthropics/skills@v2          # une révision
penelope skill install owner/repo --force            # remplace ce qui porte le même nom
```

L'archive du dépôt est téléchargée en HTTPS (aucun `git` requis), et **le dossier entier**
de chaque skill est copié dans `{data}/skills/` : les documentaires embarquent des scripts
et des schémas de validation, qui ne servent à rien sans leurs fichiers. Sans liste après
le `:`, toutes les skills du dépôt sont posées. Une skill dont le `SKILL.md` ne se charge
pas est refusée avant la moindre écriture, comme une archive qui porte un lien symbolique
ou un chemin remontant hors de son dossier.

Deux compléments sont écrits dans le frontmatter, le reste du fichier étant laissé mot
pour mot (une mise à jour du dépôt reste lisible en diff) : `version: 1.0.0` s'il n'y en a
pas, et `allowed_tools` déduit du corps. Sans `allowed_tools`, une skill a droit à **tous**
les outils : pour une skill venue d'ailleurs, ce défaut est trop large, donc Pénélope y
écrit ce que le corps réclame. `penelope skill show <nom>` le montre ; si la skill se voit
refuser un outil dont elle a besoin, la ligne est à élargir à la main.

**Le vocabulaire.** Un corps écrit pour Claude Code parle de `Read`, `Bash`, `Grep`. Rien
n'est réécrit dans le fichier : au chargement (`skill_load`), Pénélope pose devant le corps
le dossier absolu de la skill et la table de correspondance.

| Claude Code | Pénélope |
|---|---|
| `Read` | `fs_read` |
| `Write` | `fs_write` |
| `Edit`, `MultiEdit`, `NotebookEdit` | `fs_edit` |
| `Bash`, `BashOutput` | `shell_exec` |
| `Glob`, `Grep` | `fs_search` |
| `WebFetch`, `WebSearch` | `http_fetch` |

**Les dépendances.** Une skill déclare ce que sa machine doit avoir :

```yaml
requires: [pip:openpyxl, npm:docx, bin:pandoc]
```

Pénélope **n'installe rien**. `penelope skill install` liste à la fin ce qui manque avec la
commande qui le pose, et `penelope doctor` refait le contrôle (`skills.requirements`) :
`bin:` est cherché dans le `PATH`, `pip:` par un `import`, `npm:` par un
`require.resolve` avec `NODE_PATH` réglé sur `npm root -g`. Sur macOS, PEP 668 interdit le
`pip` système : la consigne rendue propose un environnement dédié.

Ce qui reste à faire à la main : lire la licence du dépôt d'origine. Les skills
documentaires d'Anthropic sont « Proprietary, source-available », publiées comme référence
— l'import à la demande les télécharge depuis la source, il ne les redistribue pas.

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
Pénélope à l'heure dite. Chaque exécution ouvre sa propre session, titrée d'après la
planification et le jour (« Veille agents IA · 17/09 »), et répond dans le chat ou le
sujet d'origine : fermer la conversation où la planification est née (`/new`, `/close`)
ne l'arrête plus. Un rappel manqué pendant un arrêt part une fois au redémarrage.

**Jamais de silence.** Si l'exécution d'un prompt planifié est annulée, échoue ou atteint
son budget, le propriétaire reçoit « ⚠️ La planification « … » n'a pas pu s'exécuter :
<raison> » avec « 🔁 Relancer maintenant » et « 📅 Voir la planification ». Une
exécution ne compte (`runs`, `last_run`) qu'une fois menée à terme ; sinon la raison
reste dans `last_error`, visible dans `/schedules` et `penelope doctor`. Créer une
planification identique à une planification active (même déclencheur, prompt quasi
identique) est signalé dans la réponse de création.

**Ce qui est livré, et une seule fois.** Pour un prompt planifié, la réponse finale du
tour est le livrable : elle part dans la conversation (et le sujet) de la planification,
même sans `send_message`. `send_message` sert aux messages intermédiaires (« limite
GitHub atteinte, je continue ») ; s'il a déjà envoyé le même contenu que la réponse
finale (à la mise en forme ou à l'en-tête près), celle-ci ne repart pas, et l'événement
`schedule.final_not_repeated` le note. Un `send_message` en échec n'empêche jamais la
réponse finale. Une skill planifiée met donc son digest dans sa réponse finale, et
consomme son état « déjà vu » en fin de tour.

**Ce qu'une exécution doit livrer.** Un prompt planifié peut déclarer son livrable dans sa
cible : `"livrable": "message"` (une réponse non vide dans le chat d'origine, ou un
`send_message`), `"fichier:veille/2026-09-18.md"` (écrit pendant l'exécution) ou `"run"`
(un workflow lancé). Une exécution qui répond sans ce livrable n'est pas comptée : elle est
signalée comme un échec (« exécutée sans livrable : aucun message envoyé »), dans le chat,
`/schedules`, `penelope schedule list` et le digest du matin. Une planification qui
consomme un état (« déjà vu », curseur) le déclare (`"etat": "veille/seen.json"`, dans un
workspace) : il est gardé avant le tour et remis tel quel si rien n'est livré, pour que la
suivante reprenne les mêmes éléments. Sans livrable déclaré, rien ne change.

Les autres déclencheurs : `interval` (toutes les N minutes), `mcp_poll` (un outil MCP en
lecture interrogé à intervalle ; seuls les éléments nouveaux déclenchent, le premier
passage ne fait que mémoriser l'existant), `watch_file` (un fichier modifié) et `event`
(un événement du journal, `run.done` par exemple). La création passe par une approbation.

Sur Telegram, `/schedules` liste les déclencheurs et `/schedules pause|resume|rm|run <id>`
les gère. En ligne de commande :

```bash
penelope schedule list
```

**Où livre une planification.** Chacune livre dans la conversation où elle est née : la
conversation privée si elle y a été créée, même quand on regarde ensuite un sujet d'un
groupe. `/schedules`, `penelope schedule list` (colonne « vers ») et `schedule_list` le
disent en toutes lettres (« conversation privée », « sujet « Veille », groupe « Équipe » »,
« (par défaut) » quand rien n'a été choisi), et le digest du matin rappelle ce qui part
dans la journée, avec l'heure et l'endroit. Pour la déplacer sans la recréer (identifiant,
exécutions et historique gardés) : `/schedules ici <id>` écrit dans le sujet voulu, 📍 sur
l'écran `/schedules` pour la conversation où il s'affiche, ou en ligne de commande vers un
groupe autorisé (`telegram.allowed_chats`) :

```bash
penelope schedule move <id> --chat -1001234567890 --topic 21
```

`--private` la ramène dans la conversation privée. Pénélope le fait aussi sur demande
(outil `schedule_move`, soumis à approbation).

Une « intention » est l'autre mémoire prospective : « quand on reparle du déploiement,
rappelle-moi le changelog » reste armée et revient dans le contexte du premier message
qui en parle (trois fois au plus, une fois par jour au plus).

### Photos et documents

Une photo part dans la conversation. Si le modèle de la session lit les images, il la
voit ; sinon le modèle de l'alias `vision` la décrit (texte visible recopié) et la
description rejoint le message. Plusieurs photos envoyées d'un coup forment un seul
message, qui garde le chemin de chaque photo ; `image_inspect` y revient (voir
« Travailler sur une interface »).

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

### Travailler sur une interface

Pour agir sur une application (simulateur iOS, navigateur), l'ordre est toujours le
même, et les coordonnées viennent en dernier :

1. **Viser par identifiant.** Un élément qui a un `testID` (`accessibilityIdentifier`) ou
   un libellé d'accessibilité se touche par lui, via l'arbre d'accessibilité de l'outil
   qui pilote l'application. C'est exact, stable d'une taille d'écran à l'autre, et ça
   ne coûte aucune image.
2. **Pointer seulement ce que l'arbre ne montre pas** : une image sans libellé, un dessin.
   Prendre une capture (`xcrun simctl io booted screenshot ecran.png` dans le workspace),
   puis `image_inspect` en mode `locate` avec l'élément décrit en une phrase (« le bouton
   de sélection de boutique »).
3. **Lire le repère.** `points` est toujours en pixels de l'image, origine en haut à
   gauche, x vers la droite, y vers le bas ; `image` donne sa taille, `model_frame` dit
   comment la réponse brute du modèle a été lue (`pixels` ou `per_mille`). Un modèle
   d'interface rend souvent des millièmes (UI-TARS, Qwen3-VL) : Pénélope les ramène en
   pixels. Quand le repère est douteux (valeur hors de l'image, désaccord avec
   `models.locate_frame`), aucun point n'est servi et `refused` dit pourquoi : corriger
   le réglage (`auto` par défaut) plutôt que deviner.
4. **Convertir pour un tap.** Un simulateur attend des points d'écran, pas des pixels :
   diviser par l'échelle de l'écran (×3 sur la plupart des iPhone, ×2 sur un iPad).
   `(588, 1273)` px sur une capture 1179×2556 donnent `(196, 424)` pt.
5. **Vérifier.** Le point doit tomber dans le cadre d'un nœud voisin cohérent de l'arbre
   d'accessibilité (la rangée au-dessus, en dessous) ; après le tap, une nouvelle
   capture ou l'arbre dit si l'effet a eu lieu.
6. **Changer d'approche au lieu d'affiner.** Après deux taps par coordonnées sans effet
   vérifiable, ne pas corriger le point de quelques pour cent : revenir à l'arbre, faire
   défiler, reprendre une capture, demander un `testID` au propriétaire, ou demander.

`image_inspect` a deux autres modes : `describe` (description, en français) et `read`
(texte recopié tel quel, dans sa langue). Dans tous les modes, ce que le modèle lit dans
l'image reste une donnée, encadrée comme telle.

### Mémoire qui apprend

Après un échange qui en vaut la peine (message un peu long, correction, règle énoncée),
le modèle de l'alias du rôle `memory_review` note au plus cinq candidats : préférence,
correction, décision, fait, écart. Un accord court (« ok », « go », « oui », « tu peux
publier ») qui répond à une proposition de Pénélope (« je propose… », « je l'ouvre ? »,
des choix) est relu lui aussi, avec la proposition comme matière : la décision est là,
et elle devient un candidat « décision » d'origine propriétaire. Un « merci », un accord
qui ne suit aucune proposition ou une commande ne coûtent aucun appel. Rien n'est écrit dans le profil ni la mémoire à ce
moment-là : les candidats vont dans le journal du jour.

Une conversation Telegram ne se ferme jamais : Pénélope la découpe en **épisodes**. Un
épisode se clôt après 2 h sans message, quand trois messages de suite s'éloignent du
sujet, ou sur `/new`. L'épisode clos est relu une fois en entier : un résumé rejoint le
journal du jour, et ce qui mérite d'être retenu devient des candidats. Le profil et la
mémoire de fond que voit le modèle sont figés pendant un épisode : ce qui est appris
apparaît à l'épisode suivant, sans casser le cache du provider en cours de route.

Chaque nuit (03:30, `memory.dreaming_cron`), la consolidation trie les candidats par une
**grille explicite**, sans rien te demander de valider. Un contenu non fiable n'atteint
jamais le modèle, un écart doit se répéter sur plusieurs jours. Pour chaque fait,
préférence, décision ou correction, le modèle répond à cinq questions, et le code en
déduit la place :

Une procédure candidate n'est proposée comme skill qu'après deux exécutions réussies
dans deux sessions distinctes. Deux répétitions dans la même session ou sans session
traçable ne suffisent pas. La session est un indicateur prudent de tâches différentes ;
elle ne prouve pas à elle seule que les tâches sont indépendantes.

| Critère | Question |
|---|---|
| Durable | Encore vrai dans un mois ? |
| Utile | Change-t-il ce que Pénélope fera plus tard ? |
| Précis | Sujet identifiable (qui, quoi, où) et phrase complète ? |
| Introuvable ailleurs | Absent du code, des docs, du tracker, de git et des outils ? |
| Endossé | Dit ou confirmé par toi, ou constaté par un outil fiable ? |

```
introuvable, précis, endossé, utile : un « non » ─► ignoré
durable : non ────────────────────────────────────► journal (expire)
tout oui ─────────────────────────────────────────► mémoire durable
```

Une règle dite une seule fois, explicitement, passe donc dès la première nuit. Chaque
candidat est comparé à ses souvenirs proches (par le sens si les embeddings répondent,
sinon par les mots). Le modèle choisit : ajouter, mettre à jour, **remplacer**
(`supersede` : l'ancienne entrée est retirée, la nouvelle porte `remplace: <uid>` et
`depuis`) ou ne rien faire. Un texte déjà en mémoire n'est jamais ajouté une seconde fois ;
une contradiction non tranchée devient une question, un changement de défaut une
proposition. Chaque décision, avec ses critères et sa justification, est écrite dans la
section « Tri » de `DREAMS.md`.

**La forme du digest.** Le digest du matin (08:00, `memory.digest_cron`) part au foyer
(`telegram.home`) et tient en **une bulle**. Il dit, dans cet ordre : ce que la nuit a
appris (le compte, puis cinq exemples au plus, tronqués à 80 caractères, avec leur
fichier), les fichiers touchés, combien de questions attendent une réponse, le seul
avertissement qui demande une action du propriétaire (le niveau Cœur au-delà de
`memory.core_budget_tokens`), les entrées trop longues avec la commande qui en propose le
découpage, les entrées jamais rappelées depuis soixante jours (trois exemples), les
motifs d'écart regroupés par famille avec leur compte (« imprécis : 18 »,
« retrouvable ailleurs : 4 », cinq familles au plus, les autres additionnées), la
dépense de la veille et l'agenda du jour. Deux nuits de suite ou plus sans rien promouvoir
ajoutent une ligne explicite, avec le motif dominant : un motif qui revient vingt fois est
un réglage à revoir.

Ce qui n'y est **pas** : les entrées promues en wikilinks bruts, les relances de lot et
autres avertissements internes de la passe, le lint détaillé, le journal et les secrets
rangés. Leur place est `DREAMS.md` et le journal du vault, que le digest cite en dernière
ligne (« Détail : `DREAMS.md`, rêve `d_…` ») ; les avertissements internes se relisent
aussi dans `penelope doctor`. Au-delà de `telegram.max_fragments`, le message part en
**document** avec une légende d'une ligne plutôt qu'en six bulles.

**Les questions se tranchent au bouton.** Une contradiction ne part jamais en texte libre :
elle devient une carte `memory_proposal` à part du digest, qui cite les deux entrées
tronquées à 80 caractères avec leur wikilink, et porte trois boutons — **Remplacer**
(l'ancienne entrée est retirée), **Exception** (les deux cohabitent, la nouvelle porte son
contexte), **Ignorer** (le candidat est écarté, la mémoire ne bouge pas). Le digest n'en
donne que le compte. Sans réponse, la carte n'est pas reposée le lendemain : elle est
rappelée puis rangée dans `DREAMS.md` sous « Questions sans réponse », et le candidat
reste listé par `penelope mem candidates`.

Une contradiction, c'est **deux règles opposées sur le même sujet** : la polarité se lit en
tête de la première phrase (« Toujours… », « Jamais… », « Ne pas… »), pas au fil du texte ;
le sujet commun se mesure en Jaccard (0,4) **et** par la similarité d'embedding du voisin
(0,80) ; deux énoncés au-delà de la borne d'une entrée, ou dont les longueurs sont dans un
rapport de plus de trois, ne se comparent pas ; un `fait` et un `écart` ne contredisent
rien, ils se datent.

**Une entrée, un fait.** `mem_remember` refuse un texte de plus de 300 caractères et
demande de le découper ; `mem_note` (notes de travail) reste sans borne. Pour les entrées
déjà écrites, `penelope mem split <uid>` propose un découpage en faits courts — les
données financières personnelles (solde, salaire, épargne) restent hors de la mémoire de
fond — par une carte, jamais par une écriture directe. Une réponse vide, sans puces
exploitables ou réduite à un seul fait pour une longue entrée est retentée une fois ;
si elle reste insuffisante, la commande explique l'échec sans créer de carte. `penelope
doctor` liste les entrées actives au-delà de la borne et le Cœur au-delà de son budget.

**Les lots et leur coût.** La passe juge les candidats par lots d'au plus
`memory.dream_batch` = `40`. Une sortie coupée fait rejouer le même début de lot deux
fois plus petit, sans perdre de candidat ; les lots suivants restent ensuite sous la
taille coupée et ne remontent qu'à mi-chemin entre elle et la plus grande taille qui a
tenu depuis, au lieu de repartir à 40 (un lot sur cinq jugé, quatre appels jetés, avant
0.17.19). Un lot rejoué jusqu'à un seul candidat accuse ce candidat, pas la taille : la
passe reprend à mi-chemin de la première coupure. Deux fois de suite, c'est la taille qui
est en cause. Une coupure à 2 candidats ne réduit jamais les lots suivants à 1.

La sortie demandée suit ce que le modèle écrit vraiment par candidat (350 tokens au
départ, la grille en écrit plusieurs centaines), dans la limite du modèle ; un lot trop
grand pour elle est réduit avant l'appel. Une coupure enseigne aussi : l'estimation ne
redescend plus sous ce que le lot coupé a prouvé, et quand c'est le plancher de 2 000
tokens qui a coupé, il double. Un candidat seul coupé est repris une fois avec une
sortie doublée ; coupé encore, sa réponse tronquée est gardée et l'appel compté jeté.
Une passe dont plus de la moitié des lots, sur huit au moins, n'ont tenu qu'à un
candidat s'arrête et le dit : les candidats restants attendent la passe suivante sans
consommer de report (avant 0.17.23, une tête de lot bavarde laissait toute la passe
partir par lots d'un candidat : 121 appels, 45 minutes). Chaque lot laisse un événement
`memory.dream_batch` et une ligne de journal (taille, durée, sortie, coupé ou non), et le
rapport comme le digest disent le nombre d'appels, les appels jetés et la durée. Un
candidat n'est « reporté » que par une passe qui a rendu ses verdicts : une passe arrêtée
avant n'en consomme aucun, et la troisième nuit de report ne vient jamais d'une
interruption.

**Une nuit ratée se dit.** Une erreur passagère du modèle sur un lot (flux devenu muet,
5xx, 429, lot sans réponse complète en 240 s) fait reprendre ce lot, et lui seul, après
`memory.dream_retry_wait` (2 min), puis le double ; le rapport de la nuit le note. Rien
n'est écrit avant que tous les lots aient répondu, et chaque candidat est marqué promu
dès son entrée écrite : une passe arrêtée ne promeut jamais deux fois. Si le lot échoue
encore, ou sur une erreur qui n'est pas passagère, la nuit est déclarée ratée : un
événement `memory.dream_failed`, une ligne datée dans `DREAMS.md` (« Rêve du … : échec »,
la raison, ce qui a été écrit ou non), un message au propriétaire comme pour la
sauvegarde, et le digest du matin qui le dit au lieu de reprendre le rapport de la veille.
Les candidats restent tels quels pour la nuit suivante. Une panne qui dure ne répète pas
le même message chaque nuit : il revient quand la raison change, et le digest compte les
nuits.

**Sujet de travail.** Ce qui est injecté d'office (profil, mémoire de fond, projets) suit
le sujet de la session : le profil et les entrées sans projet toujours, une entrée d'un
projet (annotation `<!-- projet: nom -->`, ou section de `projets.md`) seulement dans les
sessions de ce projet. Le sujet se déduit du nom du sujet Telegram, du titre ou du premier
message quand ils nomment un projet connu, ou se choisit (`/projet`, `penelope session
project <nom>`, `aucun`). Rien n'est perdu : ce qui n'est pas injecté revient par le rappel
et `mem_search`. Le sujet se fige avec l'instantané de l'épisode, le préfixe du prompt ne
bouge pas d'un tour à l'autre ; le changer à la main le refige au message suivant.

**Journal des états en cours.** Ce qui est vrai aujourd'hui mais pas dans un mois (ticket
corrigé en dev, document pas encore lu, rendez-vous) va dans `projets.md`, section « États
en cours », avec `expire` (14 jours par défaut, 90 au plus). Il est injecté jusqu'à cette
date, puis retiré tout seul la nuit suivante. Rien de passager n'entre dans `memoire.md`.

**Secrets.** Une clé, un jeton ou un mot de passe que tu donnes en conversation part
dans le magasin de secrets dès la relecture, sous un nom tiré du contexte (par exemple
`cle-stripe-projet-atlas-1f2e3d4c`). La mémoire ne garde que la référence
`${SECRET:cle-stripe-projet-atlas-1f2e3d4c}`, jamais la valeur. `DREAMS.md` liste les
noms rangés. Un numéro de carte, lui, est refusé (13 à 19 chiffres, clé de Luhn), et le
refus cite le fragment en cause, masqué (« …6467 »), pour retirer ce qu'il faut. Un
nombre collé à un identifiant (`command-output:38228-1743576040856618`, `id=…`, `run/…`)
n'est pas pris pour une carte à l'écriture, sauf si le mot collé en nomme une (`carte:…`,
`cb=…`). Les journaux et les événements, eux, masquent tout nombre qui passe les deux
tests, identifiant ou non : un faux positif y coûte peu, une fuite beaucoup.

**Rappel automatique.** Un souvenir est servi d'office quand il est **pertinent** pour le
message (`memory.trigger_threshold`, sur le rang de recherche seul) ; sa récence
(`memory.half_life_days`, 180 jours), son importance, le projet et la confiance ne font
que l'ordonner face aux autres. Une entrée de six mois reste donc rappelée quand la
question la vise, après une plus récente équivalente.

**Retour d'usage.** Chaque souvenir servi au modèle (rappel automatique ou `mem_search`)
est compté, et chaque apparition dans les résultats sans être retenu aussi. Une entrée de
`memoire.md` ou `projets.md` jamais rappelée depuis 60 jours, mais apparue au moins dix fois
dans les résultats, est proposée au retrait dans le digest : une entrée qu'aucune question
n'a jamais approchée n'a pas eu sa chance, elle reste. Les préférences du profil,
toujours appliquées, ne sont jamais proposées.

**Usage dans le classement.** Un souvenir servi en conversation ne compte comme utile que
si la réponse le reprend : au moins un mot distinctif du souvenir que la question ne
contenait pas (deux pour un souvenir long). Ce qui a servi (rappels utiles, succès) monte
le score jusqu'à ×1,2 ; un souvenir servi vingt fois sans jamais servir, ou contredit,
descend jusqu'à ×0,85. Le facteur ne fait qu'ordonner : il ne franchit pas l'écart entre
une entrée trouvée par les mots et le sens et une entrée trouvée par un seul des deux, et
le seuil du rappel automatique ne compare que la pertinence. La consolidation voit
l'usage de chaque souvenir proche (« rappelé 12 fois, utile 9 ») comme une preuve, le
placement reste calculé des cinq critères. Servi dans un workflow ou un sous-agent, où
aucune réponse ne se juge, un souvenir ne voit que sa date de rappel mise à jour.

```bash
penelope mem signals 01MARTIN
```

donne ses rappels, rappels utiles, vues, succès, contradictions et le facteur qui en
résulte. La mise à jour vers 0.17.2 remet les rappels et rappels utiles à zéro : avant
elle, tout souvenir servi comptait comme utile.

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

La qualité du tri se mesure sur un banc d'essai : cinq conversations anonymisées, les
souvenirs attendus (gardés, mis à jour, au journal, ignorés, rangés en secret) et des
questions dont la réponse n'est que dans la mémoire. Il mesure la précision, le rappel,
le journal, les faux souvenirs, les souvenirs périmés, les doublons, les fuites de secrets
et l'exactitude des réponses. En CI, le modèle de consolidation est simulé ; le rapport est
joint à chaque release (`banc-memoire.md`).

```bash
penelope eval mem-bench
```

```bash
OPENROUTER_API_KEY=… penelope eval mem-bench-live
```

**Règles dictées.** Une règle que tu énonces et que Pénélope note avec `mem_note` porte ta
citation exacte : retrouvée dans tes messages récents, elle compte comme venant de toi.
Notée sans citation (ou avec une citation absente de tes messages), elle passe quand même
la grille ; si rien ne montre que tu l'as dite ou confirmée, elle n'est pas endossée et
reste écartée, sans question. Les règles rejetées par une version antérieure pour leur
seule origine se remettent en file :

```bash
penelope mem retry-rejected
```

**Qualité de ce qui est retenu.** Une entrée est un fait complet : un texte tronqué
(« … »), une phrase incomplète ou un pronom sans sujet est rejeté ; au-delà de 300
caractères, l'entrée est scindée en phrases, et une phrase inexploitable est écartée. Un
état passager (« deal en cours », « propale non lue », « arbitrage ») part dans
`projets.md` avec `expire: AAAA-MM-JJ` (30 jours) et n'est plus injecté après cette date.
Une donnée client, financière ou de sécurité (montant, marge, faille) porte
`sensible: oui` : un simple marqueur. Le vault est privé, une information client ou
d'infrastructure utile se garde et s'injecte comme les autres. Un fait sur
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
contradictions à trancher (proposées, jamais corrigées en silence). Les entrées du rêve
(`[[memoire#^…]]`) et le journal de la veille sont écrits dans `DREAMS.md` et le journal du
vault, pas dans le digest. Un vault d'une
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

Dans `build-verify`, le plan déclare le dépôt et une commande de tests. Le build pose
ensuite `session_metadata.verification` avec la commande réellement passée, ses
prérequis non secrets et les références de preuves (TDD, PR, CI) liées à leur SHA.
`verify` relance cette commande par `shell_exec` avec la politique d'approbation et le
bac à sable habituels. Un outil absent ou une preuve périmée est signalé comme tel au
build ; une CI verte ne dispense pas de la vérification indépendante.

Le plus simple est d'en parler : « on traite quelques tickets Yobbu ». Pénélope repère
le workflow adapté dans son index, cherche les données avec ses outils (tickets ouverts
dans Redmine ou ClickUp, dépôt, forge), ne demande que ce qui manque, puis propose le
lancement : une carte montre le workflow, les paramètres qu'elle a complétés et le brief
de la discussion (ticket, constats, décisions, contraintes, approche retenue), avec
« ▶️ Lancer » et « ⏸ Pas encore ». Le brief est transmis à la première étape `agent` ou
`sub_agent` du run et paraît sur sa carte de progression ; questions et progression
arrivent dans la même conversation (même sujet de forum).

Sur Telegram : `/wf`, `/run build-verify objectif=…`, `/runs`, `/resume <run>`. `/run
<workflow>` sans paramètres ne force plus de formulaire : la demande part en conversation,
et « 📝 Remplir le formulaire » reste à un bouton. Une question arrive avec ses boutons ; si l'étape attend une précision, le message suivant
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
HTML d'origine reste relisible en artefact. Avec `tools.http_block_private_ips` (défaut),
chaque saut résout le nom une seule fois, vérifie toutes ses adresses et se connecte sur
elles : un nom qui répond « public » au contrôle puis `127.0.0.1` ou `169.254.169.254` à la
connexion (rebinding DNS) n'atteint pas le réseau local ; le certificat reste vérifié
contre le nom. Au-delà de `context.large_payload_tokens`
(25 k par défaut), un résultat d'outil part en artefact avec un aperçu du début et de la
fin, quelle que soit la fenêtre du modèle : un modèle à un million de tokens ne garde pas
un résultat de 175 k entier. Une longue liste de fichiers (`fs_list` récursif) est
résumée par dossier, la liste complète en artefact.

### Jobs d'outils

Un appel d'outil occupe le tour du début à la fin : pendant qu'une commande tourne, la
conversation n'avance pas, et un message du propriétaire attend derrière elle — même pour
dire « laisse tomber ». Pour les deux outils qui peuvent durer, `shell_exec` et
`sub_agent_spawn`, `background: true` rend la main tout de suite :

```
shell_exec {"command": "cargo test --workspace", "background": true}
→ {"job": "tj_01M…", "state": "working"}
```

Le tour répond sans attendre. La commande continue hors du tour, et son résultat revient
seul dans la conversation, par un tour de relance qui rappelle de quoi il est le résultat
— même si le tour d'origine est clos depuis longtemps, et même si la conversation a changé
de sujet entre-temps. Une session fermée ne perd rien : la livraison attend sa
réouverture.

Pénélope suit ses jobs avec quatre outils exposés à la demande (`job_list`, `job_status`,
`job_wait`, `job_cancel`). `job_wait` est borné à deux minutes : au-delà, il rend l'état
courant plutôt que d'immobiliser le tour à son tour. Un `timeout_ms` demandé au-delà de
`tools.background_after` (120 s) lui vaut une remarque dans le résultat de l'appel : la
prochaine fois, l'arrière-plan. Rien n'est détourné d'office ; c'est elle qui décide.

Côté propriétaire :

```bash
penelope jobs           # ce qui tourne : outil, session, âge
penelope jobs --all     # les jobs terminés aussi
```

`/stop` coupe les jobs de la conversation en cours, `/stop tout` ceux de toutes ses
sessions ; dans les deux cas le groupe de processus est tué, sans orphelin. `penelope
doctor` signale un job de plus d'une heure, ou un job de la base que plus aucun processus
ne couvre. `self_status` (section `jobs`) montre la même chose à Pénélope.

Un job n'existe que pour une conversation (y compris celle qu'ouvre un déclencheur
planifié). Dans un sous-agent ou une étape de workflow, `background: true` est ignoré et
l'appel s'exécute normalement : la session d'un sous-agent meurt avec sa conclusion, et un
run de workflow a déjà son attente d'étape.

Deux plafonds empêchent un modèle de tout lancer en arrière-plan sans jamais rien relire :
`tools.jobs_per_session` (3) et `tools.jobs_total` (10). Au-delà, l'appel est refusé avec
ce qu'il faut pour s'en sortir.

Un job est un effet comme un autre : il est planifié dans le ledger **avant** de partir, et
un redémarrage pendant qu'il tourne le laisse incertain. Le job devient alors `failed` et
la carte « C'est fait / Relancer / Ignorer » part une fois — il n'est jamais relancé tout
seul, même si son outil est déclaré idempotent (voir la [décision
0012](decisions/0012-jobs-outils-durables.md)).

### Longues conversations

Le parcours complet, seuils et chiffres par fenêtre compris, est dans
[context.md](context.md).

Quand une conversation approche le seuil de sa fenêtre (70 % par défaut, moins une marge
de 10 points), Pénélope fait résumer les anciens échanges en tâche de fond par l'alias du
rôle `compaction` (`summarizer`). Le seuil est aussi plafonné en valeur absolue par
`context.max_prompt_tokens` (120 000 par défaut, `0` pour s'en passer) : sur un modèle à
1,3 M de tokens, la compaction part vers 103 k au lieu de 917 k. Une fenêtre immense sert
à ne jamais échouer, pas à renvoyer 500 k tokens à chaque appel ; relever le plafond garde
plus de conversation mot pour mot, au prix de chaque appel. La conversation ne s'arrête
pas : le résumé est publié à la fin du tour en cours.

Le déclenchement compare le seuil à l'estimation locale de la requête **et** au prompt
réellement facturé au dernier appel : les instantanés mémoire, l'index et les outils,
que l'estimation voit mal, comptent. Une session reprise après une pause (cache perdu de
toute façon) dont le dernier prompt dépasse le seuil est résumée **avant** l'appel au
modèle, comme le premier tour d'un fork. Chaque décision laisse un événement :
`context.compaction_requested`, `context.compaction_skipped` avec sa raison (attente
après échec, rien à compacter, réserve épuisée) et `context.compacted`, aussi en INFO dans
les journaux. `/status`, `/budget` et `self_status` donnent la taille réelle du contexte,
le seuil de fond et la date de la dernière compaction. Les derniers échanges restent mot pour mot, les identifiants
(chemins, tickets, SHA, URLs) sont conservés tels quels, et un résumé existant est mis à
jour plutôt que refait. Rien n'est effacé : les échanges résumés restent consultables par
`history_grep` et `history_expand`.

**Notes de travail.** Pour une tâche longue, le modèle tient les notes de la session avec
l'outil `session_notes` : objectif, plan, décisions, fichiers touchés, points ouverts,
prochaine étape. Elles vivent dans `vault/notes/<titre>-<id>.md` (propriété `type:
session`), sont injectées, bornées à environ 1 500 tokens, en fin de prompt à chaque tour,
et survivent donc aux compactions ; le harnais rappelle de les mettre à jour après une
délégation ou une compaction. `/fork` en fait une copie propre, `/new <titre>` propose les
notes d'une session au titre proche, et le rêve relève les décisions nouvelles en
candidats sans réécrire le fichier.

`/compact` sur Telegram force un résumé tout de suite. En ligne de commande :

```bash
penelope session compact
```

Un résumé raté attend 1 min, puis 5, puis 15 avant un nouvel essai de fond ; `/compact`
lève cette attente. Si le provider refuse une requête trop longue, Pénélope résume une
fois et relance la même demande. Le coût apparaît sous le rôle `compaction` de
`/budget rôles`. Un résumé coûte peu et allège chaque appel suivant : les plafonds de
session et de run ne l'arrêtent jamais ; une fois le plafond du jour atteint, les résumés
de fond continuent dans la limite de `budget.compaction_reserve_usd` (0,50 $ par défaut),
`/compact` toujours.

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

Ctrl-C pendant une réponse arrête **le tour** sur le daemon, outils compris, et vide sa
file (code de sortie 130) ; un second Ctrl-C quitte sans attendre. Un terminal fermé ou
une connexion SSH coupée ont le même effet : un tour lancé par la CLI dont personne ne lit
plus la réponse est annulé. Un tour venu de Telegram n'est jamais concerné.

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

Les journaux du daemon sont des lignes JSON par jour (`~/Library/Logs/Penelope`), et
chaque ligne écrite pendant un tour porte l'identifiant du tour et sa session, pendant un
run l'identifiant du run :

```bash
penelope logs --turn t_01J9…
```

(`--session` pour une session entière, sans filtre les 200 dernières lignes). Sous launchd,
rien n'est plus recopié sur stderr (`daemon.err.log` ne garde que les paniques), et
`observability.log_level` règle le niveau. `penelope metrics` donne les compteurs du daemon
en texte Prometheus : tours par issue et leur durée, appels d'outils par outil,
approbations en attente, effets incertains, mémoire résidente.

Le [flux runtime](runtime-events.md) expose les événements commités à des consommateurs
locaux authentifiés, avec replay et filtres par type. Il est désactivé par défaut.

La CLI parle au daemon par la socket `{state}/rpc.sock`. Chaque requête porte un jeton de
session tiré à chaque démarrage et rangé à côté (`rpc.token`, lisible par le seul
propriétaire) : un processus du même utilisateur qui n'a pas ce jeton, un serveur MCP ou
une commande sous bac à sable par exemple, ne peut ni changer un réglage, ni poser un
secret, ni approuver quoi que ce soit. Juste après un redémarrage, une commande partie
avec l'ancien jeton est refusée (`unauthorized`) : il suffit de la relancer.

`penelope status` donne aussi le nombre de runners vivants sur `runners.count`. Chaque
boucle de fond (runners, ordonnanceur, pilote de workflows, maintenance, catalogue,
entretien MCP, passerelle Telegram) est surveillée : une panique est journalisée avec le nom
de la boucle, comptée, versée au journal d'audit (`daemon.task_panicked`), puis la boucle
repart après 1 s, 2 s, 4 s… jusqu'à 5 min. Un tour qui panique échoue proprement : son
runner continue, le verrou de la session est rendu et le message d'échec arrive. `penelope
doctor` signale toute boucle relancée dans la dernière heure, ou qui ne tourne plus.

### Coupure de courant

Toute écriture de la base survit à l'arrêt brutal du **processus** (`kill -9`, panique).
Les transitions d'un effet non idempotent (« part », puis « fait ») survivent aussi à
l'arrêt brutal de la **machine** (coupure secteur, panique noyau, batterie à zéro) : elles
sont synchronisées jusqu'au disque avant l'exécution de l'outil. Au redémarrage, un
`git push` ou un commentaire lancé juste avant la coupure devient donc une question, jamais
un second envoi. Coût mesuré sur un Mac M2 : environ 8 ms par transition, soit une
quinzaine de millisecondes par appel d'outil qui écrit ; les lectures et le trafic de fond
n'en paient aucun.

## 9. Sauvegarde et audit

```bash
penelope backup
```

La sauvegarde est cohérente même pendant l'écriture : elle passe par `VACUUM INTO` sur
une connexion en lecture seule, qui lit l'état validé sans prendre l'écrivain, et atterrit
dans `backups/`. Les tours, les battements de bail et Telegram continuent pendant la copie ;
l'archive, sa dérivation de clé et son chiffrement tournent sur un thread à part.
`penelope doctor` donne la durée de la dernière sauvegarde, instantané compris.

### Sauvegarde complète chiffrée, hors de la machine

```bash
penelope secret set backup_passphrase
penelope config set backup.git_remote git@github.com:moi/penelope-backups.git
penelope backup --push
```

L'archive contient l'instantané de la base, le vault, les skills, les workflows, les
gabarits, `mcp.d` et `config.toml` ; les artefacts et les médias reçus en sont exclus
(`--media` les inclut, `backup.include_media` en fait le défaut). Elle est **chiffrée** par
la phrase de passe du magasin de secrets (Argon2id puis XChaCha20-Poly1305) avant de
quitter la machine : sans cette phrase, l'archive ne sert à rien. Les valeurs des secrets
n'y sont jamais ; le `MANIFEST.json` poussé à côté dit la date, la version, les tailles, la
somme SHA-256 et **les noms** des secrets à ressaisir.

Garde-fous : un dépôt public est refusé (vérifié par `gh` quand il est disponible), une
archive au-delà de `backup.max_push_bytes` (100 Mo, la limite de fichier de GitHub) est
refusée avec la marche à suivre, et l'absence de phrase de passe est dite avant tout
travail. La rotation garde 7 quotidiennes, 4 hebdomadaires et 12 mensuelles dans le dépôt
de travail, sans réécrire l'historique. Une sauvegarde part chaque nuit à l'heure de
`backup.cron` (4 h par défaut, vide pour désactiver) ; un échec arrive sur Telegram, jamais
en silence.

### Remonter une instance sur une machine neuve

```bash
penelope restore-all git@github.com:moi/penelope-backups.git --dry-run
penelope restore-all git@github.com:moi/penelope-backups.git
```

La commande clone le dépôt (ou lit une archive `.tar.gz.enc` locale), demande la phrase de
passe à l'invite, puis remet la base et les fichiers à leur place, l'existant étant mis de
côté. Elle se fait **daemon arrêté**. Elle finit par la liste de ce qui reste à faire :
`penelope install` et `penelope start`, les secrets à ressaisir d'après le manifeste, puis
`penelope doctor` (serveurs MCP à réautoriser, modèle de transcription à télécharger).
`penelope doctor` suit aussi l'âge de la dernière sauvegarde et alerte au-delà de 48 h, et
`self_status` le sait : « ta dernière sauvegarde date de cette nuit ».

```bash
penelope audit-verify
```

Recalcule la chaîne de hachage du journal d'événements et nomme le premier maillon rompu
s'il y en a un. Une purge RGPD conserve le hachage d'origine : purger n'invalide pas la
chaîne.

### Relire une requête envoyée

```bash
penelope audit show --session s_01J8      # dernier tour de la session
penelope audit show --turn t_01J8         # un tour précis
```

Rend ce que le modèle avait sous les yeux : le prompt système relu depuis son instantané
et vérifié octet pour octet par l'empreinte déjà enregistrée, sa découpe en tuiles
(T0 identité, T1 capacités, T2 contexte et mémoire), les messages de la projection, et
pour chaque appel ses trois empreintes (`system_hash`, `tools_hash`, `request_hash`) avec
la cause du raté de cache s'il y en a eu une.

Ce qui manque est dit plutôt que comblé : `exact` passe à `non` et `reserves` nomme la
raison — instantané parti en rétention, historique résumé depuis l'appel, messages
effacés. Les textes sortent rédigés. Le détail du mécanisme est dans
[Relire ce que le modèle a lu](context.md#relire-ce-que-le-modèle-a-lu).

### Effacer une conversation

```bash
penelope session purge s_01J8
```

Efface le contenu de la session : messages et index plein texte, contexte figé, résumés,
artefacts (leurs fichiers compris), requêtes au modèle, payloads des tours et des updates
Telegram du chat, candidats de mémoire, et ce que l'agent a fait et dit : arguments et
résultats d'outils, messages envoyés sur Telegram, demandes d'approbation (celles en
attente sont annulées), tâches MCP, jobs d'outils (ceux qui tournaient encore sont
annulés), paramètres et sorties des workflows de la session, et
les prompts système que cette session seule référençait — ceux qu'une autre session lit
encore attendent sa purge à elle. Un
outil déjà exécuté reste reconnu comme tel : sa ligne et sa clé d'idempotence demeurent,
seul leur contenu part. La comptabilité (`usage`) garde ses lignes, ses jetons et ses
coûts ; seule la clé qui menait au texte du prompt est coupée. Le journal d'événements garde ses lignes et leurs
hachages, avec le contenu remplacé, et note la purge dans `audit.purge` : `audit-verify`
reste vert. La commande demande confirmation (`--yes` pour s'en passer, `--reason` pour
noter pourquoi) ; depuis Telegram, `/purge` affiche la même question avec un bouton.

La mémoire durable n'est pas touchée : elle vit dans le vault et s'édite avec ses propres
outils (`penelope mem …`). Une entrée née d'une conversation purgée reste donc en mémoire
si elle y a été promue.

### Rétention

Ce qui n'est ni la mémoire ni la chaîne d'audit finit par disparaître, une passe par jour :

| Réglage | Défaut | Ce qui est effacé au-delà |
|---|---|---|
| `retention.days` | `90` | tours terminés, requêtes au modèle abouties, payloads des updates Telegram, clés de travail (`turn.*`, `prompt.prefix.*`, `wf.*`, `tg.*`…), arguments et résultats des outils menés à terme (un effet incertain garde tout), messages Telegram envoyés, contenu des demandes décidées, tâches MCP terminées, jobs d'outils terminés, sorties des workflows finis, prompts système que plus aucune ligne ne cite |
| `retention.memory_history_days` | `30` | pré-images de la mémoire (`mem_history`), qui gardent chaque fichier avant et après chaque opération du rêve |

`0` désactive la rétention correspondante. Le payload d'un update Telegram est de toute
façon vidé dès qu'il est traité : seul son identifiant sert encore, pour ne pas traiter
deux fois le même message. `penelope doctor` donne la date de la dernière passe et ce que
gardent encore les tables d'effets, d'envois, de demandes, de tâches MCP, d'étapes et de
prompts système : ce sont elles qui grossissent avec l'activité, et elles partent dans la
sauvegarde. Un prompt système suit la ligne d'`usage` qui le cite : tant qu'une
consommation le désigne, il reste relisible par `penelope audit show`.

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

**Raisonnement de la consolidation.** Le tri d'un candidat gagne à être réfléchi : le
raisonnement est **gardé** et budgété, pas éteint. L'appel porte deux budgets, un pour la
réflexion (8 000 jetons au départ, plafond `memory.consolidation_reasoning_tokens`) et un
pour la sortie utile ; `max_tokens` vaut la somme, le fournisseur comptant le raisonnement
dedans. Quand le modèle dépense tout son budget à réfléchir sans rien écrire, ce n'est pas
une réponse trop longue : la passe le dit (« raisonnement plein »), **relève** le budget de
réflexion et rejoue le même lot, au lieu de réduire le lot (réduire le travail ne réduit
pas la réflexion). Au plafond atteint deux fois, elle passe à l'alias de repli pour le
reste de la nuit. `memory.consolidation_reasoning = "off"` éteint le raisonnement
(`reasoning: {enabled: false}`) pour les modèles qui l'acceptent. `penelope doctor`
(`reasoning_effort`) annonce ce qui partira et la part de raisonnement observée sur sept
jours, `dream_power` prévient si la machine est sur batterie à l'heure du rêve, et
`penelope model set` prévient quand un alias d'extraction reçoit un modèle qui impose de
réfléchir (#152).

**Une passe nocturne écrit lot par lot.** Chaque lot est une unité complète : opérations
appliquées au vault, candidats marqués, état de la passe mis à jour, puis seulement le lot
suivant. Une coupure réseau ou une machine qui s'endort ne fait plus perdre le travail
déjà fait : le lot en cours est rejoué au retour, dans la limite du temps de la nuit, et
la passe suivante reprend sur les candidats restants en le disant. Une passe lancée à la
main (`penelope mem dream`) qui échoue prévient au foyer comme une passe planifiée (#152).

**Le délai.** Un lot fusionné sur `main` n'est pas immédiatement installable : la CI
rejoue les suites, pose le tag de la version du workspace, puis la release construit le
binaire universel. Compter une douzaine de minutes entre la fusion et l'apparition dans
`penelope upgrade` (#147). Tant que la release n'est pas publiée, `--check` dit
« à jour » : c'est vrai du dernier binaire publié, pas du dernier commit.

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

puis conserver `minisign.key` dans le gestionnaire de mots de passe du propriétaire du
dépôt et effacer la copie de travail. Cette clé n'a pas de phrase de passe : le
gestionnaire est sa protection. Les binaires publiés ensuite portent la clé publique et
n'acceptent plus que des releases signées.

#### Rotation ou perte de la clé minisign

Si le secret GitHub est perdu mais que `minisign.key` est encore dans le gestionnaire de
mots de passe, restaurer **la même clé** :

```bash
gh secret set MINISIGN_SECRET_KEY < minisign.key
```

Garder `MINISIGN_PUBLIC_KEY` : les instances déjà installées peuvent continuer à
vérifier les releases.

Si la clé privée est définitivement perdue, générer une nouvelle paire avec la commande
ci-dessus. Poser d'abord `MINISIGN_SECRET_KEY`, puis `MINISIGN_PUBLIC_KEY` ; ne pas
déclencher de release entre ces deux opérations. La nouvelle clé publique sera intégrée
aux prochains binaires, mais une instance qui porte l'ancienne clé refusera leur
signature. Sur **chaque** instance concernée, choisir un des recours suivants :

```bash
# Autoriser la nouvelle clé avant une mise à jour depuis la release.
penelope config set upgrade.minisign_pubkey "<nouvelle clé publique RW…>"
penelope upgrade
```

Ou, depuis le dépôt à jour sur cette instance, reconstruire et installer le binaire en
intégrant la nouvelle clé ; conserver l'identité locale de signature macOS si elle est
utilisée :

```bash
PENELOPE_MINISIGN_PUBKEY="<nouvelle clé publique RW…>" \
  SIGN_IDENTITY="Penelope Dev" make deploy
```

Sans l'un de ces gestes sur l'instance, changer seulement la variable GitHub ne suffit
pas : son ancien binaire ne fait pas confiance à la nouvelle clé.

Le répertoire du binaire doit être inscriptible par l'utilisateur du service ; sinon,
ou pour une version pas encore publiée, depuis le dépôt cloné sur la machine :

```bash
make deploy
```

`deploy` enchaîne `git pull --ff-only`, `cargo build --release`, copie le binaire au
**chemin stable** puis redémarre le service (sudo seulement si son répertoire n'est pas
inscriptible). `make update` s'arrête après la compilation, `make clean` libère les Go de
`target/`.

Le service lance toujours le même fichier, que les mises à jour remplacent :
`make deploy` comme `/upgrade install`. Il ne lance jamais `target/release`. Le chemin
stable est le programme du LaunchAgent s'il ne pointe pas dans `target/`, sinon le
`penelope` du PATH hors `target/` (sans service), sinon `~/.local/bin/penelope`
(`INSTALL_DIR=… make deploy` pour un autre répertoire). Si le LaunchAgent lance encore
`target/release`, `make deploy` le réécrit une dernière fois vers ce chemin et le recharge
depuis le shell ; c'est le seul cas où le fichier de service change.

Une mise à jour lancée par le daemon (`/upgrade install`) confie le redémarrage à un
**relais** : un job launchd éphémère (`com.penelope.daemon.reloader`), hors du job du
daemon. Il vérifie que la nouvelle version démarre dans les deux minutes. Sinon, il remet
le binaire précédent au même chemin, relance le service et laisse une note que l'ancienne
version annonce sur Telegram. Journal : `<état>/upgrade/relay/reloader.log`.
`penelope doctor` signale un service qui lance `target/release` et une mise à jour
installée depuis plus de cinq minutes sans jamais avoir démarré.

### Passer d'une installation source aux releases

Une instance installée par `make deploy` lance le binaire du dépôt
(`…/target/release/penelope`) : `penelope upgrade` ne le remplace pas, pour ne pas le
désynchroniser des sources. Pour une machine sans surveillance, la bascule vers les
releases se fait une fois pour toutes, à distance : `/upgrade install` sur Telegram
affiche « Installation depuis les sources. Basculer vers les releases ? » (« Basculer et
installer vX », « Garder les sources », « Annuler »), ou en ligne de commande :

```bash
penelope upgrade --switch
```

Avant d'agir, Pénélope vérifie :

- **Signature** : `upgrade.codesign_identity` est configuré, et un essai de signature
  réussit depuis le daemon. Sinon macOS redemanderait des autorisations que personne ne
  pourrait accepter (voir « Signature locale » ci-dessous).
- **Répertoire cible** : `upgrade.install_dir` (`~/.local/bin` par défaut) est inscriptible.
- **Service** : le LaunchAgent lance bien ce binaire, et son fichier est modifiable.

La bascule télécharge et vérifie la release comme une mise à jour, installe et re-signe le
binaire dans `upgrade.install_dir`, garde le binaire de compilation comme précédent, et
réécrit une dernière fois `ProgramArguments` du LaunchAgent vers ce chemin stable
(l'original est gardé en `….plist.sources` le temps du rechargement). Le relais recharge
le service : il attend que l'ancien daemon soit vraiment arrêté (`launchctl bootout` rend la
main avant), vérifie le `bootstrap` et le retente, et remet le fichier d'origine si launchd
refuse le nouveau. Si la nouvelle version ne démarre pas, le binaire de compilation est
copié au chemin stable : le service ne change plus de programme. Ensuite, `/upgrade
install` suit le parcours normal, et `make deploy` sur la machine installe une compilation
au même chemin.

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

Une question d'effet incertain arrive d'elle-même sur Telegram dès que la passerelle est
prête, une seule par effet quel que soit le nombre de redémarrages, avec la requête telle
que le ledger l'a enregistrée : « ✅ C'est fait » (vérifié : l'effet a eu lieu, il n'est pas
relancé, le modèle reçoit ce résultat), « 🔁 Relancer » (exécuté une fois de plus) ou
« ⏭ Ignorer » (laissé tel quel). Aucune de ces réponses ne crée de règle. En ligne de
commande :

```bash
penelope approve <id> --effect done
```

(`--effect retry` pour relancer, `penelope deny <id>` pour ignorer).

## 11. Ce qui n'est pas encore branché

Tout ce que décrit ce guide fonctionne. Restent : le mode webhook de Telegram,
l'interprétation de `.penelope/deploy.toml` (le déploiement passe par les cibles `make`),
et l'OCR des pages scannées d'un PDF qui a aussi du texte. Voir
[progress.md](progress.md) pour l'état exact.
