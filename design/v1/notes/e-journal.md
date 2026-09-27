# Notes de livraison : lot E, journal d'événements, phase 1 (épopée #208, T1, T2, T3, T21)

Branche `v1-e-journal`, dérivée de `v1` à `0f1c41a`, poussée sur `origin`. Sept commits
de code et un de notes (mises à jour par le dernier commit). Périmètre tenu :
`crates/penelope-kernel/src/event.rs` (et son fichier de tests `event/transactional.rs`),
`crates/penelope-context/src/` (modules nouveaux `journal/`, `derive/`, `numbering.rs`,
`render.rs` ; deux champs et `Default` pour `SummaryJob` dans `engine.rs`, une ligne de doc
dans `lcm.rs`), `budget.toml` (descente de `engine.rs`), et, par exception accordée par le
lead, les trois littéraux `SummaryJob` des tests de `penelope-daemon/src/compaction.rs`.
Rien n'est branché : la double écriture est un lot suivant (T5).

## Ce qui est livré

| Tâche | Où | Quoi |
|---|---|---|
| T2 | `event.rs` | `append_in(tx, draft)` synchrone ; `append_with(draft, after)` : l'événement commité, puis `after` dans une seconde transaction, sous le verrou d'ordre, avant la diffusion |
| T1 | `journal/` | `KIND_*`, `FORMAT_VERSION`, `SurfaceOp`, les dix payloads, `ConvEvent::{kind, payload, decode}`, `upgrade_payload`, `DeriveError` |
| T3 | `derive/` | `derive(prefix, events) -> Surface`, `derive_until`, `Sealed::{none, fork, import}`, `Surface::{entries, projected_entries, request_messages}` |
| T21 | `numbering.rs` | `rendered_messages` (appelé par `SummaryJob::messages`), `uncovered`, `node_page` |

Tailles : aucun fichier nouveau au-dessus de 551 lignes ; `event.rs` passe de 676 à 729
lignes, ses nouveaux tests sont dans `event/transactional.rs` ; `engine.rs` (liste de dette)
passe de 1 214 à 1 148 lignes.

## Les choix

- **`SurfaceOp`, pas `Surface`, pour l'opération.** Le §2.2 appelle « surface » à la fois
  l'opération d'un événement et le résultat du pliage (§2.3). L'opération est
  `journal::SurfaceOp` (`append`, `replace`, `cut`, `inherit`, `seal`, sérialisée
  `{"op": ...}` comme au §2.2) ; le résultat est `derive::Surface`.
- **La vérification de forme est à la relecture.** `ConvEvent::decode` refuse une
  opération qui ne va pas avec son kind (`conv.user` autre qu'`append`, `conv.tool_result`
  remplaçant plus d'un nœud, `conv.summary` à bornes inversées, `conv.fork` dont l'opération
  contredit ses champs) ; le pliage ne vérifie plus que ce qui dépend de l'état.
- **`append_in` ne diffuse pas.** La transaction de l'appelant peut encore être annulée ;
  l'événement arrive aux abonnés par `range`. Il ne prend pas non plus le verrou d'ordre :
  il tourne déjà sur le thread écrivain. `append_with`, lui, diffuse l'événement même quand
  `after` échoue, puisqu'il est dans le journal. Les trois chemins passent par la même
  insertion (`insert`), donc la règle de #47 vaut pour tous.
- **Le préfixe hérité est un argument, pas une lecture.** `derive` reste sans base : le
  fork par référence se plie en deux temps, `Sealed::fork(mère, préfixe_de_la_mère,
  événements_de_la_mère, up_to)` puis `derive(&préfixe, événements_de_la_fille)`, récursif.
  `up_to` est une adresse du journal de la mère : la mère est pliée jusqu'à ce point, ce qui
  exclut ce qu'elle a fait après le fork, système compris.
- **Adresse d'un résumé.** Un `conv.summary` est rangé sous sa propre adresse et couvre
  `[from, to]` ; un remplacement ultérieur peut le citer par cette adresse ou par n'importe
  quelle adresse qu'il couvre (c'est ce que fait `SummaryJob.from_seq` en V0 pour une
  prolongation). Dans `projected_entries`, un résumé garde l'adresse 0, comme la projection
  V0.
- **Indulgence après purge seulement.** Un remplacement ou une coupe qui cite un nœud
  absent est une erreur, sauf si un `conv.*` purgé a été vu (ou si la mère d'un fork l'a
  été) : le remplacement masque alors ce qui reste de la plage (§1.4, §2.6, risque 6).
- **`cut after 0`** coupe tout : c'est le retour arrière au tout premier message, qui n'a
  pas de nœud précédent. Une coupe à travers un résumé est refusée.
