# Notes de livraison : lot I, bascule de lecture (épopée #208, T14)

Branche `v1-i-bascule`, dérivée de `v1` à `5669220` (1.0.0-alpha.6), poussée sur
`origin`. Trois obstacles levés d'abord (un commit chacun), un quatrième trouvé par la
comparaison, puis T14 et le passage du défaut. Aucune migration.

## Commits

| Commit | Quoi |
|---|---|
| `1f6e0b0` | déplacement : les tests de `config.rs` sortent dans `config/tests.rs` (`UPDATE_BUDGET` 2 790 → 2 174) |
| `ad60af5` | obstacle 1 : arguments d'appel et `reasoning_details` gardent les octets du fournisseur |
| `09aa20b` | obstacle 2 : une fille de fork hérite des messages sans leur contexte figé |
| `e98cc86` | obstacle 3 : `prompt_snapshots` devient un cache du `conv.system` |
| `20948a2` | déplacement : `atomic_write` et `diff_paths` sortent dans `config/write.rs` |
| `c6c0b6a` | déplacement : la projection V0 passe du daemon à `penelope-context/src/read.rs` |
| `456cb85` | matrice CA régénérée (`ca_4_3` a suivi ses tests) |
| `8166517` | quatrième écart : `conv.assistant` porte le drapeau `eager` |
| `c07fe6c` | T14 : `history.source`, lecture depuis le journal, comparaison, tests (`UPDATE_BUDGET` `config.rs` 2 148) |
| `8e3ce32` | T14 : le défaut passe à `journal` |

## Ce qui est livré

- **Clé `history.source`** (`tables` | `journal`, défaut `journal`), section `[history]`
  de `config.rs`. `History::effective_source` : la variable
  `PENELOPE_HISTORY_SOURCE` l'emporte, pour rejouer toute la suite sous l'autre source.
