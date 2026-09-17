## Changement

<!-- Ce qui change et pourquoi, en quelques lignes. Issue liée : #… -->

## Vérifications

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- [ ] **Documentation** : le test `docs` passe (index `docs/README.md`, liens et ancres, commandes Telegram, références des clés de configuration et des outils, section de version dans `progress.md`) ; références régénérées si besoin (`UPDATE_DOCS=1 cargo test -p penelope-evals --test docs`)
- [ ] Sections « Limites actuelles » et « pas encore branché » relues
- [ ] `docs/progress.md` : notes de version et compte de tests ; `UPDATE_CA_MATRIX=1` si un test `ca_*` a été ajouté
