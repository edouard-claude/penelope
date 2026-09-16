# Workflows GitHub Actions

Deux workflows, rien de plus.

## `ci.yml`

Sur chaque poussée vers `main` et chaque pull request. Il rejoue **exactement** la
définition de « terminé » du PRD (§20.2), sans commande maison :

| Étape | Commande | Machine |
|---|---|---|
| Format | `cargo fmt --all --check` | `macos-14` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | `macos-14` |
| Tests | `cargo test --workspace` | `macos-14` |
| Binaire | `cargo build --release --locked -p penelope-cli` | `macos-14` |
| Dépendances | `cargo deny check` | `ubuntu-latest` |

Les tests tournent sur macOS parce que c'est la plateforme cible : ils touchent Seatbelt,
`launchd` et le trousseau. `cargo deny` ne lit que `Cargo.lock`, il peut donc tourner sur
Linux, qui est plus rapide et moins cher.

Aucune suite ne demande le réseau : la CI n'a besoin d'aucun secret.

La version de la chaîne d'outils est épinglée dans `rust-toolchain.toml`, que `rustup`
lit aussi bien sur le poste de travail que sur le runner. Sans cet épinglage, une
nouvelle version stable de clippy casse la CI alors qu'aucune ligne n'a bougé, et le
lint local ne dit pas la même chose que le lint distant.

## `release.yml`

Sur un tag `vX.Y.Z`, ou à la main avec le tag en paramètre.

```
tag v1.0.1
   │
   ├── verification   fmt + clippy + tests            (on ne publie pas du rouge)
   │
   ├── construire     aarch64-apple-darwin  ─┐
   │                  x86_64-apple-darwin   ─┤
   │                                         │
   └── publier        lipo ──► binaire universel
                      3 archives .tar.gz + SHA256SUMS
                      gh release create
```

Trois archives sont publiées : `macos-universal` (celle à recommander),
`macos-aarch64` et `macos-x86_64`, plus un fichier `SHA256SUMS`.

Rien n'est signé ni notarisé : au premier lancement, Gatekeeper demandera confirmation.
Le faire proprement suppose un compte développeur Apple et deux secrets de dépôt, ce qui
n'est pas en place.

## Poser un tag

```bash
git tag -a v1.0.1 -m "Pénélope 1.0.1" && git push origin v1.0.1
```

La version du `Cargo.toml` du workspace doit correspondre au tag : rien ne le vérifie
automatiquement pour l'instant.
