# Lot K : jobs d'outils, écarts (épopée #208, T17 à T20)

Agent `k-jobs`, branche `v1-k-jobs`, base 1.0.0-alpha.12 (`3359b0d`).
Spécification : `design/v1/boucle-et-outils.md` §3.5, §5 (T17 à T20), §7 risque 3.
Existant : #204 (0.17.60), décision 0012, `penelope-executor/src/jobs.rs` (magasin et
outils `job_*`), `penelope-daemon/src/tool_jobs.rs` (lancement, livraison, tests).

## 1. Relevé d'écart, avant le lot

Légende : tenu (le test le prouve), partiel, manquant. Les tests du daemon sont dans
`crates/penelope-daemon/src/tool_jobs.rs` sauf mention.

### T17 : table `tool_jobs` et `JobStore`

| Critère | État | Preuve |
|---|---|---|
| `create` testé | tenu | `a_job_lives_until_its_result_lands` |
| `due` (livraison due) testé | tenu | `a_finished_job_is_delivered_exactly_once` (`undelivered`, `mark_delivered`) |
| `update` testé | tenu | `the_first_terminal_state_wins` (`finish`) |
| `recover_on_boot` testé | tenu | `a_running_job_is_lost_on_restart_and_never_replayed` |
| purge RGPD étendue | tenu | `penelope-ops/src/purge/tests.rs` : `purging_a_session_leaves_the_audit_chain_and_nothing_else` (colonnes `tool_jobs.request`, `result`), `retention_removes_what_is_past_its_age_only` |
| migration | tenu | `penelope-store/src/migrations/tests.rs` : `all_prd_tables_exist` |

Écart assumé (décision 0012) : pas de colonne `poll_at`, rien ne sonde un job natif.

### T18 : `Execution::Job`, `background: true`, outils `job_*`

| Critère | État | Preuve |
|---|---|---|
| `sleep 30` en arrière-plan : le tour répond | tenu | `a_background_shell_frees_the_turn_and_comes_back_as_a_nudge`, `an_owner_message_is_answered_while_a_job_runs` |
| l'effet reste `dispatching` | partiel | dit en commentaire, jamais vérifié |
| `job_wait` borné rend l'état | tenu | `the_model_can_follow_and_cancel_a_job` ; schéma borné : `penelope-tools/src/spec.rs` `long_tools_can_be_backgrounded_and_jobs_are_followed_on_demand` |
| plafonds 3/10 refusés avec texte | partiel | défauts dans `penelope-kernel/src/config/tests.rs` `tool_jobs_have_a_threshold_and_two_caps` ; refus par session seulement avec un plafond abaissé à 1 (`the_job_over_the_cap_is_refused_with_what_to_do`) ; plafond global jamais de bout en bout |

Le pipeline n'a pas de variante `Execution::Job` : le port `JobRunner` de
`penelope-agent` (`maybe_spawn`) rend un `ToolOutcome` `{job, state: "working"}`. Même
effet observable ; le renommage en variante typée relève de la découpe du pipeline, pas
de ce lot.

### T19 : livraison `Nudge`, `/stop`, orphelins, `self_status`, `doctor`

