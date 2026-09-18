# Client MCP

Le client MCP est la priorité absolue du PRD : il doit parler à **tous** les serveurs,
quelle que soit leur version de protocole, sans que l'utilisateur ait à le savoir.

## Versions de protocole

Cinq versions sont implémentées et testées, chacune contre un serveur simulé qui se
comporte comme sa version :

| Version | Ce qu'elle apporte |
|---|---|
| `2024-11-05` | Socle : `initialize`, `tools`, `resources`, `prompts`, stdio et SSE historique |
| `2025-03-26` | Streamable HTTP, `completion/complete`, annotations d'outils |
| `2025-06-18` | Sortie structurée (`structuredContent` + `outputSchema`), élicitation, `resources/subscribe` |
| `2025-11-25` | Tâches longues, ressources typées, journalisation serveur |
| `2026-07-28` | Découverte sans état par `server/discover`, tâches avec reprise (défaut) |

## Négociation

Une seule séquence, appliquée à tout serveur, quel qu'il soit :

```
1. server/discover (2026-07-28)
     ├── succès ────────────────► mode sans état, version annoncée par le serveur
     ├── 404 / méthode absente ─┐
     └── erreur transport ──────┤
                                ▼
2. initialize avec la version préférée
     ├── succès ────────────────► version annoncée par le serveur (peut être < demandée)
     └── −32022 UnsupportedProtocolVersion
                                ▼
3. initialize avec la meilleure version commune annoncée dans l'erreur
     └── échec ─────────────────► serveur marqué en échec, jamais de boucle
```

Conséquence directe, vérifiée par un test : deux serveurs de versions différentes
peuvent vivre derrière la même URL et sont chacun traités selon la sienne.

## Transports

| Transport | Quand | Notes |
|---|---|---|
| `stdio` | Serveur local lancé par Pénélope | Le processus tourne dans son propre groupe, sous profil de bac à sable `mcp-stdio` : lectures de `sandbox.deny_read` refusées (clés, secrets, base), trousseau fermé ; les orphelins sont récupérés au démarrage |
| Streamable HTTP | Serveur distant, ≥ 2025-03-26 | Session par en-tête, reprise de flux |
| SSE historique | Serveur distant < 2025-03-26 | Conservé pour les serveurs anciens, jamais choisi spontanément |

`transport = "auto"` (défaut) choisit d'après les champs présents : `command` ⇒ stdio,
`url` ⇒ HTTP.

## Déclarer un serveur

Un fichier TOML par serveur dans `mcp.d/`. Le nom du fichier donne le nom du serveur.

```toml
# mcp.d/forge.toml
url = "https://forge.exemple.fr/mcp"
headers = { Authorization = "${SECRET:FORGE_TOKEN}" }
scopes = ["repo:read", "repo:write"]
timeout = "45s"
eager_schemas = false

[tool_policy]
create_pr = "ask"
delete_branch = "ask_twice"
```

```toml
# mcp.d/compta.toml
command = "compta-mcp"
args = ["--stdio"]
env = { COMPTA_BASE = "{data}/compta" }
lazy_start = true
idle_timeout = "10m"
roots = ["{data}/compta"]
```

Un fichier invalide est **signalé**, pas fatal : les autres serveurs se chargent quand
même. Déposer, modifier ou retirer un fichier est pris en compte à chaud, sans
redémarrage.

`roots` mérite une seconde de réflexion : ce sont les répertoires exposés au serveur par
`roots/list`. Jamais le home entier.

## Registre paresseux

Un serveur peut publier des centaines d'outils. Les injecter tous dans le prompt coûterait
plus cher que le travail demandé. Le registre est donc **paresseux** : le modèle reçoit
trois méta-outils, et découvre le reste à la demande.

| Méta-outil | Rôle |
|---|---|
| `tool_search` | Cherche par mots-clés (FTS + repli lexical). Rend noms, descriptions courtes, niveau de risque ; les outils natifs à la demande d'abord (serveur `natif`) |
| `tool_describe` | Rend les schémas complets de 20 outils au plus |
| `tool_call` | Appelle un outil par son nom qualifié `mcp__<serveur>__<outil>`, arguments (`args`, objet libre, ou `args_json` en chaîne) validés contre son schéma **avant** l'envoi ; un refus rend les paramètres attendus, un nom inconnu les noms proches ; un outil natif par son nom, comme un appel direct |

