# Boucle d'agent et pipeline d'outils : état, cible V1, découpage

Périmètre : un tour de conversation, de la file des tours à la livraison, et le chemin
d'un appel d'outil, de la décision à l'enregistrement. Sources : le code de la
0.17.58, `docs/progress.md`, `docs/decisions/`, les issues GitHub citées. Le PRD n'est pas
utilisé. Chaque affirmation cite `fichier:ligne` ; les chemins sont relatifs à
`crates/` sauf mention contraire. Les trois modèles de référence sont cités par leur
dépôt cloné (`dsh`, `openclaw`, `hermes`).

Compte réel : `penelope-daemon/src/agent.rs` fait 4 239 lignes, dont 2 427 lignes non
vides avant `mod tests` (ligne 2536) et 2 160 hors commentaires ; 42 tests (`#[test]` et
`#[tokio::test]`), dont 2 dans `clone_policy_tests` (2147-2205) et 40 dans `tests`
(2536-4239). Le lot annonçait 38 : la liste complète est en §6.

---

## 1. État actuel

### 1.1 Le chemin d'un tour

```text
 canal (Telegram, CLI, planificateur, relance)
   │  enqueue_message / _with_images / enqueue_retry / enqueue_resume
   │                                              engine.rs:147-240
   ▼
 turn_queue (pending)   TurnKind = Message | Trigger | Resume | Nudge   kernel/turn.rs:37-42
   │  claim : lease + jeton de clôture, fusion des messages `pending` de même origine
   │                                              kernel/turn.rs:194-341, origin_key :90-114
   ▼
 runner_loop ─► process ─► run_and_deliver       runner.rs:45-58, 65-74, 152-277
   │   bail battu dans une tâche séparée (StopBeat :37-43, :157-183) ; lease perdu → rien
   │   n'est livré (:222-237) ; panique → Failed, runner vivant (:186-205)
   ▼
 Daemon::run_turn                                 engine.rs:276-374
   │   bus.begin → jeton d'annulation (:293) ; après : retour d'usage, revue, titre,
   │   compaction de fond (:318-372)
   ▼
 Daemon::execute_turn                             engine.rs:376-645
   ├─ 0. frontière d'épisode, une fois par message         :404-414
   ├─ 1. message écrit UNE fois (kv `turn.recorded.<id>`)   :416-454 ; fusionnés :455-473
   ├─ 1 bis. tour d'origine d'une reprise (payload.turn_id) :476-492
   ├─ 2. modèle (épinglé, collant, classifieur) ‖ vecteur   :502-512 ; select_model :852-939
   ├─ 3. provider, périmètre Codex                          :533-537
   ├─ 4. prompt T0..T4, intentions, cache figé, résumé      :540-584
   ├─ 5. NativeToolExecutor + hooks (messenger, mcp, orch.) :587-604
   └─ AgentLoop::run_conversation(spec, conv, exec, sink)   :631-633
        │
        ▼  agent.rs:535-917  « for iteration in 0..24 » (TURN_CALLS :498)
        ├─ turn.started (hash du prompt, hash des outils)                    :555-570
        ├─ [annulé ?]                                                        :573
        ├─ 1. resolve_pending  (§1.2 ci-dessous)                             :1267-1669
        │     Pending::Nothing | Resolved → continuer
        │     Pending::Stop(outcome)     → retour (AwaitingApproval, Cancelled)
        │     Pending::Loop{..}          → answer_after_loop → LoopAborted   :1192-1264
        ├─ 2. budget jour/session/run, carte « +5 $ / +20 $ / Arrêter »      :596-669
        ├─ 2 bis. turn_limits : 24 appels par tour, palier de coût           :672-674, 1777-1862
        ├─ 3. request_messages (absorbe les messages arrivés, voir 1.3)     :677
        │     relance après réponse vide, message NON enregistré              :681-687
        │     empreinte, fournisseur amont collant                           :690-697
        │     call_model (§1.2)                                              :698
        │     fenêtre dépassée → compact_for_overflow, un seul essai         :700-715
        ├─ après : coût, instantané du prompt, cause de raté, usage.record   :723-781
        │     Cancelled → le partiel reste dans le transcript                :783-789
        │     refus explicite montré tel quel                                :792-798
        │     réponse vide : une relance, puis Failed explicite              :800-877
        │     conv.record(assistant)                                         :879
        └─ 4. sans appel d'outil → Answered (+ « Coût de ce tour »)          :882-910
              sinon → itération suivante (les appels viennent d'être écrits)
        ─ sortie de boucle : Failed « le tour n'a pas convergé »             :914-916
```

Après `run_conversation` : `turns.complete | fail | cancel_leased` (runner.rs:217-221),
livraison au canal (`deliver`, runner.rs:281-288 ; Telegram telegram.rs:6903-6995),
`bus.finish` réveille les attentes (bus.rs:189-208).

### 1.2 Le chemin d'un appel d'outil (`resolve_pending`, agent.rs:1267-1669)

```text
 tail (64 dernières entrées, conversation.rs:21, :329-339)
   │ pending_calls : appels du dernier message assistant sans résultat ; abandonnés si un
   │ message non-outil est arrivé depuis                              agent.rs:2386-2408
   ▼
 PHASE 1 · décisions, dans l'ordre des appels, sans rien exécuter     :1289-1552
   normalise_call  (`cd <ws> && x` → `{command: x, cwd}`, #123)        :1291-1293 ; executor.rs:2114-2128
   describe_call   → CallInfo { effective_name, risk, idempotent, policy } :1294 ; executor.rs:2151-2197
   liste blanche   (étape / skill / sous-agent)                        :1297-1301 ; tools/lib.rs:120-127
   décision antérieure (approvals.find_for_call par call_id)           :1304-1343
        Pending  → Terminal::Stop(AwaitingApproval)  (arrête la liste)
        Denied/Expired → Step::Record(« Non exécuté : … »)
        Approved → detector.observe, puis Execute sans redemander
   garde de boucle (arguments sans `pourquoi`, #116)                   :1347-1364 ; tools/loops.rs:82-108
        Warn → Step::Record(« [avertissement du harnais] … »)
        Abort → Terminal::Loop
   precheck (schéma natif/MCP, balisage d'appel, #117)                 :1369-1387 ; executor.rs:1905-1951
        invalide → Record + observe_invalid (2 = avertit, 3 = arrête)
   politique, sur les arguments EFFECTIFS (`tool_call` → appel interne, #110)  :1392-1480
        1. règles (motif > outil > serveur > classe > défaut)          hitl/policy.rs:379-445 ; listes `&&` :451-517
        2. déclaration du serveur MCP (`tool_policy`) : Deny prime, sinon cède à une règle  :1410-1418
        3. autorisation déclarée (`workflow_plan`, `tools.shell_allow[_network]`)  :1420-1435 ; approval_mode.rs:85-130
        4. mode de session : ask (tout), reads (défaut), auto (sauf destructif)   :1436-1461 ; approval_mode.rs:50-61
        5. réseau demandé : la raison le dit                            :1463-1470
        6. `config_set` sensible → AskTwice, malgré toute règle          :1473-1480
        Deny → Record(« Refusé par la politique ») ; Ask/AskTwice → carte + Terminal::Stop  :1483-1539
        Auto → Execute { parallel: risk == Read && PARALLEL_SAFE }      :1543-1544, liste :374-400
 PHASE 2 · exécution, dans l'ordre des appels                          :1557-1612
   lectures pures consécutives : lots de PARALLEL_READS = 4 (join_all) ; tout autre
   appel est une barrière (#85)                                        :1575-1600, :369
   run_effect                                                          :1673-1728
        EffectSpec { kind, tool, args, session, step = call_id, run, idempotent }  :1681-1692
        effects.plan → Replayed(v) : jamais ré-exécuté              kernel/effects.rs:192-250
                     → NeedsDecision(id) : « décision du propriétaire requise »
                     → InFlight : « appel déjà en cours »
                     → Fresh : dispatching (CAS, fsync si non idempotent :285-309)
                                → execute_cancellable → complete | fail   :1712-1726
   finish_call : métrique, événement tool.result (forme de la ligne, #150), nudge de
   délégation (#19) collé au texte, record_result                       :1732-1773, :1888-1906
 TERMINAL                                                              :1614-1666
   Stop(outcome) → Pending::Stop
   Loop → turn.loop_aborted ; le dernier résultat réel + LOOP_STOP_NOTE deviennent le
          résultat de l'appel bloqué ; le reste « Non exécuté » ; Pending::Loop  (#31)
 admit_tool_results(n) : budget d'admission sur le GROUPE (#52)        :1667 ; conversation.rs:295-320
```

`call_model` (agent.rs:924-1187) :

```text
 candidats = [principal] + replis d'alias si le provider n'est pas OpenRouter   :933-942
 llm_state.plan (corps figé, trois clés d'empreinte, #205) → dispatching       :981-997
 chat_stream
   Err avant flux : retryable (Transient, RateLimited, UnknownModel ; router.rs:404-409)
        → 1 s, 2 s, 4 s ou Retry-After ≤ 20 s (#50), 1 seule attente s'il reste un repli,
          `request_retries` (3) sur le dernier candidat                       :1005-1055
        → sinon repli sur le candidat suivant (les alias sont ajoutés si le
          fournisseur lui-même est injoignable)                              :1056-1073
 response_started ; sink Model                                                :1076-1079
 collect_stream_observed (Delta/Reasoning/ToolCall vers le sink)              :1082-1101
   Ok  → completed ; llm.fallback_used si le modèle servi diffère             :1103-1127
   Err après du texte → pas de repli silencieux, Failed « coupée en cours d'écriture »  :1133-1143
   Err avant texte   → 1 nouvel essai (2 s ou Retry-After), puis repli client            :1148-1182
```

Le coût est celui facturé quand le provider l'expose, sinon estimé du catalogue et
marqué `estimated` (llm/provider.rs:1109-1119) ; il est attribué au tour d'origine
(`turn_id`), reprises comprises (agent.rs:756-781 ; engine.rs:476-492).

### 1.3 Invariants réellement tenus

