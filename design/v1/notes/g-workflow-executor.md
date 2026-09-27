# Lot G : workflow.rs et executor.rs découpés (épopée #208, T15 et T16)

## Livré

Branche `v1-g-workflow-executor`, six commits :

1. `workflow.rs` (2 881 lignes) devient `workflow/` : `mod.rs` (constantes, `State`,
   `StepOutcome`, clés kv, brief), `start`, `driver`, `control`, `step_ctx`, `step_agent`,
   `step_shell_tool`, `step_user`, `step_compose`, `step_verify`, `orchestrator`. Le plus
   gros, `driver.rs`, fait 438 lignes.
2. `UPDATE_BUDGET=1` : `workflow.rs` sort de `[files.oversized]` et de
   `[daemon.daemon_users]`.
3. Dérogation (`Dérogation-budget: #208`) : les 20 mentions de `Daemon` suivent les
   sous-modules (driver 8, control 3, start 3, mod 2, orchestrator 2, step_agent 1,
   step_ctx 1) ; plafond du daemon 82 213 -> 82 273.
4. `executor.rs` (2 504 lignes) devient `executor/` : `mod.rs` (traits, `ToolEnv`,
   `NativeToolExecutor`, `new`, aides de workspace, `dispatch`), `meta`, `precheck`,
   `defs`, `args`, et `tools/` : `fs_shell`, `git_http`, `self_tools`,
   `schedules_messaging`, `memory`, `skills_workflows`, `misc`. Le plus gros, `mod.rs`,
   fait 520 lignes.
5. `UPDATE_BUDGET=1` : `executor.rs` sort de `[files.oversized]` et de
   `[channel.allowed]` ; `allow_too_many_lines` 26 -> 25.
6. Dérogation : les 13 mentions du canal suivent (schedules_messaging 7, mod 5,
   skills_workflows 1) ; plafond du daemon 82 273 -> 82 585.

## Choix

- Déplacement pur : corps identiques (vérifié par comparaison sans espaces) ; rustfmt
  replie quelques signatures et chaînes que le nouveau contexte rend plus longues ou plus
  courtes. Visibilités élargies à `pub(super)` seulement (fonctions d'étape, `StepCtx`, ses
  champs et ses méthodes, méthodes de `meta.rs`, aides de `args.rs`).
- Étapes de workflow à plat (`workflow/step_*.rs`) plutôt que sous `steps/` : un niveau de
  plus aurait exigé `pub(in crate::workflow)`, hors de la règle `pub(super)`.
- Pas de `orchestrator/workflow/` comme dans decoupage-daemon.md §5.4 : le brief demande
  un répertoire du même nom ; le module `orchestrator` viendra avec son crate.
- `dispatch` garde méta-outils, spécification et validation puis appelle
  `tools::native`, aiguillage court par nom d'outil vers treize fonctions de famille de
  même signature (`fs_tools`, `shell_tools`, …, `misc_tools`), chacune sous 200 lignes :
  plus aucun `allow(clippy::too_many_lines)` dans executor. Les bras sont déplacés tels
  quels ; `shell_tools` porte un `allow(clippy::needless_return)` parce que son unique
  bras finit par un `return` qui n'était pas en position finale dans `dispatch`.
- Plafond du daemon : +372 lignes au total, sans code nouveau (en-têtes de modules,
  réexportations, enveloppes des familles, liste des noms dans l'aiguillage).

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (1 915 tests, dont scénarios, rpc_golden, docs, ca_matrix,
telegram_e2e, approval_e2e, et les tests macOS d'executor compilés sur ce Mac) : verts,
aucun attendu modifié. Aucun test `ca_*` n'a changé de fichier.

## Notes de version

#### workflow.rs et executor.rs découpés en modules (épopée #208, lot G)

Les deux fichiers sortent de la liste de référence du gel : `workflow/` (onze modules,
le plus gros à 438 lignes) et `executor/` (treize modules, le plus gros à 520 lignes).
La table de dispatch des outils natifs (1 160 lignes) devient un aiguillage court vers
une fonction par famille d'outils ; son `allow(clippy::too_many_lines)` disparaît.
Déplacement pur, aucune signature publique changée, aucun comportement modifié.

## Blocages

Aucun.
