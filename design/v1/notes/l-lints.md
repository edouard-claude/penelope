# Notes de livraison : clôture, fonctions trop longues (épopée #208, critère 2 de switch-check)

Branche `v1-l-lints`, dérivée de `v1` à `9037fda` (1.0.0-alpha.17), poussée sur `origin`.
Aucune migration, aucun changement de comportement voulu.

## Ce qui est livré

Les dix-neuf fonctions marquées `#[allow(clippy::too_many_lines)]` hors `engine.rs` passent
sous le seuil de 200 lignes du workspace (`clippy.toml`), découpées par étapes nommées ;
chaque allow est retiré. `[lints].allow_too_many_lines` descend de 20 à 1 par
`UPDATE_BUDGET` : reste `engine.rs:340`, que `t33-moteur` refond.

| Fonction (lignes avant) | Découpe |
|---|---|
| `penelope-ops` `purge::session` (209) | `session_files`, `purge_session_channel`, `purge_session_work` (struct `SessionWork`), `remove_session_files` |
| `penelope-kernel` `Config::validate` (232) | cinq étapes dans l'ordre d'origine : propriétaire et canaux, modèles, bornes du tour, échéances, MCP et runtime |
| `penelope-executor` `selfknow::status` (217) | `model_sections`, `config_summary` |
| `penelope-telegram` `classify` (221) | `classify_callback` |
| `penelope-telegram` `commands::all` (340) | six familles concaténées, même ordre |
| `penelope-telegram` `builtin_templates` (290) | cinq familles concaténées, même ordre |
| `penelope-workflow` `ticket_to_deploy` (276) | `ticket_to_deploy_parameters`, `ticket_plan_steps`, `ticket_build_steps`, `ticket_deploy_steps` |
| `penelope-cli` `route` (259) | `session_route`, `mcp_route`, `schedule_route`, `mem_route`, exhaustives sur leur enum |
| `penelope-orchestrator` `verify_step` (255) | `check_step`, `repository_head`, `evidence_references`, `mark_criteria` |
| `penelope-agent` `run_steps` (294) | `record_usage`, `empty_answer` |
| `penelope-dream` `apply` (270) | `supersede_entry`, `update_exception`, `create_entity` sur un `ApplyCtx` commun |
| `penelope-dream` `run_locked` (398) | `harvest_decisions`, `gate_groups`, `consolidate_admitted` (struct `Night`), `ReasoningBudget::starved`, `after_pass` |
| `penelope-gateway-telegram` `handle` (225) | `awaited_answer` |
| `penelope-gateway-telegram` `callback` (325) | `approval_clicked` |
| `penelope-gateway-telegram` `perform` (458) | `perform_run`, `perform_burst`, `perform_admin`, `perform_memory` |
| test `the_grid_updates_journals_and_ages_the_memory` (232) | aide `first_night_is_sorted_dated_and_journaled` |
| test `a_full_simulated_journey_leaves_a_valid_markdown_wiki` (249) | aide `assert_valid_wiki` |
| test `mcp_links_and_mrtr_elicitations_from_telegram` (223) | aide `tracker_and_drive` |
| test `workflow_approval_confirmation_stays_in_the_cards_topic` (214) | aide `fs_write_approval` |

## Choix

- **Ordre gardé partout où il se voit.** `validate` rend la même première erreur ; les
  catalogues (commandes, gabarits, étapes) sont concaténés dans l'ordre d'origine
  (`setMyCommands`, `/help`, graphe du workflow inchangés).
- **Aucune signature publique touchée.** Les nouvelles fonctions sont privées au module ;
  dans la passerelle, ni signature existante ni `use` modifiés (collision avec
  `t33-moteur`) : les aides sont ajoutées dans le même `impl`, après la fonction découpée.
- **Famine de raisonnement (#152).** Les quatre variables du budget de raisonnement de la
  nuit deviennent `ReasoningBudget` ; `starved` rend `true` quand le même lot est rejoué
  (doublement, seconde chance au plafond, bascule sur le repli), mêmes messages ;
  l'appelant garde l'arrêt de passe et le report des candidats restants.
- **Frontière canal.** L'étape Telegram de la purge s'appelle `purge_session_channel`, son
  paramètre `chat` : la découpe ne nomme pas le canal plus que le code d'origine
  (`purge.rs` passe même de 17 à 16 mentions, budget descendu).
- **Tests.** Les scénarios longs restent un seul test (ils enchaînent des nuits ou des
  clics sur un même état) ; une aide nommée porte la mise en place ou un bloc d'assertions.

## Vérifications

- `cargo fmt --all --check` : propre.
- `cargo clippy --workspace --all-targets -- -D warnings` : propre.
- `cargo test --workspace --no-fail-fast` : 2 076 verts, 1 rouge,
  `crates_stay_under_their_ceiling` (ci-dessous).

## Blocage

- **Plafond `[crates]` du daemon : 14 722 lignes pour 14 719.** La seule fonction longue
  du daemon hors moteur est le test `wiki_e2e.rs` ; son aide coûte trois lignes au
  minimum (signature, accolade, ligne vide), le commentaire « Validateur. » ayant été
  remplacé par l'appel. Rien d'autre dans le fichier ne se resserre sans toucher aux
  assertions. La refonte du moteur (T33) fait redescendre le daemon de bien plus ; à
  défaut, l'intégrateur pose le plafond à 14 722.

## Notes de version

#### Fonctions trop longues : dix-neuf découpes, un seul allow restant

Les dix-neuf fonctions qui dépassaient le seuil de 200 lignes de clippy sous un
`#[allow(clippy::too_many_lines)]` du gel 0.17, hors moteur de tours, sont découpées par
étapes nommées, sans changement de comportement : purge d'une session, validation de la
configuration, statut de soi, classement des updates, catalogues de commandes et de
gabarits, workflow `ticket-to-deploy`, routage de la CLI, étape `verify`, boucle d'agent,
application et passe de consolidation du rêve, passerelle Telegram (message, clic,
opérations d'écran), et quatre tests de scénario. `[lints].allow_too_many_lines` descend
de 20 à 1 ; le dernier, `engine.rs`, part avec la refonte du moteur (T33).
