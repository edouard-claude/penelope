# 0025 : les workflows pour les pipelines, les missions pour le code

Statut : acceptée (4 novembre 2026). Portée : `penelope-workflow` (workflows livrés, type
d'étape `mission`), `penelope-orchestrator` (étape `mission`), la règle du prompt système
(#423). Issue #425, après l'épopée #370 (missions) et les incidents des 12 et 13 octobre
(#348, #354, #362).

## Contexte

Deux workflows livrés écrivaient du code en étapes : `build-verify` (plan, build, verify,
boucle sur critères) et `tache` (spécification, tests, développement, tests locaux,
commit, vérification, revue, statut), que `backlog` appelait pour chaque story. Les
incidents des 12 et 13 octobre en viennent : `tache` qui tourne entre `dev`,
`tests-locaux` et `revue` (#348), huit refus de revue sans commit (#362), un run qui
s'annule lui-même (#354). Pendant ce temps, la mission (#364 à #369) tient une seule
boucle d'agent jusqu'à un critère de fin vérifiable, et passe `mission-bench` à 3/3 sans
intervention.

## Décision

1. **Un processus déterministe se code en workflow** : déployer, dérouler une liste,
   enchaîner des portes humaines, réagir à une planification ou à un webhook. Le
   workflow garde l'extérieur : l'ordre, les secrets, les arrêts, le budget.
2. **Une tâche ouverte se confie à une mission** : écrire, corriger ou refactorer du code
   dans un dépôt. La mission tient la boucle jusqu'à sa fin vérifiée.
3. **Le pont est l'étape `mission`.** Un workflow qui a du code à faire écrire lance une
   mission (objectif, dépôt, commande de fin, budget) et attend sa fin. `backlog` le fait
   par `story`, une mission par tâche, à la place de `tache`.
4. **Le DSL de workflow n'apprend plus rien de propre au code.** Pas de nouveau critère,
   de nouveau `verify` par juge, ni de boucle build↔verify. Ce qui existe reste pour les
   workflows d'instance, mais on n'en ajoute pas.
5. **`tache` n'est plus livré.** Un workflow d'instance de ce nom se charge toujours. Au
   démarrage, le journal signale ce fichier et chaque planification qui le lance ; rien
   n'est supprimé.
6. **`build-verify` reste livré, pour l'instant.** C'est l'ancre des plans en phases
   (`workflow_plan`, #191) : un plan nomme un workflow enregistré, et la livraison en dev
   puis en prod (#192, #193) se greffe sur le plan compilé. Le retirer casserait ces plans,
   ce que #425 ne demandait pas. Il quittera les workflows livrés quand un plan de code
   partira en mission ; d'ici là, il reste tel quel, sans mécanisme nouveau.

## Raisons

- **Une boucle au lieu de dix étapes.** Une étape `agent` qui juge une autre étape
  `agent` multiplie les allers-retours sans mémoire commune. La mission garde son
  contexte, se compacte, se relance et s'arrête sur une preuve.
- **Générique.** La mission n'est pas propre à un projet : son objectif, son dépôt et sa
  commande de fin sont des paramètres.

## Conséquences

- `backlog` coûte ce que coûtent ses missions (20 $ au plus par story par défaut). Une
  mission qui ne finit pas (bloquée, budget, arrêtée) arrête la liste comme une tâche en
  échec.
- Annuler un run arrête la mission en cours de ce run et celles de ses sous-runs.
- La mission suit son run (#456). Sa dépense est celle du run : l'usage de sa session
  entre dans le budget du run, en plus du budget propre de la mission, et la borne la
  plus basse arrête. Un run en pause ou bloqué par une borne met sa mission **en
  attente**, pas à l'arrêt : elle finit son tour, ne dépense plus, et repart à la
  reprise dans la même session. Un état final atteint pendant ce dernier tour gagne sur
  l'attente. Passer l'étape arrête la mission.
- Un plan conversationnel (`workflow_plan`) garde son ancre `build-verify` ; il ne change
  pas avec ce lot.
