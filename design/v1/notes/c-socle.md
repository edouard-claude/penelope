# c-socle : couverture des crates socle (critère 7 de bascule, R9)

Épopée #208, critère 7 (design/v1/gel-et-outillage.md §R9 et §4.5) : la couverture de
lignes de `v1` doit être au moins celle de `main` au point de fourche (0.17.62). Mesures
de départ du lead (`cargo llvm-cov --workspace`) : `main` 88,3 %, `v1` à 1f6fbc5 86,6 %.

## 1. Pourquoi les crates socle avaient baissé

Surtout un artefact de mesure. `cargo-llvm-cov` (0.9.1, `src/report.rs`, regex par
défaut) exclut de la mesure `tests/`, les fichiers `tests.rs` et `*_tests.rs`. Or R1 a
sorti les tests inline des fichiers qui débordaient vers `src/**/tests.rs` : ces lignes,
couvertes à 100 % ou presque, comptaient sur `main` et ne comptent plus sur `v1`. Le
dénominateur a fondu, le ratio aussi :

| crate | lignes mesurées main → v1 | main recalculé sans ces lignes | v1 à 1f6fbc5 |
|---|---|---|---|
| penelope-llm | 5 665 → 4 090 | 88,2 % | 88,4 % |
| penelope-store | 1 110 → 553 | 83,5 % | 85,2 % |

À mesure égale, le code produit de `v1` était donc au moins aussi couvert que sur
`main`. Le reste de l'écart vient de code nouveau ou découpé sans ses tests :

- `penelope-llm` : le découpage de `provider.rs` (`openai_compat`, `openrouter`,
  `stream`, `body`) laissait sans test les embeddings, le catalogue local, l'épinglage
  du fournisseur amont, les refus audio, la facturation d'un 5xx.
- `penelope-mcp` : `registry/search.rs`, dont `search_hybrid` n'avait aucun test et
  dont le repli lexical n'était jamais atteint.
- `penelope-kernel` : `config/validate.rs` (sorti de `config.rs`) : la moitié des refus
  sans cas.
- `penelope-tools` : `args.rs`, descendu de l'exécuteur (T09) sans ses tests.
- `penelope-memory`, `penelope-context`, `penelope-workflow`, `penelope-hitl`,
  `penelope-cli` : fonctions appelées seulement depuis d'autres crates (dream, gateway,
  executor), chemins d'erreur et d'écart jamais exercés.

Aucun test `#[ignore]` n'était compté ; aucun test n'a été perdu dans un déplacement.

## 2. Ce qui est livré

