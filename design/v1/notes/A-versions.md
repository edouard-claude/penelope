# Lot A, tâche T7 : gardes de version pour deux branches (#212)

Notes de livraison de la branche `v1-a-versions`, dérivée de `v1` au commit `80b4f36`.
Spécification : `design/v1/gel-et-outillage.md` §2.3, §2.4, §4.2, §4.3. Issue #212,
historique #147 (tag et release posés par la CI) et #36 (retour arrière après mise à jour).

## 1. Ce qui est livré

| Commit | Fichier | Contenu |
|---|---|---|
| `4e3dbdc` | `crates/penelope-daemon/src/upgrade.rs` | `parse_version` rend `None` sur un suffixe ; `is_newer` ne dit jamais « plus récente » d'une pré-release ni d'une version illisible ; trois tests |
| `eb7372b` | `crates/penelope-evals/tests/docs.rs` | parseur `Version` à l'ordre semver, verdict extrait dans `highest_section_matches`, message « section V1 sur main », deux tests unitaires |
| `ec5ed9b` | `.github/workflows/ci.yml`, `.github/workflows/release.yml` | déclencheur `[main, v1]`, étape « La branche porte sa version », garde « Pas de release 1.x avant la bascule » |
| (ce commit) | `design/v1/notes/A-versions.md` | ces notes |

Rien d'autre n'a bougé : ni `CLAUDE.md`, ni `docs/**`, ni le README des workflows, ni le
modèle de pull request. Les textes à y porter sont aux §5, §6 et §7.

## 2. Le choix fait pour `parse_version`, et pourquoi

Deux options étaient ouvertes : rendre `None` pour toute version à suffixe, ou rendre une
valeur ordonnée qui place le suffixe sous sa version pleine. C'est **`None`** qui est
livré, pour trois raisons.

1. **La signature ne change pas, donc les appelants non plus.** `release()`
   (`upgrade.rs`, sélection de la plus haute version publiée) fait `parse_version(tag)?`
   dans un `filter_map` : une pré-release sort d'elle-même de la course sans qu'une ligne
   de `release()` bouge. `install()` compare `parse_version(&r.version)` à la constante
   `FIRST_SELF_UPGRADING: (u64, u64, u64)` : intact aussi. Un type ordonné aurait obligé
   à réécrire ces deux sites, hors du périmètre, et un ordre semver honnête aurait dit
   `1.0.0-alpha.1 > 0.17.59`, précisément ce que l'issue interdit.
2. **Le repli « différent, donc plus récente » d'`is_newer` disparaît.** Il était mort en
   pratique (`crate::VERSION` se lit toujours), mais avec `None` sur les suffixes il serait
   devenu vivant et dangereux : `(None, Some)` aurait rendu `candidate != current`, soit
   vrai pour `v1.0.0-alpha.1`. Désormais une candidate illisible n'est jamais une mise à
   jour : dans le doute, on n'installe pas.