| Critère | État | Preuve |
|---|---|---|
| fin d'un job → tour `Nudge` avec le résultat | tenu | `a_background_shell_frees_the_turn_and_comes_back_as_a_nudge`, `the_delivery_loop_brings_the_result_back_on_its_own`, `a_job_finished_in_a_closed_session_is_delivered_when_it_reopens` |
| redémarrage avec job vivant → `failed` + Nudge, aucune relance | tenu | `a_restart_during_a_job_fails_it_and_asks_the_owner_once` |
| … aucune carte `effect_unknown` | **manquant** | le code posait la carte (l'effet suivait `dispatching` → `unknown`), et le test l'exigeait |
| `/stop` tue le groupe de processus | partiel | code tenu (`process_group(0)`, `kill -- -<pgid>`) ; `stop_cancels_the_jobs_of_a_session_and_leaves_no_orphan` lance un `sleep` seul, que tuer le shell suffit à arrêter |
| `/stop` Telegram coupe les jobs | tenu | `penelope-gateway-telegram/src/telegram/tests/delivery.rs` `stopping_cuts_the_tool_jobs_and_says_so` |
| `reap_orphans` connaît leurs pid | tenu | `stop_cancels_the_jobs_of_a_session_and_leaves_no_orphan` |
| `self_status` | tenu | `penelope-executor/src/selfknow.rs` `the_status_shows_the_living_tool_jobs` |
| `doctor` | tenu | `penelope-daemon/src/rpc/methods/doctor/tests.rs` `doctor_keeps_the_daemon_checks` |

### T20 : `sub_agent_spawn` en job

| Critère | État | Preuve |
|---|---|---|
| un sous-agent long ne bloque plus le tour | tenu | `a_sub_agent_can_run_as_a_job_and_report_back` |
| son résultat arrive par Nudge | tenu | même test |
| `/stop` l'interrompt (non-régression #57) | **manquant** | aucun test du chemin job ; `a_cancelled_turn_stops_its_sub_agent` (orchestrateur) passe un jeton déjà annulé à l'appel direct |

## 2. Livré

Quatre commits, un par critère manquant, tous dans `crates/penelope-daemon/tests/tool_jobs_e2e.rs`
(API publique du daemon) sauf le correctif :

1. `8bf4806` (T20) : `a_long_sub_agent_job_frees_the_turn_and_stop_interrupts_it`. Un
   sous-agent qui ne conclut jamais de lui-même tourne après la fin du tour ; l'arrêt
   de la session le coupe, plus aucun appel au modèle, job `cancelled`, effet `failed`,
   fin livrée par `Nudge`. Aucun changement de code.
2. `c9ecc98` (T19, comportement) : `JobStore::recover_on_boot` passe l'effet de chaque
   job perdu à `failed`, dans la même transaction et avant que le ledger ne tranche ses
   `dispatching`. Décision 0012 révisée, `docs/install-headless.md` corrigé. Le test de
   redémarrage devient `a_restart_during_a_job_fails_it_and_says_so_without_a_card` :
   aucune carte même après un second démarrage, relance qui nomme job, commande et
   cause, rien de rejoué. Il vérifie aussi l'effet `dispatching` pendant le job (T18).
3. `ac6fae1` (T19) : `stop_kills_the_whole_process_group_of_a_job`. Deux enfants en
   pipeline, aucun ne survit à l'arrêt (`pgrep`). Aucun changement de code.
4. `b61b25c` (T18) : `the_default_caps_refuse_the_fourth_job_of_a_session_and_the_eleventh_overall`,
   sur la configuration par défaut. Aucun changement de code.

## 3. Choix

- Tests nouveaux dans `tests/` et non dans `tool_jobs.rs` : le fichier est dans la liste
  de référence du gel (1 205) et `penelope-daemon/src` est à son plafond `[crates]`. Le
  test de redémarrage y est déplacé en changeant de critère ; `tool_jobs.rs` passe à
  1 136 (budget abaissé par `UPDATE_BUDGET`). Le daemon perd 70 lignes de `src`.
- Pas de carte pour un job mort : un job est observable (son processus meurt avec le
  daemon, `reap_orphans` tue un survivant). La carte de #83 reste réservée aux effets
  dont la complétion est inconnaissable. La règle « aucune relance automatique » ne
  change pas.
- `sub_agent_spawn` reste en job sur demande (`background: true`), comme #204 l'a
  tranché : la spécification (§3.5) cite l'outil parmi ceux que l'exécuteur peut
  détacher sans exiger que tout sous-agent le soit, et un sous-agent qui rend sa
  conclusion dans le tour reste le bon choix pour une question courte. Le texte de
  l'outil le propose.
- Une commande composée (`a | b`) demande l'accord même en mode `auto` : l'aide de test
  approuve une fois et reprend le tour, la carte n'est pas le sujet.
- Un appel identique à un effet en cours est écarté avant le plafond (« Appel déjà en
  cours ») : le test des plafonds lance des commandes distinctes.

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-fail-fast` : 81 suites, 2 026 tests, 0 échec, sur macOS
(dont `docs`, `penelope-archtest`). Les quatre tests de `tool_jobs_e2e.rs` passent trois
fois de suite. Le test de redémarrage échoue sans le correctif (vérifié).

## 5. Reste et blocages

- `Execution::Job` comme variante typée du pipeline : non fait, hors de ce lot (le port
  `JobRunner` rend l'équivalent). À reprendre avec la découpe du pipeline.
- `penelope-store/tests/two_writers.rs` cite encore l'ancien nom du test de redémarrage
  comme trace de l'incident de la 0.17.61 : laissé tel quel, c'est un historique.
- Aucune signature changée dans `penelope-agent`.

## 6. Notes de version (pour docs/progress.md)

#### Jobs d'outils : écarts de T17 à T20 comblés (épopée #208, lot K)

- Un redémarrage pendant un job d'outil ne pose plus la carte « C'est fait / Relancer /
  Ignorer » : le job et son effet passent à `failed` avec la raison, et la conversation
  reçoit le résultat comme pour tout job fini. Aucune relance automatique, comme avant
  (décision 0012, révisée).
- Nouveaux tests de bout en bout : un sous-agent long lancé en job et interrompu par
  `/stop` (#57), le groupe de processus d'un job entièrement tué par `/stop`, les
  plafonds par défaut (3 par conversation, 10 en tout) refusés avec leur texte.
