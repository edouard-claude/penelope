# Plan de bascule : `v1` devient `main`

Écrit le 27/09/2026, pendant l'essai de 24 h de la 1.0.0-alpha.18 sur l'instance réelle
(installée à 23h12 le 26/09, heure de la machine, sauvegarde et retour arrière dans
`~/penelope-rollback` sur le MBP). Référence : `design/v1/gel-et-outillage.md` §4.5 et
`design/v1/notes/A-versions.md` §8.

## Où en sont les critères (§4.5)

| # | Critère | État |
|---|---|---|
| 1 | CI de `v1` verte (Linux, macOS, dépendances) | vert sur 92afc70 |
| 2 | `budget.toml` : `[files.oversized]` vide, `daemon_users` réduit, `allow_too_many_lines` = 0 | vert (`switch-check.sh`) |
| 3 | `[ca].required` présents, matrice régénérée | vert (77 critères) |
| 4 | Fixture de la dernière 0.17 (0.17.62) et test de migration | vert ; essai réel en cours (alpha.18 sur le MBP) |
| 5 | Suites réseau ≥ ligne de base | vert : mêmes résultats que `main` 0.17.62 le 26/09 |
| 6 | Test `docs` vert | vert |
| 7 | `[scenarios].missing` vide ; couverture ≥ `main` | vert : 0 manquant ; lignes non couvertes 10 968 contre 14 045, par crate ≤ `main` (exceptions motivées dans `budget.toml`) |

Restent à la main : l'essai réel sur 24 h (en cours) et le passage par une vraie release
(`1.0.0-rc.1`) installée par `penelope upgrade --tag`, que l'alpha.18 installée à la main
n'a pas exercé.

## Séquence

Chaque étape dit qui la fait. « Propriétaire » = une commande à taper ou un bouton ; le
mode de permissions refuse à l'agent la fusion dans `main` et la pose de variables de dépôt.

1. **Agent** : vérifier que rien n'est arrivé sur `main` depuis 0.17.62
   (`git log v1..origin/main`) ; sinon `scripts/sync-main.sh` et une alpha de plus.
2. **Agent** : issues à fermer par la fusion, citées dans le message de fusion : #203,
   #206, #208, #211, #212 (après son test), #215, #219, #220, #221. Les autres restent
   ouvertes (Linux #196 à #202, workflows #185 et #191 à #193, #175, #180, #207, #222,
   #223, et l'issue de la mémoire à long terme).
3. **Agent** : PR `v1` → `main` (commit de fusion « Version 1 »), avec dans la même
   branche : `ci.yml` sans la ligne « `refs/heads/main) … 0.*` » de l'étape « La branche
   porte sa version » ; le bloc 0.17 de `docs/progress.md` archivé dans
   `docs/progress-0.17.md` ; CLAUDE.md et AGENTS.md sans la section « Travailler sur v1 ».
4. **Propriétaire** : `gh variable set V1_RELEASES --body 1 --repo edouard-claude/penelope`,
   puis `gh pr merge <n> --merge` (fusion, pas rebase : l'historique de `v1` reste lisible).
5. **Agent** : sur `main`, `make bump V=1.0.0-rc.1` et push ; la CI (`livraison`) pose le
   tag et publie la pré-release.
6. **Agent** : sur le MBP, `penelope upgrade --tag v1.0.0-rc.1` (le démarrage est confirmé
   par le mécanisme de #36, retour automatique sinon) ; vérifier que la signature reste
   « Penelope Dev » (`codesign -dr -`), `penelope doctor`, `history verify`, Telegram.
7. **Propriétaire** : 24 h de service normal sur la rc.1.
8. **Agent** : `make bump V=1.0.0` (release pleine) ; branche `0.17` créée sur le tag
   `v0.17.62` pour un correctif d'urgence (release par `workflow_dispatch`) ; suppression de
   la branche `v1` et des worktrees.

## Retour arrière

- Pendant l'essai de l'alpha.18 ou de la rc.1 : `~/penelope-rollback/retour-arriere.sh` sur
  le MBP (remet le binaire 0.17.62, la base et la configuration d'avant ; ce qui a été écrit
  pendant l'essai est perdu).
- Après la fusion : `git revert -m 1` du commit de fusion sur `main`, puis une 0.17.63
  publiée depuis la branche `0.17`.

## Points ouverts avant la fusion

- #212 : le test manuel du `workflow_dispatch` de `release.yml` (tag `v1.0.0-alpha.0`,
  doit échouer sans rien publier) n'a jamais été lancé ; le faire avant l'étape 4, puisque
  la variable `V1_RELEASES` lève ensuite la garde.
- Le cliquet de `[coverage.uncovered]` n'est tenu que par `scripts/coverage-check.sh`
  (lancé à la main) ; `check-budget.sh` ne le garde pas en CI.
- Sur `main`, la 0.17 a les défauts corrigés sur `v1` (#219 à #221, la course des jobs au
  redémarrage) : ils partent avec la fusion.
