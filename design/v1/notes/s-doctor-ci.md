# s-doctor-ci : le scénario `rpc-diagnostic` ne dépend plus de la machine

Épopée #208, critère 7 (suite du lot s-derniers).

## Constat

La CI de `v1` (Linux et macOS, run 36255764234 sur 4480a3b) échouait sur `rpc_diagnostic` :
« expected.jsonl diffère (issues n°4) … rpc : doctor ». Le test passait sur le poste,
seul, en parallèle avec les 61 autres scénarios, avec un `PATH` réduit, un `HOME` vide,
`TZ=UTC` et `CI=true`.

La cause est le contrôle `voice` : il cherche `ffmpeg` par `penelope_platform::which`,
qui ajoute au `PATH` des répertoires fixes (`/opt/homebrew/bin` entre autres). Réduire le
`PATH` ne le cache donc pas ; sur le poste, `ffmpeg` est là et `voice` rend une synthèse
d'essai, sur les runners il est absent et `voice` rend « `ffmpeg` absent » avec la
correction `brew install ffmpeg`. La liste de refus (`without`) ne le retirait pas.

Reproduction : `sandbox-exec` avec un profil qui refuse la lecture de tout chemin en
`/ffmpeg`, sur l'ancien scénario : même échec qu'en CI (issue n°4 sur 4).

## Livré

- Étape `rpc` : un filtre `only`, l'inverse de `without` et appliqué après lui ; il ne
  garde que les éléments d'une réponse en tableau dont un champ répond à un motif (`*`
  final : préfixe), dans l'ordre de la réponse. Test unitaire
  `only_keeps_the_matching_elements_in_their_order`.
- `rpc-diagnostic` : la liste de refus remplacée par une liste explicite des 38 contrôles
  propres à Pénélope. Un contrôle qu'un hôte ajoute n'atteint plus les attendus. Sortent
  des attendus `voice` (ffmpeg) et `clock` (compare l'horloge du scénario à celle du
  système) ; les deux masques qui les tenaient disparaissent. Les 38 autres lignes sont
  identiques, dans le même ordre.

## Vérifications

- Dix rejeux ordinaires, puis dix rejeux sous `sandbox-exec` sans `ffmpeg`, `PATH`
  réduit, `HOME` vide, `TZ=UTC` : tous verts.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` : voir le rapport du lot.

## Notes de version

#### Scénario du diagnostic stable en CI

Le scénario `rpc-diagnostic` compare une liste explicite des contrôles de `doctor` propres
à Pénélope (nouveau filtre `only` de l'étape `rpc`) au lieu de retirer ce qui décrit
l'hôte : le contrôle `voice`, qui cherche `ffmpeg`, faisait échouer la CI de `v1` sur les
runners qui ne l'ont pas.

## Blocages

Aucun. La CI ne tourne pas sur la branche `v1-s-doctor-ci` (seules `main` et `v1`) : la
preuve en CI viendra de l'intégration.