3. **Le binaire courant peut être une pré-release.** Sur la branche `v1`, `make deploy`
   installe un `1.0.0-alpha.N`. `is_newer` lit alors les nombres du courant par
   `version_parts` (aide privée, à côté des deux fonctions) et compare avec `>=` : `1.0.0`
   dépasse `1.0.0-rc.1`, `0.17.61` ne dépasse pas `1.0.0-alpha.7` (aucune rétrogradation
   d'une instance v1 par `--check` ou par `penelope upgrade` sans tag), une autre
   pré-release jamais sans son tag. Sur une instance v1, `--check` dit « À jour :
   1.0.0-alpha.7 (dernière publiée : 0.17.61) » : c'est exact, et c'est voulu.

Conséquences à connaître :

- **Un tag explicite reste installable** : `install()` ignore `older` quand `opts.tag`
  est donné (`upgrade.rs`, début d'`install`), et `release(Some(tag))` ne passe pas par
  `parse_version`. C'est le chemin de la bascule (`penelope upgrade --tag v1.0.0-rc.1`).
  Test : `the_latest_release_skips_prereleases_unless_asked_by_tag`.
- **Changement de comportement en production, voulu par l'issue** : une future
  `0.17.60-rc1` n'est plus vue comme plus récente qu'une `0.17.59` ; elle s'installe par
  `--tag`.
- Le suffixe de construction (`1.2.3+build`) est traité comme un suffixe : `bump.sh`
  n'en produit jamais, une règle unique suffit.
- `announces` ne change pas : elle compare la chaîne exacte, suffixe compris ; un test le
  fixe (`penelope 1.0.0-rc.1` annonce bien `1.0.0-rc.1`).
- L'assertion existante `parse_version("1.2.3-rc1") == Some((1, 2, 3))` est devenue
  `None` : c'est le seul cas existant modifié, et c'est l'objet de l'issue.

## 3. Le test `docs`

- `Version { numbers, stage }` avec `Stage::Pre(Vec<Ident>) < Stage::Full` et
  `Ident::Number < Ident::Text` : l'ordre dérivé est celui de semver, vérifié par
  `progress_versions_follow_semver` (`0.17.59 < 0.17.60 < 1.0.0-alpha.1`,
  `1.0.0-alpha.2 < 1.0.0-alpha.10 < 1.0.0-beta.1 < 1.0.0-rc.1 < 1.0.0`, `1.0.0-1 <
  1.0.0-alpha`, refus de `0.17`, `0.17.59.1`, `1.0.0-`, `1.0.0-alpha..1`).
- Le verdict est dans `highest_section_matches(progress, workspace) -> Result<(),
  String>` ; le test du dépôt l'appelle sur `docs/progress.md` et `penelope_daemon::VERSION`.
- **« section V1 sur main »** : si le majeur de la plus haute section dépasse celui du
  workspace (`1.0.0-alpha.N` contre `0.17.x`), le message commence par ces mots et nomme
  le bloc « Version 1 (branche v1) » à retirer du lot. Sinon, le message #147 inchangé
  (`make bump V=<titre exact>`, suffixe compris).
- Test `a_v1_block_passes_on_v1_and_fails_on_main` : sections `0.17.59` et
  `1.0.0-alpha.1` avec workspace `1.0.0-alpha.1` : vert ; avec `0.17.59` : rouge, bloc V1
  sur main ; `### 0.17.60` sans bump : rouge #147 ; `1.0.0-alpha.2` sans bump sur v1 :
  rouge #147 ; `1.0.0-rc.1` sous un workspace `1.0.0` : vert.
- `the_workspace_version_has_its_progress_section` n'a pas bougé : comparaison de
  chaînes, `### 1.0.0-alpha.1` passe telle quelle.
- Sur `main` aujourd'hui (workspace `0.17.59`, sections `0.17.x`) : vert, vérifié.

## 4. Les workflows

`ci.yml` :

- `on.push.branches: [main, v1]`. La concurrence reste par ref (`ci-${{ github.ref }}`) :
  un push sur `v1` n'annule pas la suite de `main`. `livraison` garde son
  `if: github.ref == 'refs/heads/main' && github.event_name == 'push'` et sa concurrence
  propre : `v1` ne pose jamais de tag.
- Étape « La branche porte sa version », première du job `tests`, après le checkout :
  `refs/heads/main` exige `version = "0.`, `refs/heads/v1` exige `version = "1.0.0-`,
  toute autre ref (pull request, branche de travail) n'est pas contrainte, conformément
  au §4.2. Une pull request vers `main` qui apporterait une 1.x est donc attrapée au push
  sur `main`, avant `livraison` (qui `needs` `tests`).
- Simulation locale, blocs `run:` extraits du YAML par ruby et rejoués sous bash :
  `main/0.17.59` passe, `main/1.0.0-alpha.1` échoue, `v1/1.0.0-alpha.1` passe,
  `v1/0.17.59` et `v1/1.0.0` échouent, `refs/pull/12/merge` sans contrainte.

`release.yml` :

- Garde « Pas de release 1.x avant la bascule », **première étape** du job
  `verification`, avant même le checkout ; `construire` et `publier` en dépendent, donc
  rien n'est construit ni publié. `VERSION` vaut `inputs.tag || github.ref_name` : la
  garde couvre le push de tag et le `workflow_dispatch`.
- `case "$VERSION" in v1*|1.*)` : échec avec `::error::release 1.x interdite avant la
  bascule (variable de dépôt V1_RELEASES absente)` sauf si `V1_RELEASES` vaut exactement
  `1` (`0`, vide, absente : refus). Le motif `v1*` couvre aussi `v10+` : sans objet
  avant la bascule, et la variable existera après.
- Simulation locale : `v1.0.0-alpha.0` sans variable échoue, avec `V1_RELEASES=1` passe,
  avec `V1_RELEASES=0` échoue ; `v1.0.0` et `1.0.0-rc.1` échouent ; `v0.17.60` et
  `v0.17.60-rc1` passent.
- `actionlint` (avec shellcheck) : rien à signaler sur les deux fichiers. Le module
  python `yaml` n'est pas installé sur le poste ; la syntaxe a été validée par actionlint
  et par `ruby -ryaml`.

## 5. Texte exact à insérer dans `CLAUDE.md`

Section « Un lot, une version, une release », après le paragraphe « Le tag et la release
sont posés par la CI » et avant « Deux conséquences » :

```markdown
**Aucun tag `v1*` avant la bascule.** La V1 vit sur la branche `v1`, versionnée
`1.0.0-alpha.N` (un `make bump V=1.0.0-alpha.N` par lot), jamais taguée : `release.yml`
refuse tout tag `v1*` tant que la variable de dépôt `V1_RELEASES` n'existe pas, et elle
n'existe pas encore ; c'est le propriétaire qui la pose à la bascule. `penelope upgrade`
n'installe jamais de lui-même une version à suffixe (`1.0.0-alpha.1`, `0.17.60-rc1`) :
seule `--tag` l'installe. Le job `tests` vérifie que `main` porte une version `0.` et `v1`
une version `1.0.0-` ; le test `docs` refuse un bloc `1.0.0-alpha.N` dans `progress.md`
quand le workspace est en 0.17 (« section V1 sur main ») : un rétroportage n'emporte
jamais la section de version (#212).
```

## 6. Texte pour `.github/workflows/README.md` (autre agent)

Le fichier est périmé (« Deux workflows », tests sur macOS seulement, section « Poser un
tag » à la main). Ce lot y ajoute trois faits à écrire :

- `ci.yml` : « Sur chaque poussée vers `main` ou `v1` et chaque pull request. » ; dans le
  job `tests`, première étape « La branche porte sa version » : `main` en `0.`, `v1` en
  `1.0.0-`, rien ailleurs (#212).
- `release.yml` : « Première étape : aucune release `v1*` tant que la variable de dépôt
  `V1_RELEASES` ne vaut pas `1` ; même un tag posé à la main ou un `workflow_dispatch`
  échoue sans rien construire (#212). »
- Section « Poser un tag » à remplacer par : « Le tag est posé par la CI (job
  `livraison`, #147), jamais à la main. Aucun tag `v1*` avant la bascule (#212). »

## 7. Section de notes de version, prête à coller dans `docs/progress.md`

Sous le prochain `### 0.17.x` (posé par `make bump` dans le lot qui fusionne cette branche) :

```markdown
#### Gardes de version pour deux branches : une pré-release ne s'installe plus d'office, le test `docs` lit `1.0.0-alpha.N`, la CI tourne sur `v1` (#212)

La V1 va vivre sur une branche `v1` versionnée `1.0.0-alpha.N`, jamais taguée, pendant
que la 0.17 continue de livrer sur `main`. Trois mécanismes ne le supportaient pas :
`parse_version` coupait le suffixe, donc un tag `v1.0.0-alpha.1` posé par erreur aurait
valu `1.0.0 > 0.17.59` et toutes les instances 0.17 l'auraient installé à leur prochaine
vérification ; le test `docs` n'acceptait que trois entiers et aurait paniqué sur un
workspace en `1.0.0-alpha.N` ; la CI ne tournait que sur `main`.

`penelope upgrade` ne juge plus jamais une version à suffixe « plus récente » : elle
sort de la liste des candidates et ne s'installe que par `--tag` (c'est ainsi que la
`1.0.0-rc.1` s'installera à la bascule) ; une instance dont le binaire est lui-même une
pré-release n'est pas rétrogradée vers une 0.17. Le test `docs` lit `x.y.z-<pré>` dans
l'ordre semver (`1.0.0-alpha.2 < 1.0.0-alpha.10 < 1.0.0-rc.1 < 1.0.0`) et refuse un bloc
`1.0.0-alpha.N` quand le workspace est en 0.17 (« section V1 sur main »). `ci.yml` tourne
sur `main` et `v1`, vérifie que chaque branche porte sa ligne de version (`0.` sur `main`,
`1.0.0-` sur `v1`) et réserve toujours `livraison` à `main` ; `release.yml` refuse tout
tag `v1*` tant que la variable de dépôt `V1_RELEASES` ne vaut pas `1`, `workflow_dispatch`
compris.
```

Si le propriétaire rejoue le `workflow_dispatch` (§8, point 1), ajouter au second
paragraphe : « Vérifié à la main le <date> : `release.yml` lancé avec
`tag=v1.0.0-alpha.0` a échoué à la première étape, aucune release ni aucun tag créés. »

## 8. Procédure pour le propriétaire (hors dépôt)

1. **Maintenant, avant la fusion** : le test attendu par l'issue que ma session n'a pas pu
   lancer (déclenchement d'un workflow refusé par le mode de permissions) :

   ```bash
   gh workflow run release.yml -f tag=v1.0.0-alpha.0 --ref v1-a-versions
   gh run list --workflow=release.yml --limit 1
   ```

   Attendu : le run échoue à l'étape « Pas de release 1.x avant la bascule », sans
   checkout, sans construction ; `gh release list` et `git ls-remote --tags origin`
   ne montrent rien de nouveau. Noter le résultat dans la section de version (§7).

2. **Rulesets GitHub** (Settings, Rules, Rulesets ; ou l'API ci-dessous). Une couche de
   plus, pas la garantie : les sessions passent par les droits du propriétaire, qui
   contourne les rulesets.

   - Branches `main` et `v1` : interdire la suppression et le push forcé. Pas d'obligation
     de pull request (le flux pousse directement), pas de statut requis.

     ```bash
     gh api -X POST repos/edouard-claude/penelope/rulesets --input - <<'JSON'
     {"name": "main et v1 : ni suppression ni push forcé", "target": "branch",
      "enforcement": "active",
      "conditions": {"ref_name": {"include": ["refs/heads/main", "refs/heads/v1"], "exclude": []}},
      "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}]}
     JSON
     ```

   - Tags `v*` : création, mise à jour et suppression réservées à l'application GitHub
     Actions (le job `livraison` pousse le tag avec `GITHUB_TOKEN`). Dans l'interface,
     ajouter « GitHub Actions » à la liste de contournement ; par l'API, `actor_type`
     `Integration` avec l'identifiant de l'application GitHub Actions, à relire dans
     l'interface avant de l'utiliser.

     ```bash
     gh api -X POST repos/edouard-claude/penelope/rulesets --input - <<'JSON'
     {"name": "tags v* : posés par la CI", "target": "tag", "enforcement": "active",
      "conditions": {"ref_name": {"include": ["refs/tags/v*"], "exclude": []}},
      "rules": [{"type": "creation"}, {"type": "update"}, {"type": "deletion"}],
      "bypass_actors": [{"actor_id": 15368, "actor_type": "Integration", "bypass_mode": "always"}]}
     JSON
     ```

   - Vérifier : `gh api repos/edouard-claude/penelope/rulesets` liste les deux ; puis un
     bump 0.17.x ordinaire sur `main` doit toujours produire son tag et sa release.

3. **À la bascule (T16)**, et seulement là :

   ```bash
   gh variable set V1_RELEASES --body 1 --repo edouard-claude/penelope
   ```

   Puis, dans `ci.yml`, retirer la ligne `refs/heads/main) … 0.*` de l'étape « La branche
   porte sa version » (la spécification la marque « retiré à la bascule ») ; la garde de
   `release.yml` peut rester, la variable la satisfait. La première release 1.x
   (`v1.0.0-rc.1`) est publiée en pré-release (`*-*`, règle existante) et s'installe par
   `penelope upgrade --tag v1.0.0-rc.1` ; la `v1.0.0` finale sera vue comme plus récente
   par toutes les instances 0.17 comme par les instances en `1.0.0-rc.N`.

## 9. Vérifications

- `cargo fmt --all --check` : propre.
- `cargo clippy -p penelope-daemon -p penelope-evals --all-targets -- -D warnings` :
  propre (un premier jet avait un `type_complexity`, corrigé en `(nombres, bool)`).
- `cargo test -p penelope-daemon upgrade` : 16 tests verts (dont les trois nouveaux).
- `cargo test -p penelope-evals --test docs` : 14 tests verts (dont les deux nouveaux).
- `actionlint` sur `ci.yml` et `release.yml` : rien à signaler.
- `cargo test --workspace` (une fois, à la fin du lot) : 1761 tests verts, 0 échec.
- `cargo clippy --workspace --all-targets -- -D warnings` (une fois, à la fin) : propre.
- `cargo clean` lancé dans le worktree `a-versions` seulement.

## 10. Blocages et limites

- Le `workflow_dispatch` réel avec `tag=v1.0.0-alpha.0` n'a pas été lancé : permission
  refusée dans ma session. Commande et résultat attendu au §8, point 1.
- La CI n'a pas tourné sur `v1-a-versions` : `ci.yml` ne se déclenche que sur `main`,
  `v1` et les pull requests. Ouvrir une pull request vers `v1` donne un run ; l'étape
  « La branche porte sa version » y dira « pas de contrainte », c'est attendu.
- Les cases restantes de l'issue sont hors de mon périmètre : texte de `CLAUDE.md` (§5),
  README des workflows (§6), section de version et bump (§7), rulesets et variable (§8).
- La garde de branche ne contraint pas les pull requests (choix du §4.2) : elle joue au
  push sur la branche, avant `livraison`.
