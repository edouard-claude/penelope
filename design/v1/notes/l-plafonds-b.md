# Lot L : plafonds, crates hors kernel, llm, tools, hitl, store, daemon, cli (épopée #208)

Agent `l-plafonds-b`, branche `v1-l-plafonds-b`, base 1.0.0-alpha.14 (`9f13770`).
Cible : charte `design/v1/README.md` §3.4, 800 lignes au plus par fichier source, tests en
fichiers frères.

## Livré

Les 25 fichiers au-dessus de 800 lignes dans le périmètre passent tous sous 800. Chaque
découpage est un déplacement seul : corps inchangés, seuls `mod`, `use super::*;`, un
`impl X {` d'ouverture et des `pub use` de réexport sont ajoutés. Aucun chemin public ne
change, aucun consommateur n'est touché. Un commit par fichier.

| Fichier | Avant | Après | Plus gros morceau sorti |
|---|---:|---:|---|
| `penelope-memory/src/index.rs` | 1 622 | 740 | `index/tests.rs` 491 (et `search.rs` 225, `signals.rs` 174) |
| `penelope-memory/src/consolidation.rs` | 1 476 | 746 | `consolidation/tests.rs` 589 (et `contradiction.rs` 144) |
| `penelope-memory/src/vault.rs` | 1 163 | 570 | `vault/tests.rs` 325 (et `practice.rs` 270) |
| `penelope-memory/src/recall.rs` | 1 066 | 493 | `recall/tests.rs` 572 |
| `penelope-memory/src/wiki.rs` | 969 | 743 | `wiki/tests.rs` 225 |
| `penelope-mcp/src/transport.rs` | 1 265 | 640 | `transport/tests.rs` 363 (et `stdio.rs` 264) |
| `penelope-mcp/src/registry.rs` | 1 200 | 641 | `registry/tests.rs` 393 (et `search.rs` 170) |
| `penelope-mcp/src/client.rs` | 1 027 | 545 | `client/tests.rs` 481 |
| `penelope-mcp/src/oauth.rs` | 895 | 616 | `oauth/tests.rs` 273 |
| `penelope-context/src/compaction.rs` | 1 122 | 667 | `compaction/tests.rs` 454 |
| `penelope-context/src/engine.rs` | 1 025 | 443 | `engine/tests.rs` 581 |
| `penelope-context/src/store.rs` | 960 | 595 | `store/search.rs` 370 |
| `penelope-workflow/src/validate.rs` | 1 101 | 737 | `validate/tests.rs` 363 |
| `penelope-workflow/src/schedules.rs` | 1 003 | 658 | `schedules/tests.rs` 344 |
| `penelope-workflow/src/runs.rs` | 981 | 595 | `runs/tests.rs` 385 |
| `penelope-platform/src/process.rs` | 1 008 | 718 | `process/tests.rs` 159 (et `tools.rs` 134) |
| `penelope-platform/src/backend/macos.rs` | 1 082 | 686 | `backend/macos/tests.rs` 395 |
| `penelope-telegram/src/api.rs` | 913 | 709 | `api/tests.rs` 203 |
| `penelope-telegram/src/templates.rs` | 877 | 650 | `templates/tests.rs` 226 |
| `penelope-dream/src/ingest.rs` | 834 | 652 | `ingest/apply.rs` 186 |
| `penelope-executor/src/executor/tests.rs` | 1 177 | 644 | `executor/tests/mcp.rs` 295 (et `config.rs` 243) |
| `penelope-mcp-host/src/tests.rs` | 981 | 663 | `tests/sandbox.rs` 322 |
| `penelope-gateway-telegram/src/telegram/tests/sessions.rs` | 911 | 656 | `tests/background.rs` 255 |
| `penelope-ops/src/purge/tests.rs` | 826 | 578 | `purge/tests/audit.rs` 248 |
| `penelope-ops/src/upgrade/tests.rs` | 810 | 446 | `upgrade/tests/source_switch.rs` 364 |

`[files.oversized]` perd 16 entrées (toutes celles de memory, mcp, context, workflow,
platform), par `UPDATE_BUDGET=1` ou par retrait de la ligne correspondante dans le commit
du fichier (même résultat, vérifié par un `UPDATE_BUDGET=1` final sans différence).
`docs/ca-matrix.md` est régénéré (`UPDATE_CA_MATRIX=1`) : 14 tests `ca_*` ont changé de
fichier, pas de nom.

## Choix

- **Découpage par responsabilité** quand sortir les tests ne suffisait pas : recherche et
  signaux de l'index mémoire, contradictions de la consolidation, pratiques du vault,
  transport stdio, recherche du registre MCP, recherche plein texte de l'historique,
  outils système de `process` (tar, git, `--version`), application des propositions
  d'ingestion. Les méthodes restent sur leur type (`impl MemoryIndex` dans un module
  enfant) : les champs privés restent privés.
- **Tests déjà en fichier** (exécuteur, hôte MCP, passerelle, ops) : répartis en modules
  enfants `tests/<thème>.rs` déclarés depuis le fichier de tests, qui garde les aides
  communes. Les noms de tests gagnent un segment de chemin (`tests::mcp::…`), aucun
  `ca_*` n'y est.
- `upgrade/tests/source_switch.rs` et non `switch.rs` : `upgrade::switch` existe déjà et
  aurait été masqué dans le glob `use super::*` des tests.
- `penelope-archtest/src/lib.rs` (912) n'est pas découpé : hors de la liste du lot et
  fichier partagé par les agents qui ajoutent des crates ; ses tests en ligne s'en
  sortiraient d'un commit.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` et
`cargo test --workspace` verts sur ce Mac (tests macOS compilés), après la régénération de
la matrice CA.

## Notes de version

#### Plafonds : les crates hors du noyau sous 800 lignes par fichier

Vingt-cinq fichiers de `penelope-memory`, `penelope-mcp`, `penelope-context`,
`penelope-workflow`, `penelope-platform`, `penelope-telegram`, `penelope-dream`,
`penelope-executor`, `penelope-mcp-host`, `penelope-gateway-telegram` et `penelope-ops`
passent sous 800 lignes (charte §3.4) : tests en fichiers frères et, pour les plus gros,
une responsabilité sortie en module enfant (recherche de l'index mémoire, transport stdio,
recherche du registre MCP, recherche de l'historique). Déplacements seuls, aucun chemin
public ne change. Seize fichiers sortent de la liste de référence du gel.

## Blocages et points ouverts

- Conflit attendu sur `docs/ca-matrix.md` et `budget.toml` avec `l-plafonds-a` : les deux
  se résolvent en régénérant (`UPDATE_CA_MATRIX=1`, `UPDATE_BUDGET=1`).
- Le commit du vault retire aussi l'entrée de `recall.rs` du budget, commitée juste
  après : l'archtest est rouge sur ce seul commit intermédiaire, vert à partir du suivant.
- Une fois, `cargo test -p penelope-platform` lancé avec l'entrée standard du terminal
  est resté bloqué ; avec `</dev/null`, et en lançant le binaire directement, il passe en
  7 s. Non reproduit, probablement un test qui lit l'entrée standard héritée.
