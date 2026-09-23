## Changement

<!-- Ce qui change et pourquoi, en quelques lignes. Issue liée : #… -->

## Vérifications

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
- [ ] **Documentation** : le test `docs` passe (index `docs/README.md`, liens et ancres, commandes Telegram, références des clés de configuration et des outils, section de version dans `progress.md`) ; références régénérées si besoin (`UPDATE_DOCS=1 cargo test -p penelope-evals --test docs`)
- [ ] Sections « Limites actuelles » et « pas encore branché » relues
- [ ] `docs/progress.md` : notes de version et compte de tests ; `UPDATE_CA_MATRIX=1` si un test `ca_*` a été ajouté
- [ ] **Version posée** : `make bump V=x.y.z` (seize lignes de `Cargo.toml` + `Cargo.lock`), égale à la section ajoutée dans `progress.md`. Le tag et la release sont posés par la CI ; une section sans bump fait échouer la CI.

## Gel de la dette (issue #216, épopée #208)

- [ ] **Budget inchangé ou abaissé** : `cargo test -p penelope-archtest` vert ; aucune valeur de `crates/penelope-archtest/budget.toml` ne monte et aucune entrée n'y est ajoutée (`scripts/check-budget.sh`) ; sinon un commit du lot porte `Dérogation-budget: #N` et la PR dit pourquoi
- [ ] **Aucun nouveau module dans le daemon** (`mod x;` hors de la liste blanche `[daemon].modules`) : une fonctionnalité va dans `v1`, `main` ne prend que des corrections
- [ ] **Scénario ajouté ou mis à jour si le visible change** (prompt, outils exposés, réponse, carte, commande) dans `crates/penelope-evals/scenarios/` ; cette case n'a d'effet qu'après le lot B (scénarios rejouables sans clé), elle reste cochable « sans objet » d'ici là
