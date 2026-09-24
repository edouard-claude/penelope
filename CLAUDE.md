# Travailler sur Pénélope

Ce fichier est lu par les sessions d'agent qui travaillent sur ce dépôt. Il ne remplace
pas le PRD ni `docs/progress.md` : il dit ce qui se perd d'une session à l'autre.

## Un lot, une version, une release

Une issue fermée, c'est **trois** choses dans le même lot :

1. le code et ses tests ;
2. une section `### x.y.z` dans `docs/progress.md`, qui devient les notes de la release ;
3. la version posée dans `Cargo.toml` (une ligne par crate, plus celle du workspace) et `Cargo.lock`.

```bash
make bump V=0.17.31        # vérifie la section, réécrit les lignes de version, commite
git push
```

**Le tag et la release sont posés par la CI**, jamais à la main : le job `livraison` de
`.github/workflows/ci.yml` tourne après les suites, sur `main`, un seul à la fois ; si la
version du workspace n'a pas encore son tag, il le pose et appelle `release.yml`. Un push
qui ne change pas la version ne fait rien.

Deux conséquences :

- une section de version **sans** bump fait échouer la CI (`the_highest_progress_section_is_the_workspace_version`) ;
- si `main` a avancé pendant ton lot, le rebase donne un conflit sur `Cargo.toml` : prends
  le numéro suivant. C'est la serrure — deux sessions ne peuvent pas livrer la même
  version.

Le 20/09, trois lots fermés sont restés une heure sans release parce que chacun attendait
que quelqu'un tague, et deux sessions ont fini par livrer en parallèle (issue #147).

## Avant de pousser

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`UPDATE_DOCS=1 cargo test -p penelope-evals --test docs` si une clé de configuration, un
outil ou une commande Telegram a changé ; `UPDATE_CA_MATRIX=1` si un test `ca_*` est
ajouté.

Le code macOS (`crates/penelope-platform/src/backend/macos.rs`) n'est pas compilé sous
Linux : `cargo check -p penelope-platform --target aarch64-apple-darwin` le relit.

Cette ligne ne couvre pas tout : les **tests** sous `#[cfg(target_os = "macos")]`, nombreux
dans `penelope-daemon`, ne sont compilés par aucune commande locale, et les crates qui
embarquent SQLite ne se compilent pas en croisé faute de SDK C. La CI macOS, elle, les
compile. Quand une signature change, relis-en les appels à la main :
`grep -rn 'cfg(target_os = "macos")' crates/`. La 0.17.40 a manqué sa release ainsi.

## Gel de la dette

