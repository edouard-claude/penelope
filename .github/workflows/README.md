# Workflows GitHub Actions

Trois workflows : `ci.yml` (les suites, puis le tag et la release), `release.yml` (les
binaires), `signature.yml` (l'exigence désignée de `make sign`). Aucune suite ne demande le
réseau : la CI n'a besoin d'aucun secret pour tester.

La version de la chaîne d'outils est épinglée dans `rust-toolchain.toml`, que `rustup` lit
aussi bien sur le poste de travail que sur le runner. Sans cet épinglage, une nouvelle
version stable de clippy casse la CI alors qu'aucune ligne n'a bougé, et le lint local ne
dit pas la même chose que le lint distant.

## `ci.yml`

Sur chaque poussée vers `main`, chaque pull request, ou à la main. Il rejoue exactement la
définition de « terminé » du PRD (§20.2), sans commande maison. Un run en cours sur la même
référence est annulé par le suivant (`concurrency: ci-<ref>`).

| Job | Machine | Ce qu'il fait |
|---|---|---|
| `tests` | `ubuntu-latest` | `cargo test --workspace --no-fail-fast` : la suite entière, dix fois moins cher que macOS, et chacun peut la rejouer (issue #102 : sans bac à sable sur Linux, les tests de logique tournent sans profil imposé) |
| `verification` | `macos-14` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, la plateforme (`penelope-platform`, Seatbelt réellement appliqué, `--ignored` compris), **`cargo test --workspace` une seconde fois** (la même suite que la release, #151 : les tests réservés à macOS ne sont compilés que là), le relais launchd contre le vrai `launchd` (issue #36, `PENELOPE_LAUNCHD_TESTS=1`), puis `cargo build --release --locked -p penelope-cli` |
| `dependances` | `ubuntu-latest` | `cargo deny check` : avis de sécurité, licences, dépendances interdites, provenance ; ne lit que `Cargo.lock` |
| `livraison` | `ubuntu-latest` | après les trois autres, sur un push vers `main` seulement, un seul à la fois : pose le tag et lance la release (ci-dessous) |

Les tests tournent donc sur Linux **et** sur macOS. Seul le cache du registre cargo est
conservé entre les runs : `target/` dépassait 7 Gio compressé et sa restauration saturait
le disque du runner.

## Le tag est posé par la CI, jamais à la main

Issue #147 : le 20/09, trois lots fermés sont restés une heure sans release parce que
chacun attendait que quelqu'un tague, puis deux sessions ont livré en parallèle. Depuis,
le job `livraison` lit la version du workspace dans `Cargo.toml` ; si le tag `v<version>`
n'existe pas, il le pose (annoté, par `github-actions[bot]`) et appelle `release.yml` en
`workflow_dispatch`, parce qu'un tag posé avec `GITHUB_TOKEN` ne déclenche pas
`on: push: tags`. Un push qui ne change pas la version ne fait rien ; un tag déjà présent
n'est jamais déplacé.

Dans le lot, la seule commande est donc :

```bash
make bump V=0.17.60     # vérifie la section de docs/progress.md, réécrit les seize lignes, commite
git push                # la CI pose v0.17.60 et publie la release
```

`git tag` à la main est interdit (`CLAUDE.md`). Une section de version écrite dans
`docs/progress.md` sans bump fait échouer le test `docs`.

Branche `v1` (décision 0015) : ses versions `1.0.0-alpha.N` ne sont **jamais** taguées ;
`livraison` reste réservé à `main`, et la variable de dépôt `V1_RELEASES` qui autorisera un
jour une release 1.x n'existe pas encore (#212 pose les gardes ; d'ici là, aucun tag `v1*`).

## `release.yml`

Sur un tag `vX.Y.Z`, ou à la main avec le tag en paramètre (c'est ainsi que `livraison`
l'appelle).

```
tag v0.17.60
   │
   ├── verification   le tag = la version du workspace, fmt + clippy + tests   (macos-14)
   │                  banc d'essai de la mémoire (rapport joint, issue #37)
   │
   ├── construire     aarch64-apple-darwin  ─┐   clé publique minisign intégrée
   │                  x86_64-apple-darwin   ─┤   au binaire (`vars.MINISIGN_PUBLIC_KEY`)
   │                                         │
   └── publier        lipo ──► binaire universel
                      3 archives .tar.gz + banc-memoire.md + SHA256SUMS (+ .minisig)
                      notes = la section « ### X.Y.Z » de docs/progress.md
                      gh release create (ou edit), pré-release si v0.* ou suffixe
```

Le job `verification` refuse un tag qui ne correspond pas à la version du workspace :
`penelope --version` mentirait sur l'archive téléchargée. Trois archives sont publiées :
`macos-universal` (celle à recommander), `macos-aarch64` et `macos-x86_64`, plus
`SHA256SUMS` et le rapport du banc d'essai de la mémoire.

`SHA256SUMS` est signé avec minisign dès que le secret `MINISIGN_SECRET_KEY` existe ; la
variable de dépôt `MINISIGN_PUBLIC_KEY` est alors intégrée aux binaires, qui refusent
ensuite toute mise à jour non signée. Une clé publique sans secret fait échouer la release
plutôt que de publier des binaires qui ne pourraient plus se mettre à jour. Création des
clés : `docs/install-headless.md`, section « Mise à jour ».

Les binaires ne sont ni signés par Apple ni notarisés : au premier lancement, Gatekeeper
demandera confirmation. Le faire proprement suppose un compte développeur Apple et deux
secrets de dépôt, ce qui n'est pas en place.

## `signature.yml`

Issue #28 : `make sign` avec une identité stable doit donner la même exigence désignée
d'un build à l'autre. Sur `macos-14`, `scripts/check-codesign.sh` le vérifie avec une
identité de test dans un trousseau temporaire, sans secret. Déclenché seulement quand
`Makefile`, `scripts/check-codesign.sh` ou le workflow lui-même changent, ou à la main.
