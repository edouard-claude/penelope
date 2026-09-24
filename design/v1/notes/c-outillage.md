# Lot C, outillage de la branche v1 : notes de livraison

Branche `v1-c-outillage`, dérivée de `v1` au commit `0f1c41a` (fusion de main 0.17.61).
Épopée #208. Spécification : `design/v1/gel-et-outillage.md` §4 et §6 (T2 côté CI, T11
pour le seuil, T14, T16), `design/v1/notes/A-archtest.md` §2, `design/v1/notes/A-versions.md`.

## 1. Ce qui est livré

| Fichier | Contenu |
|---|---|
| `crates/penelope-evals/tests/ca_matrix.rs` | le plancher « au moins 50 » devient la longueur de `[ca].required` (74 aujourd'hui) ; une liste vide ou absente est une erreur |
| `.github/workflows/ci.yml` | job `tests` : `fetch-depth: 0` et étape « Le budget ne remonte pas » après `Tests` |
| `scripts/bump.sh` | sur une version à suffixe, le message final dit « branche v1 : aucun tag, aucune release » |
| `scripts/sync-main.sh` | fusion de `origin/main` dans `v1` (T14), modes `--dry-run`, `--continue`, `--derogation N` |
| `scripts/switch-check.sh` | critères mesurables de la bascule (T16), sortie 1 tant qu'il en manque |

## 2. Les choix

**`ca_matrix.rs`** : `toml` est déjà une dépendance normale de `penelope-evals` (scénarios
R11), donc visible des tests d'intégration ; la dev-dependency suggérée par A-archtest §2
aurait été un doublon, `Cargo.toml` du crate n'a pas bougé.

**Étape CI** : la base passe par une variable d'environnement
(`BEFORE: github.event_name == 'push' && github.event.before || ''`) plutôt qu'une
expression dans `run:`, puis `scripts/check-budget.sh ${BEFORE:+"$BEFORE"}`. En push, la
base est le commit d'avant ; à zéro (premier push d'une branche), le script sort en 0
avec « pas de base ». En pull request ou `workflow_dispatch`, pas d'argument : le script
prend le point de fourche avec `origin/$GITHUB_BASE_REF` (ou `origin/main`), que
`fetch-depth: 0` rend disponible. Placée après `Tests` pour que `cargo run` ne rebâtisse
que le binaire. Essai dans une copie jetable (clone, jamais le dépôt) : hausse de
`dream.rs` sans trailer 1, avec `Dérogation-budget: #999` 0, premier push 0, pull
request contre une base simulée 1, rien ne bouge 0.

**`sync-main.sh`** : la spécification disait « prendre la version de main puis `sed` ».
C'est plus fin : le script rejoue la fusion à trois voies (`git merge-file`) de
`Cargo.toml` et `Cargo.lock` après avoir ramené les trois côtés (base, v1, main) à la
version de v1 ; ainsi un changement propre à v1 dans `Cargo.toml` (crate ou dépendance
ajoutés) n'est pas écrasé, et ceux de main sont gardés. Dans `Cargo.lock`, seule la ligne
`version` qui suit un `name = "penelope-…"` est réécrite. Puis `cargo update -w
--offline`, vérification que `Cargo.toml` ne porte plus la version de main, et `cargo
test -p penelope-archtest` : rouge, rien n'est commité, la fusion reste en cours et le
message dit les deux sorties (réduire côté v1, ou inscrire l'apport de main dans
`budget.toml` puis `--continue --derogation N`, qui pose le trailer). Les autres
conflits sont listés, avec la règle « la plus stricte clé par clé » si `budget.toml` en
fait partie ; la reprise se fait par `--continue`. `--dry-run` passe par `git merge-tree
--write-tree` (git 2.38 ou plus) et ne touche ni l'arbre ni l'index ; il fait seulement
`git fetch origin main`. Commit : « Fusion de main 0.17.X dans v1 ». Le script refuse un
workspace qui n'est pas en `1.0.0-…` (sortie 2).