La 0.17 est gelée (décision [0015](docs/decisions/0015-gel-0.17-et-branche-v1.md), épopée
#208) : `main` ne prend plus que des corrections (bug, régression, sécurité) ; **un nouveau
module ou une fonctionnalité va dans `v1`**. Les règles sont mécaniques, tenues par
`penelope-archtest` et par `crates/penelope-archtest/budget.toml`, dont les nombres ne
montent jamais (#209 à #214 les posent ; leurs messages d'erreur sont la documentation
détaillée, avec `design/v1/gel-et-outillage.md` §3).

- R1 : aucun fichier source ne dépasse 1 000 lignes, tests inline compris ; un test qui fait
  déborder va dans `<module>/tests.rs` (`#[cfg(test)] mod tests;`, `use super::*;`).
- R2 : un fichier de tests (`tests/*.rs`, `src/**/tests.rs`) tient sous 1 500 lignes.
- R3 : les fichiers déjà au-dessus sont dans `[files.oversized]` avec leur taille ; chaque
  entrée ne peut que descendre, et disparaît dès que le fichier repasse sous le plafond.
- R4 : `penelope-daemon/src` a un plafond de lignes (`[crates]`) : la 0.17 ne grossit plus.
- R5 : tout `mod x;` du daemon est dans la liste blanche `[daemon].modules` ; un module
  nouveau se fait dans `v1`, pas dans la 0.17.
- R6 : les occurrences de `Daemon` par fichier sont budgétées (`[daemon.daemon_users]`) et
  `impl Daemon` réservé à quatre fichiers : prendre `&Services` ou un trait, pas le daemon.
- R7 : `clippy::too_many_lines` à 200 lignes ; les `#[allow(clippy::too_many_lines)]`
  existants sont comptés (`[lints]`) : découper la fonction, jamais ajouter un allow.
- R8 : les 71 tests `ca_*` de `docs/ca-matrix.md` sont figés (`[ca].required`) : les
  déplacer est permis, les renommer ou les supprimer est interdit.
- Frontière canal/cœur (#214) : hors `telegram.rs` et `screens.rs`, le cœur ne nomme pas
  Telegram (`penelope_telegram::`, `cfg.telegram.`, `Origin::Telegram`, `tg_`, `chat_id`)
  au-delà du relevé `[channel.allowed]` ; il passe par `ChannelDelivery`, `Messenger`,
  `OwnerChannel`.

`UPDATE_BUDGET=1 cargo test -p penelope-archtest` réécrit `budget.toml` **vers le bas
seulement** : le lancer dès qu'un fichier listé est touché, pour ne pas laisser de marge
dormante. En CI, `scripts/check-budget.sh` compare le fichier à la base git et refuse toute
valeur qui monte et toute entrée ajoutée, sauf si un commit du lot porte le trailer
`Dérogation-budget: #N`. Le trailer est légitime pour un correctif d'urgence (sécurité,
régression) qui n'a pas d'autre sortie, une montée de chaîne qui reformate, ou un `ca_*`
dont le nom disait une chose fausse ; jamais pour une fonctionnalité. Attendu : 0 à 2 par
mois, relus par `git log --grep=Dérogation-budget`.

La sortie normale d'un correctif qui fait déborder un fichier de la liste : déplacer un
test dans `tests.rs`, pas le trailer.

**Aucun tag `v1*` avant la bascule.** La variable de dépôt `V1_RELEASES` n'existe pas
encore : `release.yml` refuse toute release 1.x sans elle, et `penelope upgrade` ignore une
version à suffixe (#212). Une pré-release 1.x publiée par erreur serait installée par toutes
les instances 0.17.

## Travailler sur v1

- La branche `v1` est créée depuis `main` **après** le gel et les filets (lots A et B), sur
  le dernier tag `0.17.x`. Elle porte les versions `1.0.0-alpha.N`, un bump par lot
  (`make bump V=1.0.0-alpha.7` : `scripts/bump.sh` accepte le suffixe), **jamais taguées**.
- Le même `docs/progress.md` : le bloc `## Version 1 (branche v1)` en tête reçoit les
  sections `### 1.0.0-alpha.N` ; les `### 0.17.x` restent dessous et arrivent par les
  fusions. Jamais de section `1.0.0-alpha.N` dans un lot poussé sur `main` : le test `docs`
  l'attrape.
- Synchronisation `main` → `v1` par **fusion** (`git merge --no-ff origin/main`, script
  `scripts/sync-main.sh` pour les lignes de version), après chaque release 0.17.x et
  au moins une fois par jour ; jamais de rebase. Une fusion en conflit depuis plus de
  **24 h bloque tout autre lot sur `v1`**.
- `budget.toml` en conflit de fusion : la valeur la plus stricte clé par clé (minimum des
  plafonds, intersection des listes de dette, union de `[ca].required`), jamais un seul
  côté.
- Sens inverse `v1` → `main` : cherry-pick `-x` au cas par cas, pour une correction
  seulement.
- La CI tourne sur `v1` (`tests`, `verification`, `dependances`) ; le job `livraison` reste
  réservé à `main` : aucun tag, aucune release depuis `v1`. Les rulesets GitHub (suppression
  et push forcé interdits sur `main` et `v1`, tags `v*` réservés à GitHub Actions) sont
  posés par le propriétaire : une couche de plus, pas la garantie.

## Conventions

- Commentaires, messages de commit et documentation en français ; noms de code en anglais.
- `#![forbid(unsafe_code)]` partout ; pas de dépendance nouvelle sans raison écrite dans
  `Cargo.toml`.
- Un commit dit ce qui change **et pourquoi**, avec l'incident ou l'issue qui l'a motivé.
- Les écarts assumés au PRD deviennent une décision dans `docs/decisions/`.
