# Workflows

Un workflow est une machine à états durable. Chaque étape est un point de reprise :
si le daemon meurt, le run repart à son étape courante, jamais au début.

Le schéma publié est [`schemas/workflow.schema.json`](../schemas/workflow.schema.json).
Il est vérifié par un test contre les workflows livrés, donc il ne peut pas dériver du
code. Le validateur de référence reste :

```bash
penelope wf validate mon-workflow.workflow.json
```

Il fonctionne **sans daemon** : éditer un workflow en SSH ou le valider en CI ne demande
rien de plus que le binaire.

## Fichier

Un fichier JSON par workflow, dans `workflows/`, nommé `<id>.workflow.json`. L'identifiant
doit être un slug `[a-z0-9-]` **égal au nom du fichier** : une incohérence est une erreur,
pas un avertissement, parce qu'elle rend le workflow introuvable.

```json
{
  "metadata": {
    "id": "compte-rendu",
    "name": "Compte rendu hebdomadaire",
    "description": "Rassemble la semaine et propose un compte rendu.",
    "parameters": [
      { "id": "semaine", "type": "integer", "required": true, "label": "Numéro de semaine" }
    ]
  },
  "entryStep": "rassembler",
  "settings": {
    "budget": { "maxUsd": 2.0 },
    "workspace": "ephemeral"
  },
  "steps": [
    {
      "id": "rassembler",
      "name": "Rassembler",
      "type": "agent",
      "phase": "plan",
      "model": "reasoning",
      "prompt": "Rassemble ce qui s'est passé en semaine {{semaine}}.",
      "transitions": [{ "goto": "rediger" }]
    },
    {
      "id": "rediger",
      "type": "agent",
      "phase": "build",
      "model": "main",
      "prompt": "Rédige le compte rendu.",
      "transitions": [{ "goto": "$done" }]
    }
  ]
}
```

Un fichier invalide est **rejeté sans remplacer la version précédente** : un workflow qui
tournait continue de tourner, même si on vient d'en écrire une version cassée.

## Métadonnées

| Champ | Obligatoire | Notes |
|---|---|---|
| `id` | oui | Slug, égal au nom du fichier |
| `name`, `description` | oui | Non vides |
| `version`, `color` | non | `1.0.0` et `#3b82f6` par défaut |
| `parameters` | non | `string`, `number`, `integer`, `boolean`, `enum` |
| `platforms` | non | `macos`, `linux`, `windows`. Vide = partout. Une étape `shell` doit alors offrir une commande pour chaque plateforme déclarée |
| `config` | non | Configuration libre, lue par le workflow lui-même |

Un `{{paramètre}}` employé dans un `prompt`, une `command`, un `cwd` ou des `args` doit
être déclaré dans `parameters` : le validateur vérifie chaque variable.

## Réglages

```json
"settings": {
  "maxIterations": 40,
  "budget": { "maxUsd": 5.0, "maxTokens": 2000000, "maxWallMs": 7200000 },
  "concurrency": { "maxConcurrent": 1, "admission": "hold" },
  "workspace": "ephemeral"
}
```

Un budget global est **obligatoire** : au moins un des trois plafonds doit être non nul.
Un workflow sans plafond est un workflow qui peut coûter n'importe quoi.

`admission` décide de ce qui arrive quand un run du même workflow tourne déjà :

| Valeur | Effet |
|---|---|
| `parallel` | Le nouveau run démarre à côté |
| `hold` (défaut) | Il attend la fin du précédent |
| `coalesce` | Il fusionne avec le run en cours |
| `drop` | Il est abandonné |

`workspace` vaut `ephemeral` (répertoire jeté à la fin) ou `persistent:<nom>` (répertoire
réutilisé d'un run à l'autre, purgé selon la rétention configurée).

## Les neuf types d'étapes

| Type | Champs propres | Ce qu'il fait |
|---|---|---|
| `agent` | `prompt` (obligatoire), `nudgePrompt`, `model`, `tools`, `agentId` | Un tour d'agent dans la session du run |
| `sub_agent` | `prompt` (obligatoire), `outputSchema`, `subAgentType` | Un sous-agent isolé qui rend une sortie structurée |
| `shell` | `command` (obligatoire), `cwd`, `successExitCodes` | Une commande, sous bac à sable |
| `tool` | `tool` (obligatoire), `args` | Un outil natif ou MCP, arguments validés contre son schéma |
| `user` | `template` (obligatoire), `choices` (non vide), `input` | Une question au propriétaire sur Telegram |
| `parallel` | `children` (non vide), `maxConcurrency` | Plusieurs enfants en parallèle |
| `workflow` | `workflowId` (obligatoire), `params` | Un sous-workflow, profondeur bornée |
| `wait` | `on` | Attend un événement, un cron, un délai ou une tâche MCP |
| `verify` | `verifier` ou `checks`, `criteriaKey` | Vérifications mécaniques puis jugement du modèle |

Les enfants d'un `parallel` sont limités à `sub_agent`, `shell` et `tool` : un `agent` ou
un `user` en parallèle rendrait la conversation incompréhensible.

`model` est un **alias** (`main`, `fast`, `reasoning`, `code`, `summarizer`), jamais un
identifiant de modèle brut : changer de modèle est un réglage, pas une réécriture de tous
les workflows.

