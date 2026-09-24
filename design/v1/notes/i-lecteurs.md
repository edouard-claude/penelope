# Notes de livraison : lot I, lecture incrémentale, lecteurs secondaires, purge (épopée #208, T15, T17)

Branche `v1-i-lecteurs`, dérivée de `v1` à `c5ecc92` (1.0.0-alpha.7), poussée sur
`origin`. Aucune migration.

## Commits

| Commit | Quoi |
|---|---|
| `b09dd51` | performance : lecture du journal incrémentale |
| `bbec605` | T15 : `audit show` replié jusqu'à l'appel, export avec la surface |
| `bb096be` | déplacement : les tests de `purge.rs` sortent dans `purge/tests.rs` (`purge.rs` quitte `[files.oversized]`) |
| `b998bd2` | T17 : purge et rétention |
| (ce commit) | golden `audit.show`, ces notes |

## Ce qui est livré

- **Lecture incrémentale** (`penelope-context/src/read/cache.rs`). Le pliage devient
  reprenable (`derive::Folding`, `Fold::resume`/`pause`) : reprendre sur les événements
  suivants donne la surface du pliage complet. `HistoryStore.reads` garde, par session
  (64 au plus, la moins récemment lue sort), la surface pliée, le préfixe hérité, les
  propriétaires d'adresses et le `seq` du dernier événement plié ; une lecture ne relit
  que `seq > last_seq` (`session_events_after`). Remplacements, coupes, héritages sont
  des événements pliés comme les autres : rien à invalider. Seule la purge change le
  journal en place : toute nouvelle ligne d'`event_purges` jette l'entrée. Le préfixe
  d'une fille s'arrête à `up_to`, ce que la mère écrit ensuite ne le touche pas. Le
  verrou n'est pas tenu pendant le pliage (entrée retirée puis remise) ; un pliage en
  erreur n'est pas gardé.
- **Mesure** (compilation de test, `a_read_after_a_new_exchange_folds_only_what_is_new`,
  projection puis queue, session de 2 000 messages et fille de fork) : avant, 115 ms à
  chaque requête (deux pliages complets) ; après, 66 ms à la première lecture, puis 8 ms
  (0 événement plié sans rien de neuf, 4 après un échange). Les 8 ms restants sont
  linéaires mais sans SQL ni JSON : décompte des lignes sans événement, clones des
  entrées, numérotation.
- **T15, `audit show`** : `HistoryStore::call_view(session, request_hash)` retrouve le
  `conv.assistant` qui porte l'empreinte de la requête (la même que la ligne d'`usage`)
  et replie la lignée jusqu'à l'adresse d'avant ; le transcript en sort (résumés de
  l'époque, contextes figés, messages masqués depuis en clair), `exact: true` si le
  nombre de messages de la requête retombe sur `msg_count`, réserve sinon. Sans réponse
  journalisée (appel antérieur au journal, en échec), la lecture d'avant.
- **T15, export** : les lignes `message` portent `sealed` (préfixe scellé), et la surface
  dérivée suit en lignes `surface` (ce que voit le modèle), après le journal.
- **T17, purge** : `forks` et `avertissement` dans le rapport (et au journal du daemon)
  quand des sessions sont nées d'un fork de la session purgée ; les instantanés de prompt
  cités par ses `conv.system` partent, sauf ceux qu'une autre session cite (l'orphelin
  signalé par `i-bascule.md`).
- **T17, rétention** : `purge_attempts` remplace le payload des `conv.attempt` plus vieux
  que `retention.days`, hash d'origine dans `event_purges` (raison `retention`), clé
  `conv_attempts` du rapport. Une tentative purgée ne rend pas le pliage indulgent
  (`fold.rs`, elle ne porte aucun nœud).

## Critères de fin

