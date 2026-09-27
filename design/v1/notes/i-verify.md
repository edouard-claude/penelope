# Notes de livraison : lot I, journal phases 3 et 4, verify et projecteur (épopée #208, T12, T13)

Branche `v1-i-verify`, dérivée de `v1` à `b58273b` (1.0.0-alpha.5), poussée sur `origin`.
Deux commits de déplacement, un commit par tâche, ce fichier. Aucune migration :
`projections_session` a déjà `last_event` et `state`.

## Commits

| Commit | Quoi |
|---|---|
| `2d1ba9a` | déplacement : les tests de `commands.rs` sortent dans `commands/tests.rs` |
| `d052a28` | déplacement : les rendus de listes sortent dans `commands/render.rs` |
| `79dc699` | T12 : `penelope history verify` |
| `dd9a28a` | T13 : projecteur, `penelope history reindex`, rattrapage |
| (ce commit) | notes, `UPDATE_BUDGET` (`commands.rs` 2 686 → 2 191) |

## Ce qui est livré

- **Cœur, dans `penelope-context`** (testable sans daemon) :
  - `replay.rs` : la lignée d'une session (son journal, le préfixe hérité par fork,
    récursivement, ou scellé), l'événement de chaque adresse, et `Expected`, ce que ses
    caches doivent contenir : lignes, contextes figés, nœuds LCM ;
  - `verify.rs` : `HistoryStore::verify(session, since)`, `verify_session`, rapport
    `VerifyReport` (divergences nommées par session, adresse du nœud, numéro de ligne) ;
  - `projector.rs` : `FOLD_VERSION`, `apply_in(tx, event)` pour chaque kind `conv.*`,
    filigrane, `HistoryStore::catch_up`, `reindex`, `dirty_projections`.
- **Daemon** : `history::verify`, `history::reindex`, `history::catch_up` (appelé en tête
  de `runner::process_turn`), `history::doctor_check` (ligne « Historique et journal » :
  sessions mises à jour dans la semaine, et rattrapages en échec) ; méthodes RPC
  `history.verify` et `history.reindex` (`rpc/methods/ops.rs`).
- **CLI** : `penelope history verify [--session]`, `penelope history reindex [--session]`,
  code de sortie non nul à la première divergence ou session refusée.
- **Scénarios** : `harness/journal.rs`, à la fin de chaque scénario, après le relevé du
  monde : `verify` à zéro ; toutes les lignes non scellées effacées (messages, plein
  texte, contextes, nœuds LCM) puis `reindex` ; `verify` à zéro ; caches identiques à
  avant l'effacement (messages avec leur numéro, contextes, résumés, plein texte). Les
  dix-huit passent.

## Critères de fin

- T12 : zéro divergence sur les bases des dix-huit scénarios (tour simple, outils,
  compaction et prolongation, niveau 1, fork, rewind, purge, crash, redémarrages…) ;
  `UPDATE messages SET content` → une divergence `content` qui nomme la session, l'adresse
  du nœud et la ligne (`a_hand_edited_row_names_its_session_and_its_node`, et côté daemon
  par RPC et `doctor`, `verify_names_a_tampered_row_through_rpc_and_doctor`) ; préfixe
  scellé modifié → divergence `digest` (`an_edited_sealed_prefix_breaks_its_digest`).
- T13 : lignes non scellées effacées puis `reindex` → `verify` à zéro et lignes
  identiques (`reindex_after_erasing_unsealed_rows_gives_them_back`, et les scénarios) ;
  `FOLD_VERSION` différente dans le filigrane → refonte
  (`a_new_fold_version_rebuilds_the_session`) ; seconde transaction en échec →
  rattrapée au prochain accès (`a_failed_second_transaction_is_caught_up_at_next_access`,
  `reindex_and_catch_up_rebuild_what_the_journal_says` côté daemon).

## Les choix

- **Appariement par `event_id`**, une ligne scellée par son numéro, comme le demandait
  `i-scellement.md`. Seul l'ordre des lignes est vérifié, pas leur numéro.
- **Numéros de ligne de la refonte** : ceux que la V0 aurait donnés. Par adresse, un
  message scellé garde son numéro, les autres prennent le suivant du précédent
  (`MAX(seq) + 1`, une coupe libère ses numéros, une copie de fork garde ceux de la
  mère). Vérifié à l'identique sur les dix-huit scénarios : la refonte ne décale ni
  `message_context`, ni les bornes LCM, ni ce que la V0 relit.
