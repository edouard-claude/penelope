# Notes de livraison : lot E suite, compaction, niveau 1, fork (épopée #208, T7, T8, T10, T18)

Branche `v1-i-compaction-fork`, dérivée de `v1` à `fbf1fe5` (1.0.0-alpha.4), poussée sur
`origin`. Un commit de déplacement, puis un commit par tâche, chacun vert seul. Aucune
migration : les colonnes de la 0020 (`messages.event_id`, `lcm_nodes.event_id`) suffisent.

## Commits

| Commit | Quoi |
|---|---|
| `Contexte : l'admission du niveau 1, la publication d'un résumé et les réécritures du canonique changent de fichier` | déplacement sans changement de corps : `engine.rs` vers `publish.rs`, `store.rs` vers `store/rewrite.rs` ; `UPDATE_BUDGET` (`store.rs` quitte la liste, `engine.rs` 1 148 → 1 025) |
| `Journal : la compaction écrit conv.summary…` | T7 |
| `Journal : le niveau 1 devient un conv.tool_result…` | T8 |
| `Journal : le fork et le retour arrière entrent au journal…` | T10 |
| `Flux runtime : le contenu de la conversation n'arrive qu'à qui le demande…` | T18 |

## Ce qui est livré

- **T7.** `ContextEngine::apply_summary_as` (le daemon passe le déclencheur) écrit un
  `conv.summary` de surface `replace` sur la plage couverte, par `append_with` ; la
  seconde transaction écrit le nœud LCM, qui cite l'événement (`lcm_nodes.event_id`), et
  marque la plage, ensemble (`Placement::write`). Le payload porte le texte rendu du nœud,
  les ancres, `previous_node_id`, le modèle, le déclencheur, `batches_left` et
  `idempotency_key`. `context.compacted` suit, inchangé.
- **T8.** `admit_tool_group` écrit l'artefact, puis `HistoryStore::externalise_as` :
  `conv.tool_result` de surface `replace` d'un seul nœud, avec le `call_id`, l'outil, le
  tour et l'étape du résultat d'origine, `artifact_id`, `artifact_sha256`,
  `original_tokens` ; la ligne est réécrite dans la seconde transaction.
- **T10.** `HistoryStore::journal_fork` (appelé par `session_ops::fork` avant la copie)
  et `HistoryStore::rewind_from` (remplace `truncate_from` dans `session_ops::rewind`,
  événement puis coupe dans la seconde transaction). La copie V0 reste.
- **T18.** Deux tests dans `runtime_events.rs`, catalogue `conv.*` complet dans
  `docs/runtime-events.md`.

Chaque tâche a son test qui plie le journal par `derive` et vérifie que les bornes
désignent des nœuds présents : `publish/tests.rs` (T7, T8), `store/dual/tests.rs` et
`session_ops.rs` (T10).

## Les choix

- **Les bornes sont des adresses du journal, pas des numéros V0.** `SummaryJob`, le
  niveau 1 et le retour arrière raisonnent en `messages.seq` ; l'événement, lui, doit
  citer ce que le pliage connaît (`offset + events.seq`, §2.3). `HistoryStore::address`
  traduit par `messages.event_id` ; l'offset est celui de la session qui porte
  l'événement ; une ligne sans événement garde son `seq` V0 (préfixe scellé, T11).
- **Idempotence de T7 par le journal.** La clé `SummaryJob::idempotency_key` est dans le
  payload ; avant d'écrire, on la cherche. Trouvée sans nœud (arrêt entre les deux
  transactions), on écrit le nœud annoncé, sous son identifiant, sans second événement.
  Le chemin « nœud vivant sur exactement cette couverture » de la V0 reste en tête.
  `Lcm` expose `insert_leaf_in` et `replace_in` (les écritures qu'appelaient déjà
  `insert_leaf` et `extend`) pour que nœud et marquage partagent une transaction : la
  réparation `context/engine.rs` (nœud sans marquage) ne sert plus que sans journal.
