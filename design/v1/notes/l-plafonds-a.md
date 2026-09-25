# Lot L : plafonds des crates socle (épopée #208, clôture)

Agent `l-plafonds-a`, branche `v1-l-plafonds-a`, base 1.0.0-alpha.14 (`9f13770`).
Cible : charte V1 §3.4, 800 lignes au plus par fichier source, tests en fichiers frères.
Périmètre : `penelope-kernel`, `penelope-llm`, `penelope-tools`, `penelope-hitl`,
`penelope-store`.

## Livré

Plus aucun fichier au-dessus de 800 lignes dans ces cinq crates, `src` et `tests`
compris. Chaque commit est un déplacement (corps inchangés, seuls `mod`, `use` et
visibilités `pub(super)`), vert sur `cargo test -p <crate>`, suivi de
`UPDATE_BUDGET=1 cargo test -p penelope-archtest`. Aucun chemin public ne change : le
module parent réexporte (`pub use x::*`).

| Fichier | Avant | Après | Plus gros morceau | Commit |
|---|---|---|---|---|
| kernel `config.rs` | 2 148 | 61 | `config/sections.rs` 699 | `04210d4` |
| kernel `turn.rs` | 1 136 | 678 | `turn.rs` 678 | `1da7c08` |
| llm `provider.rs` | 2 048 | 278 | `provider/tests.rs` 486 | `3918010`, `acc11bf`, `de2e314` |
| llm `codex.rs` | 1 533 | 513 | `codex/tests.rs` 526 | `4650e3e`, `2f2fef6` |
| llm `sse.rs` | 869 | 474 | `sse.rs` 474 | `9e16350` |
| llm `router.rs` | 805 | 464 | `router.rs` 464 | `168ad4b` |
| tools `spec.rs` | 1 377 | 233 | `spec/agent.rs` 402 | `b3136ca`, `e156a41` |
| tools `shell.rs` | 999 | 484 | `shell/tests.rs` 514 | `fbf7a6d` |
| tools `fs.rs` | 938 | 590 | `fs.rs` 590 | `3100a58` |
| hitl `policy.rs` | 1 120 | 659 | `policy.rs` 659 | `b1db3eb` |
| hitl `lib.rs` | 845 | 656 | `lib.rs` 656 | `29bfec5` |
| store `lib.rs` | 1 036 | 641 | `lib.rs` 641 | `86d9103` |
| store `migrations.rs` | 942 | 278 | `migrations/init.rs` 668 | `eb7d5a5` |
| store `tests/migration_from_0_17.rs` | 1 000 | 212 | `checks.rs` 422 | `0b7da36` |

`[files.oversized]` perd config.rs, turn.rs, provider.rs, codex.rs, spec.rs, policy.rs,
store `lib.rs` ; `[lints].allow_too_many_lines` 21 → 20.

## Choix

- **config.rs** découpé par section du schéma : `duration`, `channel` (`[owner]`,
  `[telegram]`), `providers` (fournisseurs, modèles, routage), `sections` (le reste),
  `edit` (lecture tolérante et réécriture ciblée, #76), `validate` (`impl Config`),
  `store` (`Generation`, `ConfigStore`). `config.rs` garde la structure `Config`.
- **provider.rs** : `openrouter`, `openai_compat`, `body` (corps chat completions),
  `stream` (lecture du flux, collecte). Quatre fonctions privées et
  `OpenRouterProvider::body` passent en `pub(super)`. Ses tests montant un faux serveur
  HTTP vont dans `provider/tests/server.rs` (sinon 853 lignes).
- **codex.rs** : `request`, `response`, `models` (nommé ainsi pour ne pas masquer
  `crate::catalog`).
- **spec.rs, le seul commit qui touche un corps** : `all()` était une fonction de
  960 lignes (un `allow(too_many_lines)` du gel). Chaque section commentée de la table
  devient une fonction `pub(super)` qui rend ses entrées, inchangées ; `all()` les
  concatène dans l'ordre d'origine puis trie comme avant. Même liste, même ordre.
- **migrations.rs** : `SQL_0001` (texte immuable) va dans `migrations/init.rs`, comme
  `since_0011.rs` avant lui.
- **Filet de migration** : `tests/migration_from_0_17/{seed,checks}.rs`, déclarés par
  `#[path]` (un fichier de `tests/` est une racine de crate).
- **Hors périmètre, par nécessité** : `penelope-evals/tests/docs.rs` lisait les
  structures de configuration dans `config.rs` seul ; il lit aussi `config/*.rs` (hors
  tests). `docs/ca-matrix.md` régénéré (`UPDATE_CA_MATRIX=1`) : huit tests `ca_*` ont
  suivi leurs modules de tests, noms inchangés.
- Les tests sortis gardent leurs littéraux multi-lignes à l'octet près : l'extraction
  ne désindente pas les lignes de continuation d'une chaîne.

## Points ouverts

- **Dérogation `[channel.allowed]`** : découper un fichier répartit ses mentions du
  canal sur les nouveaux fichiers, total inchangé : config.rs 25 → 2 + `config/channel.rs`
  11 + `config/validate.rs` 11 + `config/store.rs` 1 ; `spec.rs` 4 → `spec/agent.rs` 4 ;
  `migrations.rs` 30 → 8 + `migrations/init.rs` 22. `scripts/check-budget.sh` y voit des
  entrées ajoutées et demande le trailer « Dérogation-budget: #208 », posé avec l'accord
  de l'intégrateur (commit de dérogation, entrées commentées « suit X (découpage T32) »).
- **`.gitignore`** ignore tout chemin nommé `spec` : `crates/penelope-tools/src/spec/*.rs`
  a été ajouté avec `git add -f`. Un fichier nouveau dans ce répertoire demandera la
  même chose ; une exception `!crates/penelope-tools/src/spec/` serait plus sûre (hors
  périmètre).
- Le message du commit `04210d4` annonce config.rs à 41 lignes : il en fait 61.

## Notes de version

#### Plafonds des crates socle (lot L)

- `penelope-kernel`, `penelope-llm`, `penelope-tools`, `penelope-hitl`, `penelope-store` :
  plus aucun fichier au-dessus de 800 lignes (charte V1 §3.4). Déplacements seuls, tests
  en fichiers frères, chemins publics réexportés ; aucun changement de comportement.
- Sept fichiers sortent de la liste de référence du gel (`config.rs`, `turn.rs`,
  `provider.rs`, `codex.rs`, `spec.rs`, `policy.rs`, `penelope-store/src/lib.rs`) ; le
  catalogue d'outils perd son `allow(clippy::too_many_lines)`.