Des tests de comportement, un commit par crate, chacun avec ses assertions (aucun test
qui n'exécute sans vérifier) :

- store : store fermé, réouverture, migration en échec nommée, index FTS5 recréé quand
  sa table virtuelle ne se construit plus (#158), verdict du pool confirmé par le fichier.
- llm : faux serveur HTTP à plusieurs connexions, routé par chemin
  (`provider/tests/compat.rs`) ; corps OpenAI complet, raisonnement (#152), embeddings
  réordonnés, catalogue local et fenêtre (#53), épinglage par slug mis en cache (#17),
  refus de synthèse et de transcription (#41), 5xx facturable, serveur injoignable
  transitoire, `build_providers` pour le serveur local et Codex.
- hitl : portées de règles, pouvoirs, chaque obstacle nommé par `why_composed` (#150).
- tools : `args.rs` (#106, #110), résumés go, JavaScript, pytest et make (#32), coupes.
- cli : rendus de `session list`, `mcp list`, `schedule list`, routage fixe.
- kernel (tests seulement, `s-derniers` y travaille) : un cas par refus de `validate`,
  contradictions de routage, de budget et de contexte, ULID et identifiants typés.
- workflow : un refus par règle de `validate` (§12.4), par chemin JSON et message.
- mcp : recherche hybride (#11), repli lexical, en-têtes et refus du transport HTTP.
- memory : OCR, entités, balisage tronqué, `confirm_by_owner`, `update_annotations`,
  `by_slug`, origines et états d'intention.
- context : écarts de `history verify`, fork rattrapé ou refondu, niveaux 2 et 4 de la
  compaction, T4 sans message utilisateur, blocs image et audio.

Les planchers R9 :

- `budget.toml` : `[coverage.main]` (relevé fixe de `main` au point de fourche, deux
  décimales, crates socle) et `[coverage.crates]` (plancher entier par crate, la mesure
  arrondie à l'inférieur). `ratchet.rs` tenait déjà `coverage.crates` comme table de
  planchers : `check-budget.sh` refuse toute baisse.
- `scripts/coverage-check.sh` : lance `cargo llvm-cov --workspace --no-fail-fast
  --lcov`, additionne `LF`/`LH` par crate (identique au JSON du lead à la ligne près),
  compare au plancher et, pour le socle, à `main` (à quatre décimales). `--update`
  remonte les planchers, jamais ne les baisse, et ajoute les crates nouvelles.
  `COVERAGE_LCOV=<fichier>` relit une mesure déjà faite ; `-- <args>` passe aux binaires
  de test. Sortie 0, 1 (sous plancher ou sous main), 2 (mesure impossible). Pas en CI :
  vingt minutes et plus.
- `scripts/switch-check.sh` : le point 7 lance `coverage-check.sh` (ou relit
  `COVERAGE_LCOV`, ou le saute avec `SWITCH_SKIP_COVERAGE=1`, ce qui compte comme
  manque) ; la couverture n'est plus « à vérifier à la main ».

## 3. Mesures

Mesure finale sur ce Mac (`cargo llvm-cov --workspace`, lignes, filtre par défaut,
`two_replays_of_every_scenario_are_identical` sauté, voir §4), comparée au relevé du
lead :

| crate | main (0.17.62) | v1 à 1f6fbc5 | v1-c-socle |
|---|---|---|---|
| penelope-cli | 48,63 | 48,06 | 50,25 |
| penelope-context | 96,28 | 95,03 | 96,51 |
| penelope-hitl | 97,27 | 96,80 | 97,46 |
| penelope-kernel | 92,13 | 91,36 | 93,55 |
| penelope-llm | 91,51 | 88,44 | 93,48 |
| penelope-mcp | 92,88 | 91,00 | 94,10 |
| penelope-memory | 94,67 | 92,93 | 95,27 |
| penelope-store | 91,80 | 85,17 | 95,47 |
| penelope-tools | 94,69 | 93,89 | 95,10 |
| penelope-workflow | 94,66 | 93,53 | 96,02 |

Les dix crates socle sont au-dessus de `main`. `scripts/coverage-check.sh` : sortie 0.
Planchers posés par `--update` sur cette mesure (entiers, arrondis à l'inférieur), pour
les 27 crates du workspace.

Attention, trois limites des planchers tels que le brief les fixe :

- Arrondir à l'inférieur laisse parfois très peu de marge : workflow 96,02 pour un
  plancher de 96, tools 95,10 pour 95, mcp 94,10 pour 94. D'une mesure à l'autre, j'ai vu
  jusqu'à 0,05 point d'écart (daemon 87,56 puis 87,61), et un test rouge fait baisser la
  crate qu'il couvre. La spécification (§R9) disait « mesure moins 1 point » : c'est une
  décision du lead.
- Mesure faite sur macOS : `penelope-platform` y compile `backend/macos.rs`. Sur Linux,
  §R9 prévoyait d'exclure ces fichiers (`[coverage].ignore`), non repris ici.
- `new_file_floor` (90 % par fichier nouveau sur v1, §R9) n'est pas posé : hors brief.

## 4. Relevés, non corrigés (hors périmètre ou changement de comportement)

- `penelope_hitl::cmdline::why_composed` : une ligne dont le premier obstacle suit des
  guillemets simples **fermés** (`echo 'a'; b`) est décrite « guillemets non fermés » :
  la boucle rend ce message dès la quote fermante au lieu de continuer.
- `penelope_kernel::coherence::private_host` : `::1` n'est jamais reconnu (le découpage
  sur `:` vide l'hôte avant la comparaison) ; la branche qui le nomme est morte.
- `penelope_memory::ingest::html_to_text` : le texte d'un commentaire jamais fermé
  (`<!-- …` en fin de fichier) entre dans le texte extrait.
- Code mort repéré (aucun appel dans le workspace, tests compris), non supprimé : la
  suppression groupée a été refusée par le garde-fou de la session, je l'ai laissée à
  décider. context `compaction::is_tool_group` ; llm `Fingerprint::system_hash_of`,
  `MockProvider::set_models`, `StreamAccumulator::has_pending_calls`,
  `TokenEstimator::messages_tokens`, `UsageAnchor::from_usage`, `LlmError::with_status` ;
  mcp `McpClient::set_log_level` ; memory `MemoryIndex::with_params`,
  `index::annotations_of`, `InjectionMarker::mark_all` ; tools
  `shell::is_read_pipeline`, `ToolSpec::new`, `ToolContext::workspace` ; workflow
  `Step::is_terminal_kind`. Dans kernel (tests seulement) : `Config::quiet_range` (appelé
  depuis ce lot par un test, par aucun code produit), `KernelError::invalid_state`, `RiskClass::colour`, `RiskClass::label_fr`,
  `schema::property_default`, `session::provenance_kind`, `SessionStore::get_metadata`.
- `penelope-evals`, test `two_replays_of_every_scenario_are_identical` : sous
  instrumentation, il ne finit pas (plus de dix minutes à 0 % de CPU, même état chez
  `c-extraites`) ; sans instrumentation il passe, lentement. Mes mesures le sautent
  (`scripts/coverage-check.sh -- --skip two_replays_of_every_scenario_are_identical`).
- `penelope-gateway-telegram`, `telegram::tests::bursts::a_burst_asks_before_answering_and_can_be_ingested` :
  rouge une fois sur trois mesures instrumentées, vert sinon et dans `cargo test
  --workspace` ; probablement sensible au temps.
- Sous instrumentation, deux `default_*.profraw` apparaissent à la racine du dépôt : un
  test lance un processus sans l'environnement de `cargo-llvm-cov`. À ignorer (ou à
  mettre dans `.gitignore`, hors périmètre).
- `penelope-telegram` (hors liste du brief) est à 91,46 % contre 91,56 % sur `main`.

## 5. Section de notes de version, prête pour `docs/progress.md`

```markdown
#### La couverture du socle au niveau de main, et ses planchers (épopée #208, critère 7, R9)

La couverture des crates socle de v1 semblait avoir baissé depuis le point de fourche
(86,6 % contre 88,3 % pour le workspace). C'était surtout un effet de mesure : R1 a sorti
les tests inline vers des fichiers `tests.rs`, que `cargo-llvm-cov` ne compte pas, et ces
lignes couvertes ont quitté le dénominateur. Le reste venait de code découpé ou descendu
sans ses tests. Des tests de comportement comblent l'écart crate par crate : fournisseurs
OpenAI-compatibles contre un faux serveur, recherche hybride MCP, chaque refus de la
validation de configuration et de workflow, écarts de `history verify`, index FTS5
irréparable. Les planchers R9 sont posés dans `budget.toml` (`[coverage.crates]`, qui ne
descendent jamais, et `[coverage.main]`, le relevé de main). `scripts/coverage-check.sh`
mesure et compare ; `switch-check.sh` l'appelle pour le critère 7.
```
