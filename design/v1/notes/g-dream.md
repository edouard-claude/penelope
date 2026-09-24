# Lot G : découpage de `dream.rs` (agent g-dream)

## Livré

`crates/penelope-daemon/src/dream.rs` (3 457 lignes) devient `dream/`, selon
`design/v1/decoupage-daemon.md` §5.2 :

| Module | Contenu | Lignes |
|---|---|---|
| `mod.rs` | en-tête, imports, constantes, `DreamOutcome`, `Trigger`, `run`, `run_as`, `run_locked`, réexports | 633 |
| `candidates.rs` | `submission_order`, `core_overflow`, `Clash`, `contradiction`, `Item`, `Neighbour`, `nearby_*`, `ids_for`, `is_journal`, `sort_and_plan` | 349 |
| `clash.rs` | `ask_about_clash`, `file_unanswered_clash`, `expired_journal`, `unused_entries` | 172 |
| `snapshot.rs` | `VaultSnapshot`, `markdown_files` | 127 |
| `consolidate.rs` | `CONSOLIDATION_PROMPT`, `usage_note`, `CallOutcome`, `consolidate` | 274 |
| `batches.rs` | `BatchSizer`, `OutputBudget`, `write_batch`, repli de raisonnement, `output_cap`, `batch_event`, `consolidate_retrying`, pannes réseau et délais | 506 |
| `apply.rs` | `target_file` à `wiki_review` | 563 |
| `runs.rs` | ledger des passes, `last_report`, `history`, `restore`, `learned` | 294 |
| `digest.rs` | familles de rejets, `night_summary`, `digest_text` | 307 |
| `nightly.rs` | `system_crons`, `nightly`, échecs de la nuit, `vault_sync`, `vault_check`, `vault_path` | 307 |

Commits : le déplacement (un seul, le fichier se découpe en une étape), le déplacement à la
main de l'entrée `[daemon.daemon_users]` (`Dérogation-budget: #208`), la sortie de
`dream.rs` de `[files.oversized]` par `UPDATE_BUDGET=1`.

## Choix

- Déplacement pur : le fichier reconstitué à partir des modules (en-têtes retirés,
  `pub(super) ` effacé) ne diffère de l'original que par deux signatures repliées par
  rustfmt (`contradiction`, `ask_about_clash`).
- Modèle du lot F (`agent/`) : chaque sous-module commence par `use super::*;`, `mod.rs`
  importe explicitement ce dont les frères et les tests ont besoin. `pub(super)` n'est posé
  que là où un autre fichier s'en sert (vérifié : aucun import inutilisé) ; les champs
  élargis sont ceux que le compilateur a demandés. Les imports servant aux seuls tests sont
  sous `#[cfg(test)]`.
- Écarts à la spécification, voulus par le brief (déplacer sans casser de cycle) :
  `core_overflow` reste dans `candidates.rs` et `vault_sync` dans `nightly.rs` ; leur départ
  vers vault et `vault_git` appartient à un lot suivant. `consolidate` est coupé en
  `consolidate.rs` et `batches.rs` pour garder de la marge sous 800 lignes.
- Les 20 occurrences de `Daemon` se répartissent sans changer de total : nightly 5,
  batches 4, mod 4, apply 2, candidates 2, clash 1, consolidate 1, digest 1.
- Aucun test `ca_*` dans dream : `docs/ca-matrix.md` n'a pas changé.

## Blocage

Le plafond de crate `[crates] "penelope-daemon" = 82213` n'avait aucune marge : le
découpage ajoute 75 lignes (en-têtes des neuf modules, déclarations et réexports de
`mod.rs`, replis rustfmt), soit 82 288. `crates_stay_under_their_ceiling` est rouge ; le
relèvement est laissé à l'intégration (comme `d915731`), hors du périmètre de ce lot.

## Notes de version

#### Consolidation nocturne découpée en modules

`dream.rs` (3 457 lignes) devient le répertoire `dream/` : dix modules de 127 à 633 lignes
(passe et phases, candidats, contradictions, instantané du vault, appel du modèle, lots,
application au vault, ledger, digest, crons). Déplacement pur : aucun comportement ni aucun
chemin public ne change, et le fichier sort de la liste de référence du gel (épopée #208,
lot G).
