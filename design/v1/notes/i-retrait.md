# Notes de livraison : lot I, retrait du chemin direct et décision 0017 (épopée #208, T16, T19)

Branche `v1-i-retrait`, dérivée de `v1` à `64b8e3d` (1.0.0-alpha.16), poussée sur
`origin`. Aucune migration.

## Commits

| Commit | Quoi |
|---|---|
| `2ddb8ff` | T16 : retrait du chemin direct, journal obligatoire, clés retirées, règle d'architecture |
| `c5bdd7a` | T16 : la fixture `config-0.17.toml` perd `[history]` (vue par la suite complète) |
| `d887dee` | T19 : décision 0017, documentation, ces notes |

## Ce qui est livré

### T16

- **Le journal est obligatoire.** `HistoryStore::new(store, clock, events)` ;
  `with_events`, `journals()` et toutes les branches « sans journal » disparaissent.
  `journaled(session, event, write)` prend un événement, jamais une option : c'est le seul
  chemin d'écriture des caches. Un message système appendu est refusé (le préfixe a son
  `conv.system`) ; externaliser une ligne qui n'est pas un résultat d'outil est une
  erreur (avant : réécrite sans événement).
- **Méthodes publiques retirées** : `copy_messages`, `truncate_from`, `externalise`,
  `journal_fork`, `append_user_turn_at`. `mark_compacted` devient `pub(crate)` (publication
  d'un résumé déjà en base, réparation d'un marquage). Les écritures de `Lcm`
  (`insert_leaf`, `extend`, `supersede`, `insert_condensed`) sont réservées aux tests.
  Restent publiques, toutes journalisées : `append`, `append_as`, `append_queued`,
  `freeze_context`, `journal_system`, `fork`, `rewind_from`, `externalise_as`,
  `seal_legacy`, `reindex`, `catch_up`, `prompt_sent`, et `ContextEngine::apply_summary*`.
- **Fork et archive par le projecteur.** `HistoryStore::fork(child, parent)` écrit
  `conv.fork` puis projette la fille dans la seconde transaction (`projector::project_in`,
  la refonte) : lignes héritées, résumés actifs recopiés, et lignes scellées de la mère
  recopiées octet pour octet avec leur drapeau (`copy_sealed_row` : la refonte savait
  garder une ligne scellée, pas la créer). `rewind_from` projette l'archive depuis le
  `conv.rewind` de la mère (`archive_expected`, qui existait pour `verify`) puis coupe ;
  il rend `Rewound { removed, archived }`. `penelope-ops` n'appelle plus que ces deux
  méthodes.
- **Lecture toujours dans le journal.** `ContextEngine::projected_entries(session)` et
  `tail(session, limit)` : plus de paramètre de source, plus de comparaison en mode
  `tables`, plus de `ReadError`. Repli sur les caches pour une session que le journal ne
  sait pas redonner (archive, lignes V0 non scellées) ou qui ne se plie pas (erreur au
  journal du daemon).
- **Clé `history.source` retirée.** `RETIRED_KEYS` et `config::retired` dans
  `penelope-kernel` : `Config::parse` retire une clé retirée de la liste des inconnues et
  l'avertit avec sa raison. Test `a_file_with_the_retired_history_source_still_loads`
  (parse et `ConfigStore::load_or_create`, `tables` comme `journal`). Référence des clés
  et `config.get` doré régénérés.
- **`kv turn.recorded.*` → `HistoryStore::recorded(session, turn_id)`.** Le message d'un
  tour entre au journal sous l'identifiant du tour par `append_queued` quel que soit son
  genre (message, déclencheur, relance, photo) : avant, seuls les messages de la file
  étaient idempotents par eux-mêmes, les autres l'étaient par la clé. La frontière
  d'épisode et la photo se décident sur `recorded`.
- **`kv prompt.prefix.*` → `HistoryStore::retained_prefix(session)`.** Le préfixe (T0, T1,
  T2) du dernier `conv.system` de la session, relu par ses tuiles, sauf si un événement de
  `PREFIX_RELEASES` (`context.compacted`, `session.project`) est arrivé depuis le dernier
  `conv.system`, `conv.assistant` ou `conv.attempt`. Même comportement que la clé, que
  `refresh_snapshot` effaçait à la compaction et au changement de projet :
  `session_project::set` journalise désormais `session.project`.
