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

- `openrouter_api_key` ;
- `telegram_bot_token`.

La valeur se passe **sur l'entrée standard**, jamais en argument : un argument resterait
dans l'historique du shell et serait visible dans `ps`. Le plus propre est de copier la
clé dans le presse-papiers, puis :

```bash
pbpaste | penelope secret set openrouter_api_key
```

La commande fonctionne sans daemon, donc avant le tout premier démarrage. Elle affiche le
nom, le backend et la longueur de la valeur, jamais la valeur elle-même.

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

`sticky` compte plus qu'il n'y paraît : changer de modèle en pleine session casserait le
cache du provider et ferait payer tout le contexte une seconde fois.

Le budget se règle à côté, en dollars :

```toml
[budget]
daily_usd = 20.0
session_usd = 5.0
alert_ratio = 0.8
```

`model set` signale un identifiant absent du catalogue (`known: false`) quand le catalogue
est chargé ; sinon, une faute de frappe ne se verra qu'au premier appel.

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

```bash
cargo build --release && sudo cp target/release/penelope /usr/local/bin/penelope && penelope restart
```

Au démarrage suivant, la reprise (§17) s'exécute : les tours interrompus sont remis en
file, les runs repartent à leur étape courante, les effets restés en vol deviennent des
questions plutôt que des relances. `penelope approvals` montre ce qui attend une réponse.

## 11. Ce qui n'est pas encore branché

Conversation (CLI et Telegram), approbations et catalogue de modèles fonctionnent. Le
superviseur MCP, l'ordonnanceur, le moteur de workflows et le rêve nocturne ne sont pas
encore lancés par le daemon. Voir [progress.md](progress.md) pour l'état exact.
