# Lot L : documentation d'architecture et décisions 0013, 0014 (épopée #208, T31)

Agent `l-docs`, branche `v1-l-docs`, base 1.0.0-alpha.13 (`12b187e`).
Spécification : `design/v1/decoupage-daemon.md` §6 T31 (la décision prend le numéro 0013,
0011 étant pris) ; charte `design/v1/README.md` §3 et §9. Aucun code.

## Livré

- `docs/architecture.md` : couches (niveaux calculés par `cargo metadata`, réduction
  transitive des arêtes), table des 27 crates (rôle, dépendances internes directes,
  lignes de `src/`), ce que le daemon garde, la composition par la CLI, les ports de
  `penelope-app` et de la boucle avec leurs implémentations et leurs consommateurs
  (relevés par `grep` des `impl … for` et des `dyn …`), les contextes, la frontière
  canal, le journal comme source de lecture, les règles d'archtest et les valeurs du gel,
  ce qui reste.
- `docs/decisions/0013-decoupage-du-daemon.md` et `0014-boucle-pipeline.md`, au format de
  0015 (contexte, décision, raisons, conséquences, fait et reste, alternatives écartées).
- `docs/README.md` : les deux décisions, le guide dans « Par fichier » et une ligne
  « Comprendre le code » dans « Par besoin » ; la phrase des numéros réservés ne garde
  que 0016 et 0017.
- `docs/progress.md` : deux lignes dans la table des décisions, même phrase de numéros
  réservés. Pas de section de version (périmètre du lot).

## Choix

- **Ce qui existe, pas ce qui était prévu.** Chaque chiffre vient du code de
  1.0.0-alpha.13 : 14 986 lignes au daemon, 21 modules, `Daemon` nommé 65 fois dans 16
  fichiers, 23 fichiers dans la liste de référence, 77 `ca_*`, 31 fichiers et 249
  mentions dans `[channel.allowed]`. Les écarts à la spécification sont écrits dans 0013
  (daemon à 14 986 et non 9 200, `penelope-app` sans `penelope-telegram`,
  `decide_approval` resté dans la boucle, `DigestSource` dans `penelope-dream`, `Slot`
  au lieu d'`Option`, trois règles non posées) et dans 0014 (T07, T08, T21 à T23
  non faits).
- **R9 et R10 dits absents** : `ratchet.rs` annonce R9 « à venir », `budget.toml` n'a pas
  de section `coverage` ; R11 est la suite `scenarios`.
- **Pas de lien vers `design/`** depuis `docs/` : les pages sont embarquées dans le
  binaire, `design/` ne l'est pas ; les chemins sont cités en code.
- **Titre « Ce qui reste »** plutôt que « Limites actuelles » : le test
  `no_doc_presents_something_shipped_as_missing` lit les sections de manque, et celle-ci
  cite des outils livrés.
- **Le juge (#203) n'est pas présenté comme fait** : `k-juge` y travaille en parallèle.
- **Alignement sur `k-api`** (intégré à `v1` pendant le lot, signalé par l'intégrateur) :
  port `CacheAudit` retiré des tableaux (dernier appel lu dans le `BudgetLedger`, parties
  pures dans `penelope_llm::cache`), T24, T25 et T27 passés de « reste » à « fait » dans
  0014. 0014 cite la décision 0016 sans lien : le fichier n'existe pas sur cette branche
  avant le rebase, et le test des liens le refuserait.

## Vérifications

`UPDATE_DOCS=1 cargo test -p penelope-evals --test docs` : 14 tests verts, aucun fichier
régénéré. `cargo test -p penelope-executor selfdocs` : vert. Aucun tiret cadratin dans les
fichiers écrits.

## Collisions attendues

`k-api` a ajouté 0016 dans `docs/README.md` et la table de `docs/progress.md`, et a
réécrit la même phrase des numéros réservés : au rebase, garder 0013 à 0016 dans les
listes et « Le numéro 0017 est réservé » dans les deux phrases. Ensuite, 0014 peut lier
`[0016](0016-ptc-hors-v1.md)`.

## Notes de version (pour docs/progress.md)

#### Architecture documentée, décisions 0013 et 0014 (#208, T31)

- Nouveau guide `docs/architecture.md` : les crates et leurs couches, les ports de
  `penelope-app` et qui les implémente, la frontière canal, le journal comme source de
  lecture, les règles d'architecture et les valeurs du gel, ce qui reste avant la
  bascule. Pénélope le lit aussi, embarqué dans son binaire.
- Décision [0013](decisions/0013-decoupage-du-daemon.md) : le daemon découpé en crates, la
  passerelle Telegram au-dessus de lui.
- Décision [0014](decisions/0014-boucle-pipeline.md) : la boucle d'agent est un pipeline
  d'étapes typées.