- Performance : chiffres ci-dessus ; `a_resumed_read_matches_a_full_fold_at_every_step`
  (surface reprise = pliage complet = tables après échanges, niveau 1, résumé,
  prolongation, retour arrière ; purge) ; scénarios verts (dont
  `both_history_sources_send_the_same_requests`), aucun `surface.jsonl` modifié.
- T15 : `a_turn_is_replayed_from_its_fingerprint` vert ;
  `a_turn_is_replayed_exactly_after_a_compaction` (même transcript avant et après la
  compaction, `exact: true`, le résumé d'aujourd'hui absent) ;
  `export_writes_jsonl_and_rebuild_restores_search` vert et étendu (lignes `surface`,
  événement `conv.user`, `sealed`).
- T17 : `purging_a_session_leaves_the_audit_chain_and_nothing_else` vérifie que les
  `conv.*` portent le mot avant et plus après (`word_is_gone` lit déjà `events.payload`),
  puis `reindex` de la session purgée : vide, `verify` ok ;
  `purging_a_forked_session_warns_about_its_forks` ;
  `retention_purges_old_attempts_by_payload` (ancienne purgée, récente gardée,
  idempotent, `audit verify` ok, `history verify` ok, surface intacte).

## Les choix

- **Outils `history_*` et relecture d'épisode inchangés** : le §4.3 les fait lire « les
  caches alimentés par le projecteur », que `verify` compare au journal ; `history_grep`
  a besoin de l'index plein texte, qui est un cache.
- **Golden `audit.show`** : l'appel du contrat RPC n'a plus de réserve (il se replie
  exactement), la clé optionnelle `reserves` sort de la forme figée (`UPDATE_GOLDEN`).
- **Avertissement de purge** : dans le rapport et au journal du daemon. Demander une
  confirmation à `penelope purge` avant d'agir touche `penelope-cli`, hors périmètre.

## Reste et risques

- Le cache vit par processus : une autre instance de `HistoryStore` sur la même base
  (CLI hors daemon) relit le journal pour les nouveaux événements, donc reste juste ; un
  journal modifié en place autrement que par une purge ne serait pas vu (la chaîne de
  hachage l'interdit).
- Les tentatives purgées d'une session vivante apparaissent en `purged` dans `audit
  verify` : à dire dans la décision 0011 (§ risques de la spécification).

## Plafond du daemon

`crates_stay_under_their_ceiling` rouge (54 155 pour 53 836) : rouge toléré, dont les
tests d'audit, d'export et de purge. `purge.rs` descend de 1 242 à 574 lignes et quitte
la liste de dette. Aucun fichier nouveau au-dessus de 700 lignes (`purge/tests.rs`).

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
propres. `cargo test --workspace --exclude penelope-evals --no-fail-fast` : vert sauf le
plafond du daemon ; `penelope-evals` en deux passes (`--test scenarios` : 19 sur 19 ; le
reste, dont `rpc_golden` après `UPDATE_GOLDEN` et `docs`) : vert.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements : lecture incrémentale, audit exact, purge (#208, T15, T17)

- **Lecture incrémentale** : une requête ne replie plus tout le journal de la session et
  de ses ancêtres, seulement les événements arrivés depuis la précédente (8 ms au lieu de
  115 ms pour une session de 2 000 messages en compilation de test). Une purge fait tout
  relire.
- **`penelope audit show` exact après une compaction** : la requête d'un tour passé est
  repliée depuis le journal jusqu'à l'appel (résumés de l'époque, messages résumés depuis
  en clair) au lieu d'être relue dans les lignes d'aujourd'hui.
- **Export** : le préfixe scellé est marqué (`sealed`) et la conversation telle que le
  modèle la voit suit en lignes `surface`.
- **Purge** : le rapport nomme les sessions nées d'un fork de la session purgée, qui
  perdent le préfixe hérité ; les prompts système journalisés par la session partent
  avec elle. **Rétention** : le texte partiel des tentatives d'appel (`conv.attempt`)
  est purgé après `retention.days`, la chaîne d'audit reste vérifiable.
```

## Blocages

Aucun.
