# Notes de livraison : clôture du gel avant bascule (épopée #208, lot L, critères 4 et 7)

Branche `v1-l-gel-fin`, dérivée de `v1` à `9037fda` (1.0.0-alpha.17). Périmètre tenu :
`crates/penelope-store/tests/**`, `crates/penelope-context/tests/seal_from_0_17.rs`,
`crates/penelope-archtest` (module `scenarios`, cliquet), `budget.toml` (`[scenarios]`),
`scripts/check-budget.sh`, `scripts/switch-check.sh`, ce fichier. Aucune ligne
`version =`, aucun fichier de `docs/`.

## Ce qui est livré

| Commit | Contenu |
|---|---|
| `f7b2c7a` | `penelope-0.17.62.db`, produite par le code du tag `v0.17.62`. `REQUIRED_FIXTURES` (migration) et `SEALED_DIGESTS` (scellement) exigent la 0.17.59 et la 0.17.62. README des fixtures : la recette pour une 0.17 plus récente. |
| `71bb428` | R10 : module `scenarios` d'archtest, test `every_visible_surface_has_a_scenario`, `[scenarios].missing` posé à 213, resserrement par `UPDATE_BUDGET`, cliquet qui accepte la pose d'une liste. |
| `8024556` | R11 dans `scripts/check-budget.sh`. |
| `72b2b73` | `switch-check.sh` : le critère 7 nomme les surfaces sans scénario. |
| suivant | Extraction tenue au découpage des catalogues (`commands/*.rs`, `spec.rs` et `spec/*.rs`, hors tests), et ces notes. |

## Fixture 0.17.62

- Le générateur n'existe que sur `v1` (`migration_from_0_17.rs` et
  `migration_from_0_17/{seed,checks}.rs`), pas sur `main`. Il a été porté dans un worktree
  jetable détaché sur `v0.17.62` (les trois fichiers copiés, `penelope-kernel` ajouté aux
  dev-dependencies du store), lancé avec `UPDATE_FIXTURE=1`, puis le worktree et son
  `target` ont été supprimés (`git worktree remove --force`). Rien n'a été commité côté 0.17.
- Résultat : 221 184 octets, pages de 1 Kio, 19 migrations (dont `0019_tool_jobs`, absente
  de la 0.17.59). Le générateur a relu la base par `check_fixture` sur le code 0.17.62.
- Sur v1 : `a_real_0_17_database_migrates_and_reads_back` et
  `a_real_0_17_database_is_sealed_once` passent sur les deux fixtures (boucle sur le
  répertoire). Scellement : une session, 12 messages, second passage sans effet,
  `HistoryStore::verify` sans divergence, `EventLog::verify` à 8 maillons. L'empreinte du
  préfixe scellé est la même pour les deux (`ce06951c…`) : le semis est identique, seul le
  schéma d'origine diffère.
- Nouveauté : les deux tests refusent qu'une fixture disparaisse (liste exigée), et le
  scellement exige une empreinte par fixture présente.

## R10

- Surfaces lues dans le code par motifs, comme le reste d'archtest (pas de dépendance aux
  crates) : `c("nom", …)` dans `commands.rs` et `commands/*.rs` (50), `spec("nom", …)`
  dans `penelope-tools/src/spec.rs` et `spec/*.rs` (61, liste identique à la référence
  `reference:outils` des docs), `pub const X: &str = "…"` dans `api::method` (108).
  Modules et fichiers de tests exclus. Un découpage de `commands::all` en sections
  (`l-lints`) ne fait donc rien disparaître. `the_real_catalogs_are_read` garde les motifs
  honnêtes (comptes minimaux, noms connus).
- Ce qu'un scénario exerce est **lu, pas déclaré** : une étape `kind = "command"` du
  `scenario.toml`, un champ `"name"` des lignes `tool_calls` du `model.jsonl` (le nom passé
  à `tool_call` compte aussi). La spec proposait des surfaces déclarées dans
  `scenario.toml` ; le `Spec` du chargeur est `deny_unknown_fields` et hors de mon
  périmètre, et une déclaration peut mentir.
