# Lot A, partie archtest : notes de livraison

Branche `v1-a-archtest`, dérivée de `v1` au commit 80b4f36. Issues #209, #210, #211 (le
compteur d'allows seulement), #213, #214 (la garde de frontière seulement). Spécification :
`design/v1/gel-et-outillage.md` §3 et `design/v1/decoupage-daemon.md` §1.3 et §7.

## 1. Ce qui est livré

### Le fichier de budget

`crates/penelope-archtest/budget.toml`, relevé sur l'état réel de l'arbre (les chiffres
des issues datent d'un autre commit) :

| Table | Contenu | Relevé |
|---|---|---|
| `[files]` | `ceiling = 1000`, `test_ceiling = 1500`, `test_modules` (les deux suites e2e du daemon) | |
| `[files.oversized]` | les fichiers de `src/` au-dessus de 1 000 lignes, avec leur taille exacte | 42 entrées, de `telegram.rs` (13 716) à `workflow/schedules.rs` (1 003) |
| `[crates]` | `penelope-daemon = 81168` (78 668 lignes de `src/` + 2 500) | |
| `[daemon].modules` | toutes les déclarations `mod x;` de fichier séparé du daemon | 62 noms (61 dans `lib.rs`, plus `screens`) |
| `[daemon].impl_daemon` | `runtime.rs`, `engine.rs`, `runner.rs`, `supervisor.rs` | 4 |
| `[daemon.daemon_users]` | occurrences du mot `Daemon` par fichier, hors commentaires, avant le module de tests, frontière de mot | 41 fichiers, de `scheduler.rs` (26) à `vault_inventory.rs` (1) |
| `[lints].allow_too_many_lines` | `#[allow(clippy::too_many_lines)]` dans les sources | 0 |
| `[ca].required` | les tests `ca_*` présents | 70 noms |
| `[channel.allowed]` | mentions du canal par fichier des crates agnostiques | 32 fichiers : 24 du daemon (`doctor.rs` 44, `scheduler.rs` 39), 5 du kernel (`session.rs` 37, `config.rs` 25), `store/migrations.rs` 30, `tools/spec.rs` 4, `observe/redact.rs` 2 |

Les entrées permanentes de `[channel.allowed]` portent leur justification en commentaire
de ligne : `store/migrations.rs`, `kernel/config.rs`, `observe/redact.rs`, `daemon/bus.rs`
(pour `Origin::Telegram`, en attendant T37), et `daemon/lib.rs` (`pub mod telegram;`,
qui part avec T29). Elles restent des entrées comptées : elles descendent si le fichier
maigrit, et ne remontent qu'avec le trailer.

Deux comptes diffèrent des issues, par définition et non par dérive : `[daemon.daemon_users]`
a 41 entrées et non 42 parce que les deux suites e2e (`ticket_to_deploy_e2e.rs`,
`wiki_e2e.rs`) sont des fichiers de tests et ne sont comptées nulle part « hors tests » ;
`[ca].required` a 70 noms parce que la liste est un ensemble de noms : le « 71 » de
`docs/ca-matrix.md` compte `ca_8_3_oauth_discovery_and_pkce` deux fois, dans
`evals/tests/mcp_conformance.rs` et dans une fixture de `evals/src/ca_matrix.rs`.

### Les règles (`crates/penelope-archtest/src/`)

| Fichier | Rôle |
|---|---|
| `snapshot.rs` | Lecture du workspace étendue à `src/`, `tests/` et `examples/` (`Snapshot`, `SourceFile`), et la coupe « code / tests » commune : `#[cfg(test)]` en colonne 0 suivi d'un `mod x {` en ligne ouvre les tests ; une déclaration `mod x;` ou un `#[cfg(test)] fn` isolé ne coupent pas. |
| `budget.rs` | `Budget::parse` / `Budget::load`, `Measures`, et `tighten` : la réécriture vers le bas par `toml_edit` (les commentaires restent). |
| `freeze.rs` | Une fonction pure par règle sur `(Snapshot, Budget)` : `size_violations` (R1, R2, R3), `crate_size_violations` (R4), `daemon_module_violations` (R5), `daemon_coupling_violations` (R6, occurrences et `impl Daemon`), `too_many_lines_allow_violations` (R7), `acceptance_test_violations` (R8), `channel_violations` et `undeclared_channel_crates` (R8 du découpage), `measure` pour `UPDATE_BUDGET`. |
| `freeze/tests.rs` | Les tests sur le workspace réel (`no_source_file_exceeds_the_ceiling`, `oversized_files_only_shrink`, `crates_stay_under_their_ceiling`, `daemon_modules_are_whitelisted`, `daemon_coupling_never_grows`, `too_many_lines_allows_never_grow`, `acceptance_tests_never_disappear`, `channel_agnostic_surfaces_do_not_name_a_channel`) et un test de détecteur par règle sur des fichiers fictifs, messages des issues vérifiés mot pour mot. |
| `ratchet.rs` | La comparaison base/HEAD de deux `budget.toml` (`regressions`), testée sur des paires fictives : hausse, entrée ajoutée, critère retiré, renommage accepté, plafond de crate retiré, plancher de couverture abaissé. |
| `bin/check-budget.rs` | Le binaire que le script appelle : deux fichiers TOML, un fichier de renommages, une étiquette de base ; sortie 1 si régression. |

Choix à connaître :

- **Fichiers de tests (R2)** : `crates/*/tests/**`, tout fichier sous un répertoire
  `tests/` de `src/` (les sous-modules d'un `mod tests` scindé, la sortie que R2 prescrit
  elle-même), `tests.rs`, `*_tests.rs`, `testing.rs` (aides de test), et
  `[files].test_modules` (`snapshot::is_test_path`). C'est la forme des fichiers que le
  lot #215 (a-tests-lints) sort des sept gros fichiers du daemon : `src/<module>/tests.rs`,
  `src/<module>/tests/mod.rs` et `tests/<thème>.rs`, `src/agent/clone_policy_tests.rs`,
  `src/mcp/testing.rs`. Ils ont le plafond de 1 500, sont exclus de R5, R6 et de la garde
  de canal (« hors tests »), et `forbidden_patterns` (`ca_2_3`) les ignore en entier,
  comme il ignorait ce qui suivait leur ancien `#[cfg(test)]` (`workflow/tests.rs` porte
  un `"/tmp/projet"`). Le message de R1 rappelle la règle.
- **Frontière canal** : la mesure est celle de `decoupage-daemon.md` §1.3, en un seul
  motif (`telegram` en toute casse, `tg_…`, `chat_id`, `topic_id`, `callback_data`), plus
  `find_by_topic` que le §7 range dans les identifiants ; les cinq familles de l'issue
  sont couvertes par ce motif, sans double compte d'une même occurrence. `telegram.rs`
  et tout `telegram/` sont exclus (les morceaux que le lot G en tirera restent de la
  passerelle). `CHANNEL_AGNOSTIC_CRATES` liste kernel, store, observe, platform, tools,
  hitl, llm, context, memory, mcp, skills, workflow, daemon, et déjà app, agent,
  executor, vault, dream ; `CHANNEL_CRATES` (telegram, gateway-telegram, cli, evals,
  archtest) a le droit de nommer le canal ; un crate nouveau doit se déclarer d'un côté
  (`every_crate_is_on_one_side_of_the_channel_boundary`).
- **Message R8** : il dit « budget.toml [ca].required le cite » plutôt que
  « docs/ca-matrix.md le cite » (la matrice se régénère et ne citerait plus le nom).
- **`UPDATE_BUDGET=1`** : `[files.oversized]`, `[daemon.daemon_users]` et
  `[channel.allowed]` descendent à `min(inscrit, courant)` et perdent les entrées passées
  sous leur plafond ou à zéro ; `[daemon].modules` et `impl_daemon` perdent ce qui n'est
  plus déclaré ; `[lints]` descend ; `[ca].required` gagne les `ca_*` nouveaux ; `[files]`
  et `[crates]` ne bougent pas. Aucune entrée n'est jamais ajoutée à une liste de
  référence, aucune valeur ne monte. Le resserrement se fait une fois par processus, avant
  que le premier test lise le fichier.
- **R7** : le motif compte `#[allow(…)]`, `#![allow(…)]` et `#[expect(…)]` contenant
  `clippy::too_many_lines`, commentaires exclus, dans tous les fichiers (tests compris).
  Le lot #211 pose les allows et fixe la valeur ; ici 0.
- Le compteur ne se compte pas lui-même : le message de R7 et deux fixtures de tests sont
  assemblés (`{lint}`, `concat!`) pour ne contenir ni l'attribut ni un `fn ca_…` en clair,
  que `ca_names` et `docs/ca-matrix.md` prendraient pour un critère.

### Le script de non-remontée

`scripts/check-budget.sh [BASE]` : compare `budget.toml` de HEAD à celui de BASE (un
commit ; sinon `git merge-base HEAD origin/$GITHUB_BASE_REF`, ou `origin/main`), reporte
les renommages (`git diff --name-status --find-renames BASE HEAD -- crates`), appelle le
binaire, et n'accepte une régression que si un commit de `BASE..HEAD` porte
`Dérogation-budget: #<issue>` (`git log -i --grep`). Sorties : 0 (rien ne remonte, ou
dérogation), 1 (refus), 2 (comparaison impossible). Une base d'avant le gel, sans
`budget.toml`, ou une branche nouvelle (`before` à zéro) donnent 0 avec un message.

Essai en local, joué avant livraison (deux commits fictifs sur une branche jetable) :

```sh
git checkout -b essai-budget
sed -i '' 's|dream.rs" = 5859$|dream.rs" = 5900|' crates/penelope-archtest/budget.toml
git commit -qam 'essai : remonter dream.rs'
scripts/check-budget.sh HEAD~1      # sortie 1 : « passe de 5859 à 5900 par rapport à … »
git commit -q --amend -m 'essai : remonter dream.rs' -m 'Dérogation-budget: #999'
scripts/check-budget.sh HEAD~1      # sortie 0 : « accepté par dérogation »
scripts/check-budget.sh             # sortie 0 : base d'avant le gel, rien à comparer
git checkout v1-a-archtest && git branch -D essai-budget
```

### Scénarios des issues rejoués

- `telegram.rs` + 1 ligne : `oversized_files_only_shrink` rouge (« 13 717 lignes, la
  liste de référence lui en accorde 13 716 ») ; `UPDATE_BUDGET=1` laisse l'entrée à
  13 716 et le test rouge.
- `telegram.rs` − 1 ligne : vert, entrée inchangée ; puis `UPDATE_BUDGET=1` : l'entrée
  descend à 13 715, rien d'autre ne change dans le fichier (commentaires compris).
- `UPDATE_BUDGET=1` sur l'arbre tel quel : `budget.toml` identique.
- Contre-mesure indépendante en perl des comptes `Daemon` (scheduler 26, compaction 21,
  runtime 6, telegram 7, lib 1, doctor 1) et canal (doctor 44, scheduler 39, session 37,
  migrations 30, bus 9, media 1) : identiques.

## 2. À insérer ailleurs (hors de mon périmètre, pour l'intégrateur)

### `.github/workflows/ci.yml`, job `tests`

Le checkout est sans historique ; `git merge-base` a besoin du point de fourche. Deux
lignes sur le checkout, une étape après `Tests` (les dépendances d'archtest sont alors
déjà compilées, `cargo run` ne rebâtit que le binaire) :

```yaml
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0   # check-budget.sh compare HEAD au point de fourche (#209)
```

```yaml
      # Le cliquet du gel (#209) : budget.toml ne remonte jamais sans le trailer
      # « Dérogation-budget: #N ». En PR, la base est le point de fourche avec la branche
      # cible (GITHUB_BASE_REF) ; en push, le commit d'avant.
      - name: Budget de la dette
        run: scripts/check-budget.sh ${{ github.event_name == 'push' && github.event.before || '' }}
```

Si `fetch-depth: 0` est refusé, l'alternative est `git fetch --no-tags --depth=1 origin
"$GITHUB_BASE_REF"` puis `scripts/check-budget.sh "origin/$GITHUB_BASE_REF"` : la base
est alors la pointe de la branche cible, pas le point de fourche ; une PR non rebasée
après un `UPDATE_BUDGET` sur la cible verrait une fausse hausse, que le rebase efface.

### `CLAUDE.md`, nouvelle section après « Avant de pousser »

```markdown
## Gel de la dette

`crates/penelope-archtest/budget.toml` fige ce qui ne doit plus grossir : 1 000 lignes
par fichier source (tests inline compris), 1 500 par fichier de tests, la liste des
fichiers déjà en dépassement (chacun avec sa borne), `penelope-daemon/src` à 81 168
lignes, ses 62 modules, le nombre de fois où chaque fichier nomme `Daemon` ou le canal
Telegram, les `#[allow(clippy::too_many_lines)]`, et les critères d'acceptation `ca_*`.
Les nombres ne montent jamais ; `cargo test -p penelope-archtest` le vérifie contre les
sources, `scripts/check-budget.sh` contre la base git.

- Un correctif fait déborder un fichier de la liste : déplacer ses tests dans
  `src/<module>/tests.rs` (`#[cfg(test)] mod tests;`, `use super::*;`) suffit.
- Un fichier listé a maigri, un `&Daemon` ou une mention de Telegram a disparu :
  `UPDATE_BUDGET=1 cargo test -p penelope-archtest` resserre `budget.toml`, vers le bas
  seulement.
- Une fonction dépasse 200 lignes : la découper, jamais ajouter un allow.
- Un test `ca_*` change de fichier : permis ; le renommer : interdit.
- Remonter un nombre ou ajouter une entrée est une décision : le commit porte le trailer
  `Dérogation-budget: #<issue>`, sinon la CI refuse (`git log --grep Dérogation-budget`
  les retrouve).
```

### `crates/penelope-evals/tests/ca_matrix.rs:11` (#213)

Le seuil `>= 50` devient la taille de `[ca].required`. `penelope-evals` n'a pas `toml` ;
l'ajouter en `[dev-dependencies]` (`toml.workspace = true`, déjà dans le workspace, avec
la raison en commentaire : lire le budget du gel) plutôt que de dépendre d'archtest, qui
n'est pas dans la liste des dépendances internes que `scripts/bump.sh` réécrit.

```rust
    // Le plancher est la liste figée par le gel (#213) : budget.toml [ca].required.
    let budget = std::fs::read_to_string(root.join("crates/penelope-archtest/budget.toml"))
        .expect("budget.toml lisible");
    let required = budget
        .parse::<toml::Value>()
        .expect("budget.toml valide")
        .get("ca")
        .and_then(|c| c.get("required"))
        .and_then(|r| r.as_array())
        .map_or(0, Vec::len);
    assert!(
        tests.len() >= required,
        "trop peu de tests d'acceptation trouvés : {} (attendus : {required})",
        tests.len()
    );
```

### `docs/progress.md`, section de la version

```markdown
#### Le gel de la dette est mécanique : budget.toml, huit règles, un cliquet (#209, #210, #211, #213, #214)

Rien n'arrêtait la croissance : 42 fichiers au-dessus de 1 000 lignes portent 56 % du
workspace, `penelope-daemon` en fait 48 % à lui seul, 41 de ses fichiers nomment le type
`Daemon` et le cœur nomme Telegram dans 32 fichiers hors passerelle. `penelope-archtest`
vérifiait les dépendances, les motifs propres à un OS et `unsafe`, jamais une taille, un
module ou un couplage, et ne lisait que `src/`.

`crates/penelope-archtest/budget.toml` fige tout cela sur la mesure : plafond de 1 000
lignes par fichier source (tests inline compris) et 1 500 par fichier de tests, liste de
référence des 42 fichiers en dépassement avec leur borne, `penelope-daemon/src` à 81 168
lignes, liste blanche de ses 62 modules, budget d'occurrences de `Daemon` par fichier et
`impl Daemon` dans quatre fichiers, allows de `clippy::too_many_lines` comptés, les 70
critères `ca_*` figés (renommer interdit, déplacer permis), et la frontière canal / cœur
avec sa liste par fichier. Chaque règle a son test sur le workspace et son test de
détecteur ; `archtest` parcourt désormais `src/`, `tests/` et `examples/`. Les nombres ne
montent jamais : `UPDATE_BUDGET=1 cargo test -p penelope-archtest` resserre le fichier
vers le bas seulement, et `scripts/check-budget.sh`, en CI, refuse toute remontée par
rapport à la base git, sauf un commit portant `Dérogation-budget: #<issue>`.
```

## 3. Blocages et restes

- Aucun blocage. Tout ce qui touche d'autres fichiers (`ci.yml`, `CLAUDE.md`,
  `tests/ca_matrix.rs`, `progress.md`) est ci-dessus, prêt à coller.
- `Cargo.lock` est modifié par l'ajout de `toml_edit` à archtest (dépendance déjà dans le
  workspace, raison écrite dans `crates/penelope-archtest/Cargo.toml`).
- La CI ne connaît pas encore le script : jusqu'à l'étape `ci.yml`, le cliquet git n'est
  tenu qu'en local.
- Non fait, hors périmètre : la dépendance morte `penelope-workflow → penelope-telegram`
  (#214, autre agent), les lints de workspace et les allows (#211, autre agent), la règle
  de dépendance de `penelope-workflow` dans `dependency_rules`.
- Non fait, facultatif dans la spécification : le mode `--staged` du script (pré-commit).