- **Archive d'un `/rewind` : vérifiée, pas exclue.** Elle reste une copie V0 sans journal
  (point ouvert de `i-compaction-fork.md`). `verify` la reconnaît (`conv.rewind` d'une
  autre session qui la nomme dans `archive_session`) et la compare aux messages que la
  coupe a retirés de sa mère : dérivation de la mère juste avant le `conv.rewind`, nœuds
  après `after`, mêmes numéros. `reindex` la refait de la même façon. Le rapport la liste
  (`archives`). En faire un fork par référence reste pour T14, quand la lecture passera au
  journal.
- **Contextes hérités par une fille de fork : non attendus.** `copy_messages` ne recopie
  pas `message_context` : les messages hérités d'une fille partent sans leur contexte
  figé dans ses requêtes (scénario `fork-puis-divergence`, appel 4), alors que la surface
  dérivée les a. `verify` ne compare que les contextes de la fille elle-même. **Pour T14** :
  la bascule changera la requête d'une fille (contexte réapparu sur les messages hérités).
- **Nœuds LCM comparés** : bornes (en numéros de ligne), texte, identifiant (sauf copie
  d'un résumé hérité, tiré au hasard), `event_id`, jetons, ancres.
- **Contenu comparé structurellement.** Le journal range les clés des objets JSON
  (`canonical_json`), la ligne garde l'ordre du fournisseur : `{"path", "content"}` d'un
  côté, `{"content", "path"}` de l'autre pour le même appel d'outil (scénario
  `approbation-apres-redemarrage`). **Pour T14, risque pour le cache de prompt** : une
  requête construite depuis le journal enverrait les arguments d'appel dans un autre
  ordre que la V0, octets différents.
- **Le projecteur ne double pas le chemin direct.** Tant que les tables sont la source
  (jusqu'à T14), les lignes s'écrivent dans la seconde transaction de chaque événement.
  `apply_in` est une écriture idempotente (« ce qui manque ») ; `catch_up` rejoue les
  événements postérieurs au filigrane, à l'ouverture de chaque tour. Le chemin direct
  devient idempotent en retour (`insert_row` et la publication d'un résumé retrouvent ce
  que le projecteur a déjà écrit) : un rattrapage qui passe entre les deux transactions
  d'un `append_with` (compaction de fond pendant l'ouverture d'un tour) ne double rien.
  Le chemin direct n'avance pas le filigrane : le rattrapage relit les événements du
  tour précédent, quelques dizaines de requêtes courtes.
- **Rattrapage** : une transaction ; une erreur annule tout, puis pose `dirty` et l'erreur
  dans `projections_session.state`, que `doctor` affiche avec la commande de refonte. Un
  fork dont la copie manque, ou des bornes introuvables, déclenchent une refonte de la
  session plutôt qu'une réparation partielle. Sans filigrane, le rattrapage repart du
  premier événement (une fois par session).
- **Refonte refusée** pour une session qui a des lignes sans événement ni scellement
  (sessions « sautées » par T11) : elles seraient perdues. Rien n'est effacé, la session
  est nommée dans `refused`.
- **Plein texte d'un corps externalisé** : la V0 garde le texte d'origine dans
  `messages_fts` (`externalise` n'y touche pas) ; la refonte le relit sur l'événement
  d'ajout.
- **`prompt_snapshots` n'est pas maintenu par le projecteur** : l'écriture est dans le
  daemon (`prompt_snapshot.rs`) et hors du périmètre ; `conv.system` est un no-op
  d'`apply_in`. À reprendre avec T14 ou T16.
- **Un retour arrière qui coupe dans le préfixe scellé** retire des lignes que
  l'empreinte couvre : `verify` le dit en note (« empreinte non vérifiable ») plutôt que
  de crier à la falsification.

## Défauts trouvés par `verify`, corrigés

1. **Offset d'une session scellée à zéro.** `origin_in` (`store/dual.rs`) lisait
   `$.offset` à la racine du payload ; celui d'un `conv.import` est dans son opération de
   surface (`$.surface.offset`). Toute session scellée avait donc un offset nul : le
   `conv.context` d'un message écrit après le scellement, les bornes d'un résumé et la
   coupe d'un retour arrière visaient des adresses fausses (défaut que
   `i-scellement.md` avait prévu pour les contextes). Les événements déjà écrits par une
   base d'alpha gardent leur adresse fausse : `verify` les signalera, `reindex` n'y peut
   rien (le journal ne se réécrit pas). Aucune base de production n'a encore de
   `conv.import`.
2. **Contexte figé orphelin après un retour arrière.** `truncate_in` retirait les lignes
   sans leur `message_context` ; le message suivant, écrit au même numéro, trouvait le
   contexte « déjà figé » : il héritait du bloc de l'ancien et son `conv.context` n'était
   jamais journalisé. Scénario `rewind` : un `conv.context` de plus dans `expected.jsonl`,
   `surface.jsonl` inchangé (le bloc avait le même texte, l'horloge n'avançant pas).

## Écarts de périmètre

- `crates/penelope-kernel/src/api.rs` : les constantes `HISTORY_VERIFY` et
  `HISTORY_REINDEX` et leur place dans `method::ALL` (le plan de `i-scellement.md` le
  prévoyait) ; `crates/penelope-cli/src/client.rs` : les deux méthodes dans la liste des
  méthodes longues.
- `crates/penelope-daemon/src/runner.rs` : une ligne, l'appel de `history::catch_up` en
  tête de `process_turn` (« à l'ouverture d'une session ») ; `engine.rs` est à sa borne
  de dette, `runner.rs` est le premier point commun à tous les tours.
- `crates/penelope-evals/tests/golden/` : `history.verify.json`, `history.reindex.json`
  (nouveaux), `doctor.json` (ordre des clés de la forme, par la ligne ajoutée) ;
  `docs/install-headless.md` (les deux commandes, à côté d'`audit-verify`).
  `UPDATE_DOCS=1` ne change rien : aucune table générée ne liste ces méthodes.

## Plafond du daemon

`penelope-daemon/src` grossit de 228 lignes nettes (230 ajoutées, 2 retirées), dont 110
de tests (`history/tests.rs`, `rpc/tests.rs`) : `crates_stay_under_their_ceiling` est
rouge (86 690 pour 86 462), rouge toléré, le lead pose le plafond. Aucun fichier de la
liste de dette ne grossit ; `commands.rs` en descend (2 686 → 2 191). Aucun fichier
nouveau au-dessus de 712 lignes.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` : propres. `cargo test --workspace --no-fail-fast` : vert sauf `crates_stay_under_their_ceiling` (rouge toléré, ci-dessus). Pendant le lot :
`penelope-context` (151 tests, dont 13 nouveaux), `penelope-daemon` (lib complète),
`penelope-evals --test scenarios` (18 sur 18, avec vérification et refonte),
`--test docs`, `--test rpc_golden`, `penelope-cli`, `penelope-archtest` (seul le plafond
du daemon est rouge).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements : vérification et projecteur (#208, T12, T13)

- **`penelope history verify [--session <id>]`** dérive chaque conversation de son
  journal (événements `conv.*`, préfixe d'avant le journal scellé, mère d'un fork) et la
  compare à ses tables : nombre et ordre des messages, contenu de chacun, drapeau de
  compaction, contextes figés, résumés actifs, empreinte du préfixe scellé. Rapport JSON ;
  code de sortie non nul dès la première divergence, qui nomme la session, le nœud et la
  ligne. L'archive d'un `/rewind` est vérifiée contre ce que la coupe a retiré. Méthode
  RPC `history.verify` ; `penelope doctor` vérifie les sessions de la semaine.
- **`penelope history reindex [--session <id>]`** efface les lignes de cache non scellées
  (messages et plein texte, contextes figés, résumés) et les réécrit depuis le journal,
  aux mêmes numéros ; une session que le journal ne sait pas refaire est laissée intacte
  et nommée. Méthode RPC `history.reindex`.
- **Les caches se rattrapent seuls** : à l'ouverture de chaque tour, ce qu'une écriture
  interrompue a laissé derrière le journal est refait depuis le filigrane
  (`projections_session`) ; un rattrapage en échec ne fait pas échouer le tour, il
  apparaît dans `doctor`. Une nouvelle version du pliage refond chaque session à son
  prochain tour.
- Corrigé : dans une session scellée, un message écrit après le scellement recevait une
  adresse de journal fausse (offset lu au mauvais endroit), et après un `/rewind` le
  message suivant héritait du contexte figé du message retiré.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.
```

## Blocages

Aucun. Pour T14 : l'ordre des clés des arguments d'appel dans le journal, les contextes
hérités d'une fille de fork, l'archive de `/rewind` en fork par référence,
`prompt_snapshots` hors projecteur (voir les choix).