- **`HistoryStore::journaled`** : « l'événement, puis l'écriture avec son identifiant »,
  ou l'écriture seule sans journal. La double écriture des messages (T5), T7, T8 et la
  coupe de T10 y passent.
- **Adresses d'une fille (T10).** La note du lot E laissait l'offset d'un fork à T10.
  Trois changements pour que le pliage de la fille tombe juste : `copy_messages` recopie
  `event_id` (une ligne héritée garde l'adresse qu'elle a chez la mère) ; le
  `conv.context` d'une fille vise `offset + seq` ; le premier `conv.system` d'une fille
  est un `replace` du préfixe hérité (raison `first`), l'`append` d'avant étant refusé
  par le pliage dès que la mère a un préfixe. `up_to` est la dernière adresse de la mère
  (tout est hérité) et sert d'`offset`.
- **L'archive d'un retour arrière reste une copie V0, sans `conv.fork`.** Le §2.6 en fait
  un fork par référence ; tant que la copie V0 est la source de lecture, un `conv.fork`
  sur l'archive lui donnerait une surface (toute la mère) qui contredit ses lignes (la
  seule partie retirée). À trancher avec T12 ou T14.
- **`conv.tool_result` de remplacement seulement pour un résultat d'outil.** Une ligne
  sans `call_id` est réécrite sans événement : le pliage exige le même appel.

## Plafond du daemon

Le lot fait grossir `penelope-daemon/src` de 183 lignes (84 819 → 85 002), dont
environ 170 de tests (`runtime_events.rs`, `session_ops.rs`). `[crates]` n'est pas
relevé : `crates_stay_under_their_ceiling` est rouge, et lui seul (rouge toléré, le lead
pose le plafond). `daemon/compaction.rs`, sur la liste de dette, garde sa taille (une
ligne changée). Côté contexte, aucun fichier au-dessus de 1 000 lignes.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast` : verts, sauf `crates_stay_under_their_ceiling`
(ci-dessus). `cargo test -p penelope-evals --test scenarios` : 18 sur 18. Aucun
`surface.jsonl` ne bouge. Les `expected.jsonl` régénérés (`UPDATE_SCENARIOS=1`) ont été
comparés à leur base par script, une fois retirés les événements ajoutés par la tâche :
ne bougent que les `seq` suivants, le compte de l'audit et la `target` des `conv.context`
postérieurs (une adresse, décalée d'autant) ; en plus, pour `fork-puis-divergence`, la
`target` de la fille passe à `offset + seq` et son `conv.system` devient un `replace` du
préfixe hérité (T10, voir les choix).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements : compaction, niveau 1, fork et retour arrière (#208, T7, T8, T10, T18)

- **La compaction entre au journal** : un résumé publié est un `conv.summary` qui
  remplace la plage qu'il couvre (texte du nœud, ancres, résumé prolongé, modèle,
  déclencheur). Le nœud LCM cite son événement et la plage est marquée dans la même
  transaction que lui ; republier le même travail n'écrit rien de plus, et un arrêt
  entre l'événement et le nœud se répare depuis l'événement.
- **Le niveau 1 aussi** : un gros résultat d'outil parti en artefact est un
  `conv.tool_result` qui remplace ce seul nœud, avec l'artefact et son empreinte ; le
  nœud garde sa place.
- **Fork et retour arrière** : `/fork` écrit `conv.fork` en tête de la session fille
  (héritage par référence de toute la mère), `/rewind` écrit `conv.rewind` avant de
  couper. Les adresses d'une fille suivent son héritage (contexte figé, préfixe
  système, lignes copiées).
- **Flux runtime** : un consommateur ne reçoit un `conv.*` que s'il le nomme ou n'a pas
  de filtre, rédigé et borné à 64 Kio ; le catalogue des `conv.*` est dans
  `docs/runtime-events.md`.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.
```

## Blocages

Aucun. Reste pour la suite : l'archive d'un `/rewind` (voir les choix).