- **Couverture : 6 surfaces sur 219** : commandes 4 / 50 (`/compact`, `/fork`,
  `/rewind`, `/purge`), outils 2 / 61 (`fs_read`, `fs_write`), méthodes RPC 0 / 108 (le
  harnais n'a pas d'étape RPC ; ses commandes appellent les fonctions du daemon, pas la
  méthode RPC équivalente, et ne la couvrent donc pas).
- `[scenarios].missing` : identifiants `/nom`, `outil:nom`, `rpc:nom`, triés. Le test
  refuse une surface sans scénario absente de la liste, et une entrée dormante (couverte
  ou disparue du code). `UPDATE_BUDGET=1` retire, n'ajoute jamais, ne crée pas la table.
- Cliquet (`ratchet.rs`) : `scenarios.missing` était déjà une liste qui raccourcit ; une
  liste absente de la base n'est plus comptée comme ajout (c'est la pose de la règle).
  La retirer ensuite est rattrapé par R10, qui échoue sans elle.

## R11

Faisable mécaniquement dans `check-budget.sh`, comme la spec le décrit : une plage qui
touche `penelope-telegram/src/commands.rs` (ou `commands/`), `penelope-tools/src/spec.rs`,
`penelope-tools/src/spec/*.rs` (hors `tests.rs`) ou `penelope-kernel/src/api.rs` sans
toucher `crates/penelope-evals/scenarios/` sort en 1. Échappatoires : trailer
`Sans-scénario: <raison>` (un déplacement sans changement visible, un découpage de
`commands::all`) ou `Dérogation-budget: #N`. Un trailer distinct pour que les
déplacements ne gonflent pas le décompte des dérogations au budget. Essayé sur cinq
plages : lot sans catalogue 0, `api.rs` seul 1, avec trailer 0, scénario touché 0,
`spec/tests.rs` seul 0.

Ce qui reste hors mécanique : le changement de gabarit de carte ou de prompt. Il est tenu
par le rejeu lui-même (`surface.jsonl` et `expected.jsonl` diffèrent, le test des
scénarios échoue jusqu'à `UPDATE_SCENARIOS=1`), pas par le script.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Clôture du gel : fixture 0.17.62, R10, R11 (#208, lot L)

- **La dernière 0.17 dans le filet de migration** : `penelope-0.17.62.db` (221 184 octets,
  19 migrations), produite par le code du tag `v0.17.62` et non par v1. Les tests de
  migration et de scellement rejouent la 0.17.59 et la 0.17.62 et exigent les deux ;
  `verify` sans divergence, chaîne vérifiée.
- **R10, chaque surface visible a un scénario** : archtest lit les commandes Telegram, les
  outils natifs et les méthodes RPC dans le code, et ce que les scénarios rejouables jouent
  vraiment. Une surface nouvelle sans scénario fait échouer
  `every_visible_surface_has_a_scenario` ; ce qui manque au départ est inscrit dans
  `budget.toml` `[scenarios].missing` (213 sur 219), liste qui ne fait que rétrécir.
- **R11, dans le même lot** : `scripts/check-budget.sh` refuse une plage qui touche un
  catalogue de surfaces sans toucher `crates/penelope-evals/scenarios/`, sauf trailer
  `Sans-scénario: <raison>`.
```

## Blocages et points pour l'intégrateur

- **Critère 7 de `switch-check`** : il ne dit plus « `[scenarios]` absent », il dit
  « 213 surfaces sans scénario ». C'est l'état réel ; le vider, c'est écrire des
  scénarios (T19, par lots), et les 108 méthodes RPC demandent d'abord une étape `rpc` au
  harnais (`penelope-evals/src/scenario.rs`, hors périmètre).
- `CLAUDE.md` (section « Gel de la dette ») devrait nommer R10, R11 et le trailer
  `Sans-scénario:` : hors périmètre.
- Collision possible avec `l-lints` : découper `commands::all` touchera `commands.rs`, donc
  R11 demandera `Sans-scénario:` dans leur lot si aucun scénario ne bouge.