- **Textes V0 recopiés.** `MERGE_NOTE` et `summary_message` reproduisent octet pour octet
  `conversation.rs` ; la consigne de relance vient du payload (`retry_prompt`). Aucun test ne
  peut les comparer depuis `penelope-context` (le daemon en dépend, pas l'inverse) : la
  comparaison de la phase 3 (T14) le fera ; vérifié à la main par `grep` à la livraison.

## Numérotation à trous (T21)

`SummaryJob` porte deux champs nouveaux, à la demande du lead (compter les en-têtes du
texte rendu, première version du lot, cassait en silence si le rendu changeait) :

- `chunk_messages` : le nombre d'entrées du lot, rempli par `prepare_summary`.
  `messages()` le rend ; à 0 (travail préparé avant T21 et relu depuis
  `compaction.pending.*`, `#[serde(default)]`), il retombe sur `to - from + 1`, exact pour
  une numérotation contiguë.
- `previous_to_seq` : la dernière adresse du résumé prolongé. `summarizer_messages`
  l'affiche comme fin du résumé précédent au lieu de « #chunk_from - 1 », avec le même
  repli.
- `SummaryJob` dérive `Default`. **Exception de périmètre accordée par le lead** : les
  trois littéraux des tests de `penelope-daemon/src/compaction.rs` (fidélité, #179)
  reçoivent `chunk_messages` et `..Default::default()` à la place de
  `previous_summary: None, previous_node_id: None` ; le fichier garde sa taille (la liste
  de dette le lui impose), rien d'autre n'y change.
- Pour que `engine.rs` ne grossisse pas, le rendu du transcript du résumeur
  (`render_transcript`, `render_entry`, `sample`, `plan_batches`) est sorti tel quel dans
  `render.rs`, réexporté par `engine` ; `budget.toml` descend de 1 214 à 1 148 pour
  `engine.rs` (`UPDATE_BUDGET=1`).
- `numbering::uncovered` et `node_page` travaillent sur les adresses existantes.

Reste côté daemon, non touché : `history_expand` (`executor.rs`) pagine déjà par entrée
(`skip`/`take`), il ne régresse pas avec des trous, `node_page` est la même règle ;
`rewind` (`session_ops.rs`) et `conversation.rs:124` (`covered_to + 1`) sont à reprendre.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` : verts. `cargo test -p penelope-evals --test scenarios` : 18 sur
18, sans attendu modifié. `penelope-archtest` : vert, `budget.toml` non touché. Tests
nouveaux : 5 dans `event/transactional.rs` (dont #47 et #162 pour `append_with`), 7 dans
`journal/tests.rs`, 17 dans `derive/tests.rs`, 4 dans `numbering.rs` (dont un
`prepare_summary` réel sur des adresses trouées).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements, briques pures (#208, T1, T2, T3, T21)

- **`EventLog::append_in` et `append_with`** : un événement dans la transaction de
  l'appelant, ou un événement commité puis une seconde transaction (`after`) sur le même
  thread écrivain, sous le verrou d'ordre (#162), avant la diffusion. Une erreur de
  `after` laisse l'événement dans le journal et remonte. Les trois chemins d'écriture
  partagent la même insertion : une lecture en erreur ne forge jamais de maillon (#47).
- **Vocabulaire `conv.*`** (`penelope_context::journal`) : dix kinds (`conv.system`,
  `user`, `context`, `assistant`, `tool_result`, `attempt`, `summary`, `rewind`, `fork`,
  `import`), payloads typés, `"v": 1`, opération de surface (`append`, `replace`, `cut`,
  `inherit`, `seal`). Relecture stricte : un `v` futur, un kind inconnu non marqué
  `ignorable`, une opération qui ne va pas avec son kind sont refusés.
- **Pliage pur** (`penelope_context::derive`) : `derive(préfixe, événements) -> Surface`,
  sans base ; adresses `offset + seq` avec des trous ; résumé, niveau 1 (l'adresse reste),
  nouveau système, coupe, fork par référence récursif, préfixe V0 scellé, tentative dont
  la consigne de relance entre dans la requête suivante, note de fusion. Conversion en
  entrées (`compacted` = masqué par un résumé) et en requête, textes identiques à la
  projection V0. Un journal incohérent est une erreur ; seule une purge rend le pliage
  indulgent.
- **Numérotation à trous** : `SummaryJob` porte le nombre d'entrées de son lot et la fin
  du résumé qu'il prolonge ; `messages()` et les bornes montrées au résumeur ne se
  déduisent plus de `to - from + 1`. `numbering::uncovered` et `node_page` travaillent sur
  les adresses existantes.
- Rien n'est branché dans le daemon : la double écriture vient avec T5.
```

## Blocages

Aucun.
