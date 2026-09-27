# Lot L : réexports de transition retirés, commands.rs découpé (épopée #208, T30)

Agent `l-transition`, branche `v1-l-transition`, base 1.0.0-alpha.14 (`9f13770`).
Spécification : `design/v1/decoupage-daemon.md` §6 (T30), charte §3.4 (800 lignes).

## Livré

| Tâche | Commit | Critère | Preuve |
|---|---|---|---|
| T30 | `e943ff6` | plus de `pub use` de transition dans le daemon ni dans la passerelle ; consommateurs sur les crates d'origine | `lib.rs` n'exporte que ses modules, `Daemon`, `VERSION` ; grep ci-dessous |
| T30 (fichiers) | `4776cef` | `tool_jobs.rs` sous 800 | tests dans `tool_jobs/tests.rs` ; sort de `[files.oversized]` |
| T30 (fichiers) | `48ae1e2` à `30d7e59` | `commands.rs` sous 800 | six modules fils, un par commit ; sort de `[files.oversized]` |
| T30 (docs) | `2c40dc7` | architecture.md, section des réexports | « Ce que le daemon garde », tableau des crates, « Ce qui reste » |

Ce que la CLI et les évaluations citent encore du daemon :

```bash
grep -rhoE 'penelope_daemon::[A-Za-z_]+(::[A-Za-z_]+)?' crates/penelope-evals crates/penelope-cli | sort | uniq -c
#  8 Daemon, 1 Daemon::new, 5 VERSION, 3 runner::process, 1 runner::run_pool, 1 runner,
#  1 rpc::Rpc, 2 compaction::context_of, 1 agent::decide_approval
```

## Découpages (avant → après, plus gros morceau)

| Fichier | Avant | Après | Plus gros morceau |
|---|---|---|---|
| `penelope-daemon/src/tool_jobs.rs` | 1 135 | 345 | `tool_jobs/tests.rs` 789 |
| `penelope-cli/src/commands.rs` | 2 128 | 225 | `commands/cli.rs` 644 (grammaire clap) |

`commands.rs` garde `run` (le répartiteur) et `daemon` (la composition du processus).
Les modules fils : `cli.rs` (644, `Cli`, `Command` et les sous-commandes), `route.rs`
(306, `route` et la lecture des valeurs), `offline.rs` (320, commandes hors daemon et
service), `interactive.rs` (398, `chat`, `onboard`, `model auth`), `restore.rs` (208),
`upgrade.rs` (74). Chemins publics inchangés : `commands::{Cli, Command, run, route}`.

`[files.oversized]` : 23 → 21 entrées. Plafond `[crates]` du daemon : 14 752 lignes
mesurées pour 14 847 accordées, laissé à l'intégrateur.

## Choix

- **Ce qui reste au daemon, et pourquoi.** `agent::{services_of, decide_approval,
  close_unopened, close_interrupted_turns}` : dériver `AgentServices` de `Services` ne peut
  pas descendre (`penelope-agent` ne nomme pas `Services`, qui porte le moteur de contexte,
  `reach.rs` ; `penelope-app` ne connaît pas la boucle). `workflow::{context_of,
  orchestrator_of}` et `compaction::context_of` : ils lisent des champs du `Daemon`
  (providers, bus, état des runs et des compactions) absents de `Services`.
  `selfknow::codex_view` : servi par `impl Admin for Daemon`.
- **Les enveloppes `&Arc<Daemon>` disparaissent.** Les dix de `workflow`, les six de
  `scheduler` et les deux de `dream` : l'appelant passe `&workflow::context_of(d)` à
  `penelope_orchestrator`, ou compose `penelope_dream::digest_text(&d.dream(),
  scheduler::digest_inputs(&d.services).await, mcp)`. Le module `scheduler` est retiré de
  la liste blanche R5 ; `dream`, `executor`, `ingest` deviennent des modules de tests
  (`#[cfg(test)] mod …`). Occurrences de `Daemon` : `workflow/mod.rs` 13 → 3,
  `scheduler/mod.rs` 7 → 0, `dream/mod.rs` 2 → 0.
