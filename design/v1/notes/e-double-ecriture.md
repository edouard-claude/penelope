# Notes de livraison : lot E, double écriture du journal (épopée #208, T4, T5, T6)

Branche `v1-e-double-ecriture`, rebasée par l'intégrateur sur `v1` à `f2fa148` (lot G), poussée sur `origin`. Les
trois tâches sont livrées, chacune dans son commit, vertes seules ; les déplacements
qu'elles demandaient sont dans des commits séparés, sans changement de corps.

## Commits

| Commit | Quoi |
|---|---|
| `Moteur : le schéma et la lecture de la classification…` | déplacement (engine/classify.rs) |
| `Migrations : les tests sortent…` | déplacement (migrations/tests.rs) |
| `Historique : les tests de store.rs sortent…` | déplacement (store/tests.rs) |
| `Conversation : les tests sortent…` | déplacement (conversation/tests.rs) |
| `Migrations : 0011 à 0019 sortent…` | déplacement (migrations/since_0011.rs), `UPDATE_BUDGET` |
| `Moteur : conversation_aliases sort…` | déplacement (engine/aliases.rs) |
| `Journal : chaque tour est fermé…` | T4 |
| `Journal : chaque message est écrit en double…` | T5 |
| `Journal : le préfixe système et le contexte figé…` | T6 |
| `Journal : la forme des bornes de tour passe dans penelope_context::journal` | à la demande du lead, `UPDATE_BUDGET` |

## Ce qui est livré

- **T4.** `agent/turn_log.rs` borne la boucle : `turn.started` (avec `turn_id`,
  `origin_turn`, `kind`, `attempt` pour un tour de la file) et `turn.finished` sur toutes
  les sorties, `reason` compris, même quand la boucle remonte une erreur. Un tour tombé
  avant sa boucle est ouvert et fermé par `run_turn` (`close_unopened`, drapeau
  `TurnMeta::opened`). `run_conversation` garde sa signature (workflows, sous-agents) ;
  le moteur appelle `run_conversation_as`. Un test par variante de `TurnOutcome`
  (`agent/tests/turn_bounds.rs`).
- **T5.** Migration `0020_history_journal` (`messages.event_id`, `messages.sealed`,
  `lcm_nodes.event_id`, index partiel sur `turn_message_id` des `conv.user`).
  `HistoryStore::with_events` : `append`, `append_as`, `append_queued` écrivent l'événement
  par `append_with`, puis la ligne avec `event_id` (`store/dual.rs`). La traduction est
  pure (`journal/build.rs`, `Provenance`, `AssistantPayload::of_response`). La boucle passe
  la provenance par `Conversation::record_as` (méthode par défaut : les autres
  transcripts ne changent pas).
- **T6.** `HistoryStore::journal_system` (appelé en fin de `stable_prefix`) et
  `freeze_context` journalisé en `conv.context`.

## Les choix

- **Tous les écrivains passent par `HistoryStore`.** Telegram, workflows, planificateur,
  jobs d'outils écrivent des messages sans être touchés : la double écriture est dans le
  magasin, qui tient l'EventLog comme `BudgetLedger::with_events`. **Exception de
  périmètre accordée par le lead** : `runtime.rs` attache le journal
  (`.with_events(events.clone())`) dans `bootstrap` et `for_tests`, où `let events`
  remonte avant `ContextEngine` ; rien d'autre dans le fichier.
- **La forme des bornes de tour est pure** (`penelope_context::journal::turn` :
  `TurnReason`, `TurnEnd`, `TurnIdentity`, `started_payload`, `finished_payload`) ; le
  daemon ne garde que la traduction de `TurnOutcome` et l'écriture.
- **Idempotence par le journal (§2.7).** `append_queued` cherche la ligne, puis
  l'événement : un crash entre les deux transactions ne réécrit que la ligne, sous
  l'`event_id` existant. Une course entre deux écrivains du même message (impossible
  aujourd'hui : un tour par session) donnerait un `conv.user` en double, la ligne restant
  unique.