- **`penelope-context/src/read.rs`** : `ContextEngine::projected_entries(session,
  source)` et `ContextEngine::tail(session, limit, source)`. En mode `journal`, le
  journal de la session est plié par `derive` après son préfixe hérité (`Lineage`), et
  les entrées reçoivent le numéro de leur ligne (`Lineage::row_seqs`, sortie de
  `expected` : le niveau 0 cite `seq:N` dans la requête). `HistoryStore::read_journal`
  rend `None` quand le journal ne sait pas redonner la session (lignes sans événement ni
  scellement, archive d'un `/rewind`) : les tables font foi.
- **Comparaison en mode `tables`** (`debug_assertions`, donc tous les tests) : chaque
  lecture est comparée à celle du journal, octet pour octet (sérialisation JSON des
  entrées) ; une divergence fait échouer la requête avec la session, l'entrée et les
  deux textes (`ReadError::Diverged`).
- **Daemon** : `SessionConversation::projected_entries` et `tail` ne sont plus qu'un
  appel, avec la source en vigueur. Le reste de la requête (préfixe par les tuiles, note
  de fusion, consigne de relance, niveaux 0, 2, 4) est inchangé sous les deux sources.
- **Obstacles de `i-verify.md`** :
  1. `conv.assistant.verbatim` : le texte JSON exact des arguments d'appel et des
     `reasoning_details` que la forme canonique du journal réordonnerait (ou dont elle
     retirerait le `.0` d'un flottant) ; le pliage le préfère. Une valeur déjà canonique
     n'est pas doublée.
  2. `Sealed::fork` vide les contextes hérités : la fille repart comme la copie V0, ce
     que le §2.3 dit déjà (le fork hérite des nœuds et des messages).
  3. `prompt_snapshots` : `journal_system` écrit l'instantané dans la seconde transaction
     de son `conv.system`, `apply_in` (rattrapage) et `reindex` le refont s'il manque.
     `uses` part de zéro, le daemon le compte toujours à chaque appel.
- **Quatrième écart**, trouvé par la comparaison : la réponse d'abandon de boucle est
  écrite `eager` ; `conv.assistant.eager` le garde (sans effet sur la requête, mais une
  ligne refaite doit être la même).

## Critères de fin

- `cargo test --workspace` vert sous les deux sources : **1 984 tests** avec le défaut
  `journal`, **1 984** avec `PENELOPE_HISTORY_SOURCE=tables` (1 862 hors
  `penelope-evals`, 122 dans `penelope-evals`). Sous `tables`, la comparaison tourne à
  chaque requête de chaque test et de chaque scénario.
- `ca_5_4_each_request_extends_the_previous_one`,
  `the_projection_only_reads_what_is_not_summarised`, `ctx_safety_recovery_after_crash` :
  verts sous les deux.
- `both_history_sources_send_the_same_requests` (`tests/scenarios.rs`) : les seize
  scénarios rejoués avec `history.source` imposé à `tables` puis à `journal`, chaque
  requête comparée en valeur et en octets. Aucun `surface.jsonl` ne bouge ; seul
  `expected.jsonl` d'`approbation-apres-redemarrage` gagne le champ `verbatim`.
- Tests sans daemon (`read/tests.rs`) : mêmes octets sous les deux sources après
  compaction et prolongation, niveau 1, arguments hors ordre canonique, scellement,
  retour arrière (la mère), archive lue dans les tables ; ligne modifiée à la main
  nommée par la comparaison et ignorée en mode `journal`.

## Les choix

- **La lecture journal plie, elle ne lit pas les caches du projecteur.** Le §4.3 parle
  des caches alimentés par le projecteur ; la consigne du lot demandait `derive`. C'est
  ce qui est fait : les tables ne sont plus lues du tout en mode `journal` (sauf repli).
- **Repli sur les tables, jamais d'échec du tour en production.** Un journal qui ne se
  plie pas en mode `journal` : erreur au journal du daemon, lecture dans les tables
  (`penelope history verify` nomme la session). Sous la comparaison, le même cas fait
  échouer la requête.
- **`the_projection_only_reads_what_is_not_summarised`** écrivait `lcm_nodes` et
  `compacted` à la main, sans événement : il publie désormais son résumé par
  `apply_summary`, le chemin de la compaction (même couverture, même assertion).
- **Variable d'environnement** plutôt que défaut changeant : la documentation montre le
  défaut du code, la variable ne sert qu'à rejouer une suite. Sous
  `PENELOPE_HISTORY_SOURCE`, `both_history_sources_send_the_same_requests` ne compare
  rien (il le dit et passe) : la source est imposée aux deux rejeux.
- **Événements écrits avant ce lot** (bases d'alpha) : sans `verbatim` ni `eager`, ils se
  relisent comme avant (clés rangées). Aucune base de production n'a de journal.

## Reste et risques

- **Coût de lecture** : en mode `journal`, chaque requête relit et plie tout le journal
  de la session et de ses ancêtres (`conv.system` compris, texte entier). Invisible sur
  les tests ; linéaire en longueur de session. Piste : une surface mise en cache par
  session sous le dernier `event_id`, ou lire les caches du projecteur (§4.3) quand T16
  en aura fait la seule écriture.
- **Instantané orphelin** : un `conv.system` dont l'appel n'aboutit jamais laisse un
  `prompt_snapshots` qu'aucune ligne d'`usage` ne cite ; la purge d'une session ne le
  voit pas (elle part des `system_hash` d'`usage`), la rétention l'emporte à son terme.
  À reprendre avec T17 (purge d'une session : partir de ses `conv.system`).
- Non touchés, pour T15 : outils `history_*`, `audit show`, relecture d'épisode, export ;
  l'archive d'un `/rewind` reste une copie V0 lue dans les tables.
- Le message du commit `c07fe6c` parle de « dix-huit » scénarios : il y en a seize (dix-
  huit tests dans `tests/scenarios.rs`).

## Plafond du daemon

`penelope-daemon/src` descend (la projection V0 part dans `penelope-context`) :
`crates_stay_under_their_ceiling` vert. `config.rs` descend de 2 790 à 2 148. Aucun
fichier nouveau au-dessus de 615 lignes (`config/tests.rs`, déplacé).

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
propres. `cargo test --workspace --no-fail-fast` : vert sous `journal` (défaut) et sous
`PENELOPE_HISTORY_SOURCE=tables`, lancé en deux passes par source (hors
`penelope-evals`, puis `penelope-evals`) pour tenir sous dix minutes chacune.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements : la conversation se relit depuis le journal (#208, T14)

- **Nouvelle clé `history.source`** (`journal` par défaut, `tables` pour revenir à la
  lecture d'avant) : chaque requête relit la conversation en pliant le journal
  d'événements de la session (préfixe scellé, mère d'un fork compris) au lieu des
  tables `messages`. Une session que le journal ne sait pas redonner se lit dans les
  tables.
- **Aucune requête ne change** : les seize scénarios enregistrés envoient les mêmes
  requêtes, octet pour octet, sous les deux sources ; en compilation de test, le mode
  `tables` compare chaque lecture à celle du journal et échoue à la première différence.
- Le journal garde désormais l'ordre exact des arguments d'appel et des blocs de
  raisonnement tels que le fournisseur les a envoyés, et le drapeau `eager` d'une
  réponse ; une fille de fork hérite des messages de sa mère sans leur contexte figé,
  comme avant ; l'instantané du prompt système est refait depuis le journal par
  `penelope history reindex`.
```

## Blocages

Aucun.