- **`EPHEMERAL_KEYS`** perd `prompt.prefix.` (`turn.` reste : `turn.burst_card.*`,
  `turn.intents.*`) ; la rétention efface `prompt.prefix.*` et `turn.recorded.*` quel que
  soit leur âge (`RETIRED_KEYS` de `purge.rs`).
- **Écritures déplacées dans `penelope-context`** : purge des caches d'une session
  (`HistoryStore::purge_session_in`, dans la transaction de `penelope-ops`), rétention
  des instantanés (`retire_prompts_in`), instantané envoyé (`prompt_sent`, appelé par
  `prompt_snapshot::record` du daemon). `penelope-ops` passe `penelope-context` de
  `[dev-dependencies]` à `[dependencies]` et `OPS_ALLOWED_DEPS` la nomme.
- **Règle d'architecture** (`penelope-archtest/src/caches.rs`) : hors de
  `penelope-context`, un `INSERT` (`OR …` compris), `REPLACE`, `UPDATE` ou `DELETE` qui
  nomme `messages`, `messages_fts`, `message_context`, `lcm_nodes`, `lcm_edges`,
  `prompt_snapshots` ou `projections_session` échoue
  (`only_the_context_crate_writes_the_conversation_caches`), requête sur plusieurs lignes
  comprise ; commentaires et tests exceptés. Seule exemption nommée :
  `penelope-evals/src/scenario/harness/journal.rs` (base temporaire, CA 4.6). Garde-fou
  `a_cache_write_outside_the_context_crate_is_caught` (cinq écritures vues, un `SELECT`,
  une table voisine, un commentaire et un bloc de tests ignorés).

### T19

- `docs/decisions/0017-journal-source-unique.md` (format de 0015 : contexte, décision,
  raisons, conséquences, alternatives), à partir du brouillon d'`i-ca.md` ; cite #205,
  #206 et `design/v1/source-de-verite.md`.
- `docs/context.md` : section « Le journal ». `docs/README.md` et `docs/progress.md` :
  0017 indexée, la réserve du numéro retirée. `docs/install-headless.md` : `history
  verify` et `reindex` à jour (le journal seule source, l'archive refaite depuis sa mère),
  clés retirées, `prompt.prefix.*` sorti de la rétention. `docs/architecture.md` : section
  « Le journal, source unique », dépendances d'ops, ce qui reste.
  `docs/runtime-events.md` (deux phrases devenues fausses) : les tables ne sont plus la
  source de lecture ; `turn_message_id` pour tout message d'un tour.

## Les choix

- **Crochets du brouillon tranchés avec ce qui existe** : la règle d'archtest est posée
  (point 3), la clé est retirée (point 4) ; l'écart « `conv.attempt` d'une réponse vide
  sans `llm_request_id` » est comblé depuis le lot K (`07a6ecc`) et sort de la liste ; la
  purge nomme bien les forks avant confirmation (`session.purge_preview`, alpha.9).
- **`append` reste public** : c'est `append_as` sans provenance, journalisé ; les tests
  de toutes les crates s'en servent. `append_user_turn_at` disparaît (un seul chemin
  pour la file : `append_queued`).
- **Ce qui remplace `both_history_sources_send_the_same_requests`** : CA 4.5
  (`harness/visible.rs`, à la fin de chaque scénario, chaque requête reçue par le modèle
  comparée octet pour octet au pliage du journal d'avant sa réponse) et CA 5.5 (ce qui
  peut se réécrire dans un tour). Le test comparait deux lectures dont l'une n'existe plus.
  Côté unitaire, `the_journal_and_its_caches_read_the_same_bytes` compare la lecture au
  cache que le projecteur a écrit.