Une étape `shell` peut porter une commande par système, la clé la plus spécifique gagnant
(`macos` avant `unix`) :

```json
{ "id": "ouvrir", "type": "shell",
  "command": { "macos": "open rapport.pdf", "unix": "xdg-open rapport.pdf" } }
```

Une étape `wait` attend exactement une chose :

```json
"on": { "cron": "0 8 * * 1" }
"on": { "duration_ms": 900000 }
"on": { "event": "pr.merged" }
"on": { "mcp_task": "{{tache}}" }
```

## Transitions

Les transitions sont évaluées **dans l'ordre déclaré** ; la première vraie gagne. Si
aucune n'est vraie, le run passe à `$blocked` et attend une décision humaine. C'est
volontaire : un workflow bloqué se voit, un workflow qui continue au hasard ne se voit
pas.

```json
"transitions": [
  { "goto": "verifier",
    "condition": { "type": "metadata_all_in", "key": "criteria",
                   "field": "status", "values": ["completed", "passed"] } },
  { "goto": "construire" }
]
```

Une transition sans `condition` vaut `{"type": "always"}`.

| Type de condition | Champs | Vrai quand |
|---|---|---|
| `always` | — | Toujours |
| `step_result` | `result` | Le résultat de l'étape vaut `result` |
| `metadata_all_match` | `key`, `field`, `value` | Tous les éléments de la liste ont ce champ à cette valeur |
| `metadata_any_match` | `key`, `field`, `value` | Au moins un élément |
| `metadata_all_in` | `key`, `field`, `values` | Tous les éléments ont ce champ dans l'ensemble |
| `output_match` | `path` + `equals` \| `in` \| `regex` | La sortie de l'étape correspond |
| `all` | `of` | Toutes les sous-conditions |
| `any` | `of` | Au moins une sous-condition |
| `not` | `cond` | La sous-condition est fausse |

**Vacuité assumée** : une condition `metadata_all_*` sur une liste vide est **vraie**.
Un workflow qui n'a encore produit aucun critère avance donc ; c'est la sémantique du PRD
§12.4, et un test la fixe explicitement pour qu'elle ne soit pas « corrigée » par accident.

`output_match` utilise un chemin simplifié : `resultat.items[0].statut`, le `$.` de tête
étant optionnel.

`$done` termine le run, `$blocked` l'arrête en attente d'un humain. Aucune étape ne peut
porter un identifiant commençant par `$`.

## Sous-groupes

`subGroup` marque un ensemble d'étapes qui forment une boucle logique. Le champ est lu et
validé, mais la sortie par transition taguée (sémantique OpenFox du §12.7) n'est pas
encore appliquée : les transitions d'un sous-groupe se suivent comme les autres.

## Cycle de vie d'un run

```
                   admission
  déclencheur ──────────────► running ──────────────► done
                                │                      ▲
                                ├── $blocked ──────────┤ (décision humaine)
                                ├── paused ────────────┤ (pause / resume)
                                └── cancelled
```

```bash
penelope wf runs
```

```bash
penelope wf trace <run>
```

```bash
penelope wf control <run> pause
```

Opérations : `pause`, `resume`, `cancel`, `retry-step`, `skip-step`, `goto:<étape>`.
Les deux dernières exigent une approbation du propriétaire : sauter une étape ou aller
ailleurs change ce que le workflow garantit.

## Reprise

Chaque franchissement d'étape écrit, dans la même transaction : la ligne du run, son
étape courante, ses sorties accumulées et une entrée dans le journal d'étapes. Au
redémarrage, un run `running` est repris là où il en était, et son compteur d'itérations
n'est pas rejoué.

Les effets déclenchés par une étape passent par le ledger **avant** exécution. Un effet
resté en vol au moment du crash devient une question, pas une seconde exécution.

```bash
cargo test -p penelope-evals --test resilience
```

## Workflows livrés

| Identifiant | Rôle |
|---|---|
| `build-verify` | Planifier, implémenter, vérifier, boucler tant que les critères ne sont pas remplis |
| `review` | Lint, tests et relecture en parallèle, résultats dans `review_findings` |
| `ticket-to-deploy` | Scénario de référence du §12.10 : du ticket au déploiement, avec portes humaines |
| `deploy-generic` | Déploiement paramétrable, appelé en sous-workflow |

Ils sont validés au chargement comme n'importe quel fichier utilisateur : un workflow
livré qui deviendrait invalide ferait échouer les tests plutôt que de se charger à moitié.

## Lancer, planifier, suivre

Le daemon pilote les runs en tâche de fond et sert toutes les méthodes `wf.*` et
`schedule.*` : `penelope wf run <id>`, `/run <id>` sur Telegram, ou une planification
(`penelope schedule add`, `/schedules`) qui lance un workflow sur un cron, un intervalle,
un fichier surveillé ou un sondage MCP. Une carte de progression suit le run sur
Telegram ; les questions d'une étape `user` arrivent avec leurs boutons.

## Limites actuelles

- Sous-groupes : voir plus haut, la sortie par transition taguée n'est pas appliquée.
- La saisie `form:<schema>` d'une étape `user` et l'attente `mcp_task` ne sont pas
  implémentées.
- Le scénario `ticket-to-deploy` de bout en bout contre des mocks (CA 12) reste à écrire.

Voir [progress.md](progress.md).
