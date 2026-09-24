# Notes de livraison : lot E suite, tentatives et reprise après crash (épopée #208, T9, T20)

Branche `v1-i-tentatives`, dérivée de `v1` à `fbf1fe5` (1.0.0-alpha.4), poussée sur
`origin`. Deux commits de code, un par tâche, chacun vert seul, plus ce fichier. Aucune
migration, aucun déplacement de code.

## Commits

| Commit | Tâche | Quoi |
|---|---|---|
| `76c7498` | T9 (#206) | `conv.attempt` pour un flux coupé, une erreur d'avant flux, un repli, une réponse vide |
| `082fa5c` | T20 | `turn.finished {reason: interrupted}` au démarrage pour un tour laissé ouvert |

## Ce qui est livré

- **T9.** `agent/attempts.rs` : `Attempts` (étape, consigne de relance en vigueur,
  compte), `Partial` (texte et raisonnement reçus d'un flux), `failure_cause`,
  `stream_cut_message`, `EMPTY_RETRY_PROMPT`, `MAX_ATTEMPTS_PER_TURN = 10`.
  `call_model` et `answer_after_loop` prennent `&Attempts` ; `run_steps` en crée un par
  exécution de la boucle. Côté forme (`penelope_context::journal`) :
  `AttemptPayload::new`, `AttemptPayload::of_response`, `AttemptCause::as_str`.
- **T20.** `agent::close_interrupted_turns` (dans `agent/turn_log.rs`), appelé par une
  ligne de `Daemon::recover` juste après `TurnQueue::recover_on_boot` ; forme pure
  `journal::interrupted_payload` (l'identité du `turn.started`, rien d'autre).
  `turn.started.attempt` existait depuis T4 (`TurnMeta::of` lit `turn.attempts`) : le
  tour rejoué porte déjà la tentative suivante, le test le vérifie.
- `penelope logs --turn` : chaque tentative est aussi une ligne de journal
  « tentative sans réponse » (`turn`, `session`, `step`, `cause`, `model`, `provider`,
  `upstream`, `error`, `text` = les 200 premiers caractères), dans le span `turn` du
  runner ; la commande la relit sans changement de code (filtre par span ou par champ).

## Les choix

- **Cause d'une tentative échouée** : `fallback` quand la suite décidée par `RetryPlan`
  passe au modèle suivant, quelle que soit la phase ; sinon `before_stream` (erreur avant
  le flux) ou `stream_cut` (erreur pendant le flux, avant ou après du texte). Une erreur
  de dépassement de fenêtre est une tentative `before_stream` (scénario
  `depassement-prouve`).
- **La consigne de relance suit le pliage.** Le pliage efface la consigne au premier
  `conv.assistant` ; la boucle le fait désormais aussi (`set_retry_prompt(None)` après
  une réponse écrite). Avant, `empty_retry` restant vrai, la consigne repartait sur
  chaque appel suivant du tour quand la relance rendait des appels d'outils : la
  requête envoyée divergeait de l'historique. Une tentative d'avant flux pendant la
  relance porte aussi `retry_prompt` (§2.2 : « si la requête suivante l'ajoute ») : sans
  cela, le pliage l'aurait perdue alors que la boucle l'envoie encore.
- **Test d'égalité, phase 1** : `the_retry_prompt_sent_is_the_one_the_journal_derives`
  compare la requête réellement envoyée après une réponse vide à
  `derive_until(journal, seq de la tentative).request_messages()` : égales message par
  message, système et contexte figé compris.
- **Aucune ligne d'usage** n'est écrite par une tentative : une réponse vide est déjà
  comptée par `budget.record`, son `conv.attempt` reprend l'usage et le coût pour la
  relecture ; une tentative échouée n'a pas d'usage connu.
- **Rédaction** (#134) : `partial_text`, `partial_reasoning` et `error` passent par
  `penelope_observe::redact`, comme la citation faite au propriétaire.
- **Plafond** : compté par exécution de la boucle (une reprise après approbation repart
  de zéro). Au-delà de dix, l'événement n'est plus écrit, la ligne de journal reste, et
  un avertissement unique le dit.
- **Purge** : les `conv.attempt` sont des événements de session, effacés par
  `purge_session` comme les autres (vérifié dans le test du flux coupé). Pas de table
  `turn_attempts` : le §2.2 a tranché pour le journal.
- **Tour ouvert** = la dernière borne (`turn.started` ou `turn.finished`) d'une session
  est un `turn.started` non purgé. Une requête `GROUP BY session_id` avec `max(id)` sur
  les deux kinds, au démarrage seulement. Idempotent : dix redémarrages n'écrivent
  qu'une fermeture (`redemarrages-en-serie`).
- **Écart de périmètre** : un test d'intégration `crates/penelope-cli/tests/logs_turn.rs`
  (binaire à part, comme `penelope-daemon/tests/log_spans.rs`, pour que l'abonné JSON
  soit seul) lance un vrai tour au flux coupé et relit ses lignes par
  `penelope logs --turn` ; il demande `penelope-llm` en dépendance de développement de
  la CLI (raison écrite dans `Cargo.toml`). `commands.rs`, dans la liste de dette, n'est
  pas touché.

## Scénarios

Aucun `surface.jsonl` ne bouge. Les `expected.jsonl` régénérés par `UPDATE_SCENARIOS=1` :

- `reponse-vide-relancee`, `depassement-prouve` : un `conv.attempt` ajouté (seq et
  compte de l'audit décalés d'un) ;
- `flux-coupe` : un `conv.attempt stream_cut` ajouté, et le texte d'erreur change, **parce
  que T9 le demande** (« message au propriétaire qui cite le début conservé ») : l'outcome,
  le `turn.finished` et la ligne `turn_queue` citent maintenant « Le plan de migration se
  déroule en trois » ;
- `crash-deux-vies`, `redemarrages-en-serie` (T20) : un `turn.finished interrupted`.

## Critères de fin

- T9 : flux coupé après 200 caractères → tour en échec, `conv.attempt` en base,
  requête du tour suivant sans le partiel et `derive` identique avec et sans la
  tentative (`a_stream_cut_after_text_keeps_the_partial_out_of_the_history`) ; `/stop`
  inchangé (branche non touchée, `a_stop_closes_the_turn_as_cancelled` vert) ; trois
  échecs d'avant flux → trois tentatives, une ligne d'usage
  (`failures_before_the_stream_leave_one_attempt_each_and_one_usage_line`) ; plafond
  (`a_turn_keeps_at_most_ten_attempts`) ; `a_stream_cut_before_any_text_is_retried_then_falls_back`
  et `an_empty_answer_is_retried_once_then_reported` verts ; `logs_of_a_turn_show_its_attempts`.
- T20 : `a_turn_open_at_the_crash_is_closed_as_interrupted_then_replayed` (un
  `turn.started` sans fin et un `conv.assistant` à appel sans résultat → une fermeture
  `interrupted` après deux redémarrages, tour rejoué en `attempt` 2 qui résout l'appel
  `c1` sans réécrire le message) ; `an_uncertain_effect_marked_done_is_replayed_not_rerun`
  et voisins verts.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
propres. `cargo test --workspace --no-fail-fast` : 58 suites, toutes vertes sauf
`crates_stay_under_their_ceiling`, rouge toléré : `penelope-daemon/src` mesure 85 447
lignes pour un plafond de 84 819 (+628 net sur la base, dont environ 420 de tests :
`agent/tests/attempts.rs`, `agent/tests/recovery.rs`). `budget.toml` n'est pas touché :
aucun fichier de la liste de dette ne change, `call_model` reste sous 200 lignes sans
`allow`. `cargo test -p penelope-evals --test docs` et `--test scenarios` : verts.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Tentatives hors surface et reprise après crash (#208, T9, T20, #206)

- **Un flux coupé en cours d'écriture n'est plus perdu** : le début reçu est gardé dans
  le journal (`conv.attempt`, cause `stream_cut`), hors de l'historique ; le message au
  propriétaire le cite au lieu de parler d'un début « affiché ». Le tour suivant ne le
  renvoie pas au modèle.
- **Chaque appel sans réponse est relisible** : erreur avant le flux (`before_stream`),
  passage au modèle de repli (`fallback`), réponse vide relancée (`empty_answer`, avec
  la consigne de relance, l'usage et le coût déjà comptés : aucune seconde ligne
  d'usage). Dix tentatives gardées par tour au plus ; toutes sont dans
  `penelope logs --turn` (« tentative sans réponse » : modèle, cause, début du texte).
- La consigne de relance après une réponse vide n'accompagne plus que la requête qui
  suit : elle n'était auparavant retirée qu'en fin de tour.
- **Reprise après crash** : au démarrage, un tour que l'arrêt du processus a laissé
  ouvert est fermé `turn.finished {reason: interrupted}` ; le tour rejoué ouvre sa
  propre borne avec la tentative suivante et reprend l'appel resté sans résultat.
```

## Blocages

Aucun. Reste à l'intégrateur : le plafond du daemon (`[crates]`).