- **Un message système dans l'historique n'est pas journalisé** (`message_event` rend
  `None`) : le §2.2 n'a pas de kind pour lui, le préfixe a `conv.system`.
- **`conv.system` écrit en fin de `stable_prefix`, pas dans la boucle** : le préfixe y
  est définitif pour le tour, et les transcripts en mémoire (sous-agents, workflows) n'en
  écrivent pas, puisque leurs messages ne sont pas dans `messages`. La raison
  `compaction` se lit à un `context.compacted` postérieur au dernier `conv.system` ; un
  résumé publié en fin de tour se voit donc au tour suivant, celui où le préfixe change.
- **`conv.context.target`** : le `seq` de l'événement `conv.user`, adresse du §2.3 pour une
  session sans héritage ; une ligne V0 sans événement garde son `seq` V0. L'`offset` d'un
  fork ou d'un scellement n'est pas encore ajouté (il n'existe pas avant T8/T10).
- **Ce qui manque au payload `conv.assistant`** : `llm_request_id` (la réponse du
  fournisseur ne le porte pas jusqu'à la boucle) et `projection.steps` (les niveaux 0, 2,
  4 ne remontent pas à la boucle). `conv.tool_result` n'a ni `turn` ni `step` : le
  pipeline qui l'écrit ne les connaît pas. À reprendre quand la boucle sera découpée.

- **Déplacements de tests** : le corps est inchangé, à une nuance près : les
  littéraux SQL sur plusieurs lignes des tests de `migrations.rs` et de `store.rs`
  perdent quatre espaces d'indentation avec leur module (sans effet sur le SQL).

## Plafond du daemon

Le lot fait grossir `penelope-daemon/src` de **570 lignes** (84 249 à 84 819 sur la base
`f2fa148`), dont environ 350 de tests (`agent/tests/turn_bounds.rs`,
`engine/tests/journal.rs`, le test chaud/froid de `cache_audit`). `[crates]` n'est pas
relevé : `crates_stay_under_their_ceiling` est rouge sur la branche, et lui seul, le lead
pose le plafond à la mesure à l'intégration de la vague. Les fichiers de la liste de
référence touchés ont été découpés, jamais relevés : `engine.rs` 1 179 → 1 106,
`store.rs` 1 379 → 1 064, `conversation.rs` et `migrations.rs` sortent de la liste.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast` : verts (56 suites), sauf `crates_stay_under_their_ceiling`, rouge toléré (ci-dessus). `cargo test -p penelope-evals --test
scenarios` : 18 sur 18 ; aucun `surface.jsonl` ne bouge ; chaque `expected.jsonl` est
égal à sa base une fois retirés les kinds ajoutés par la tâche, `seq` et compte de l'audit
ignorés (script de comparaison), à une exception attendue : le nombre d'événements purgés
du scénario `purge` (9 → 16). `migration_from_0_17` vert.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal d'événements, double écriture (#208, T4, T5, T6)

- **Chaque tour est fermé** : `turn.finished` est écrit sur toutes les sorties de la
  boucle avec `reason` (`answered`, `awaiting_approval`, `cancelled`, `failed`,
  `budget_exceeded`, `loop_aborted`, `calls_exhausted`), y compris un tour tombé avant
  d'appeler le modèle. `turn.started` et `turn.finished` portent `turn_id`,
  `origin_turn`, `kind` et `attempt`. Un `turn.started` sans fin désigne désormais un
  arrêt du processus, et rien d'autre.
- **Double écriture de l'historique** : chaque message écrit dans `messages` l'est aussi
  dans le journal (`conv.user`, `conv.assistant`, `conv.tool_result`), l'événement
  d'abord, la ligne ensuite avec son `event_id` (migration 0020). La réponse du modèle
  porte son modèle, son usage, son coût et les empreintes du prompt. Un message de la
  file rejoué après un crash n'est ni réécrit ni rejournalisé.
- **Le prompt système et le contexte figé entrent au journal** : `conv.system` porte le
  texte entier du préfixe, une fois par changement (`first`, `cold`, `compaction`) ;
  `conv.context` le bloc volatil figé avec son message.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.
```

## Blocages

Aucun.
