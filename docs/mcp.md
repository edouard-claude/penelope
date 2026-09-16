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
| `stdio` | Serveur local lancé par Pénélope | Le processus tourne dans son propre groupe, sous profil de bac à sable `mcp-stdio` ; les orphelins sont récupérés au démarrage |
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
| `tool_search` | Cherche par mots-clés (FTS + repli lexical). Rend noms, descriptions courtes, niveau de risque |
| `tool_describe` | Rend les schémas complets de 20 outils au plus |
| `tool_call` | Appelle un outil par son nom qualifié `mcp__<serveur>__<outil>`, arguments validés contre son schéma **avant** l'envoi |

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

## Limites actuelles

- Les requêtes `sampling/createMessage` d'un serveur sont refusées et les formulaires
  d'élicitation déclinés.
- Les filtres d'outils `include`/`exclude` d'Hermes n'ont pas d'équivalent : tous les
  outils d'un serveur sont exposés, la politique par outil (`tool_policy`) en restreint
  l'usage.

Voir [progress.md](progress.md).
