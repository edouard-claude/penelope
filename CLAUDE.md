# Travailler sur Pénélope

Ce fichier est lu par les sessions d'agent qui travaillent sur ce dépôt. Il ne remplace
pas le PRD ni `docs/progress.md` : il dit ce qui se perd d'une session à l'autre.

## Un lot, une version, une release

Une issue fermée, c'est **trois** choses dans le même lot :

1. le code et ses tests ;
2. une section `### x.y.z` dans `docs/progress.md`, qui devient les notes de la release ;
3. la version posée dans `Cargo.toml` (seize lignes) et `Cargo.lock`.

```bash
make bump V=0.17.31        # vérifie la section, réécrit les seize lignes, commite
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

## Conventions

- Commentaires, messages de commit et documentation en français ; noms de code en anglais.
- `#![forbid(unsafe_code)]` partout ; pas de dépendance nouvelle sans raison écrite dans
  `Cargo.toml`.
- Un commit dit ce qui change **et pourquoi**, avec l'incident ou l'issue qui l'a motivé.
- Les écarts assumés au PRD deviennent une décision dans `docs/decisions/`.