- **Tests qui fabriquent une base 0.17** : `append_legacy` et `freeze_legacy`
  (`store/legacy.rs`, `#[cfg(test)]`) au lieu d'un `HistoryStore` sans journal.
  Deux tests retirés parce que leur cas n'existe plus : `without_a_journal_the_row_is_written_alone`,
  `without_a_journal_nothing_is_journaled_and_the_node_has_no_event`.
- **Réparation du marquage** dans `prepare_summary_capped` gardée : interne, idempotente,
  elle ne sert plus qu'à un préfixe scellé d'une 0.17 arrêtée entre nœud et marquage.
- **`session.project`** plutôt qu'une clé : le changement de projet à la main est un
  fait, il libère le préfixe comme la compaction ; aucune autre lecture ne l'utilise.

## Reste et risques

- **Défaut trouvé, antérieur à T16** : un `/rewind` qui coupe dans le préfixe scellé
  retire des lignes que le `conv.import` compte ; la mère ne se replie plus (« préfixe
  fourni Import { messages: 6 }, le journal annonce Import { messages: 8 } »), sa lecture
  retombe sur ses caches avec une erreur au journal, et `verify` signale une divergence
  `journal`. La note « empreinte non vérifiable » de `verify` visait ce cas sans que le
  pliage le supporte. Cas réel : revenir en arrière de plusieurs échanges juste après la
  mise à jour, avant tout message nouveau. Le test
  `an_archive_cut_inside_the_sealed_prefix_keeps_its_sealed_rows` fige ce qui est juste
  (l'archive) et le dit. Piste : garder les lignes scellées coupées (le préfixe scellé
  est une vérité, pas un cache) et ne couper que la surface, ou interdire la coupe dans
  le préfixe scellé comme celle avant le dernier résumé.
- L'archive d'un `/rewind` n'a toujours pas de journal à elle (pas de fork par
  référence) : elle se lit dans ses caches, projetés depuis sa mère.
- Plafond du daemon : `penelope-daemon/src` descend (engine.rs 1 091 → 1 067,
  `UPDATE_BUDGET`) ; aucun fichier nouveau au-dessus de 120 lignes.

## Vérifications

Pendant le lot : `penelope-context` (159 tests), `penelope-kernel`, `penelope-archtest`,
`penelope-ops`, `penelope-conversation`, `penelope-vault`, `penelope-app`,
`penelope-daemon`, `penelope-agent`, `penelope-executor`, `penelope-orchestrator`,
`penelope-dream`, `penelope-cli`, `penelope-evals` (scénarios : 23 sur 23 ; aucun
`surface.jsonl` ni `expected.jsonl` modifié) ; `UPDATE_DOCS`, `UPDATE_GOLDEN`
(`config.get` seul), `UPDATE_BUDGET`. Fin de lot : `cargo fmt --all --check` et
`cargo clippy --workspace --all-targets -- -D warnings` propres ; `cargo test --workspace
--no-fail-fast` : 2 074 tests verts et un rouge,
`the_fixture_carries_every_key_of_the_reference` (la fixture portait encore `[history]`),
corrigé par `c5bdd7a` et relancé vert.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal : la seule source de la conversation (#208, T16, T19)

- **Plus aucune écriture de la conversation sans son événement** : messages, contextes
  figés, résumés, fork et retour arrière entrent au journal d'abord, et les tables
  `messages`, `message_context`, `lcm_nodes`, `prompt_snapshots` n'en sont que les
  caches, écrits par le moteur de contexte seul (un test d'architecture l'interdit
  ailleurs). Un `/fork` et l'archive d'un `/rewind` sont refaits depuis le journal, lignes
  scellées comprises.
- **La clé `history.source` est retirée** : la conversation se relit toujours depuis le
  journal. Un fichier de configuration qui la porte encore se charge, avec un
  avertissement ; la ligne peut être effacée.
- Les clés de travail `turn.recorded.*` et `prompt.prefix.*` disparaissent : le message
  d'un tour déjà écrit et le préfixe retenu se lisent dans le journal. La rétention efface
  celles qui restent. Un projet fixé à la main entre au journal (`session.project`).
- Décision [0017](decisions/0017-journal-source-unique.md) : le journal d'événements est
  la source unique de la conversation.
```

## Blocages

Aucun.