Essai dans une copie jetable avec un dépôt nu comme `origin` : bump 0.17.62 plus une
ligne de `Cargo.toml` sur main, résolu et commité, la ligne de main gardée, seize lignes
en `1.0.0-alpha.2`, aucune trace de 0.17.62 ; second passage « rien à fusionner » ;
bump 0.17.63 avec conflit sur `README.md` et `purge.rs` gonflé de 20 lignes : arrêt
sur `README.md` (sortie 1), `--continue` arrêté par archtest (`purge.rs` 1 265 lignes
pour une borne de 1 245, sortie 1, fusion toujours en cours), borne inscrite puis
`--continue --derogation 999` commité, et `check-budget.sh HEAD~1` l'accepte par
dérogation.

**`switch-check.sh`** : points 1 et 6 (dernier run `ci.yml` en push sur `v1`, terminé,
`success`, et sur `origin/v1` ; le test `docs` en fait partie), 2 (les trois tables), 3
(au moins 71 noms, chacun trouvé comme `fn <nom>(` dans `crates/`), 4 (test de
migration, et fixture `penelope-<dernière 0.17 de progress.md>.db`), 7 (`[scenarios]`
présent et `missing` vide). Les points 4 (essai réel), 5 (suites réseau) et la
couverture du point 7 sont rappelés, pas vérifiés. La lecture de `budget.toml` se fait
en awk, sans dépendance. La « décision 0011 indexée » du point 6 est périmée : le numéro
0011 a été pris par le prompt système journalisé, la décision du gel est la 0015, déjà
indexée ; le script ne la vérifie pas. Lancé aujourd'hui : six manques (run CI de v1 en
cours, 43 fichiers en dépassement, 38 fichiers hors `impl_daemon`, 27 allows, fixture
`0.17.61` absente, `[scenarios]` absent), `[ca].required` complet, sortie 1.

## 3. Section de notes de version, prête pour `docs/progress.md`

```markdown
#### La branche v1 outillée : cliquet du budget en CI, fusion de main scriptée, critères de bascule vérifiés (#208, #209, #213)

Le cliquet de `budget.toml` n'était tenu qu'en local, la fusion de `main` dans `v1`
conflictait à chaque bump sur les seize lignes de version, et rien ne disait quand `v1`
pourrait devenir `main`. Le job `tests` de la CI lit tout l'historique et lance
`scripts/check-budget.sh` contre le commit d'avant (push) ou le point de fourche (pull
request) : une borne qui remonte sans `Dérogation-budget: #N` rend la CI rouge.
`scripts/sync-main.sh` fusionne `origin/main` dans `v1`, résout `Cargo.toml` et
`Cargo.lock` en gardant la `1.0.0-alpha.N` de v1 et les autres changements de main, et
ne commite que si `penelope-archtest` est vert (sinon il dit comment inscrire l'apport
de main avec le trailer) ; `--dry-run` montre les conflits sans rien toucher.
`scripts/switch-check.sh` vérifie les critères mesurables de la bascule (CI de v1,
budget sans dette, critères d'acceptation, fixture de migration, scénarios) et liste
ce qui manque. Le plancher de `tests/ca_matrix.rs` est désormais la liste figée
`[ca].required`, et `make bump` sur une `1.0.0-alpha.N` ne promet plus de release.
```

## 4. Restes et blocages

- Non fait : la résolution automatique de `budget.toml` en conflit (« la plus stricte clé
  par clé », §4.4). Le script s'arrête et rappelle la règle ; l'écrire en shell serait
  fragile, et le bon endroit est un sous-commande du binaire d'archtest (hors de mon
  périmètre).
- `sync-main.sh` lance `cargo test -p penelope-archtest`, pas `cargo test --workspace`
  que citait la spécification : le brief demandait archtest, la suite complète est
  rappelée avant le push.
- `CLAUDE.md` § « Travailler sur v1 » cite déjà `scripts/sync-main.sh` ; il pourrait
  mentionner `--dry-run`, `--continue`, `--derogation` et `switch-check.sh` (hors de mon
  périmètre).
- L'étape CI n'a pas encore tourné sur GitHub : elle jouera au premier push sur `v1`.