| Invariant | Où il est tenu | Preuve |
|---|---|---|
| Ledger **avant** effet ; `completed` rejoué, jamais ré-exécuté | `run_effect` planifie puis exécute ; `dispatching` en CAS, durable si non idempotent (#75) | agent.rs:1694-1726 ; kernel/effects.rs:191-250, 285-325 ; tests `completed_effects_are_replayed_not_reexecuted`, `only_non_idempotent_effects_pay_a_durable_commit` ; CA 4.2, 17.2 |
| Effet incertain = question, jamais de relance seule, jamais de règle | `recover_on_boot` : `dispatching` → `unknown` ; `plan` → `NeedsDecision` ; une demande par effet ; « C'est fait / Relancer / Ignorer » passe au ledger avec fenêtre `Once` forcée | kernel/effects.rs:343-409 ; runtime.rs:488-525 ; agent.rs:2214-2272 ; tests `an_uncertain_effect_*` (3) ; CA 17.3 |
| Aucun effet avant approbation ; reprise sans redemander ; l'appel reste dans le transcript | la carte arrête la liste ; `find_for_call(call_id)` à la reprise ; `Resume` ne réécrit pas de message | agent.rs:1304-1343, 1490-1539 ; engine.rs:418 ; tests `write_tools_suspend_the_turn_for_approval`, `an_approved_call_runs_on_resume_without_asking_again` |
| Écriture unique du message utilisateur | kv `turn.recorded.<turn>` ; les fusionnés gardent leur id et leur heure | engine.rs:416-473 ; test engine `replaying_a_turn_does_not_duplicate_the_user_message`, `claimed_messages_keep_separate_user_entries_and_arrival_times` |
| Messages arrivés pendant le tour absorbés avant l'appel modèle suivant, sans parallélisme (#161) | `claim` fusionne ; `absorb_pending` sous bail (LeaseLost sinon) à chaque `request_messages` ; note système ; rafale → carte + annulation du tour | kernel/turn.rs:194-341, 345-404 ; conversation.rs:171-238, 86-100 ; tests engine `a_running_turn_absorbs_a_new_message_before_the_next_model_call`, runner `messages_arriving_during_tools_hit_the_burst_limit_before_another_model_call` |
| Budget vérifié **avant** chaque appel modèle ; une carte par plafond (#32) ; session du propriétaire suspendue, jour et runs arrêtés | `status` puis `find_for_call("budget:<scope>:<cents>")` | agent.rs:596-669 ; test `exceeded_budget_stops_before_calling_the_model` ; CA 10.4 |
| Plafond de 24 appels sur **tout** le tour, reprises comprises ; palier de coût (#19) ; « Continuer » plutôt que « Réessayer » (#139) | `turn_totals(turn_id)` lu dans `usage` ; `CALLS_EXHAUSTED` reconnu par Telegram | agent.rs:1777-1862, :502 ; telegram.rs:6066 ; tests `the_call_cap_spans_resumptions_and_suggests_delegating`, `a_costly_turn_asks_before_going_on` |
| Détecteur de boucles : 3 identiques ou A/B/A/B → avertissement, puis arrêt ; appels invalides comptés (#117) ; le tour répond quand même avec l'erreur réelle et des choix (#31) ; le tour suivant voit la note | fenêtre de 40, `warned` ; `answer_after_loop` avec `tool_choice: None` | tools/loops.rs:49-119 ; agent.rs:1347-1387, 1617-1665, 1192-1264 ; tests `loop_detector_aborts_the_turn`, `a_stopped_loop_still_answers_with_the_real_error_and_choices` |
| Replis modèle : tentatives bornées avant flux, repli côté serveur chez OpenRouter, jamais de repli silencieux après du texte (#5, #50) | `call_model` | agent.rs:1005-1074, 1129-1183 ; tests `a_transient_failure_falls_back_to_the_next_model`, `a_transient_error_before_the_stream_is_retried_with_openrouter`, `repeated_connection_timeouts_say_how_many_attempts_were_made`, `a_stream_cut_before_any_text_is_retried_then_falls_back` |
| Une erreur d'outil revient au modèle, une erreur de harnais au propriétaire | `ToolOutcome::error`, `for_model` ; `?` réservé au store | agent.rs:1717-1725 ; tools/error.rs:56-96 ; test `tool_errors_go_back_to_the_model` |
| La première décision gagne ; « Toujours » crée une règle visible et révocable, bornée au motif de l'appel (#67), une par famille d'une liste `&&` (#150), réseau par famille (#106), aucune sur une ligne composée (#111) ni sans arguments (#83) | `approvals.decide` en CAS ; `arg_patterns`/`arg_pattern` | hitl/lib.rs:342 ; agent.rs:2274-2384, 2002-2145 ; tests `a_second_decision_does_not_win`, `always_decision_creates_a_revocable_rule`, `an_always_rule_is_bounded_to_the_call_it_was_granted_for`, `network_is_granted_to_a_command_family_never_to_the_shell`, `a_request_without_arguments_never_creates_a_rule`, `a_quoted_query_url_is_not_chaining_and_its_family_applies` ; CA 9.1, 9.3 |
| Lectures pures en parallèle par 4, résultats dans l'ordre des appels, toute autre chose est une barrière (#85) ; `/stop` interrompt tout le lot (#57) | `PARALLEL_SAFE`, `join_all`, jeton passé à `execute_cancellable` | agent.rs:374-400, 1557-1612, 1714-1716 ; tests `reads_requested_together_run_together_and_keep_their_order`, `a_write_between_reads_is_a_barrier`, `stop_interrupts_a_whole_batch_of_reads` |
| Annulation lue avant chaque étape et pendant les attentes | `is_cancelled` :573, :593, :678, :1561 ; `sleep_unless_cancelled` :2434-2446 | tests `cancellation_stops_the_turn`, `a_stop_during_the_retry_wait_ends_the_turn` |
| Machine d'état des appels LLM : `planned → dispatching → response_started → completed/failed`, `send_unknown` au redémarrage (§4.3) | `llm_state.*` autour de `chat_stream` | agent.rs:981-997, 1076, 1104, 1130 ; llm/state.rs:167-260 ; CA 17.4 |
| Coût mesuré (facturé sinon estimé, marqué), empreinte et cause de raté du cache (#17), instantané du prompt (#205) | `usage.record` après chaque appel ; `prompt_snapshot::record` hors latence | agent.rs:723-781 ; cache_audit.rs:158-192 ; CA 5.4 |
| Résultats volumineux : niveau 1 à l'unité, admission de groupe après un lot (#52) | `record` puis `admit_tool_results` | conversation.rs:277-289, 295-320 ; test conversation `parallel_tool_results_are_admitted_as_one_group` |
| Arguments validés **avant** toute carte (#117) ; erreurs expliquées avec les paramètres attendus (#110) | `precheck` → `validate_call` → `explain` | agent.rs:1369-1387 ; executor.rs:1905-2052 ; test engine `invalid_calls_are_refused_before_any_approval_card` |
| La carte dit l'intention, pas la politique (#116) ; `pourquoi` retiré avant l'exécution MCP et la garde de boucle | `call_intention`, `turn_goal`, `without_intention` | agent.rs:1496-1499, 1929-1968 ; executor.rs:1820 |
| Bail tenu pendant les outils longs ; un runner évincé ne livre rien (#43) ; une panique n'enferme ni le runner ni la session (#84) | battement séparé, `catch_unwind` | runner.rs:152-277 ; tests runner `a_runner_that_lost_its_lease_delivers_nothing`, `a_panicking_turn_neither_kills_its_runner_nor_locks_its_session` ; CA 3.3, 17.7 |

### 1.4 Ce qui est implicite ou enfoui dans `agent.rs`

1. **La politique est une suite de `if` qui réécrivent `verdict`** (agent.rs:1410-1480) : six
   couches (règle, déclaration MCP, autorisation déclarée, mode de session, réseau,
   `config_set`) sans nom ni type ; seule la chaîne `verdict.reason` en garde la trace, et
   c'est cette chaîne que la carte affiche et que les tests lisent.
2. **Trois enums privées pour une machine d'état** : `Step`, `Terminal`, `Pending`
   (agent.rs:402-436). Une carte d'approbation est un `break` dans la boucle de décision
   (:1538) : les appels qui suivent sont différés à la reprise sans que rien ne le dise
   au modèle ni au propriétaire (commentaire :1284-1286).
3. **`resolve_pending` fait deux phases en une fonction de 400 lignes** (décision puis
   exécution), et `run_conversation` en mélange six (audit, garde de budget, appel,
   relance vide, refus, réponse finale et son coût).
4. **La politique de tentatives est calculée en ligne** dans `call_model` avec six
   variables mutables (`candidates`, `retries`, `waited_secs`, `stream_retried`,
   `client_fallbacks`, `attempt`, agent.rs:933-954) : intestable hors du réseau simulé.
5. **Le contrat des cartes est un `json!` non typé** partagé avec `telegram.rs` (rendu),
   `rpc.rs`, `engine.rs` (`payload.turn_id`, :482-489) et `decide_approval`
   (`payload.get("arguments")`, :2331) ; idem pour les cartes de budget (`budget: true`,
   `checkpoint: true`) et les conventions de `call_id` (`budget:<scope>:<cents>` :619-623,
   `checkpoint:<turn>:<level>` :1805).
6. **Le steering est un effet de bord d'une lecture** : `Conversation::request_messages`
   absorbe la file, écrit l'historique, émet `turn.merged`, peut envoyer une carte de
   rafale et **annuler le tour** (conversation.rs:171-238). Rien dans la boucle ne le nomme.
7. **Deux messages du harnais ne sont jamais enregistrés** : la relance après réponse
   vide (agent.rs:681-687) et la consigne d'après boucle (:1205-1213). La requête
   réellement envoyée diffère de l'historique (#206).
8. **`PARALLEL_SAFE` est une liste à la main** (agent.rs:374-400) alors que `ToolSpec`
   porte déjà `risk` et `idempotent` (tools/spec.rs:10-21) : un nouvel outil de lecture
   n'est parallèle que si quelqu'un y pense.
9. **Le nudge de délégation est collé au texte d'un résultat d'outil** (agent.rs:1768-1770)
   et la note de boucle aussi (:1645) : volontaire pour le cache, invisible comme
   mécanisme.
10. **Un `/stop` au milieu d'un lot laisse des appels sans résultat** : `Pending::Stop`
    revient avant d'écrire les résultats restants (agent.rs:1561-1563) ; c'est
    `penelope-context/src/transcript.rs:117-150` (`repair_pairs`) qui pose des stubs à la
    projection. La boucle ne s'en sait pas responsable.
11. **Les mode-de-session et cas spéciaux d'outils vivent dans la boucle** :
    `shell_exec` (:1439, :1450-1454), `config_set` (:1473-1480), `mcp__` (via
    `describe_call`, executor.rs:2167 : idempotent = `risk == Read`, une politique
    implicite).
12. **Deux sources de coût** : `cost` local (agent.rs:545, :723) pour `turn.finished`, et
    `turn_totals` en base pour les plafonds (:1789) ; le texte « Coût de ce tour » lit la
    seconde (:888).
13. **Le prompt système est un type du daemon** (`prompt_snapshot::PromptPrefix`,
    construit depuis `penelope_context::Tiers`) alors que `Conversation::prompt_prefix`
    (agent.rs:150) est un contrat de la boucle.
14. **Tout passe par `Services`** (25 champs, runtime.rs:28-55) et par `crate::cache_audit`,
    `crate::prompt_snapshot`, `crate::approval_mode`, `crate::executor::{effective_arguments,
    wants_network}`, `crate::budget_alert::usd` : la boucle ne compile pas hors du daemon.
15. **Les noms d'événements sont des littéraux dispersés** : `turn.started`, `turn.finished`,
    `turn.empty_answer`, `turn.loop_aborted`, `tool.result`, `llm.retried`,
    `llm.fallback_used`, `approval.decided`, `turn.merged` (agent.rs, conversation.rs).
16. **Quatre appelants, un seul chemin** : chat (engine.rs:631), étape `agent` de workflow
    (workflow.rs:1314), sous-agent (workflow.rs:1509) et installation de skill
    (skill_install.rs:137) ; le sous-agent transforme `AwaitingApproval` en erreur
    (workflow.rs:1515-1517) : implicite, à garder.
17. `TurnRequest` et `AgentLoop::run` (agent.rs:349-359, 514-533) et l'alias
    `resume_after_approval` (:1919-1925) ne servent plus qu'aux tests.

---

## 2. Ce que Pénélope fait mieux que les trois modèles, à garder intact

| Point | Pénélope | DSH | OpenClaw | Hermes |
|---|---|---|---|---|
| Ledger d'effets avec état `unknown` | clé d'idempotence (run, session, step, outil, args, tentative) ; `dispatching` → `unknown` au redémarrage ; décision humaine, jamais de relance seule, jamais de règle (kernel/effects.rs:151-163, 343-409 ; agent.rs:2214-2272) | `tool/call` journalisé avant exécution, mais aucune clé d'idempotence ni rejeu : « a hard process loss before settlement leaves no durable attempt stream » (`dsh/docs/architecture.md`, « Session log ») | note « tools may have partially executed » au tour suivant (`turn-interruption.ts:32-34`) | aucun ledger |
| File de tours durable, bail avec jeton de clôture, fusion à la réclamation | kernel/turn.rs:1-23, 194-341 ; runner.rs:152-277 | inbox durable projetée du journal, un seul processus (`inbox.ts:27-65`) | file en mémoire (`getSteeringMessages`) | boucle en mémoire |
| Bornage des règles « Toujours » | motif dérivé de l'appel (famille, répertoire, origine, réseau), lexer unique, une règle par étape d'une liste, aucune sur une ligne composée (agent.rs:2002-2145 ; hitl/cmdline.rs:1-24 ; hitl/policy.rs:451-517) | pas d'autorisation durable : `allowed-once` est la seule ouverture (`approval.md`, « Identity and outcome ») | idem, `ask` par appel | liste permanente par texte exact ou glob, refusée dès qu'un opérateur paraît (`approval_floors.py:144-204`) |
| Classes de risque et ordre d'évaluation documenté | Read/Write/Destructive/External/Unknown ; règle > outil > serveur > surcharge > défaut ; annotations MCP = indices (kernel/risk.rs:8-83 ; hitl/policy.rs:1-11) | `ask`/`never` par session, guards monotones (`tools.md`, « ToolGuard ») | par outil | motifs regex « dangereux », planchers (`approval.py:1052-1066`) |
| Planchers déterministes déjà en place | Deny de règle et de déclaration MCP priment sur tout (agent.rs:1410-1418), `config_set` sensible en AskTwice malgré toute règle (:1473-1480), destructif jamais `auto` (:1449) | guards | aucun | `_floor_block` avant yolo (`approval.py:1052-1066`) |
| Coût mesuré et bornes | facturé sinon estimé et marqué ; attribué au tour d'origine ; plafonds jour/session/run avec carte ; palier par tour ; plafond d'appels ; nudge de délégation (llm/provider.rs:1109-1119 ; agent.rs:596-669, 1777-1886) | compteur de tokens, pas de garde de budget dans la boucle | aucune | avis de fin de budget temps (`conversation_loop.py:72-78`) |
| Machine d'état LLM avec `send_unknown` (facturation incertaine) | llm/state.rs:1-9, 227-260 | `assistant/attempt` garde le contenu, pas l'incertitude de facturation | non | non |
| Boucle arrêtée qui répond quand même | dernier résultat réel + note + réponse sans outil + choix (#31) | non | intervention puis arrêt (`agent-loop.ts:1371-1439`), sans réponse finale | garde de répétition |
| Admission de groupe des résultats (#52) | conversation.rs:295-320 | spill par sous-appel (`tools/ptc-dispatch-log`) | non | budget par tour de résultats (`tool_executor.py`, `enforce_turn_budget`) |
| Carte : intention d'abord, arguments validés avant (#116, #117) | agent.rs:1369-1387, 1496-1499 | `reason` de l'appelant, pas d'arguments dupliqués (`approval.md`) | non | description du motif |
| Cause d'un raté de cache et instantané du prompt (#17, #205) | cache_audit.rs:158-192 ; prompt_snapshot.rs:1-24 | `request/header` et `request/context` journalisés | non | non |

Ce qui doit rester tel quel dans la V1 : les signatures de `EffectLedger` et de
`Planned`, l'ordre des couches de politique, la dérivation des motifs de règles, les
textes de refus renvoyés au modèle (les tests les lisent), les gardes de budget avant
appel, et la règle « une erreur d'outil au modèle, une erreur de harnais au propriétaire ».

---

## 3. Cible V1

### 3.1 Principes

- **Un pipeline nommé, en étapes distinctes, un type par étape.** L'entrée et la sortie
  de chaque étape sont des types Rust ; l'ordre est fixé dans le code, pas par
  enregistrement dynamique. Ce que DSH obtient par des waterfalls (`agent/pre-step`,
  `agent/request`, `tools/pre-execute`, guards, `tools/execute`, `tools/post-execute`,
  `dsh/docs/architecture.md` « Turn flow », `tool-execution-pipeline.md`), Pénélope
  l'obtient par des **traits à implémentation unique** (les ports) et des **chaînes de
  gardes typées** (monotones : une garde peut refuser, jamais autoriser).
- **Fail-closed partout où un modèle ou un service auxiliaire entre** : un juge absent,
  un schéma faux, un délai dépassé rendent la carte d'aujourd'hui (DSH : « callers fail
  closed unless it is `allowed-once` », `approval.md`).
- **Les planchers déterministes passent avant tout ce qui est nouveau** : règle `Deny`,
  déclaration MCP `deny`, classe `Destructive`, `config_set` sensible. Le juge (#203)
  et le mode `auto` ne voient jamais ces appels (Hermes : `_floor_block` avant yolo,
  `approval.py:1052-1066`).
- **Rien ne change dans le transcript sans être nommé** : les messages du harnais
  deviennent des `Injection` avec une politique de persistance explicite.
- **Chaque étape est testable sans réseau ni daemon** : le crate a son propre
  `AgentServices::for_tests` sur `Store::open_memory`.

### 3.2 Le pipeline

```text
                       ┌──────────────────────────────────────────────────────────┐
 Turn (kernel) ───────►│ ADMISSION (reste dans le daemon, engine.rs)              │
                       │ message écrit une fois · fusionnés · tour d'origine ·    │
                       │ modèle · provider · prompt · executor · TurnSpec         │
                       └───────────────┬──────────────────────────────────────────┘
                                       ▼
 ╔═════════════════════════ penelope-agent : TurnDriver::run ═══════════════════════════╗
 ║  ┌─ étape 0 · ToolPipeline::resolve(pending)  ──────────────────────────────────────┐ ║
 ║  │   (appels sans résultat en queue de transcript : premier passage = reprise)     │ ║
 ║  │   Resolution::{Nothing, Resolved{n}, Suspended(id), Stopped(outcome), Loop(..)} │ ║
 ║  └──────────────────────────────────────────────────────────────────────────────────┘ ║
 ║  ┌─ étape 1 · TurnGuards (chaîne fixe) : Cancel → Budget → CallCap → CostCheckpoint ┐ ║
 ║  │   GuardVerdict::{Proceed, Suspend(approval_id), Stop(TurnOutcome)}              │ ║
 ║  └──────────────────────────────────────────────────────────────────────────────────┘ ║
 ║  ┌─ étape 2 · Assembly : Inbox::claim(BeforeModelCall) → conv.request_messages     ┐ ║
 ║  │   + Injection::RequestOnly (relance vide) → PreparedRequest                     │ ║
 ║  │   { messages, fingerprint, pinned_upstream, tool_choice, tools }                │ ║
 ║  └──────────────────────────────────────────────────────────────────────────────────┘ ║
 ║  ┌─ étape 3 · ModelCaller::call(PreparedRequest) → CallOutcome                     ┐ ║
 ║  │   RetryPlan (pur) : RetrySame(wait) | Fallback(model) | GiveUp                  │ ║
 ║  │   chaque essai → Attempt → AttemptSink (#206) ; llm_state autour de l'appel     │ ║
 ║  │   CallOutcome::{Response(ChatResponse, attempts), Failed(CallFailure, attempts)} │ ║
 ║  └──────────────────────────────────────────────────────────────────────────────────┘ ║
 ║  ┌─ étape 4 · Settlement : usage.record · prompt snapshot · miss cause · cancel ·  ┐ ║
 ║  │   refus · vide → StepResult::{Final(text), Calls(Vec<ToolCall>), RetryEmpty,    │ ║
 ║  │   Stop(TurnOutcome)} ; conv.record(assistant)                                   │ ║
 ║  └──────────────────────────────────────────────────────────────────────────────────┘ ║
 ║   Final → TurnOutcome::Answered ; Calls → retour à l'étape 0                          ║
 ╚═══════════════════════════════════════════════════════════════════════════════════════╝
```

Le pipeline d'un appel d'outil (`ToolPipeline`, une étape = un type) :

```text
 ToolCall (transcript)
   │ 1  normalise      ToolExecutor::normalise_call        → NormalisedCall
   │ 2  describe       ToolExecutor::describe_call         → DescribedCall { info: CallInfo }
   │ 3  gardes         CallGuards (chaîne fixe, monotone)  → Option<Refusal>
   │        Allowlist · PriorDecision · LoopGuard · Precheck
   │        PriorDecision::Pending(id) est une Suspension, pas un refus
   │ 4  politique      PolicyStage::evaluate               → Verdict { decision, layer: VerdictLayer, reason }
   │        VerdictLayer = Rule(id) | ServerDeclaration | DeclaredAllow | SessionMode
   │                    | Floor(Destructive | SensitiveConfig) | Default | Judge(#203)
   │ 5  juge (#203)    Judge::judge (seulement si Ask ∧ shell_exec ∧ sans motif ∧ non destructif)
   │ 6  approbation    Approver::ask(ApprovalCard)         → AskOutcome::{Suspended(id), Decided(bool)}
   │ 7  ordonnancement ExecutionMode::{Parallel, Exclusive} (dérivé de ToolSpec.parallel_safe)
   │ 8  ledger         EffectStage::plan                   → Planned::{Replayed, NeedsDecision, InFlight, Fresh}
   │ 9  exécution      Execution::{Sync(ToolOutcome), Job(JobId)} (#204)
   │ 10 post           PostProcessors (chaîne fixe, monotone) : Redact · Nudge · Metrics · Event
   │ 11 enregistrement conv.record + sink ; admission de groupe
   ▼
 Recorded { call_id, ok, preview }
```

Types par étape (signatures cibles, noms en anglais) :

```rust
pub enum Resolution { Nothing, Resolved { recorded: usize }, Suspended { approval_id: String },
                      Stopped(TurnOutcome), Loop { report: String, tool: String, last: Option<String> } }

pub enum GuardVerdict { Proceed, Suspend { approval_id: String }, Stop(TurnOutcome) }
pub trait TurnGuard { async fn check(&self, cx: &TurnContext) -> anyhow::Result<GuardVerdict>; }

pub struct PreparedRequest { messages: Vec<ChatMessage>, tools: Vec<ToolDef>,
                             tool_choice: Option<ToolChoice>, fingerprint: Fingerprint,
                             pinned_upstream: Option<String> }

pub enum RetryAction { RetrySame { wait_s: u64 }, Fallback { model_id: String }, GiveUp }
pub struct RetryPlan { /* état pur : candidats, tentatives, attente cumulée, flux relancé */ }
impl RetryPlan { pub fn on_error(&mut self, e: &LlmError, phase: Phase) -> RetryAction }

pub struct Attempt { seq: u32, model: String, provider: String, upstream: Option<String>,
                     phase: Phase, finish: Option<FinishReason>, error: Option<String>,
                     text: String, usage: Usage, request_only_note: Option<String> }
pub trait AttemptSink { async fn record(&self, turn: &TurnSpec, a: &Attempt) -> anyhow::Result<()>; }

pub enum StepResult { Final(String), Calls(Vec<ToolCall>), RetryEmpty, Stop(TurnOutcome) }

pub enum Refusal { Allowlist, Denied { why: String }, Expired, LoopWarn(String),
                   Invalid(String), Policy { reason: String } }
pub enum Suspension { Prior { approval_id: String }, Ask { approval_id: String } }
pub trait CallGuard { async fn check(&self, call: &DescribedCall, cx: &TurnContext)
                          -> anyhow::Result<Option<GuardStop>>; }   // GuardStop = Refusal | Suspension | LoopAbort
pub struct Verdict { decision: PolicyDecision, layer: VerdictLayer, reason: String }
pub enum AskOutcome { Suspended { approval_id: String }, Decided { approved: bool } }
pub trait Approver { async fn ask(&self, card: ApprovalCard) -> anyhow::Result<AskOutcome>; }
pub enum ExecutionMode { Parallel, Exclusive }
pub enum Execution { Sync(ToolOutcome), Job { id: String } }
pub trait PostProcessor { fn after(&self, call: &DescribedCall, out: ToolOutcome, cx: &TurnContext) -> ToolOutcome; }
```

La reprise après approbation garde son mécanisme : « une itération commence toujours
par résoudre les appels en attente » (agent.rs:1-12). La V1 ne change pas le contrat
`AwaitingApproval` → `enqueue_resume` → même chemin (engine.rs:213-240).

### 3.3 Points d'interception (l'équivalent des waterfalls, en Rust)

| DSH | Pénélope V1 | Forme | Peut |
|---|---|---|---|
| `agent/pre-step` (reject / enter) | `TurnGuards` + `Inbox::claim` | chaîne fixe de `TurnGuard` | suspendre ou arrêter le tour ; jamais ajouter un message |
| `agent/request` (config de l'appel) | `RequestShaper` | trait, implémentation par appelant (chat, sous-agent, workflow) | `fit_modalities`, `tool_choice`, replis, fournisseur épinglé |
| `llm/stream` | `Provider::chat_stream` (existant) | trait existant | inchangé |
| `agent/request-error` (retry) | `RetryPlan` | fonction pure | `RetrySame`, `Fallback`, `GiveUp` |
| `assistant/attempt` | `AttemptSink` | trait | enregistrer hors historique |
| `tools/pre-execute` (allow / deny / cancel / ask) | `CallGuards` puis `PolicyStage` | chaîne fixe + fonction | refuser, suspendre, demander ; **jamais réécrire les arguments** (DSH : « Input rewriting is excluded because arguments are already logged », `tools.md`) : la normalisation reste l'étape 1, avant toute décision (#123) |
| guards monotones | `CallGuard` retourne `Option<GuardStop>` | même contrat que `ToolGuard` : pas de résultat « allow » | une garde ajoutée ne peut qu'ajouter un refus |
| `approval/request` (fail-closed) | `Approver` | trait ; `Err` ou variante inconnue → carte, jamais `Auto` | suspendre ; une décision antérieure est lue **avant** |
| `tools/execute` (around) | `EffectStage` (ledger) + `Execution` | **pas un point d'interception** : le ledger est une étape fixe | délai, annulation, job |
| `tools/post-execute` (accept / block / replace) | `PostProcessor` chaîne fixe | monotone : peut réduire, annoter, marquer `is_error`, jamais rendre `ok` un échec | rédaction (#134), nudge (#19), enveloppe non fiable (#13, #92) |
| `tools/result` (observe) | `TurnSink` + `EventLog` | existant | observer |
| `agent/turn-stopping` | post-traitement de `run_turn` (revue, titre, compaction) | reste dans le daemon | inchangé |

Ce qui n'est **pas** repris de DSH : l'enregistrement dynamique de listeners avec
`next()` (Cordis). En Rust, une chaîne fixe déclarée dans `CallGuards::default_chain()`
et testée par un test d'ordre remplit le même rôle sans indirection.

### 3.4 Steering explicite

Aujourd'hui trois mécanismes sans nom (§1.4 point 6, 7, 9). La V1 les nomme, sur le
modèle de l'inbox à trois cibles de DSH (`agent.ts:162-172` : `followup` = next-turn,
`steer` = next-step, `inject` = next-step sans réveil) et des checkpoints d'OpenClaw
(`types.ts:301-317` : « Sequential execution checks before each tool starts […] A
non-empty result skips calls that have not started »).

```rust
pub enum Checkpoint { BeforeModelCall, BetweenCalls, AfterBatch }
pub trait Inbox {
    /// Messages du propriétaire arrivés pendant le tour, sous le bail du tour.
    async fn claim(&self, at: Checkpoint) -> anyhow::Result<Vec<Steer>>;
}
pub enum Injection {
    /// Collé au texte d'un résultat d'outil : entre dans l'historique (nudge, note de boucle).
    ToolResultSuffix(String),
    /// Note système placée APRÈS le dernier message utilisateur (pas dans le préfixe).
    Note(String),
    /// Seulement dans la requête envoyée : enregistré comme tentative (#206), jamais dans `messages`.
    RequestOnly(String),
}
```

- **Next-turn** reste la file `turn_queue` avec fusion à la réclamation (kernel/turn.rs)
  : rien ne change.
- **Next-step** : `Inbox::claim(BeforeModelCall)` remplace l'absorption cachée dans
  `request_messages` (conversation.rs:173-238). La lecture ne fait plus d'effet de bord ;
  la boucle écrit les messages absorbés, émet `turn.merged`, applique la règle de rafale.
- **`BetweenCalls`** (nouveau, OpenClaw `agent-loop.ts:574-581`) : un message arrivé
  pendant un lot d'outils ne fait pas attendre la fin du lot pour être lu. Les appels
  **déjà démarrés** finissent (le ledger l'exige) ; les appels **non démarrés** reçoivent
  « Non exécuté : nouveau message du propriétaire » (équivalent des résultats
  synthétiques de DSH `tool-calls.ts:250-260` et du motif `steering` d'OpenClaw
  `agent-loop.ts:675-680`), puis le message est écrit, puis le modèle est rappelé. Le
  transcript reste protocolairement complet **par la boucle**, sans dépendre de
  `repair_pairs`. La règle d'abandon de `pending_calls` (agent.rs:2386-2408) reste, en
  ceinture.
- **Note d'interruption** : après un `Cancelled` pendant des outils, les appels non
  démarrés reçoivent « Non exécuté : arrêté par le propriétaire » et le tour suivant
  reçoit `Injection::Note` « le tour précédent a été interrompu ; des outils ont pu
  s'exécuter partiellement » (OpenClaw `turn-interruption.ts:32-68`), placée après le
  dernier message utilisateur pour ne pas casser le préfixe (décision 0008).
- La **note de fusion** actuelle est insérée après les messages système
  (conversation.rs:86-100) : elle change la chaîne dès l'index 1 et coûte un raté
  « historique ». En V1, elle devient `Injection::Note` en queue.

### 3.5 Jobs asynchrones (#204)

Le modèle est celui de `penelope-mcp/src/tasks.rs` (états `working | input_required |
completed | failed | cancelled`, `poll_at`, `recover_on_boot`, :15-49, :196-218) : une
table `tool_jobs` (ou `mcp_tasks` étendue à `kind = native`) avec `effect_id`.

Dans le pipeline : l'étape 9 rend `Execution::Job { id }` quand l'exécuteur le décide
(`shell_exec` avec `background: true`, ou `timeout_ms` au-delà de `tools.background_after`,
qui **propose** plutôt qu'impose ; `sub_agent_spawn`). L'effet reste `dispatching` tant
que le job tourne ; c'est le coureur de jobs (hors tour, comme le poller MCP) qui appelle
`effects.complete | fail`. Le résultat enregistré dans le transcript est
`{job, state: "working"}` ; le tour continue. À la fin, un tour `TurnKind::Nudge` porte
le résultat dans la session d'origine (kernel/turn.rs:37-42, engine.rs:423) ; les outils
`job_status`, `job_wait` (borné), `job_cancel`, `job_list` sont à la demande (#104).

Règle de reprise (à écrire dans la décision) : un job dont le processus est mort au
redémarrage passe en `failed` avec la raison, et son effet aussi ; un tour `Nudge` le
dit. Aucune relance automatique. La carte `effect_unknown` reste réservée aux effets
dont la complétion est inconnaissable (#83) ; un job est observable (pid, journal). Le
modèle peut relancer, avec une nouvelle carte si la politique le demande.

Bornes : 3 jobs par session, 10 en tout ; `/stop` annule les jobs de la session (#155
pour `/stop tout`) ; `reap_orphans` (runtime.rs:465-471) connaît leurs pid.

### 3.6 Le juge (#203)

Étape 5, appelée **uniquement** quand la politique a rendu `Ask` (jamais `Deny`, jamais
`AskTwice`), pour `shell_exec`, sans motif possible (`arg_patterns` vide), classe non
destructive. Sous les planchers, jamais au-dessus : une règle `Deny` du propriétaire
l'emporte (hitl/policy.rs:379-445), un `Destructive` ne le voit pas.

```rust
pub struct Judgement { powers: Vec<Power>, paths: Vec<String>, hosts: Vec<String>,
                       verdict: JudgeVerdict, why: String, model: String, cost_usd: f64 }
pub trait Judge { async fn judge(&self, cmd: &str, cwd: &Path, workspaces: &[PathBuf])
                      -> Option<Judgement>; }   // None = indisponible, hors schéma, délai : carte d'aujourd'hui
```

Le texte est hostile (Hermes `approval_smart.py:1-9`, `:17-32`, `:56-65`) : commentaires
retirés, commande dans un bloc délimité, consigne d'ignorer toute directive interne ;
le juge ne reçoit ni transcript, ni mémoire, ni secret ; `tool_choice: none` ; 10 s.
Modes `off` (défaut), `explain` (carte enrichie, bouton « Toujours pour ces pouvoirs »
qui écrit une règle dérivée des pouvoirs), `auto_read` (lecture pure dans les workspaces,
sans réseau : passe sans carte). Événement `approval.judged`, usage `role =
"approval_judge"`. Hermes ajoute un disjoncteur après trois refus consécutifs
(`approval.py:58-111`) : repris tel quel, il coûte trois lignes.

La mesure préalable (#203, « Mesure préalable ») décide si l'étape est livrée : c'est la
tâche T21.

### 3.7 Tentatives (#206)

`Attempt` et `AttemptSink` (§3.2). Trois cas alimentent le registre : flux coupé après
du texte (le partiel, agent.rs:1133-1143), erreur avant flux à chaque essai
(:1005-1055), relance après réponse vide avec sa consigne (`Injection::RequestOnly`,
:681-687) et la consigne d'après boucle (:1205-1213). Table `turn_attempts` hors de
`messages`, purge avec la session, dix par tour au plus. Test central : `request_messages()`
identique avec et sans tentatives. Le message d'échec cesse de promettre un « début
affiché » (#206). DSH : `assistant/attempt` « retains settled failed, retried, cancelled,
and stream-error attempts without adding model history » (`architecture.md`, « Session
log ») ; `agent.ts:449-459` et `:472-475`.

### 3.8 PTC (`run_code`) : hors V1, avec une couture

DSH envoie le programme et ses sous-appels dans le même pipeline ; les sous-appels
portent le jeton du parent, sont journalisés `tool/ptc-dispatch`, et **une demande
d'approbation à l'intérieur d'un programme est un refus** (`tool-execution-pipeline.md`
: « sub-calls carry the parent token […] return denials as binding rejections » ;
`tools.md`, « ToolExecutionInput.parent »).

Argument pour le reporter :

1. Le modèle d'approbation de Pénélope suspend **le tour** et le reprend en relisant
   les appels sans résultat du transcript (agent.rs:1-12). Un programme en vol ne se
   suspend pas : DSH le résout par « ask = deny » dans un programme. Pénélope devrait
   donc soit refuser tout appel non `Auto` dans un programme (un programme n'aurait
   accès qu'aux lectures et aux règles « Toujours »), soit inventer une reprise de
   programme. La première voie est cohérente mais n'a de valeur qu'avec beaucoup de
   lectures : c'est exactement ce que #85 (lectures parallèles) et #204 (jobs) couvrent
   déjà en Rust, sans runtime embarqué.
2. Il faut un runtime (TypeScript ou Python) confiné : l'instance tourne en profil
   `full` assumé, sans bac à sable, et le crate `penelope-platform` n'embarque aucun
   interpréteur. Les motifs interdits de l'archtest (`penelope-archtest/src/lib.rs:129-143`)
   disent la même chose : pas d'appel shell hors plateforme.
3. Le ledger doit planifier **chaque** sous-appel (une clé par sous-appel, `step =
   <call_id>:ptc:<n>`) : compatible, mais c'est un lot entier.

Couture à poser en V1 (T24, taille S) : `CallContext { call_id, parent: Option<CallId>,
root: CallId }` traverse le pipeline ; la règle « un appel imbriqué qui demande une
approbation est refusé » est écrite et testée ; la décision `0012-ptc-hors-v1.md` fixe
que `run_code` viendra, s'il vient, comme un `ToolExecutor` de plus qui dispatche par le
même `ToolPipeline`.

### 3.9 Ce que la cible ne change pas

`TurnOutcome` et ses consommateurs (telegram.rs:6903-6995, rpc.rs:1426-1440,
scheduler.rs:731-841, workflow.rs:1317-1360, 1513-1522), `ChannelDelivery`, `Bus`,
`TurnQueue`, `EffectLedger`, `ApprovalStore`, `PolicyEngine`, les textes des cartes et
des refus, `TURN_CALLS = 24`, `PARALLEL_READS = 4`, les seuils de configuration
(`budget.*`, `tools.*`, config.rs:641-676, 1109-1155).

---

## 4. Où ça vit : crate `penelope-agent`

### 4.1 Modules (chacun sous 800 lignes, tests compris)

| Module | Contenu | Vient de | Taille visée |
|---|---|---|---|
| `lib.rs` | `#![forbid(unsafe_code)]`, ré-exports | | 60 |
| `outcome.rs` | `TurnOutcome`, `TurnEvent`, `TurnSink`, `NullSink`, `RecordingSink` | agent.rs:28-127 | 130 |
| `conversation.rs` | `Conversation`, `Compactor`, `MemoryConversation`, `PromptPrefix` (rendu + tuiles, sans `Tiers`) | agent.rs:129-263 ; prompt_snapshot.rs:43-70 | 200 |
| `executor.rs` | `ToolExecutor`, `CallInfo`, `call_arguments`, `effective_arguments`, `wants_network` | agent.rs:267-332 ; executor.rs:2057-2085, 2201-2203 | 150 |
| `ports.rs` | `AgentServices` (effects, budget, approvals, policies, llm_state, events, catalog, clock, config) ; traits `SessionModes`, `SessionInfo`, `PromptSnapshots`, `CacheAudit`, `Inbox`, `AttemptSink`, `Judge`, `JobRunner`, `Approver` ; `for_tests` | nouveau (remplace `Services`) | 350 |
| `spec.rs` | `TurnSpec`, `TurnContext`, `AgentLoop`, constantes | agent.rs:334-366, 498-502 | 120 |
| `turn.rs` | `TurnDriver::run` (étapes 0 à 4) | agent.rs:535-917 | 400 |
| `guards.rs` | `TurnGuard` chaîne : budget, plafond d'appels, palier, nudge de délégation ; `budget_exceeded_text` | agent.rs:186-217, 596-669, 1777-1886 | 350 |
| `model/mod.rs` | `ModelCaller` (plan, dispatch, collect, événements) | agent.rs:924-1187 | 300 |
| `model/retry.rs` | `RetryPlan` pur + tests de table | agent.rs:1005-1074, 1129-1183, 2411-2446 | 250 |
| `model/errors.rs` | `CallFailure`, `humanise_llm_error`, `fit_modalities` | agent.rs:163-180, 2448-2513 | 180 |
| `attempts.rs` | `Attempt`, `Phase`, `AttemptSink` (impl mémoire) | nouveau (#206) | 120 |
| `pipeline/mod.rs` | `ToolPipeline::resolve` (phase décision, phase exécution, terminal) | agent.rs:1267-1300, 1554-1669 | 350 |
| `pipeline/decide.rs` | `CallGuards` : allowlist, décision antérieure, garde de boucle, precheck ; `DescribedCall`, `Refusal`, `Suspension` | agent.rs:1289-1387 | 300 |
| `pipeline/policy.rs` | `PolicyStage`, `VerdictLayer`, `declared_allow`, `local_draft_allow`, mode de session, planchers | agent.rs:1392-1480 ; approval_mode.rs:85-130 | 350 |
| `pipeline/approval.rs` | `ApprovalCard` typé (`tool, arguments, reason, why, why_from, double, call_id, turn_id`), `Approver` par `ApprovalStore`, `call_intention`, `turn_goal`, `without_intention` | agent.rs:1490-1539, 1929-1968 | 250 |
| `pipeline/schedule.rs` | `ExecutionMode`, lots parallèles, barrière, annulation de lot | agent.rs:369-400, 1557-1612 | 200 |
| `pipeline/effect.rs` | `EffectStage` (plan, dispatching, complete/fail), `effect_kind`, `server_of` | agent.rs:1673-1728, 2515-2533 | 180 |
| `pipeline/record.rs` | `finish_call`, `record_result`, `line_shape`, `PostProcessor` chaîne | agent.rs:1732-1773, 1888-1906, 1973-1983 | 200 |
| `pipeline/jobs.rs` | `Execution::Job`, `JobRunner` port, plafonds | nouveau (#204) | 250 |
| `pipeline/judge.rs` | étape 5, `Judgement`, texte hostile, règle par pouvoirs | nouveau (#203) | 300 |
| `steering.rs` | `Inbox`, `Checkpoint`, `Steer`, `Injection`, résultats « Non exécuté » | nouveau ; conversation.rs:171-238 | 200 |
| `loop_abort.rs` | `answer_after_loop`, `split_choices`, `last_result_of`, `LOOP_STOP_NOTE`, choix par défaut | agent.rs:438-496, 1192-1264 | 200 |
| `decisions.rs` | `decide_approval`, `decide_uncertain_effect`, `EFFECT_*` | agent.rs:2207-2384 | 250 |
| `rules.rs` | `arg_pattern`, `arg_patterns`, `family_of`, `always_creates_no_rule`, `MAX_FAMILIES_PER_CLICK` | agent.rs:1985-2145 | 250 |
| `pending.rs` | `pending_calls` | agent.rs:2386-2408 | 60 |
| `events.rs` | `TurnEventKind` avec `as_str()` : `turn.started`, `turn.finished`, `turn.empty_answer`, `turn.loop_aborted`, `turn.merged`, `tool.result`, `llm.retried`, `llm.fallback_used`, `llm.attempt`, `approval.decided`, `approval.judged`, `tool.job.*` | littéraux dispersés | 80 |

Total visé : environ 5 500 lignes tests compris, contre 4 239 aujourd'hui dans un
fichier (les tests migrent avec leur module).

### 4.2 Dépendances autorisées

`penelope-kernel` (effets, budget, risque, événements, config, ids, horloge),
`penelope-llm` (types, `Provider`, `LlmStateMachine`, `Router::should_fallback`,
`Catalog`), `penelope-tools` (`LoopDetector`, `ToolOutcome`, `ToolError`, `is_allowed`,
`tool_spec`, `shell::{is_read_command, may_destroy}`), `penelope-hitl` (`ApprovalStore`,
`PolicyEngine`, `cmdline`, `Decision`), `penelope-observe` (`redact_json`, métriques).
Externes : `serde_json`, `anyhow`, `async-trait`, `tokio`, `futures`, `tracing`.

Interdits : `penelope-daemon` (évidemment), `penelope-context` (le prompt arrive rendu),
`penelope-telegram`, `penelope-workflow`, `penelope-memory`, `penelope-skills`,
`penelope-mcp` (les méta-outils sont décrits par l'exécuteur), `reqwest`.

Règle à ajouter dans `penelope-archtest/src/lib.rs:200-236` : `m.insert("penelope-agent",
vec!["penelope-kernel", "penelope-llm", "penelope-tools", "penelope-hitl",
"penelope-observe"])`. Sans cette ligne, `dependency_violations` (:240-262) ignore le
crate : la règle est ce qui rend la frontière réelle.

### 4.3 Ce qui reste dans le daemon

- `engine.rs` : admission (message écrit une fois, fusion, tour d'origine, modèle,
  provider, prompt, `SessionConversation`, `NativeToolExecutor`, `TurnSpec`) et le
  post-tour (`run_turn`). C'est l'étape « ADMISSION » du schéma.
- `executor.rs` : `NativeToolExecutor` et tout `dispatch` ; il implémente
  `penelope_agent::ToolExecutor` et importe `call_arguments`/`effective_arguments` du
  crate (executor.rs:1872-1879, 1918-1928, 2119-2120 les utilisent).
- `conversation.rs` : `SessionConversation` (projection, admission, compaction) ; elle
  perd l'absorption, qui devient l'impl daemon de `Inbox` (sur `TurnQueue::absorb_pending`
  et `ChannelDelivery::offer_burst`).
- `cache_audit.rs`, `prompt_snapshot.rs`, `approval_mode.rs` (la partie kv) : impl des
  ports `CacheAudit`, `PromptSnapshots`, `SessionModes`.
- `runner.rs`, `bus.rs`, `telegram.rs`, `workflow.rs`, `scheduler.rs`, `rpc.rs` :
  inchangés, ils consomment `TurnOutcome` et `decide_approval`.

### 4.4 Façade de compatibilité

`penelope-daemon/src/agent.rs` devient `pub use penelope_agent::*;` plus les impl des
ports. Les appelants hors boucle (`decide_approval` : engine.rs, telegram.rs, rpc.rs,
ingest.rs, workflow.rs ; `EFFECT_DONE`, `budget_exceeded_text`, `arg_pattern`,
`always_creates_no_rule`, `TURN_CALLS`, `without_intention`, `effect_kind` : telegram.rs,
rpc.rs, runtime.rs, workflow.rs, executor.rs) ne changent pas d'import.

---

## 5. Tâches fines

Chaque tâche est livrable seule : les 42 tests d'`agent.rs`, les 31 d'`engine.rs`, les
12 de `conversation.rs`, les 6 de `runner.rs`, les 30 d'`executor.rs` et la suite
`resilience` restent verts à chaque étape (`cargo test -p penelope-daemon` puis `-p
penelope-evals --test resilience`). Tailles : S < 1 j, M 1 à 2 j, L 3 j et plus.

| # | Titre | Périmètre | Dépend de | Critère de fin | Taille |
|---|---|---|---|---|---|
| T01 | Décision `0011-pipeline-d-agent.md` et `docs/agent-loop.md` | `docs/decisions/`, `docs/`, `docs/README.md` | aucune | le schéma ASCII de §1 et §3 est dans `docs/`, le test `docs` passe (`UPDATE_DOCS=1` si l'index change) | S |
| T02 | Éclater `agent.rs` en `agent/` dans le daemon, sans changer une signature | `penelope-daemon/src/agent/{mod,outcome,conversation,executor,spec,turn,guards,model,pipeline,loop_abort,decisions,rules,pending}.rs` | aucune | `git diff --stat` ne montre que des déplacements ; `pub use` garde tous les chemins `crate::agent::*` ; 42 tests inchangés | M |
| T03 | `RetryPlan` pur | `agent/model/retry.rs` | T02 | table de vérité testée : (erreur, phase, provider, candidats restants) → action ; `call_model` l'appelle ; les 6 tests de replis passent sans modification | M |
| T04 | `TurnGuard` et `GuardVerdict` | `agent/guards.rs` | T02 | budget, plafond, palier, annulation sont quatre gardes ; un test d'ordre (garde enregistreuse) ; `exceeded_budget_stops_before_calling_the_model` et `a_costly_turn_asks_before_going_on` inchangés | S |
| T05 | `CallGuards` typées (`DescribedCall`, `Refusal`, `Suspension`, `GuardStop`) | `agent/pipeline/decide.rs` | T02 | la phase 1 de `resolve_pending` est une itération sur une chaîne fixe ; test d'ordre ; textes de refus identiques (tests `tools_outside_the_allowlist_are_refused`, `a_denied_call_is_reported_to_the_model`) | M |
| T06 | `PolicyStage` et `VerdictLayer` | `agent/pipeline/policy.rs` | T02 | chaque couche a une variante ; `Verdict.reason` byte-identique à aujourd'hui (test doré sur 8 cas : règle, déclaration deny, déclaration auto contre règle, déclaré, mode ask, mode auto, réseau, config_set) ; `config_set_asks_twice_for_sensitive_settings_even_with_an_always_rule` inchangé | M |
| T07 | `ApprovalCard` typée et `Approver` | `agent/pipeline/approval.rs`, telegram.rs (lecture du payload par le type) | T05 | `serde` produit le même JSON qu'aujourd'hui (test doré) ; aucun chemin ne rend `Auto` sans `VerdictLayer` ∈ {Rule, ServerDeclaration, DeclaredAllow, SessionMode} (test) | M |
| T08 | `ExecutionMode` dérivé de `ToolSpec.parallel_safe` ; `schedule.rs`, `effect.rs`, `record.rs` | `penelope-tools/src/spec.rs`, `agent/pipeline/{schedule,effect,record}.rs` | T05 | test : l'ensemble des specs `parallel_safe` est exactement `PARALLEL_SAFE` d'aujourd'hui ; la liste est supprimée ; les 3 tests #85 inchangés | M |
| T09 | `ports.rs` : `AgentServices` et traits `SessionModes`, `SessionInfo`, `PromptSnapshots`, `CacheAudit` ; plus aucun `crate::` dans `agent/` | `agent/ports.rs`, runtime.rs (`Services::agent()`), cache_audit.rs, prompt_snapshot.rs, approval_mode.rs, budget_alert.rs (`usd` → kernel `budget::usd`) | T03 à T08 | `grep -rn 'crate::' penelope-daemon/src/agent/` ne rend que `crate::agent::` ; `AgentServices::for_tests` sur `Store::open_memory` fait tourner les 42 tests | L |
| T10 | Crate `penelope-agent` | `crates/penelope-agent/`, `Cargo.toml` (16 lignes + 1), archtest, daemon `agent.rs` = façade | T09 | `cargo check -p penelope-agent` seul ; règle archtest ajoutée et `ca_3_1` vert ; aucun import changé hors du daemon ; 42 tests dans le crate | L |
| T11 | Sous-agents, étapes de workflow et `skill_install` appellent le crate directement | workflow.rs:1305-1360, 1499-1522 ; skill_install.rs:137 | T10 | le sous-agent transforme toujours `AwaitingApproval` en erreur (test existant `workflow`) | S |
| T12 | `Inbox` et `Checkpoint::BeforeModelCall` : l'absorption sort de `request_messages` | `penelope-agent/src/steering.rs`, conversation.rs:171-238, engine.rs:582-584 | T10 | `request_messages` est sans effet de bord (test : deux appels consécutifs, une seule absorption) ; `a_running_turn_absorbs_a_new_message_before_the_next_model_call` et le test de rafale de runner.rs inchangés | M |
| T13 | `Checkpoint::BetweenCalls` : appels non démarrés « Non exécuté : nouveau message » | `steering.rs`, `pipeline/schedule.rs` | T12 | test : 3 lectures + 1 écriture, message pendant la 1re → la 1re finit, les 3 autres sont « Non exécuté », le message est écrit, le modèle est rappelé une fois ; transcript complet sans `repair_pairs` | M |
| T14 | Note d'interruption après `/stop` pendant des outils | `steering.rs`, `pipeline/schedule.rs` | T08 | test : `/stop` pendant un lot → résultats « Non exécuté : arrêté » pour les non démarrés, `Injection::Note` au tour suivant, préfixe inchangé (empreinte `system_hash` égale) | S |
| T15 | Tentatives : `Attempt`, `AttemptSink`, câblage dans `ModelCaller` et la relance vide | `penelope-agent/src/attempts.rs`, `model/mod.rs`, `turn.rs` | T03 | impl mémoire en test : flux coupé après 200 caractères → 1 tentative avec le texte ; 3 replis → 3 tentatives, un seul `usage.record` ; relance vide → tentative `request_only_note` ; `request_messages()` identique avec et sans | M |
| T16 | Tentatives : table `turn_attempts`, purge, `penelope logs --turn`, message d'échec corrigé, événement `llm.attempt` | store `migrations.rs`, daemon (impl du port, purge.rs, rpc/CLI logs), `docs/runtime-events.md` | T15 | purge de session vide la table ; plafond 10 par tour ; le message ne dit plus « début affiché » ; test `docs` vert | M |
| T17 | Jobs : table `tool_jobs` et `JobStore` sur le modèle de `mcp_tasks` | `penelope-mcp/src/tasks.rs` (ou nouveau module kernel), migrations, purge.rs | aucune | `create / due / update / recover_on_boot` testés comme `tasks.rs:255-333` ; purge RGPD étendue (test de #78) | M |
| T18 | Jobs : `Execution::Job`, `shell_exec background: true`, outils `job_*` à la demande | `pipeline/jobs.rs`, `pipeline/effect.rs`, executor.rs (`shell_exec`), tools/spec.rs, tools_on_demand | T08, T17 | `sleep 30` en arrière-plan : le tour répond ; l'effet reste `dispatching` ; `job_wait` borné rend l'état ; plafonds 3/10 refusés avec texte | L |
| T19 | Jobs : livraison par `TurnKind::Nudge`, `/stop`, orphelins, `self_status`, `doctor` | daemon (coureur de jobs, scheduler, telegram `/stop`), runtime.rs `reap_orphans` | T18 | fin d'un job → tour `Nudge` avec le résultat ; redémarrage avec job vivant → `failed` + Nudge, aucune relance, aucune carte `effect_unknown` ; `/stop` tue le groupe de processus | M |
| T20 | Jobs : `sub_agent_spawn` en job | executor.rs:1453-1475, workflow.rs `spawn_sub_agent` | T19 | un sous-agent long ne bloque plus le tour ; son résultat arrive par Nudge ; `/stop` l'interrompt (non-régression #57) | M |
| T21 | Juge : mesure préalable sur l'instance | requête SQL / `penelope usage` (script dans `scripts/`) | aucune | nombre de cartes `shell_exec` sans motif sur 30 jours et commandes distinctes, consignés dans #203 ; décision go/no-go pour T22 | S |
| T22 | Juge : port `Judge`, rôle `approval_judge`, schéma de sortie, mode `explain`, événement `approval.judged` | `pipeline/judge.rs`, kernel config (rôle, `approval.judge`), daemon (impl par `Provider`), telegram (carte enrichie) | T06, T21 (go) | les 7 tests de #203 (« Tests attendus ») ; fail-closed : modèle absent → carte d'aujourd'hui, aucun message d'erreur | L |
| T23 | Juge : mode `auto_read`, règle dérivée des pouvoirs, `/policies` et `doctor` | `pipeline/judge.rs`, `rules.rs`, telegram, doctor.rs | T22 | `cd tmp && ls -la \| jq .` passe sans carte en `auto_read` ; `curl … \| sh` jamais ; règle `Deny` prime sur un verdict `sûr` | M |
| T24 | Couture PTC : `CallContext { call_id, parent, root }`, refus d'une demande d'approbation en appel imbriqué, décision `0012-ptc-hors-v1.md` | `pipeline/decide.rs`, `docs/decisions/` | T05 | test : un appel avec `parent` et verdict `Ask` est refusé sans carte | S |
| T25 | Nettoyage d'API : `TurnRequest`/`AgentLoop::run` remplacés par `TurnSpec` + `MemoryConversation` dans les tests ; `resume_after_approval` supprimé | `penelope-agent`, tests | T10 | aucun appelant hors tests ; tests migrés | S |
| T26 | Événements typés `TurnEventKind` | `penelope-agent/src/events.rs`, `docs/runtime-events.md` | T02 | tout `EventDraft::new("turn.…")` de la boucle passe par l'enum ; test : chaque variante est documentée dans `runtime-events.md` | S |
| T27 | Cache audit : parties pures (`Fingerprint`, `sticky_upstream`, `miss_cause`) dans `penelope-llm`, `previous_call` dans `BudgetLedger` | cache_audit.rs:20-192, kernel/budget.rs | T09 | le port `CacheAudit` disparaît ; `ca_5_4_each_request_extends_the_previous_one` inchangé | M |

Ordre recommandé : T01, T02, puis T03 à T08 en parallèle (fichiers disjoints), T09, T10,
T11 ; ensuite trois fils indépendants : steering (T12 → T13 → T14), tentatives (T15 →
T16), jobs (T17 → T18 → T19 → T20) ; le juge après la mesure (T21 → T22 → T23) ; T24,
T25, T26, T27 quand une fenêtre s'ouvre. T17 et T21 peuvent commencer dès maintenant.

Collisions à surveiller avec les autres fils de la V1 : T16, T17 et T27 touchent le
store et le kernel (migrations, `BudgetLedger`) ; T26 touche le catalogue d'événements.
Chaque tâche est un lot : code, section `### x.y.z` dans `docs/progress.md`, `make bump`.

---

## 6. Ce qui ne doit pas régresser

### 6.1 Les 42 tests d'`agent.rs`

| Test (ligne) | Ce qu'il protège | Issue / CA |
|---|---|---|
| `clone_always_rule_is_limited_to_the_source_origin` (2152) | motif `$origin` d'un `git_clone`, `file://` et chemin local sans règle | #67 |
| `approving_clone_always_creates_only_a_scoped_rule` (2167) | « Toujours » sur `git_clone` écrit une seule règle bornée à l'origine | #67 |
| `a_plain_answer_finishes_in_one_iteration` (2690) | une réponse sans outil finit en une itération, aucun exécuteur appelé | |
| `read_tools_run_without_approval` (2709) | une lecture ne crée pas de carte | §9 |
| `reads_requested_together_run_together_and_keep_their_order` (2729) | lectures en parallèle (< 600 ms pour 3 × 300 ms), échec isolé, ordre des résultats | #85 |
| `a_write_between_reads_is_a_barrier` (2773) | une écriture attend la lecture d'avant et bloque celle d'après | #85 |
| `stop_interrupts_a_whole_batch_of_reads` (2815) | `/stop` interrompt les trois lectures en moins de 2 s, `Cancelled` | #57, #85 |
| `write_tools_suspend_the_turn_for_approval` (2850) | aucun effet avant approbation, une demande avec `call_id` | §9 |
| `an_approved_call_runs_on_resume_without_asking_again` (2880) | reprise avant décision = même demande, pas de doublon ; après = exécution sans redemander ; transcript complet | §9.2 |
| `only_non_idempotent_effects_pay_a_durable_commit` (2943) | 0 fsync pour les lectures, 2 pour un effet non idempotent | #75 |
| `an_uncertain_effect_marked_done_is_replayed_not_rerun` (3040) | « C'est fait » rejoue sans relancer, aucune règle même « toujours », une seule demande par redémarrage | #83 |
| `an_uncertain_effect_retried_runs_once` (3084) | « Relancer » exécute une fois | #83 |
| `an_uncertain_effect_ignored_is_not_rerun` (3103) | « Ignorer » laisse `failed`, le modèle l'apprend ; « Autoriser » sans choix d'effet est refusé | #83 |
| `a_request_without_arguments_never_creates_a_rule` (3141) | budget ou demande sans arguments : aucune règle | #83, régression de #67 |
| `a_denied_call_is_reported_to_the_model` (3170) | la raison du refus revient au modèle, rien n'est exécuté | §9 |
| `always_decision_creates_a_revocable_rule` (3209) | « Toujours » crée une règle `Auto` sur l'outil (bornée par motif) | §9.2 |
| `a_session_window_creates_a_rule_bound_to_the_session` (3236) | fenêtre `Session` avec `window_ref` | §9.2 |
| `a_second_decision_does_not_win` (3262) | la première décision gagne | CA 9.1 |
| `completed_effects_are_replayed_not_reexecuted` (3292) | même appel, même session : rejoué depuis le ledger | §4.2, CA 17.2 |
| `tool_errors_go_back_to_the_model` (3325) | le tour continue après une erreur d'outil ; effet `failed` | §8.4 |
| `loop_detector_aborts_the_turn` (3352) | 8 appels identiques → `LoopAborted` avec rapport et 3 choix par défaut | §11 |
| `a_stopped_loop_still_answers_with_the_real_error_and_choices` (3382) | réponse sans outil citant l'erreur, choix extraits, note dans le transcript, rien relancé au message suivant | #31 |
| `choices_are_split_from_the_answer` (3470) | analyse de la ligne `CHOIX :` | #31 |
| `cancellation_stops_the_turn` (3481) | jeton levé avant le tour → `Cancelled` sans appel | #57 |
| `exceeded_budget_stops_before_calling_the_model` (3498) | plafond atteint : aucun appel modèle, une carte | #32, CA 10.4 |
| `a_costly_turn_asks_before_going_on` (3545) | palier de 1 $ reprises comprises, texte de la carte, mention du coût, refus = arrêt | #19 |
| `the_call_cap_spans_resumptions_and_suggests_delegating` (3631) | nudge au 10e appel, plafond de 24 sur le tour entier, préfixe `CALLS_EXHAUSTED` | #19, #139 |
| `the_budget_message_names_the_key_of_the_scope_reached` (3682) | texte du plafond par périmètre | #4 |
| `tools_outside_the_allowlist_are_refused` (3700) | liste blanche : refus sans carte | §12 |
| `deltas_are_streamed_to_the_sink` (3725) | fragments vers le sink, réponse enregistrée | §1 |
| `a_transient_failure_falls_back_to_the_next_model` (3748) | une attente puis repli d'alias | #50, CA 10.3 |
| `network_is_granted_to_a_command_family_never_to_the_shell` (3772) | réseau borné à la famille ; règle sans réseau ne le donne pas | #106 |
| `an_always_rule_is_bounded_to_the_call_it_was_granted_for` (3824) | familles, `;`/`&&`/`$()`/redirection redemandent, répertoire, origine, MCP sans motif | #67 |
| `a_quoted_query_url_is_not_chaining_and_its_family_applies` (3918) | `&` entre guillemets, `VAR=x cmd`, tube vers `jq`, affectations qui détournent | #141 |
| `a_transient_error_before_the_stream_is_retried_with_openrouter` (3996) | nouvel essai tracé `llm.retried` | #50 |
| `repeated_connection_timeouts_say_how_many_attempts_were_made` (4019) | « 4 tentatives, 7 s » | #50 |
| `a_stop_during_the_retry_wait_ends_the_turn` (4046) | `/stop` pendant l'attente : aucun nouvel appel | #50 |
| `a_stream_cut_before_any_text_is_retried_then_falls_back` (4080) | coupure avant texte : essai puis repli ; après texte : échec explicite, un seul appel | #5 |
| `an_empty_answer_is_retried_once_then_reported` (4128) | une relance non enregistrée, puis erreur nommant le modèle | |
| `pending_calls_ignore_answered_and_abandoned_ones` (4177) | appels répondus et abandonnés | |
| `effect_kinds_and_servers_are_derived_from_names` (4197) | `EffectKind` et serveur par préfixe | |
| `blind_models_get_a_mention_instead_of_images` (4208) | images retirées pour un modèle aveugle, gardées sinon | §10.4 |

### 6.2 Tests voisins de la boucle

- `engine.rs` (31) : `claimed_messages_keep_separate_user_entries_and_arrival_times`,
  `a_running_turn_absorbs_a_new_message_before_the_next_model_call` (#161),
  `explained_errors_keep_the_loop_guard_and_tool_call_rules_stay_bounded` (#110),
  `invalid_calls_are_refused_before_any_approval_card` (#117),
  `an_and_list_gets_a_rule_per_family_and_stops_asking` et
  `a_composed_line_records_that_no_rule_was_written` (#150),
  `shell_commands_are_classified_before_asking` (#111),
  `a_cd_into_the_workspace_is_the_working_directory` et
  `always_after_a_cd_rules_the_real_command` (#123),
  `a_quoted_query_url_is_ruled_by_its_family` et
  `a_declared_family_covers_a_quoted_query_url` (#141),
  `always_on_a_multiline_heredoc_resumes_without_panic` (#130),
  `a_copied_key_is_stored_masked_and_executed_whole` (#134),
  `config_set_asks_twice_for_sensitive_settings_even_with_an_always_rule`,
  `the_model_reaches_mcp_tools_through_the_supervisor`,
  `replaying_a_turn_does_not_duplicate_the_user_message`,
  `an_approval_suspends_then_a_resume_turn_finishes_the_work`,
  `the_sticky_model_is_revisited_at_boundaries` (#82).
- `conversation.rs` (12) : `parallel_tool_results_are_admitted_as_one_group` (#52),
  `a_lone_tool_result_is_left_whole`, `huge_tool_results_are_externalised`,
  `recorded_messages_come_back_in_the_request`,
  `the_projection_only_reads_what_is_not_summarised` (#55),
  `the_stable_prefix_does_not_move_between_turns`.
- `runner.rs` (6) : rafale sans appel modèle, rafale pendant les outils, réponse fusionnée
  vers le dernier message, pool, panique (#84), lease perdu (#43).
- `executor.rs` (30), en particulier `invalid_clone_source_is_rejected_before_approval_even_via_tool_call`,
  `invalid_arguments_are_rejected_before_running`,
  `an_invalid_mcp_call_is_explained_without_reaching_the_server`,
  `mcp_meta_tools_are_read_only_but_calls_carry_the_target_risk`,
  `a_rare_native_tool_is_found_and_called_like_a_direct_one` (#104),
  `a_command_asking_for_network_is_an_external_action` (#106),
  `every_native_tool_execution_is_in_the_runtime_log`.
- `tools/loops.rs` (7), `kernel/effects.rs` (8), `hitl/policy.rs` et `hitl/lib.rs`,
  `approval_mode.rs` (2), `cache_audit.rs` (2).

### 6.3 Critères d'acceptation concernés

CA 3.2 et 3.3 (kernel/turn.rs:1091, :976), CA 4.2 (kernel/effects.rs:541), CA 5.4
(cache_audit.rs:285), CA 9.1, 9.2, 9.3 (hitl/lib.rs:702, :729 ; hitl/policy.rs:861),
CA 10.3 et 10.4 (llm/router.rs:733 ; kernel/budget.rs:916), CA 13.1 et 13.2
(evals/tests/security.rs:11, :40 ; observe/redact.rs:735), CA 17.1 à 17.7
(evals/tests/resilience.rs), dont `an_idempotent_tool_may_be_retried_without_a_human`
et `double_planning_inside_one_process_is_rejected`. La matrice (`docs/ca-matrix.md`)
se régénère avec `UPDATE_CA_MATRIX=1` si un test `ca_*` s'ajoute (candidats : un
`ca_9_4` pour le fail-closed du juge, un `ca_17_8` pour un job survivant au redémarrage).

---

## 7. Risques

1. **Dérive de comportement pendant l'extraction.** Les textes de `verdict.reason`, des
   refus et des cartes sont lus par Telegram, le CLI et les tests. Parade : tests dorés
   sur le JSON des cartes (T07) et sur les huit raisons (T06) avant tout déplacement.
2. **Le steering `BetweenCalls` change l'ordre du transcript.** Un message écrit entre
   un lot et le suivant doit toujours suivre des résultats complets, sinon
   `pending_calls` abandonne des appels et la projection répare à l'aveugle. Parade :
   T13 écrit les « Non exécuté » avant le message, test de forme du transcript.
3. **Jobs contre ledger.** Un effet `dispatching` pendant des heures est nouveau. La
   règle « job mort = `failed` + Nudge, jamais `unknown` » doit être écrite avant T18,
   sinon un redémarrage produit des cartes `effect_unknown` pour des `cargo test`.
4. **Le juge ouvre une porte.** Parades : jamais appelé sous `Deny`, `AskTwice`,
   `Destructive` ni règle existante ; `off` par défaut ; `auto_read` refuse tout
   pouvoir hors lecture dans les workspaces ; tests d'injection réels ; la mesure (T21)
   peut conclure que l'étape ne vaut pas son coût.
5. **Cache de préfixe.** Toute note système insérée avant le dernier message
   utilisateur change la chaîne dès son index : un raté « historique » à l'appel où
   elle apparaît, un autre à celui où elle disparaît (cache_audit.rs:177-187). Les
   `Injection::Note` vont en queue ; la note de fusion actuelle est à déplacer (§3.4).
6. **Frontière de crate sans règle.** `dependency_violations` ignore un crate absent de
   la table (archtest:240-262) : la règle est à ajouter dans le même lot que le crate.
7. **Harnais de test du crate.** Les 42 tests utilisent `Services::for_tests`. Le crate
   a besoin d'un `AgentServices::for_tests` équivalent (store en mémoire, ledgers,
   catalogue, `MockProvider`) : c'est l'essentiel du coût de T09/T10, pas le déplacement.
8. **Concurrence avec les autres fils de la V1** (kernel événementiel, gel et outillage) :
   migrations (T16, T17), `BudgetLedger` (T27), catalogue d'événements (T26). Prendre
   le numéro de version suivant au rebase, c'est la serrure prévue (CLAUDE.md).
9. **Tests macOS non compilés localement** : aucun test `cfg(target_os = "macos")` dans
   `agent.rs`, mais le daemon en a ; une signature de `ToolExecutor` ou de `TurnSink`
   qui change doit être relue à la main (`grep -rn 'cfg(target_os = "macos")' crates/`).
10. **Volume des tentatives et données personnelles** (#206) : partiels de réponses en
    base ; purge et rédaction livrées dans le même lot que la table (T16).