- **Les `pub use` d'une autre crate dans les modules propres du daemon** (`runtime`,
  `engine`, `tool_jobs`, `approval_mode`, `cache_audit`, `prompt_snapshot`,
  `runtime_events`) deviennent des `use` privés : `penelope_daemon::runtime::Services`,
  `penelope_daemon::tool_jobs::store` ou `penelope_daemon::approval_mode::ApprovalMode`
  n'existent plus.
- **Le code interne du daemon nomme aussi les crates** (`crate::bus::` devient
  `penelope_app::bus::`, etc.), pas seulement les consommateurs : un `use` privé à la
  racine aurait gardé l'ambiguïté que T30 retire.
- **Nouvelles dépendances** (toutes internes, raison écrite dans chaque `Cargo.toml`) :
  la passerelle prend agent, conversation, dream, executor, ops, orchestrator, vault,
  mcp-host ; les évaluations app, agent, conversation, dream, executor, ops (et
  orchestrator en dev) ; la CLI agent (et app en dev). Aucune règle d'archtest ne
  contraint ces trois crates ; les niveaux ne changent pas.
- `engine.rs` et `tool_jobs.rs` auraient dépassé leur borne à cause des chemins plus
  longs que rustfmt replie : des `use` regroupés les tiennent sous la borne (1 091 et
  1 135).

## Non fait, et pourquoi

- **`engine.rs` (1 091) et `supervisor.rs` (930) restent au-dessus de 800.** Presque tout
  `engine.rs` est dans des blocs `impl Daemon` ou `impl Admin for Daemon`, que R6 réserve
  à quatre fichiers (`budget.toml [daemon].impl_daemon`) : un `engine/turn.rs` avec un
  `impl Daemon` serait refusé, et l'ajouter à la liste demande une dérogation. Les couper
  demande de convertir des méthodes en fonctions sur `&Services` ou sur un port (T33),
  pas un déplacement. `supervisor.rs` a des tests inline déplaçables, mais la consigne
  commune réserve ce fichier (touché par plusieurs agents) : seules deux lignes d'appel
  y changent.
- Les commentaires de `budget.toml` sur `workflow/mod.rs` (« entrées en &Arc<Daemon>
  jusqu'à T30 ») sont périmés ; le fichier ne se modifie que par `UPDATE_BUDGET`, qui ne
  réécrit pas les commentaires. À reprendre avec T32.
- Hors périmètre, deux commentaires parlent encore de T30 au futur :
  `penelope-dream/src/lib.rs:6` et `penelope-app/src/helpers.rs:211`.
- Le tableau « Le gel » d'architecture.md est daté de la 1.0.0-alpha.13 : R3 (21 fichiers,
  1 au daemon), R5 (20 modules) et R6 y sont désormais plus bas.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` verts ; `UPDATE_BUDGET=1 cargo test -p penelope-archtest` après
chaque fichier ; `UPDATE_CA_MATRIX=1` sans changement de `docs/ca-matrix.md` (aucun
`ca_*` déplacé) ; `UPDATE_DOCS` inutile (le test `docs` passe sans).

## Notes de version

#### Le daemon ne réexporte plus rien (épopée #208, lot L, T30)

- Les réexports de transition posés pendant le découpage (T21 à T29) disparaissent :
  `penelope_daemon` n'exporte plus que ses modules propres, `Daemon` et `VERSION`. La
  passerelle Telegram, les évaluations, la CLI, les tests et l'exemple du daemon nomment
  la crate où vit le code (`penelope_app::services::Services`,
  `penelope_orchestrator::workflow::start_run`, `penelope_ops::session_ops`…).
- Les enveloppes `&Arc<Daemon>` de `workflow`, `scheduler` et `dream` sont retirées :
  un seul adaptateur, `workflow::context_of`, dérive du daemon le contexte de
  l'orchestrateur. Le module `scheduler` du daemon disparaît.
- `penelope-cli/src/commands.rs` passe de 2 128 à 225 lignes en six modules
  (`cli`, `route`, `offline`, `interactive`, `restore`, `upgrade`) ;
  `penelope-daemon/src/tool_jobs.rs` de 1 135 à 345, ses tests à côté. Les deux sortent
  de la liste de référence du gel.
- Aucun changement de comportement.
