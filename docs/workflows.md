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
| `maxCachedTokens` | Tokens servis depuis le cache, comptés **à part** (1.0.48, #337). Optionnel : absent ou `0`, sans plafond ; le compteur s'affiche quand même |
| `maxWallMs` | Durée de **travail** du run, depuis son démarrage : le temps passé `paused` ou `blocked` (1.0.48, #337) et l'attente d'une étape `user` (une carte laissée une nuit, 1.0.20, #193) ne comptent pas. `0` : sans plafond |
| `maxIterations` | Étapes exécutées |

`/runs` (écran d'un run), la carte de progression et `penelope wf runs` montrent chaque
plafond face à sa consommation :

```
Budget : durée 12/120 min · tokens 31 k/2 M · cache 1,2 M · coût 0.40/5.00 $ · itérations 7/40
```

Avant 0.17.20, `maxTokens` comptait l'entrée entière : un run d'agent qui renvoie un
préfixe de 40 000 tokens à chaque appel se bloquait vers 2 millions de tokens à moins de
10 % de son plafond en dollars. Les quatre workflows livrés gardent `maxTokens: 2000000` :
avec la nouvelle mesure, ce même run en compte environ 220 000.

Un run bloqué par une borne le dit avec ses chiffres (« budget de tokens atteint (440000
tokens facturés sur 300000) ») et la commande qui la relève, pour ce run seul :

```bash
penelope wf control <run> budget --tokens 4000000 --usd 10
penelope wf control <run> budget --minutes 240 --iterations 80 --cached-tokens 50000000
```

Le relèvement laisse une trace (`workflow.budget_raised`) ; `resume` reprend ensuite à
l'étape courante, sans rejouer les effets faits. Un `resume` sur un run encore au-dessus
d'une borne, quelle qu'elle soit (durée et itérations comprises), répond « reprise
impossible, toujours bloqué : … » avec la commande qui la relève, sans changer son état :
il n'annonce plus `running` pour se rebloquer la seconde d'après (#337). Relever le
plafond de la session (`session budget`) ne relève pas celui d'un run.

### Plafond d'appels d'une étape

Un tour d'étape `agent` ou `sub_agent` a droit à `maxCalls` appels au modèle
(`workflows.step_max_calls`, 60 par défaut ; une conversation garde ses 24). Arrivée au
plafond sans `step_done()`, l'étape ne s'arrête pas d'emblée : les appels restés en
attente sont fermés (« non exécuté »), puis un **tour de reprise** lui demande de faire le
point avec `session_notes` (fait, reste, prochaine action) et de conclure, ou de
continuer avec un plafond neuf. Il lui est accordé au plus `maxTurns` fois
(`workflows.step_max_turns`, 2 par défaut) ; au-delà, l'étape échoue en le disant. Une
étape qui sait qu'il lui faut davantage rend `return_value(result="partial")` puis
`step_done()` : la transition décide, plutôt qu'un échec.

```json
{ "id": "dev", "type": "agent", "prompt": "…", "maxCalls": 120, "maxTurns": 3 }
```

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

## Les dix types d'étapes

| Type | Champs propres | Ce qu'il fait |
|---|---|---|
| `agent` | `prompt` (obligatoire), `nudgePrompt`, `model`, `tools`, `agentId`, `context`, `maxCalls`, `maxTurns` | Un tour d'agent dans la session du run, ou dans une session neuve (`context: fresh`) |
| `sub_agent` | `prompt` (obligatoire), `outputSchema`, `subAgentType`, `maxCalls`, `maxTurns` | Un sous-agent isolé qui rend une sortie structurée |
| `shell` | `command` (obligatoire), `cwd`, `successExitCodes`, `network` | Une commande, sous bac à sable ; réseau coupé sauf `network: true` (montré dans l'aperçu) ou `sandbox.shell_network` |
| `tool` | `tool` (obligatoire), `args` | Un outil natif ou MCP, arguments validés contre son schéma |
| `user` | `template` (obligatoire), `choices` (non vide), `input` | Une question au propriétaire sur Telegram |
| `parallel` | `children` (non vide), `maxConcurrency` | Plusieurs enfants en parallèle |
| `workflow` | `workflowId` (obligatoire), `params` | Un sous-workflow, profondeur bornée |
| `wait` | `on` | Attend un événement, un cron, un délai ou une tâche MCP |
| `verify` | `verifier` ou `checks`, `criteriaKey` | Vérifications mécaniques puis jugement du modèle ; `project_tests` relit la commande validée dans `session_metadata.verification`, sinon celle de `project` |
| `delivery` | `delivery` (`pull_request`, `ci`, `e2e`, `prod_report`, `prod_pull_request`) | Livraison en dev : PR vers la branche de développement, verdict de la CI, vérification externe de l'environnement de dev (voir « Livraison en dev ») ; bilan vérifié et PR dev → prod après approbation humaine (voir « Gate de production ») |

`context: fresh` donne à chaque visite de l'étape une session neuve, fille de celle du
run : l'agent ne voit ni la conversation du propriétaire ni les échanges des autres
étapes, seulement sa consigne, le brief et les sorties (`content` de `return_value`)
rendues avant lui, les six dernières. Une reprise après redémarrage retrouve la session
de la visite et continue la conversation, sans rejouer la consigne. Le préfixe de toute
étape `agent` entre au journal comme celui d'un tour de chat.

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

### Crédits épuisés

Sur des heures de travail, l'abonnement Codex peut atteindre sa limite, un crédit
OpenRouter s'épuiser (402) ou le budget journalier (`budget.daily_usd`) être atteint
(1.0.48, #339). Le run ne meurt pas : il se met en **pause au prochain point sûr**, la fin
de l'appel d'outil en cours. Les résultats déjà rendus sont au journal, l'étape n'a pas de
résultat enregistré, rien n'est perdu.

- **Pas de boucle** : une limite d'usage n'est jamais relancée sur le même modèle (les
  relances Codex de la 1.0.40 ne s'y appliquent pas). Un repli configuré
  (`[models.profiles.<nom>].fallback`) reste prioritaire, et il est annoncé, jamais
  silencieux ; sans repli (« rester sur Codex »), le run passe `paused`.
- **Un message, une fois**, dans le sujet du run :
  « ⏸ Je me suis arrêtée là, crédits Codex épuisés. Dernier point : story 12/53, étape
  dev, commit `abc1234` poussé. Reprise prévue à 17 h 40, au retour du quota. » L'heure
  vient du quota (`resets_at`, en-têtes `x-codex-*`) quand le fournisseur la donne ; pour
  le budget journalier, c'est minuit chez le propriétaire.
- **Reprise** automatique à l'heure dite si `workflows.resume_on_quota = true` (défaut) ;
  sinon, ou sans heure connue (402 : il faut recharger), par ▶️ Reprendre sur le run dans
  `/runs`, `/resume <run>` ou `penelope wf control <run> resume`. Le temps de pause sort
  de la durée maximale du run.

Un tour de conversation arrêté de même garde sa réponse partielle ; le message
« ⏸ Je me suis arrêtée là, … » porte un bouton ▶️ Reprendre qui repart du même
transcript. Une fenêtre de quota dont l'heure de remise à zéro est passée ne retient
plus les appels : avant la 1.0.48, le retrait de Codex durait jusqu'au redémarrage.

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
dans le sujet d'origine montre le plan et son bouton « Vas-y (vN) ». Ce clic approuve la
révision montrée et lance son run (section suivante). Le lancement direct avec
`workflow_start` est refusé dans une conversation de canal, avant toute carte
d'approbation, et le refus dit l'état réel : plan en revue, approuvé en attente du clic,
ou déjà lancé, avec le run et son état (#302). Le clic écrit l'événement
`workflow.plan.launched` dans la session d'origine et une note pour son prochain tour
(bloc `<evenements>` du contexte : « run lancé, `workflow_status` pour suivre ») ;
« vas-y » tapé en texte après le clic reçoit l'état du run sans tour de modèle ; deux
refus de `workflow_start` dans un même tour l'arrêtent avec cet état (`turn.halted`).
`/run <id>` passe toujours
par cette conversation, même quand des paramètres sont fournis. La CLI et les runs
techniques continuent d'utiliser le moteur existant. Une nouvelle demande dans la même
session conserve le plan approuvé précédent, et son run. `/stop` dans la conversation met
en pause les runs des plans qu'elle a lancés, en plus du tour et des jobs en cours (1.0.16 ;
voir [telegram.md](telegram.md#commandes)) ; `/resume <run>` ou `wf control <run> resume`
les reprend.

## Plan approuvé exécuté en phases

Un plan approuvé (#191, T3 de #185) est compilé en workflow du moteur de runs, puis
exécuté comme n'importe quel run : durable, repris au redémarrage, piloté par
`wf control`, arrêté par `/stop`.

- **Rien avant « vas-y ».** Un plan en revue ne compile pas. « Vas-y » porte la version
  et l'**empreinte** de la révision montrée (but, pas, version, paramètres, brief) ; un
  bouton d'une autre révision est refusé comme clic périmé.
- **Un seul run par plan.** L'identifiant du run (`r_plan_…`) dérive de la session et de
  l'empreinte : un double clic, un clic rejoué après un redémarrage ou `wf.plan.go`
  rendent le même run. La définition compilée (`plan-<empreinte>`), le lien vers le plan,
  l'origine et le brief sont écrits avant la ligne du run.
- **Une phase, un agent à contexte neuf.** Chaque pas devient une étape `agent` en
  `context: fresh`. L'orchestrateur choisit le modèle par phase, résolu sur la
  configuration au lancement : `reasoning` pour spécifier et relire, le rôle `code` pour
  les tests et le code, `main` pour vérifier. Un plan de deux pas au plus, sans revue ni
  vérification, reste à **un seul agent**.
- **Cartes d'OK.** À la fin de la spécification, des tests et du code, une étape `user`
  montre la sortie de la phase : « Continuer », « Laisse filer » ou « Arrêter ». « Laisse
  filer » saute les cartes d'OK ordinaires qui suivent, jamais un point dur : le gate
  « vas-y » est avant le run, la limite de reprises arrête le run, la livraison en dev
  n'a pas de variante libre, le gate de production (#193) non plus. « Arrêter » bloque le
  run avec sa raison ; rien ne le relance seul.
- **Revue contradictoire bornée.** Une revue ou une vérification rend `passed` ou `failed`
  par `return_value`. `failed` renvoie au début du dernier bloc de code, au plus deux
  fois ; la troisième fois, le run s'arrête (« limite de 2 reprises atteinte »). Le budget
  et les itérations du workflow d'origine bornent le reste.

Tout le chemin est déplié à la compilation : l'identifiant d'étape porte le numéro de
reprise et « laisse filer » (`e3-code-r1-libre`, `ok-e1`), si bien que la ligne du run
suffit à savoir où il en est. `wf.plan.show` rend le plan, son empreinte et ses runs ;
`wf.plan.go` est le « vas-y » de la socket.

```
  e1-spec ─► ok-e1 ─Continuer─► e2-tests ─► ok-e2 ─► e3-code ─► ok-e3 ─► e4-revue ─passed─► livraison
               │                                                          │
               └─Laisse filer─► e2-tests-libre ─► e3-code-libre ─► …      └─failed─► e3-code-r1 ─► …
```

Rejouer : `cargo test -p penelope-evals --test scenarios plan_en_phases rpc_plans`.

## Livraison en dev

Un plan qui écrit du code et finit par une revue ou une vérification est **livré** (#192,
T4 de #185) : le `passed` de ce dernier juge ne termine plus le run, il ouvre la PR vers
la branche de développement, attend la CI du projet, puis vérifie l'environnement de dev
depuis l'extérieur. Un plan sans code, ou sans juge final, n'ouvre aucune PR : personne
n'a dit que le travail satisfait.

```
  juge ─passed─► livraison-pr ─passed─► livraison-ci ─passed─► livraison-e2e ─passed─► gate de production
                    │                      │                      │
                    └─► carte « livraison bloquée » : Réessayer (la même étape) ou Arrêter
```

**Le dépôt.** La phase de code reçoit la consigne de travailler dans le dépôt, sur une
branche de travail, de commiter et de déclarer le dépôt : `session_metadata`, op `set`, clé
`project`, entrée `{"dir": "<dépôt>"}` (un chemin relatif se lit comme pour les outils
de fichiers). La livraison pousse cette branche, sous le même nom, sur le remote du dépôt ;
elle refuse une tête détachée, la branche de dev elle-même et des changements suivis non
commités.

**La configuration** est `.penelope/delivery.toml` dans le dépôt. Ce que le dépôt dit de
lui-même la complète ; le reste n'est **jamais supposé** :

```toml
[forge]
kind = "gitlab"            # github | gitlab ; déduit d'un remote github.com ou gitlab.com
api = "https://git.exemple.fr/api/v4"   # déduit de l'hôte du remote
repo = "equipe/service"    # déduit du chemin du remote
remote = "origin"
token_secret = "gitlab_token"           # défaut : github_token ou gitlab_token

[branches]
dev = "develop"            # obligatoire
prod = "main"              # obligatoire pour le gate de production (#193)

[ci]
provider = "gitlab"        # github | gitlab | none ; déduit de .github/workflows ou .gitlab-ci.yml
timeout_minutes = 60

[e2e]
url = "https://dev.exemple.fr"          # obligatoire

[[e2e.checks]]
path = "/health"           # http (défaut) : statut attendu `status`, sinon tout 2xx
contains = "ok"

[[e2e.checks]]
kind = "graphql"           # POST {"query": …} sur /graphql : ni `errors`, ni `data` nul
query = "{ version }"

[[e2e.checks]]
kind = "command"           # l'outil E2E du projet (Playwright, Maestro…), E2E_BASE_URL posé
command = "npx playwright test"

[prod]
max_age_minutes = 60       # âge maximal d'un bilan approuvable (défaut : 60)
```

Sans contrôle déclaré, un `GET` de l'URL de dev doit répondre 2xx. Le jeton se pose par
`penelope secret set gitlab_token`. Une information absente pose la carte avec **la
liste des clés manquantes**, pourquoi chacune compte et le fichier où l'écrire ; « Réessayer »
relit la configuration.

**Une seule PR par run.** Le push et l'ouverture passent par le ledger, planifiés avant
l'appel, sous une clé qui ne dépend ni du commit ni de la visite. Avant d'ouvrir, la
livraison cherche sur le forgeur une PR de la branche vers la branche de dev : après un
arrêt brutal pendant l'ouverture, elle est **retrouvée**, jamais ouverte deux fois.
« Réessayer » après une CI rouge ne rouvre rien.

**CI et E2E sont deux résultats.** La CI se lit sur le commit poussé (check runs et
statuts GitHub, pipelines GitLab), à intervalle croissant de 15 s à 5 min : en attente,
le run attend ; rouge, la carte le dit avec les contrôles en échec ; sans verdict après
`ci.timeout_minutes`, ou injoignable trois lectures d'affilée (« CI indisponible »), la
carte aussi. L'E2E joue tous ses contrôles, même après un rouge, et garde ses preuves
(requête, statut, extrait de réponse, durée) dans la sortie de l'étape (`wf trace`) et dans
`livraison/e2e-<n>.json` de l'espace du run. Chaque résultat vert est dit dans le sujet.

Rejouer : `cargo test -p penelope-evals --test scenarios livraison_dev plan_en_phases`,
et `cargo test -p penelope-orchestrator delivery` contre les faux forgeurs GitHub et GitLab.

## Gate de production

Après l'E2E de dev, un run livré ne propose rien seul (#193, T5 de #185) : même tout vert,
**aucune PR vers la production sans le clic du propriétaire**.

```
  livraison-e2e ─► livraison-bilan ─► gate-prod ─Proposer la PR prod─► livraison-prod ─passed─► $done
                        │                │ Re-vérifier ─► livraison-pr         │ bilan périmé ─► livraison-pr
                        │                │ Refuser ─► run bloqué               │ plan révisé ─► run bloqué
                        └─► carte bloquée (branche de prod absente…)          └─► carte bloquée (PR dev non fusionnée…)
```

**Le bilan.** `livraison-bilan` fige ce qui vient d'être vérifié : le plan exact (version
et empreinte) et le run, la PR dev, sa branche et son commit, la CI et l'E2E de **ce**
commit (avec le fichier des preuves) et l'heure ; son empreinte reste dans la trace du
run. La carte d'approbation le montre, avec trois choix : « Proposer la PR prod », « Re-vérifier » (PR,
CI et E2E refaits, puis une carte neuve), « Refuser » (le run s'arrête, « PR prod refusée
par le propriétaire », et le reste après un redémarrage ; le reprendre le termine sans rien
proposer). `branches.prod` est demandée avant toute carte ; elle ne peut pas être la
branche de dev.

**Le clic n'est pas cru sur parole.** Juste avant la PR, `livraison-prod` relit :

- le plan actif de la conversation : une révision plus récente (un autre plan, ou la même
  version d'un autre contenu) arrête le run, sans PR ;
- l'âge du bilan (`prod.max_age_minutes`), le commit que porte la PR dev sur le forgeur et
  la CI de ce commit : un bilan trop vieux, une PR dev avancée d'un commit ou une CI qui
  n'est plus verte le rendent **périmé**. Aucune PR : le sujet le dit, le run refait PR,
  CI et E2E et pose une carte neuve. Le bouton de l'ancienne carte ne vaut plus rien (sa
  visite d'étape est passée : « Déjà traité ») ;
- la PR dev doit être fusionnée dans la branche de dev, sans quoi la PR dev → prod ne
  porterait pas le travail : la carte le dit, « Réessayer » relit ;
- une PR dev fusionnée **après** le bilan (ou fusionnée avec un autre commit que celui
  vérifié) rend le bilan périmé : l'environnement de dev a pu être redéployé depuis le
  commit de fusion. Le run refait la CI **de ce commit de fusion**, puis l'E2E de dev, et
  pose une carte neuve ; aucune PR prod entre les deux, aucune si l'E2E est rouge. Le bilan
  approuvable est donc celui du commit de fusion.

**Au plus une PR prod par run.** L'effet est au ledger sous une clé qui ne dépend que du
run, planifié avant l'appel ; la PR porte la marque du run dans sa description et elle
est cherchée sur le forgeur (sa marque, puis une PR dev → prod déjà ouverte) avant d'être
ouverte. Une approbation suivie d'un redémarrage, même pendant l'ouverture, donne une
seule PR ; un envoi interrompu est terminé sans rejuger un bilan déjà approuvé. Le lien
part dans le sujet du run.

**Le déploiement reste hors de l'automatisme** : Pénélope ne fusionne pas la PR de
production et ne déploie rien ; le déploiement suit la politique du projet.

L'attente du propriétaire ne compte pas dans la borne de durée du run (`maxWallMs`) :
elle mesure le travail, pas le temps d'une décision. Une carte d'approbation laissée une
nuit reste approuvable ; le temps qui suit la réponse compte de nouveau.

Rejouer : `cargo test -p penelope-evals --test scenarios livraison_prod livraison_dev`
et `cargo test -p penelope-orchestrator prod`.

## Limites actuelles

- Le contenu de `.penelope/deploy.toml` n'est pas encore interprété : c'est un marqueur,
  les commandes viennent des cibles `make`.
- Une tâche MCP n'est suivie qu'une fois connue de l'étape `wait` : un appel d'outil qui
  rend une tâche n'enregistre rien tout seul.
- Le gate de production propose la PR dev → prod ; il ne la fusionne pas et ne déploie
  rien. L'E2E vérifie l'environnement de dev depuis l'extérieur : Pénélope ne sait pas
  quel commit y est déployé, elle suppose que le déploiement de dev suit la branche de dev
  (le commit de fusion) une fois la CI de ce commit passée.
- La livraison ne lit que la CI du forgeur (GitHub, GitLab) ; une CI externe au forgeur
  se déclare `none`. Corriger une CI rouge passe par un nouveau commit et un nouveau plan :
  « Réessayer » relit la CI du même commit (utile après une relance sur le forgeur).

Voir [progress.md](progress.md).
