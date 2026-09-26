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

Les plafonds R9 (critère redéfini par l'intégrateur : lignes de produit **non
couvertes** par crate, v1 ≤ main ; un pourcentage dépend de l'endroit où vivent les
tests, ce nombre non) :

- `budget.toml` : `[coverage.main]`, relevé fixe des lignes non couvertes de `main` au
  point de fourche pour les 16 crates qui existent des deux côtés (le daemon de main a
  été découpé : pas de relevé) ; `[coverage.uncovered]`, un plafond par crate du
  workspace, la mesure, abaissé seulement.
- `scripts/coverage-check.sh` : lance `cargo llvm-cov --workspace --no-fail-fast
  --lcov`, compte `LF - LH` par crate (identique au JSON du lead à la ligne près),
  compare au plafond et à `main`. `--update` abaisse les plafonds, jamais ne les
  remonte, et ajoute les crates nouvelles. `COVERAGE_LCOV=<fichier>` relit une mesure
  déjà faite ; `-- <args>` passe aux binaires de test. Sortie 0, 1 (au-delà du plafond
  ou de main), 2 (mesure impossible). Pas en CI : vingt minutes et plus.
- `scripts/switch-check.sh` : le point 7 lance `coverage-check.sh` (ou relit
  `COVERAGE_LCOV`, ou le saute avec `SWITCH_SKIP_COVERAGE=1`, ce qui compte comme
  manque) ; la couverture n'est plus « à vérifier à la main ».
- `check-budget.sh` ne garde pas `[coverage.uncovered]` : `ratchet.rs` (hors de mon
  périmètre) ne connaît que `coverage.crates` comme table de **planchers**. Il faudrait
  y déclarer `coverage.uncovered` comme plafond (pas de hausse), en permettant l'ajout
  d'une crate nouvelle, ce que `CEILING_TABLES` refuse aujourd'hui.

## 3. Mesures

Lignes non couvertes par crate (`LF - LH`), sur ce Mac, `two_replays_of_every_scenario_are_identical`
sauté (voir §4). « main » et « v1 » : relevés du lead ; « c-socle » : ma mesure finale.

| crate | main (0.17.62) | v1 à 1f6fbc5 | c-socle |
|---|---|---|---|
| penelope-archtest | 102 | 118 | 118 |
| penelope-cli | 1 121 | 1 138 | 1 090 |
| penelope-context | 157 | 351 | 247 (puis 3 tests de plus, non remesurés) |
| penelope-evals | 151 | 467 | 473 |
| penelope-hitl | 55 | 56 | 45 |
| penelope-kernel | 521 | 521 | 394 |
| penelope-llm | 481 | 473 | 269 |
| penelope-mcp | 346 | 313 | 205 |
| penelope-memory | 374 | 370 | 251 |
| penelope-observe | 93 | 83 | 85 |
| penelope-platform | 822 | 696 | 690 |
| penelope-skills | 77 | 75 | 75 |
| penelope-store | 91 | 82 | 25 |
| penelope-telegram | 333 | 311 | 311 |
| penelope-tools | 235 | 225 | 181 |
| penelope-workflow | 257 | 252 | 155 |
| total workspace | 14 045 | 13 647 | 12 730 |

Au départ (v1 à 1f6fbc5), cinq crates en avaient plus que main : context (+194),
evals (+316), archtest (+16), cli (+17), hitl (+1). Après ce lot, cli et hitl passent
dessous. Restent :

- **penelope-context** : 247 contre 157. Les sept fichiers qui existaient sur main
  (`anchors`, `compaction`, `engine`, `lcm`, `store`, `tiers`, `transcript`) n'ont plus
  que 58 lignes non couvertes sur 2 607. Les 189 autres sont dans le code **nouveau** de
  v1 (source de vérité, T12 à T22 : `projector`, `store/dual`, `derive/fold`, `verify`,
  `read`, `store/seal`, `replay`, `publish`, `store/rewrite`…), 4 481 lignes sans
  équivalent sur main. Ce qui reste non couvert y est surtout la propagation d'erreurs
  SQLite (`)?;`) et des branches de refus du repli ; les tests de ce lot ont pris les
  branches de comportement (écarts de verify, fork refondu, lignes V0 refusées, message
  système refusé, stub du niveau 2, dernier recours du niveau 4).
- **penelope-evals** (+322) et **penelope-archtest** (+16) : le harnais de scénarios
  (R10, R11) et les règles du gel sont du code nouveau d'outillage ; hors de mon
  périmètre.

Variation d'une mesure à l'autre, sans changement de code : jusqu'à 5 lignes par crate
(kernel 389 puis 394, daemon 791 puis 788). Un plafond posé à la mesure exacte peut donc
échouer sur une mesure suivante sans qu'aucune ligne ait changé : c'est à l'intégrateur
de décider d'une marge.

Mesure faite sur macOS : `penelope-platform` y compile `backend/macos.rs`. Sur Linux,
§R9 prévoyait d'exclure ces fichiers (`[coverage].ignore`), non repris ici.
`new_file_floor` (§R9) n'est pas posé : hors brief.

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
#### La couverture du socle au niveau de main, et ses plafonds (épopée #208, critère 7, R9)

La couverture des crates socle de v1 semblait avoir baissé depuis le point de fourche
(86,6 % contre 88,3 % pour le workspace). C'était surtout un effet de mesure : R1 a sorti
les tests inline vers des fichiers `tests.rs`, que `cargo-llvm-cov` ne compte pas, et ces
lignes couvertes ont quitté le dénominateur. Le reste venait de code découpé ou descendu
sans ses tests. Des tests de comportement comblent l'écart crate par crate : fournisseurs
OpenAI-compatibles contre un faux serveur, recherche hybride MCP, chaque refus de la
validation de configuration et de workflow, écarts de `history verify`, index FTS5
irréparable. Le critère compte les lignes de produit non couvertes par crate, qui ne
dépendent pas de l'endroit où vivent les tests : `budget.toml` porte le relevé de main
(`[coverage.main]`) et un plafond par crate qui ne monte jamais (`[coverage.uncovered]`).
`scripts/coverage-check.sh` mesure et compare ; `switch-check.sh` l'appelle pour le
critère 7.
```