Les mêmes méta-outils mènent aux outils natifs rares (planification, git, skills,
intentions, gestion des workflows), sortis de la liste de chaque appel de conversation
pour la garder sous 20 définitions : voir « Outils natifs » dans
[install-headless.md](install-headless.md#outils-natifs). Contrairement à la promotion
MCP ci-dessous, un outil natif décrit ou appelé rejoint la liste dès le tour suivant : la
liste change à son arrivée et à son départ (dix tours sans usage), deux préfixes non
cachés au lieu de 3 000 tokens de schémas payés à chaque appel.

Un outil réellement utilisé est marqué pour promotion dans l'ensemble « collant », qui
sera injecté directement au prompt. La promotion ne prend effet **qu'à une frontière de
compaction** : promouvoir en cours de session casserait le préfixe stable et donc le
cache du fournisseur. C'est l'unique moment où la liste d'outils change.

`eager_schemas = true` force les schémas d'un petit serveur critique en tuile T1, sous un
plafond global d'octets.

## Risque et approbation

Les annotations d'outil (`readOnlyHint`, `destructiveHint`, `openWorldHint`) sont des
**indices**, pas une autorité : elles viennent du serveur, donc d'une source non fiable.
Elles alimentent une classification de risque que la configuration peut surcharger, outil
par outil, via `tool_risk`.

```toml
[mcp.policy]
read = "auto"
write = "ask"
destructive = "ask_twice"
external = "ask"
unknown = "ask"
```

Un résultat d'outil qui contient `isError: true` est transmis tel quel au modèle : c'est
une réponse, pas une panne du harnais, et le modèle doit pouvoir la corriger seul.

Les **descriptions** et les schémas des outils viennent eux aussi du serveur, et le modèle
les lit comme des consignes (« tool poisoning »). `tool_search` et `tool_describe` les
rendent encadrés comme contenu non fiable, avec l'alerte du détecteur local s'il y voit une
consigne ; un outil exposé d'office (`eager_schemas`) porte la mention de son serveur, et sa
description est retirée si elle est suspecte. Chaque inscription d'outil est journalisée
(`mcp_tool_suspicious` quand le détecteur signale quelque chose) et la carte d'approbation
d'un outil signalé le dit.

Chaque outil est aussi **épinglé** par l'empreinte de sa description, de son schéma et de
ses annotations. Si un serveur les change en silence (`notifications/tools/list_changed`
après une mise à jour du paquet), les règles « Toujours » de cet outil sont révoquées,
l'événement `mcp.tool_changed` est journalisé et le propriétaire est prévenu : la prochaine
utilisation redemande. Une liste identique ne change rien.

## OAuth 2.1

Le flux complet est implémenté et testé contre un serveur d'autorisation simulé.

```
appel MCP ──► 401 + WWW-Authenticate: resource_metadata="…"
                 │
                 ▼
         RFC 9728  métadonnées de la ressource protégée
                 │
                 ▼
         RFC 8414  métadonnées du serveur d'autorisation
                 │
                 ▼
  enregistrement du client : CIMD  >  pré-enregistré  >  DCR (RFC 7591)
                 │
                 ▼
  authorize + PKCE S256 + resource (RFC 8707) + state
                 │
                 ▼
  redirection ──► validation de `iss` (RFC 9207) et de `state`
                 │
                 ▼
             échange du code ──► jeton, rafraîchi avant expiration
```

Deux modes de redirection, réglés par `mcp.oauth_redirect_mode` :

- `paste_back` (défaut, headless) : Pénélope envoie l'URL d'autorisation sur Telegram,
  l'humain se connecte depuis son téléphone, puis recolle l'URL de redirection dans la
  conversation. Rien n'a besoin d'écouter sur un port.
- `public_callback` : une URL publique de rappel, pour le cas où la machine est joignable.

Le consentement incrémental est géré : si un appel exige une portée manquante, seules les
portées nouvelles sont demandées, en conservant celles déjà accordées.

## Supervision

- Démarrage paresseux par défaut, extinction après `idle_timeout`.
- Backoff exponentiel borné après un échec, avec un plafond de tentatives : un serveur
  cassé ne mange pas la machine.
- Éviction LRU au-delà de `mcp.max_processes` (24 par défaut).
- Quantiles de latence et taux d'erreur par serveur, visibles dans `penelope doctor`.
- Au démarrage, les processus orphelins d'une vie antérieure sont tués à partir des PID
  laissés dans `state/mcp-pids`.

## Tester

La matrice versions × transports × primitives tourne sans réseau ni sous-processus, via
un transport en boucle qui implémente le même contrat que les vrais :

```bash
cargo test -p penelope-evals --test mcp_conformance
```

19 tests : négociation sur toute la matrice, versions mélangées derrière une URL,
annotations vers risque, sortie structurée validée, `isError` transmis, ressources,
gabarits, complétions, abonnements, journalisation, santé, tâches, élicitation
`input_required`, découverte OAuth, PKCE, rejet d'un `iss` invalide, consentement
incrémental, et le flux `paste_back` de bout en bout à travers le mock Telegram.

## Administration

Le daemon démarre le superviseur et sert toutes les méthodes `mcp.*` : `penelope mcp
list|show|add|edit|rm|enable|disable|restart|test|auth|logs` en ligne de commande, `/mcp`
sur Telegram. Un serveur qui demande une autorisation OAuth passe en
`auth_required` ; le lien arrive sur Telegram (`/mcp auth <nom>` le redemande) et
l'adresse de retour se colle dans la conversation, avec ou sans `http://`. Le détail
d'installation est dans [install-headless.md](install-headless.md), section « Serveurs
MCP ».

`penelope import hermes` reprend les `mcp_servers` d'un `config.yaml` Hermes : chaque
serveur est converti en `mcp.d/<nom>.toml`, essayé, puis marqué `ok`, `auth_required` ou
`failed` ; les secrets partent dans le SecretStore.

## Élicitation

Un serveur peut demander au propriétaire de confirmer une action, de remplir un
formulaire ou d'ouvrir un lien (`elicitation/create`). La demande arrive sur Telegram,
dans une carte qui nomme le serveur et cite son message :

- **confirmation** (schéma sans champ) : Accepter, Refuser, Annuler ;
- **formulaire** : « Remplir » ouvre un champ par écran (texte, nombre, booléen, choix
  simple ou multiple, titré ou non, valeurs par défaut), récapitulatif modifiable, puis
  Envoyer ; la saisie est validée contre `requestedSchema` ;
- **lien** (2025-11-25 et suivantes) : domaine en gras, adresse entière, alerte si le
  domaine est en Punycode ; rien n'est ouvert ni téléchargé sans l'accord du
  propriétaire. Après accord, un bouton ouvre le lien ; la fin signalée par le serveur
  (`notifications/elicitation/complete`) met la carte à jour.

Sans réponse avant `elicitation_timeout` (10 min par défaut, réglable par serveur), la
demande est annulée (`cancel`) et la carte le dit. Pendant l'attente, le délai de l'appel
d'outil qui a déclenché la demande est suspendu.

```toml
# mcp.d/redmine.toml
command = "redmine-mcp"
elicitation_timeout = "5m"
```

En 2026-07-28, la demande arrive dans un résultat `input_required` : l'appel est relancé
avec les réponses (`inputResponses`) et l'état du serveur (`requestState`), quatre fois au
plus. Une erreur −32042 (lien exigé) présente les liens, attend leur fin, puis retente
l'appel une fois.

Le modèle lit dans le résultat de l'outil qui a répondu : le propriétaire (accepté,
refusé, annulé), le délai dépassé, ou une annulation sans sollicitation quand aucun canal
ne joint le propriétaire. Il n'a donc pas à deviner pourquoi un serveur a renoncé.

L'élicitation n'est **annoncée** que si Telegram est configuré, et le sampling, toujours
refusé, ne l'est jamais : un serveur qui sait se passer de confirmation garde ses
replis.

## Limites actuelles

- Les requêtes `sampling/createMessage` d'un serveur sont refusées (et la capacité n'est
  pas annoncée).
- Les filtres d'outils `include`/`exclude` d'Hermes n'ont pas d'équivalent : tous les
  outils d'un serveur sont exposés, la politique par outil (`tool_policy`) en restreint
  l'usage.

Voir [progress.md](progress.md).
