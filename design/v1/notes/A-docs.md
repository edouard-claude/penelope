# Notes de livraison : lot A-docs (issue #216)

Branche `v1-a-docs`, dérivée de `v1` à `80b4f36`, poussée sur `origin`. Périmètre tenu :
`CLAUDE.md`, `AGENTS.md`, `.github/pull_request_template.md`, `.github/workflows/README.md`,
`docs/**`, un seul commentaire de doc dans `crates/penelope-kernel/src/config.rs`, et ce
fichier. Aucun `.yml`, aucun `Cargo.toml`, aucune autre source Rust touchés.

## Ce qui est livré

| Commit | Contenu |
|---|---|
| `2b17749` | `CLAUDE.md` : sections « Gel de la dette » (R1 à R8 en une phrase chacune, plus la frontière canal/cœur de #214, `UPDATE_BUDGET=1 cargo test -p penelope-archtest`, `scripts/check-budget.sh`, le trailer `Dérogation-budget: #N` et quand il est légitime, « un nouveau module ou une fonctionnalité va dans `v1` », « aucun tag `v1*` avant la bascule, `V1_RELEASES` n'existe pas encore ») et « Travailler sur v1 » (branche créée après les lots A et B sur le dernier tag 0.17.x, versions `1.0.0-alpha.N` jamais taguées, un seul `progress.md`, fusion `main` → `v1` après chaque release et au moins une fois par jour, règle des 24 h, `budget.toml` fusionné au plus strict, cherry-pick `-x` en sens inverse, `livraison` réservé à `main`). `AGENTS.md` ne fait plus que renvoyer à `CLAUDE.md`. |
| `2e851ea` | `.github/pull_request_template.md` : section « Gel de la dette » à trois cases (budget inchangé ou abaissé ; aucun nouveau module dans le daemon ; scénario ajouté ou mis à jour si le visible change, sans effet avant le lot B). `.github/workflows/README.md` réécrit d'après les trois `.yml` : `ci.yml` (quatre jobs, tests sur Linux **et** macOS, `livraison`), `release.yml`, `signature.yml`, section « Le tag est posé par la CI, jamais à la main » (#147) à la place de « Poser un tag », règle de la branche `v1`. |
| `0524b42` | `docs/decisions/0015-gel-0.17-et-branche-v1.md` (contexte, décision, raisons, conséquences, alternatives écartées), indexée dans `docs/README.md` ; table des décisions de `docs/progress.md` complétée de 0009, 0010, 0011 et 0015, avec la phrase « 0012 à 0014 et 0016 réservés par la charte, pas encore écrits » ; bloc `## Version 1 (branche v1)` en tête de `progress.md`, vide, avant `## Résumé` ; lien « Version 1 » dans l'index ; la routine de livraison cite `UPDATE_BUDGET=1`. |
| `aee28cd` | Les deux corrections d'incohérence : commentaire de doc de `telegram.max_fragments` dans `config.rs` (rendu dans la référence des clés par `UPDATE_DOCS=1`), et le noyau d'outils à 16 (19 définitions) dans `docs/install-headless.md`. |

Vérifications : `cargo test -p penelope-evals --test docs` vert (12 tests) après chaque
livrable, sans la variable ; `UPDATE_DOCS=1` n'a réécrit que la ligne de
`telegram.max_fragments` ; `cargo fmt --all --check` propre ; aucun tiret cadratin dans les
textes ajoutés.

## Écarts par rapport au brief, à relire par l'intégrateur

- **Le modèle de PR existait déjà** (`fea6c99`, #147), contrairement à ce que dit l'issue
  #216 (« Il n'existe pas de modèle de pull request ») : je l'ai complété d'une section au
  lieu de le créer, ses cases existantes (test `docs`, `make bump`) restent.
- **`AGENTS.md` et `CLAUDE.md` étaient identiques** à `80b4f36` (`diff` vide) : pas de
  divergence à réconcilier, seulement le risque qu'elle revienne, d'où le renvoi.
- **Routine de livraison de `docs/progress.md`** : une demi-phrase ajoutée à l'étape 1
  (`UPDATE_BUDGET=1` dès qu'un fichier de la liste de référence est touché), parce que la
  tâche T6 de `gel-et-outillage.md` §6 cite « `docs/progress.md` § Routine » ; c'est en
  dehors de la liste explicite du brief pour ce fichier, à retirer si non voulu.
- **Frontière canal/cœur (#214)** ajoutée comme neuvième ligne de la section « Gel de la
  dette », après R1 à R8 : c'est une règle du lot A que les sessions doivent lire.
- **Noms non encore fixés par le code** (les issues #209 à #214 sont en cours dans
  d'autres worktrees) : `crates/penelope-archtest/budget.toml` et ses tables
  (`[files.oversized]`, `[crates]`, `[daemon].modules`, `[daemon.daemon_users]`, `[lints]`,
  `[ca].required`, `[channel.allowed]`), `scripts/check-budget.sh`, `scripts/sync-main.sh`
  (T14, sans issue encore), `UPDATE_BUDGET=1`, `V1_RELEASES`. Ils sont écrits comme dans
  les issues ; à aligner si le code atterrit sous un autre nom.
- **`CLAUDE.md` énonce des gardes que #212 n'a pas encore posées** (`release.yml` refuse
  `v1*` sans `V1_RELEASES`, `penelope upgrade` ignore un suffixe) : formulées comme règles
  avec la référence « (#212) », pas comme un état du code.

## Proposition de test « AGENTS.md et CLAUDE.md cohérents » (pour `docs.rs`)

`AGENTS.md` doit soit être identique à `CLAUDE.md`, soit ne faire que renvoyer vers lui.
À ajouter dans `crates/penelope-evals/tests/docs.rs`, section « index et liens » :

```rust
/// #216 : un seul texte de référence pour les sessions d'agent. `AGENTS.md` est
/// identique à `CLAUDE.md`, ou ne fait que renvoyer vers lui.
#[test]
fn agents_md_defers_to_claude_md() {
    let claude = read(&root().join("CLAUDE.md"));
    let agents = read(&root().join("AGENTS.md"));
    if agents == claude {
        return;
    }
    let short = agents.lines().count() <= 12;
    let refers = links(&agents).iter().any(|l| l == "CLAUDE.md");
    assert!(
        short && refers,
        "AGENTS.md doit être une copie de CLAUDE.md ou un renvoi de quelques lignes \
         vers [CLAUDE.md](CLAUDE.md) : il fait {} lignes{}",
        agents.lines().count(),
        if refers { "" } else { " et ne le cite pas" }
    );
}
```

`links()` et `read()` existent déjà dans `docs.rs`. Le test ne compile pas `AGENTS.md`
dans le binaire : `build.rs` n'embarque que `README.md` et `docs/`.

## `telegram.max_fragments` : branchée depuis la 0.17.27

- L'annotation « Sans effet dans cette version » vivait dans le commentaire de doc du
  champ, `crates/penelope-kernel/src/config.rs:166-167` ; `config_reference()` de
  `docs.rs` (fonctions `config_structs`, `config_rows`) le recopie dans le bloc généré
  `reference:config` de `docs/install-headless.md` (ligne 289 avant correction). Il n'y
  avait donc qu'un endroit à corriger, puis `UPDATE_DOCS=1`.
- La clé est lue : `crates/penelope-daemon/src/telegram.rs:6385`
  (`self.daemon.services.config.config().telegram.max_fragments`) dans
  `reply_or_document` (`:6376-6393`), qui appelle
  `penelope_telegram::render::should_send_as_document`
  (`crates/penelope-telegram/src/render.rs:525-527`, `fragments > max_fragments`) quand
  `max_fragments > 0`, et joint le texte en document sinon.
- Qui l'appelle : `send_text` (`telegram.rs:7194-7198`), la livraison des avis internes
  par le `Messenger` (digest du matin, rapport de veille), pas les réponses de
  conversation (`reply`, qui découpe sans document). D'où le nouveau libellé : « un avis
  interne (digest du matin, rapport de veille) part en document ».
- Origine : 0.17.27, issue #145 (`docs/progress.md:1824-1836` : « `should_send_as_document`,
  jusque-là appelé nulle part »). Le contrat fonctionnel le relève en
  `design/v1/contrat-fonctionnel.md:424`.
- Non modifié : la fixture `crates/penelope-kernel/tests/fixtures/config-0.17.0.toml:17`
  (`max_fragments = 3`, simple valeur).

## Compte du noyau d'outils : 16, pas 17

- Le code : `all()` rend 57 outils ; `ON_DEMAND` en liste 39
  (`crates/penelope-tools/src/spec.rs:996-1037`) ; deux sont `workflow_only`
  (`return_value`, `step_done`) ; `core_exposed()` (`spec.rs:1045-1050`) exclut les deux
  familles : 57 − 39 − 2 = **16**. Même compte dans le bloc généré `reference:outils` de
  `install-headless.md` (57 lignes, 39 « à la demande », 2 « dans un workflow »).
- Le 17 datait de #104 (`a873d2e`, « 20 définitions par appel au lieu de 52 » : 54 outils,
  35 à la demande, 2 de workflow, 17 au noyau). `workflow_start` est passé à la demande à
  `d9287b7` (#189, 0.17.56) ; `image_inspect`, `schedule_move` et `workflow_plan`, ajoutés
  depuis, sont à la demande aussi. Le noyau est donc à 16 depuis la 0.17.56.
- Ce que le modèle reçoit : `chat_tool_defs` (`crates/penelope-daemon/src/executor.rs:2259-2273`)
  = `core_exposed()` + les trois méta-outils de `ToolRegistry::meta_tools()` = **19
  définitions**. Les tests ne fixent qu'une borne (`core.len() + 3 <= 20`,
  `spec.rs:1230` ; `base.len() <= 20`, `executor.rs:3685`).
- Corrigé : `docs/install-headless.md:1187-1188` (« 16 outils », « 19 définitions » ; le
  « environ 2 600 tokens » de #104 est gardé, aucune mesure nouvelle).
- Restent, hors de mon périmètre : le commentaire de doc `crates/penelope-tools/src/spec.rs:995`
  (« 17 outils d'usage courant ») ; `docs/progress.md:1128` est une note de version 0.17.x,
  laissée telle quelle ; `docs/mcp.md:164` (« sous 20 définitions ») est une borne, toujours
  vraie.

## Table des décisions

`docs/progress.md` s'arrêtait à 0008 ; `docs/README.md` listait 0001 à 0011. Ajoutés :
0009, 0010, 0011 et 0015, plus la phrase sur 0012 à 0014 et 0016 réservés (charte
`design/v1/README.md` §9). **Collision à arbitrer** : l'arbre principal porte un fichier
non commité `docs/decisions/0012-jobs-outils-durables.md` (lot #204, `git status` de la
session principale) alors que la charte réserve 0012 au journal source unique de la
conversation. Si #204 atterrit sous 0012, la charte et ma phrase dans `progress.md` et
`docs/README.md` sont à décaler (0012 → 0017 pour le journal, par exemple).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Le gel écrit là où les sessions le lisent (#216)

- **`CLAUDE.md`** : sections « Gel de la dette » (règles R1 à R8 et frontière canal/cœur
  en une phrase chacune, `UPDATE_BUDGET=1`, `scripts/check-budget.sh`, le trailer
  `Dérogation-budget: #N` et ses trois cas légitimes, « un nouveau module ou une
  fonctionnalité va dans `v1` », « aucun tag `v1*` avant la bascule ») et « Travailler sur
  v1 » (versions `1.0.0-alpha.N` jamais taguées, un seul `progress.md`, fusion `main` →
  `v1` après chaque release, règle des 24 h). `AGENTS.md` ne fait plus que renvoyer à
  `CLAUDE.md`.
- **Modèle de pull request** : trois cases de gel (budget inchangé ou abaissé, aucun
  nouveau module dans le daemon, scénario ajouté ou mis à jour si le visible change ; la
  dernière sans effet avant le lot B).
- **`.github/workflows/README.md`** : trois workflows, tests sur Linux et macOS, tag posé
  par la CI (#147) ; la section « Poser un tag » à la main disparaît.
- **Décision [0015](decisions/0015-gel-0.17-et-branche-v1.md)** : gel de la 0.17 et
  branche `v1`. La table des décisions passe de 0008 à 0015 (0012 à 0014 et 0016
  réservés) ; un bloc « Version 1 (branche v1) » vide attend les sections
  `1.0.0-alpha.N` en tête du fichier.
- **Deux incohérences corrigées** : le noyau d'outils compte 16 entrées (19 définitions
  avec les méta-outils), pas 17, depuis que `workflow_start` est passé à la demande
  (0.17.56) ; `telegram.max_fragments` n'est plus « sans effet », elle borne les avis
  internes (digest, veille) depuis la 0.17.27.
```

## Blocages

Aucun. Reste pour l'intégrateur : le test `agents_md_defers_to_claude_md` ci-dessus
(`docs.rs`, hors de mon périmètre), le commentaire `spec.rs:995`, l'arbitrage du numéro
0012, et l'alignement des noms (`budget.toml`, scripts) quand #209 à #214 atterrissent.
