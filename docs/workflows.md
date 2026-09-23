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
être déclaré dans `parameters` : le validateur vérifie chaque variable. Sont toujours
disponibles : `{{workdir}}`, `{{run.id}}`, `{{now}}`, `{{os}}`, `{{arch}}`, `{{reason}}`
(saisie ou erreur de l'étape précédente), `{{criteriaList}}`, `{{criteriaCount}}`,
`{{pendingCount}}`, `{{modifiedFiles}}`, `{{stepOutput.…}}`, `{{steps.<étape>.…}}` et
`{{brief}}`, le résumé de la conversation qui a lancé le run (vide sinon).

**Dans une `command`, les valeurs sont citées par le moteur.** `{{workdir}}` contient des
espaces sur macOS, où le répertoire de données est `~/Library/Application Support/Penelope`
(issue #154). Écrire simplement :

```json
"command": "git clone -b dev https://gitlab.example.com/{{repo}}.git {{workdir}}/repo"
```

La valeur part entre apostrophes, donc en **un seul** argument, et une apostrophe qu'elle
contiendrait est échappée. Un gabarit qui cite déjà (`"{{workdir}}/repo"`) n'est pas cité
deux fois. Les `prompt`, `cwd` et `args` ne passent par aucun shell : rien n'y est cité.

Pour le cas rare d'une variable qui porte une **liste d'arguments** (`{{extra_args}}`),
où citer ferait un seul argument de plusieurs, l'étape déclare `"quote": false` et
l'auteur reprend la citation à sa charge.

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

Ce que mesure chaque plafond :

| Plafond | Mesure |
|---|---|
| `maxUsd` | Coût facturé des appels du run, d'après le ledger d'usage : la borne de référence |
| `maxTokens` | Tokens **facturés** : entrée hors cache plus sortie. Un préfixe servi par le cache (au dixième du prix) ne compte pas : c'est l'économie voulue (décision 0008) |
| `maxWallMs` | Durée depuis le démarrage du run |
| `maxIterations` | Étapes exécutées |

Avant 0.17.20, `maxTokens` comptait l'entrée entière : un run d'agent qui renvoie un
préfixe de 40 000 tokens à chaque appel se bloquait vers 2 millions de tokens à moins de
10 % de son plafond en dollars. Les quatre workflows livrés gardent `maxTokens: 2000000` :
avec la nouvelle mesure, ce même run en compte environ 220 000.

Un run bloqué par une borne le dit avec ses chiffres (« budget de tokens atteint (440000
tokens facturés sur 300000) ») et la commande qui la relève, pour ce run seul :

```bash
penelope wf control <run> budget --tokens 4000000 --usd 10
```

Le relèvement laisse une trace (`workflow.budget_raised`) ; `resume` reprend ensuite à
l'étape courante, sans rejouer les effets faits. Un `resume` sur un run encore au-dessus de
sa borne répond « toujours bloqué : … » sans changer son état. Relever le plafond de la
session (`session budget`) ne relève pas celui d'un run.

`admission` décide de ce qui arrive quand un run du même workflow tourne déjà :

| Valeur | Effet |
|---|---|
| `parallel` | Le nouveau run démarre à côté |
| `hold` (défaut) | Il attend la fin du précédent |
| `coalesce` | Il fusionne avec le run en cours |
| `drop` | Il est abandonné |

`workspace` vaut `ephemeral` (répertoire jeté à la fin) ou `persistent:<nom>` (répertoire
réutilisé d'un run à l'autre, purgé selon la rétention configurée). Un sous-workflow
travaille dans l'espace de son parent, sauf s'il demande un espace persistant.

`forms` déclare les formulaires des étapes `user` (voir plus bas) : un JSON Schema d'objet
par identifiant.

## Les neuf types d'étapes

| Type | Champs propres | Ce qu'il fait |
|---|---|---|
| `agent` | `prompt` (obligatoire), `nudgePrompt`, `model`, `tools`, `agentId` | Un tour d'agent dans la session du run |
| `sub_agent` | `prompt` (obligatoire), `outputSchema`, `subAgentType` | Un sous-agent isolé qui rend une sortie structurée |
| `shell` | `command` (obligatoire), `cwd`, `successExitCodes`, `network` | Une commande, sous bac à sable ; réseau coupé sauf `network: true` (montré dans l'aperçu) ou `sandbox.shell_network` |
| `tool` | `tool` (obligatoire), `args` | Un outil natif ou MCP, arguments validés contre son schéma |
| `user` | `template` (obligatoire), `choices` (non vide), `input` | Une question au propriétaire sur Telegram |
| `parallel` | `children` (non vide), `maxConcurrency` | Plusieurs enfants en parallèle |
| `workflow` | `workflowId` (obligatoire), `params` | Un sous-workflow, profondeur bornée |
| `wait` | `on` | Attend un événement, un cron, un délai ou une tâche MCP |
| `verify` | `verifier` ou `checks`, `criteriaKey` | Vérifications mécaniques puis jugement du modèle ; `project_tests` relit la commande validée dans `session_metadata.verification`, sinon celle de `project` |

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
"on": { "mcp_task": "forge:{{steps.lancer.structuredContent.taskId}}" }
"on": { "mcp_task": { "server": "forge", "task": "{{params.tache}}" } }
```

`mcp_task` suit une tâche MCP longue (extension Tasks) : elle est enregistrée dans
`mcp_tasks`, sondée par `tasks/get` à intervalle croissant (2 s à 1 min) et survit à un
redémarrage du daemon. L'étape se déclenche (`fired`) quand la tâche se termine, quel que
soit son sort : la sortie porte `status` (`completed`, `failed`, `cancelled`) et `result`
(lu par `tasks/result`). `timeoutMs` borne l'attente (`timeout`).

`timeoutMs` borne aussi une étape `agent`, `sub_agent`, `shell`, `tool`, `verify` ou
`parallel` : au-delà, l'étape rend `timeout`, ce résultat est journalisé et le workflow
suit sa transition (`"condition": {"type": "step_result", "result": "timeout"}`). Seule
l'étape est arrêtée : le run continue, et dans un `parallel` les frères vont au bout. Un
`timeout` ne déclenche pas de nouvelle tentative `retry` (qui ne joue que sur `failure` et
`error`) : une étape qui expire doit être traitée par sa transition.

Une étape `user` peut demander une saisie : `"input": "text"` (un message libre après le
choix) ou `"input": "form:<id>"`, qui renvoie à `settings.forms` :

```json
"settings": {
  "forms": {
    "deploy": {
      "type": "object",
      "required": ["environnement", "version"],
      "properties": {
        "environnement": { "type": "string", "title": "Environnement", "enum": ["prod", "staging"] },
        "version": { "type": "string", "title": "Version" },
        "notifier": { "type": "boolean", "title": "Prévenir l'équipe" }
      }
    }
  }
}
```

Sur Telegram, le choix ouvre le formulaire : un champ par écran (boutons pour une
énumération ou un booléen, un message pour le reste), « Précédent », « Passer » pour un
champ facultatif, récapitulatif puis « Envoyer ». La saisie est validée contre le schéma
et arrive en objet dans `stepOutput.input`. En ligne de commande :
`penelope wf control <run> answer --choice Déployer --input '{"environnement": "prod", "version": "1.4.2"}'`.

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

`subGroup` marque les étapes d'une même boucle (une tranche, sémantique OpenFox du
§12.7). Une boucle ne se quitte que par une transition **taguée** : toute transition d'une
étape du groupe vers une étape hors du groupe, ou vers `$done`, porte un `tag` qui dit
pourquoi la boucle s'arrête. `$blocked` reste l'issue d'échec implicite.

```json
{ "id": "verifier", "type": "shell", "subGroup": "correctif",
  "command": "make test",
  "transitions": [
    { "goto": "livrer", "tag": "vert", "condition": { "type": "step_result", "result": "success" } },
    { "goto": "corriger" }
  ] }
```

Au chargement : une sortie sans tag est une erreur, un groupe sans aucune sortie taguée
aussi (il ne pourrait que boucler), un tag sur une transition qui reste dans le groupe est
signalé comme sans effet. À l'exécution, la sortie est tracée par l'événement
`workflow.subgroup_exited` (groupe, tag, étapes de départ et d'arrivée).

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

La rétention (`workflows.workspace_retention_days`, sept jours par défaut) ne concerne
que les runs terminés. Un run `paused` conserve son espace de travail. Une fois par
jour, le daemon mesure les workspaces des runs `running`, `paused` et `blocked` sous
`state/runs/<run_id>` ; à partir de 1 Gio, il écrit un avertissement avec l'état, le
chemin et la taille, ainsi qu'un événement `workflow.workspace_large`. Il ne suit pas
les liens symboliques et ne supprime pas ces workspaces. Si un build a rempli le
disque, examiner le run et ses artefacts avant de le reprendre ou de l'annuler.

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
| `deploy-generic` | Déploiement du dépôt cloné, appelé en sous-workflow |

Ils sont validés au chargement comme n'importe quel fichier utilisateur : un workflow
livré qui deviendrait invalide ferait échouer les tests plutôt que de se charger à moitié.
Un workflow utilisateur de même identifiant remplace celui livré.

**Le contrat des critères** (`build-verify`, `ticket-to-deploy`). Le plan pose la liste
avec `session_metadata` op=`set` key=`criteria` entry=`[{"id": "…", "text": "…",
"status": "pending"}, …]` ; `label` vaut `text`, un statut absent vaut `pending`. On coche
un critère rempli par op=`update` entry=`{"id": "<id>", "status": "completed"}`. Les
statuts sont `pending`, `completed`, `passed` et `failed` ; `completed` et `passed`
cochent. Une entrée sans `id` connu, un `id` en double ou un statut hors vocabulaire sont
refusés avec ce qu'il faut, au lieu d'être écrits pour rien. La boucle `build` part vers
`verify` quand tous sont cochés ; sinon l'événement `workflow.step` et le journal disent
lesquels la retiennent (« tests (pending) », « doc (absent) »). `session_metadata`
n'écrit que l'état de la session et passe sans approbation, comme `session_notes`.

`build-verify` fait aussi déclarer le projet par son plan : op=`set` key=`project`
entry=`{"dir": "<dépôt>", "test_command": "<commande>"}`. Son `verify` lance cette
commande dans ce répertoire ; sans elle, la cible `make test`, sinon `cargo test`, `npm
test` ou `go test ./...` selon le dépôt. `review` reste écrit pour un dépôt Rust.

Pendant `build`, l'agent pose ou actualise `session_metadata.verification` avec `dir`,
`test_command` (la commande **effectivement validée**, prérequis PATH ou `ulimit`
explicites), `prerequisites` et `evidence` : liste de `{kind, ref, sha}`. Les preuves
`pr`, `ci` et `tdd_green` portent le SHA de la révision contrôlée ; `tdd_red` peut
documenter le commit antérieur. Les références de TDD portent un artefact ou un chemin
lisible. Un contrat sans preuve est refusé. Le contrat persiste avec la session en cas de redémarrage ;
il ne contient pas d'environnement complet ni de secrets. Le vérificateur reçoit
l'objectif, les critères, ce contrat, les sorties des contrôles et les règles
`AGENTS.md`/`CLAUDE.md` du dépôt. Il consulte les preuves et peut les contester ; un
SHA périmé est refusé avant son jugement. Les tests passent par `shell_exec` avec ses
politiques d'approbation et son bac à sable. Le résultat distingue `prerequisite_missing`,
`evidence_missing`, `stale_evidence`, `test_failed` et `criterion_failed`, afin que la
boucle `build` corrige la bonne cause.

`ticket-to-deploy` enchaîne : lecture du ticket par un sous-agent dans son tracker,
dépôt et forge résolus par un sous-agent (question si besoin, puis nouvelle résolution),
clone dans l'espace du run sur la branche `penelope/<ticket>`, analyse et plan (agent,
qui reçoit le ticket et `{{brief}}`), proposition au propriétaire, implémentation
jusqu'aux critères remplis, vérification parallèle (tests, lint, relecture, vérificateur),
push, demande de fusion, porte de déploiement, déploiement, commentaire de clôture sur le
ticket. Tests et lint prennent la cible `make test` / `make lint` du dépôt, sinon
`cargo`, `npm` ou `go`.

Tracker et forge ne sont pas figés : les étapes qui les touchent (`fetch_ticket`,
`create_pr`, `update_ticket`, `report`) n'ont que `tool_search`, `tool_describe` et
`tool_call`, et trouvent à l'exécution l'outil du serveur MCP connecté (Redmine ou
ClickUp pour le ticket, pull request GitHub ou merge request GitLab pour la forge). Le
paramètre facultatif `tracker` nomme le serveur quand l'adresse du ticket ne suffit pas ;
`repo` donne le dépôt s'il est connu. Les écritures passent par les approbations comme
tout outil MCP.

`deploy-generic` exige `.penelope/deploy.toml` dans le dépôt et lance `make deploy`,
`make smoke`, puis `make rollback` en cas d'échec, avec `ENV=<environnement>`
([décision 0007](decisions/0007-deploiement-par-makefile.md)). Un dépôt minimal :

```make
test:
	cargo test
deploy:
	./scripts/deploy.sh $(ENV)
smoke:
	curl -fsS https://service.exemple.fr/health
rollback:
	./scripts/rollback.sh $(ENV)
```

Le scénario complet tourne dans la suite `workflow` contre des mocks (tracker et forge
MCP, dépôt git local, déploiement `make`), avec approbations par le mock Telegram et un
redémarrage du daemon entre chaque passage :

```bash
cargo test -p penelope-daemon ca_12_1
```

## Lancer, planifier, suivre

Le daemon pilote les runs en tâche de fond et sert toutes les méthodes `wf.*` et
`schedule.*` : `penelope wf run <id>`, `/run <id>` sur Telegram, ou une planification
(`penelope schedule add`, `/schedules`) qui lance un workflow sur un cron, un intervalle,
un fichier surveillé ou un sondage MCP. Une carte de progression suit le run sur
Telegram ; les questions d'une étape `user` arrivent avec leurs boutons.

En conversation Telegram, Pénélope complète les paramètres avec ses outils, demande
ce qui manque, puis propose un plan avec `workflow_plan` (`goal`, `steps`, `params`,
`brief`). Les corrections et retours arrière créent de nouvelles versions. Une carte
dans le sujet d'origine montre le plan et son bouton « Vas-y ». Ce clic persiste l'état
approuvé ; l'exécution de ce plan viendra avec T3 de #185. Le lancement direct avec
`workflow_start` est refusé dans une conversation Telegram. `/run <id>` passe toujours
par cette conversation, même quand des paramètres sont fournis. La CLI et les runs
techniques continuent d'utiliser le moteur existant.

## Limites actuelles

- Le contenu de `.penelope/deploy.toml` n'est pas encore interprété : c'est un marqueur,
  les commandes viennent des cibles `make`.
- Une tâche MCP n'est suivie qu'une fois connue de l'étape `wait` : un appel d'outil qui
  rend une tâche n'enregistre rien tout seul.

Voir [progress.md](progress.md).
