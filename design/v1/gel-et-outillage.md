# Gel de la dette et outillage de la V1

Mesures prises le 23 septembre 2026 sur `main` à `fbbf906` (`git log -1` : 11:20 +0400).
L'arbre de travail portait des modifications non commitées d'autres sessions (`git status` :
10 fichiers modifiés, `AGENTS.md`, `audit.rs` et `prompt_snapshot.rs` non suivis) : toutes
les tailles ci-dessous sont donc lues à HEAD (`git show HEAD:<fichier> | wc -l`). Aucun
fichier du dépôt n'a été touché.

Références utilisées : `CLAUDE.md`, `Makefile`, `scripts/bump.sh`, `.github/workflows/*.yml`,
`deny.toml`, `rust-toolchain.toml`, `Cargo.toml`, `crates/penelope-archtest/src/lib.rs`,
`crates/penelope-evals/{src,tests}`, `docs/progress.md`, `docs/ca-matrix.md`,
`docs/decisions/`, issues #102, #147, #151, #153, et les copies des dépôts comparés sous
`scratchpad/comparatif/` (`openclaw/`, `dsh/`) pour les mécanismes du §2.6. Le PRD n'a pas
été lu (dépassé).

---

## 1. Mesures

### 1.1 Répartition des fichiers Rust par taille

```bash
git ls-files 'crates/*.rs' | while read f; do echo "$(git show HEAD:"$f" | wc -l) $f"; done \
  | awk '{n=$1; if(n<=200)a++; else if(n<=500)b++; else if(n<=1000)c++; else if(n<=1500)d++;
          else if(n<=3000)e++; else f++; s+=n} END {print a,b,c,d,e,f,s}'
```

| Tranche (lignes) | Fichiers | Part des fichiers |
|---|---|---|
| 1 à 200 | 43 | 20 % |
| 201 à 500 | 68 | 32 % |
| 501 à 1 000 | 59 | 28 % |
| 1 001 à 1 500 | 23 | 11 % |
| 1 501 à 3 000 | 12 | 6 % |
| plus de 3 000 | 7 | 3 % |
| **Total** | **212 fichiers, 160 739 lignes** | |

170 fichiers sur 212 (80 %) tiennent sous 1 000 lignes. Les 42 autres portent 90 430
lignes, soit 56 % du workspace ; les 7 de plus de 3 000 lignes en portent 37 987 (24 %).

### 1.2 Liste de référence initiale : les 42 fichiers de plus de 1 000 lignes

« Hors tests » = lignes avant le premier `#[cfg(test)]` de colonne 0 suivi d'une ligne
`mod …` (c'est le repère qu'utilise déjà `penelope-archtest`, `lib.rs:170-176`). Trois cas
particuliers vérifiés : `agent.rs:2100` ouvre `mod clone_policy_tests` avant `mod tests`,
`mcp.rs:1954` ouvre `pub(crate) mod testing` (aides de test, `#[cfg(test)]`), et
`policy.rs:200` porte un `#[cfg(test)] fn` isolé (le module de tests commence à `:658`).

| Fichier | Total | Hors tests | Tests inline |
|---|---:|---:|---:|
| `crates/penelope-daemon/src/telegram.rs` | 13 716 | 7 948 | 5 768 |
| `crates/penelope-daemon/src/dream.rs` | 5 859 | 3 453 | 2 406 |
| `crates/penelope-daemon/src/agent.rs` | 4 192 | 2 099 | 2 093 |
| `crates/penelope-daemon/src/workflow.rs` | 4 044 | 2 912 | 1 132 |
| `crates/penelope-daemon/src/executor.rs` | 3 712 | 2 477 | 1 235 |
| `crates/penelope-daemon/src/mcp.rs` | 3 291 | 1 953 | 1 338 |
| `crates/penelope-daemon/src/engine.rs` | 3 173 | 1 218 | 1 955 |
| `crates/penelope-kernel/src/config.rs` | 2 738 | 2 145 | 593 |
| `crates/penelope-cli/src/commands.rs` | 2 651 | 2 278 | 373 |
| `crates/penelope-daemon/src/rpc.rs` | 2 467 | 1 728 | 739 |
| `crates/penelope-daemon/src/telegram/screens.rs` | 2 435 | 2 435 | 0 |
| `crates/penelope-daemon/src/doctor.rs` | 2 357 | 1 969 | 388 |
| `crates/penelope-daemon/src/compaction.rs` | 2 225 | 1 373 | 852 |
| `crates/penelope-llm/src/provider.rs` | 2 048 | 1 187 | 861 |
| `crates/penelope-daemon/src/hermes.rs` | 1 971 | 1 565 | 406 |
| `crates/penelope-daemon/src/upgrade.rs` | 1 926 | 1 192 | 734 |
| `crates/penelope-daemon/src/scheduler.rs` | 1 833 | 1 146 | 687 |
| `crates/penelope-memory/src/index.rs` | 1 622 | 1 128 | 494 |
| `crates/penelope-llm/src/codex.rs` | 1 533 | 1 004 | 529 |
| `crates/penelope-memory/src/consolidation.rs` | 1 476 | 882 | 594 |
| `crates/penelope-context/src/store.rs` | 1 379 | 1 138 | 241 |
| `crates/penelope-tools/src/spec.rs` | 1 290 | 1 110 | 180 |
| `crates/penelope-mcp/src/transport.rs` | 1 265 | 898 | 367 |
| `crates/penelope-daemon/src/mcp_auth.rs` | 1 246 | 690 | 556 |
| `crates/penelope-daemon/src/ingest.rs` | 1 225 | 855 | 370 |
| `crates/penelope-daemon/src/conversation.rs` | 1 218 | 683 | 535 |
| `crates/penelope-store/src/migrations.rs` | 1 215 | 956 | 259 |
| `crates/penelope-context/src/engine.rs` | 1 214 | 630 | 584 |
| `crates/penelope-mcp/src/registry.rs` | 1 200 | 803 | 397 |
| `crates/penelope-memory/src/vault.rs` | 1 163 | 835 | 328 |
| `crates/penelope-kernel/src/turn.rs` | 1 136 | 676 | 460 |
| `crates/penelope-context/src/compaction.rs` | 1 122 | 665 | 457 |
| `crates/penelope-hitl/src/policy.rs` | 1 120 | 657 | 463 |
| `crates/penelope-workflow/src/validate.rs` | 1 101 | 735 | 366 |
| `crates/penelope-platform/src/backend/macos.rs` | 1 082 | 684 | 398 |
| `crates/penelope-memory/src/recall.rs` | 1 066 | 491 | 575 |
| `crates/penelope-daemon/src/codex_auth.rs` | 1 040 | 695 | 345 |
| `crates/penelope-store/src/lib.rs` | 1 036 | 636 | 400 |
| `crates/penelope-mcp/src/client.rs` | 1 027 | 543 | 484 |
| `crates/penelope-platform/src/process.rs` | 1 008 | 846 | 162 |
| `crates/penelope-daemon/src/purge.rs` | 1 005 | 495 | 510 |
| `crates/penelope-workflow/src/schedules.rs` | 1 003 | 656 | 347 |

Lecture : 21 fichiers restent au-dessus de 1 000 lignes **même sans leurs tests** ; les 21
autres passeraient sous le plafond rien qu'en déplaçant leurs tests dans un fichier voisin.

### 1.3 Par crate (lignes de `src/`, à HEAD)

| Crate | Lignes | Fichiers | Tests `#[test]`/`#[tokio::test]` |
|---|---:|---:|---:|
| penelope-daemon | 77 548 | 63 | 542 |
| penelope-kernel | 9 844 | 17 | 133 |
| penelope-memory | 9 791 | 13 | 132 |
| penelope-llm | 8 143 | 11 | 127 |
| penelope-mcp | 6 643 | 10 | 111 |
| penelope-platform | 6 421 (+ 273 dans `tests/`) | 19 | 99 |
| penelope-tools | 6 286 | 10 | 104 |
| penelope-workflow | 6 187 | 9 | 88 |
| penelope-telegram | 6 084 | 11 | 98 |
| penelope-context | 5 537 | 8 | 94 |
| penelope-cli | 3 227 | 4 | 28 |
| penelope-hitl | 2 744 | 3 | 31 |
| penelope-store | 2 480 | 4 | 27 |
| penelope-observe | 1 905 | 5 | 37 |
| penelope-evals | 1 623 (+ 3 769 dans `tests/`) | 6 | 99 |
| penelope-skills | 1 381 | 2 | 17 |
| penelope-archtest | 466 | 1 | 10 |

Le daemon fait 48 % du workspace à lui seul. 1 775 attributs de test dans les sources
(dont 20 `#[ignore]` : 12 suites réseau, 6 `backend/macos.rs`, `ocr.rs`, `ingest.rs`,
`mcp.rs`) ; `docs/progress.md:11` annonce 1 735 tests verts.

### 1.4 Fonctions (heuristique par comptage d'accolades, hors tests)

92 fonctions dépassent 100 lignes, 25 dépassent 200. Les plus longues :

| Lignes | Fonction |
|---:|---|
| 1 500 | `crates/penelope-daemon/src/telegram.rs:1093` `command` |
| 1 490 | `crates/penelope-daemon/src/telegram/screens.rs:413` `build_screen` |
| 1 155 | `crates/penelope-daemon/src/rpc.rs:50` `dispatch` |
| 908 | `crates/penelope-tools/src/spec.rs:83` `all` (table des outils) |
| 470 | `crates/penelope-daemon/src/dream.rs:120` `run_locked` |
| 457 | `crates/penelope-daemon/src/telegram/screens.rs:1906` `perform` |
| 403 | `crates/penelope-daemon/src/agent.rs:1267` `resolve_pending` |
| 383 | `crates/penelope-daemon/src/agent.rs:535` `run_conversation` |
| 352 | `crates/penelope-telegram/src/commands.rs:35` `all` (catalogue) |
| 338 | `crates/penelope-daemon/src/telegram.rs:3591` `callback` |

Quatre d'entre elles sont des tables de données (`spec.rs`, `commands.rs`,
`templates.rs:353`, `screens.rs`) : longues par nature, à traiter à part (voir R7 et §7).

### 1.5 Couplage au `Daemon`

`pub struct Daemon` est à `crates/penelope-daemon/src/runtime.rs:305-324` : huit champs
publics (`services`, `handle`, `bus`, `hooks`, `compaction`, `workflows`, `embeddings`,
`tasks`). Quatre fichiers portent un bloc `impl Daemon` : `runtime.rs:369`,
`engine.rs:145`, `runner.rs:279`, `supervisor.rs:90`.

**42 des 63 fichiers du daemon nomment le type `Daemon` hors tests** (occurrences du mot
`Daemon`, commentaires exclus, avant le module de tests) :

```
26 scheduler.rs   21 compaction.rs   20 workflow.rs   20 dream.rs   13 onboarding.rs
10 supervisor.rs  10 backup.rs        9 rpc.rs         8 runner.rs   7 telegram.rs
 7 session_ops.rs  7 ingest.rs        7 episodes.rs    7 embeddings.rs 7 concepts.rs
 6 ticket_to_deploy_e2e.rs  6 runtime.rs  6 mcp_auth.rs  5 voice.rs  5 tasks.rs
 5 purge.rs  4 upgrade.rs  4 mem_audit.rs  4 hermes.rs  4 budget_alert.rs
 3 vision.rs  3 titles.rs  3 review.rs  3 mem_split.rs  3 engine.rs  3 codex_scope.rs
 3 codex_auth.rs  2 wiki_e2e.rs  2 vault_git.rs  2 skill_install.rs  2 runtime_events.rs
 2 images.rs  2 codex_quota.rs  1 vault_inventory.rs  1 session_project.rs  1 doctor.rs
 1 cache_audit.rs
```

C'est la liste blanche initiale de la règle R6.

---

## 2. Ce qui existe déjà

### 2.1 `penelope-archtest`

- Lit le workspace **sans `cargo metadata`** : `read_dir` de `crates/`, puis `Cargo.toml` de
  chaque crate parsé avec `toml` (`lib.rs:35-88`) ; dépendances `penelope-*` et externes,
  les `dev-dependencies` exclues.
- `sources(c)` ne parcourt que `src/**/*.rs` (`lib.rs:90-107`) : les `tests/` d'un crate
  ne sont pas vus.
- `Violation { crate_name, file, line, rule, text }` avec un `Display` (`lib.rs:113-131`).
- Règles : motifs interdits hors `penelope-platform` (`FORBIDDEN_PATTERNS`, `lib.rs:136-151`,
  test `ca_2_3`), en sautant ce qui suit un `#[cfg(test)]` de colonne 0 et les lignes de
  commentaire (`lib.rs:170-181`) ; règles de dépendance entre crates (`dependency_rules`,
  `lib.rs:205`, test `ca_3_1`) ; cycles ; `#![forbid(unsafe_code)]` obligatoire dans chaque
  point d'entrée (`lib.rs:297-310`, `UNSAFE_ALLOWED` vide) ; dépendances propres à un OS
  interdites hors plateforme (`OS_SPECIFIC_CRATES`).
- Les dix tests sont dans le même fichier (`mod tests`, `lib.rs:355`) et tournent avec
  `cargo test --workspace` (suite `arch`, `suites.rs:180-184`).

Le style à reprendre pour les règles de gel : une constante ou un fichier de données, une
fonction `xxx_violations() -> Vec<…>`, un test qui affiche toutes les violations d'un coup.

### 2.2 Lints actifs

- Aucun `[workspace.lints]` ni `[lints]` dans les 18 manifestes (`grep -rn lints
  --include=Cargo.toml` : vide) ; ni `clippy.toml` ni `rustfmt.toml` : largeur 100 par
  défaut, donc le nombre de lignes est une mesure stable.
- `#![forbid(unsafe_code)]` dans les 17 points d'entrée (16 `lib.rs` + `penelope-cli/src/main.rs:5`).
- 19 `#[allow(…)]` : 15 `clippy::too_many_arguments`, 2 `clippy::type_complexity`,
  1 `dead_code`, 1 `clippy::result_large_err`.
- Le `-D warnings` n'est écrit que dans les commandes : `CLAUDE.md` (« Avant de pousser »),
  `ci.yml:84`, `release.yml` (job `verification`). Chaîne épinglée : `rust-toolchain.toml:11`
  (`channel = "1.98"`), donc un lint nouveau ne casse pas la CI par surprise.

### 2.3 CI

`ci.yml` : déclencheurs `push: branches: [main]`, `pull_request`, `workflow_dispatch`
(`ci.yml:8-11`) ; concurrence par ref (`:13-14`). Quatre jobs :

| Job | Machine | Contenu |
|---|---|---|
| `tests` (`:25-54`) | ubuntu-latest | `cargo test --workspace --no-fail-fast` |
| `verification` (`:58-110`) | macos-14 | fmt (`:81`), clippy `-D warnings` (`:84`), tests plateforme, **`cargo test --workspace` à nouveau** (`:99`, exigé par #151), `launchd_relay`, binaire release (`:110`) |
| `dependances` (`:112-132`) | ubuntu-latest | `cargo deny check` |
| `livraison` (`:138-181`) | ubuntu-latest | `needs` les trois, **`if: github.ref == 'refs/heads/main' && github.event_name == 'push'`** (`:141`), un seul à la fois (`:144-146`) ; pose `v<version>` si absent et lance `release.yml` |

`release.yml` : `on: push: tags: ["v*"]` (`:8`) ou `workflow_dispatch` ; vérifie tag =
version du workspace, fmt, clippy, tests, banc mémoire ; construit deux cibles, publie ;
`v0.*` ou toute version à suffixe (`*-*`) est une pré-release (`:241-243`).

Durées mesurées le 23/09 (`gh run list`) : 9 à 16 min par run. Le dépôt est public
(`gh api repos/… : private=false`) : les minutes macOS ne sont pas facturées, le coût est
la file d'attente.

**Aucune protection de branche ni ruleset** (`gh api …/branches/main/protection` : 404,
`…/rulesets` : `[]`). N'importe quel push de tag `v*` par un porteur des droits déclenche
une release.

`.github/workflows/README.md` est périmé : « Deux workflows », tests « sur macOS » seulement,
et une section « Poser un tag » à la main qui contredit `CLAUDE.md`.

Faits relevés par le lead, repris tels quels : aucun outil de couverture dans `ci.yml` ni
dans le `Makefile` ; cibles `make` : `help pull bump build sign update install restart
deploy test clean` (`Makefile:31`) ; 17 sites `cfg(target_os = "macos")` dans `crates/`,
répartis sur 12 fichiers (7 dans `penelope-platform`, 4 dans `penelope-daemon`, 1 dans
`penelope-tools`), que `CLAUDE.md` dit compilés par aucune commande locale sous Linux ;
aucune section `[workspace.lints]`, `#![forbid(unsafe_code)]` pour seul lint d'attribut
dans chaque `lib.rs`, `-D warnings` uniquement dans la commande clippy (celle de
`CLAUDE.md`, que `ci.yml:84` et `release.yml` rejouent). Sur le poste : `cargo-llvm-cov`
0.9.1 est installé, le composant `llvm-tools` n'est pas dans `rust-toolchain.toml:12`.

### 2.4 Version et journal

- `Cargo.toml:6` `version = "0.17.58"` et quinze `penelope-* = { path, version }` (`:75-89`) :
  seize lignes. `scripts/bump.sh` : regex `^x.y.z(-suffixe)?$` (`bump.sh:17`, le suffixe est
  accepté), arbre propre, section `### x.y.z` exigée dans `docs/progress.md` (`:25`), seize
  réécritures vérifiées (`:36`), `cargo update -w --offline`, commit « Version x.y.z ».
- `docs/progress.md` : 88 sections `### x.y.z`, routine de livraison à `:2715-2739`.
- `crates/penelope-evals/tests/docs.rs:516-524` : la version du workspace a sa section ;
  `:530-548` : la plus haute section est la version du workspace. **Le parseur de `:530`
  n'accepte que trois entiers séparés par des points** : une version `1.0.0-alpha.1`
  fait paniquer le test (« version du workspace illisible »). À corriger avant tout bump
  sur `v1` (T7).
- `crates/penelope-daemon/build.rs:1-30` embarque **tout** `docs/*.md` dans le binaire :
  chaque page ajoutée sous `docs/` grossit le binaire et devient ce que Pénélope « sait ».
- 48 commits « Version 0.17.x » en septembre (`git log -- Cargo.toml`), jusqu'à 12 par jour
  (19/09) ; 18 à 103 commits par jour sur `main` du 16 au 22/09.
- `penelope upgrade` (`crates/penelope-daemon/src/upgrade.rs:137-149`) : `parse_version`
  **coupe le suffixe** (`split(['-', '+'])`) et `release()` (`:191-215`) prend la plus
  haute version parmi les releases non brouillon, pré-releases comprises. Conséquence :
  une release `v1.0.0-alpha.1` publiée par erreur serait vue comme `1.0.0 > 0.17.58` et
  **installée par toutes les instances 0.17**.

### 2.5 Suites d'évaluation

Catalogue `crates/penelope-evals/src/suites.rs:30-132` (drapeau `network`), commandes
`cargo_filter` (`:180-243`), variables exigées `required_env` (`:244-256`) ; garde des
suites réseau dans `src/live.rs:1-12` (tests `#[ignore]`, lancés par `penelope eval
<suite>`, `commands.rs:1262-1316`).

| Suite | Réseau | Commande | Tests |
|---|---|---|---:|
| unit | non | `cargo test --workspace` | 1 775 attributs |
| arch | non | `-p penelope-archtest` | 10 |
| ctx-safety | non | `-p penelope-context ctx_safety` | filtre |
| mem-learning | non | `-p penelope-memory` | 132 |
| mem-bench | non | `--test mem_bench` (modèle simulé, rapport joint aux releases) | 1 |
| mcp-conformance | non | `--test mcp_conformance` | 22 |
| telegram | non | `-p penelope-telegram` (mock Bot API `src/mock.rs:33`) | 98 |
| hitl | non | `-p penelope-hitl` | 31 |
| workflow | non | `-p penelope-workflow -p penelope-daemon` | 88 + 542 |
| hot-reload | non | `--test hot_reload` | 14 |
| resilience | non | `--test resilience` | 11 |
| security | non | `--test security` | 11 |
| ctx-recall | **oui** | `--test ctx_recall -- --ignored` (`OPENROUTER_API_KEY`) | 1 |
| mem-longitudinal | **oui** | idem | 1 |
| mem-bench-live | **oui** | idem | 1 |
| live-openrouter | **oui** | idem | 5 |
| live-telegram | **oui** | `PENELOPE_LIVE_TELEGRAM_TOKEN`, `_CHAT` | 2 |
| ab-hermes | **oui** | `PENELOPE_AB_HERMES_CMD` | 1 |

Hors catalogue mais bloquants : `docs.rs` (12 tests : index, liens, commandes Telegram,
clés de configuration, outils, version), `ca_matrix.rs` (3), `workflow_schema.rs` (4).

Les six suites réseau sont « écrites, pas encore lancées » (`docs/progress.md:51`).

Critères d'acceptation : 71 tests `ca_<section>_<n>_<nom>` dans 14 sections
(`docs/ca-matrix.md:12`, généré par `ca_matrix::collect`, `src/ca_matrix.rs:93`) ; le
garde-fou n'exige que « au moins 50 » (`tests/ca_matrix.rs:11`). Les sections §11 (outils
natifs) et §16 (observabilité) n'ont aucun CA.

### 2.6 Trois mécanismes déjà éprouvés ailleurs (copies sous `scratchpad/comparatif/`)

**Cliquet de taille d'OpenClaw** (`openclaw/scripts/check-max-lines-ratchet.mts`,
`openclaw/scripts/lib/shrink-ratchet.mts`). La règle de taille est un lint (`max-lines`
d'oxlint) ; les fichiers qui la violent portent une suppression sur place et sont listés
dans `config/max-lines-baseline.txt` (789 lignes), dont l'en-tête dit « this list may only
shrink. Split files; never add entries » (`check-max-lines-ratchet.mts:26-35`). Le script
échoue dans trois cas distincts : une suppression nouvelle (« split these files », `:235`),
une liste qui s'allonge par rapport à la base git (« may only shrink; remove these
entries », `:240`), une entrée périmée dont le fichier ne viole plus la règle (« Remove
stale … entries (or run with --prune) », `:262`) ; `--prune` réécrit la liste (`:248`).
La base git est le **merge-base** avec `origin/main`, pas la pointe (« Branches own their
grandfathered debt from the fork », `shrink-ratchet.mts:56-67`) ; un fichier renommé garde
son entrée grâce à `git diff --name-status --find-renames` (`:132-138`,
`baselineWithVerifiedRenames`, `check-max-lines-ratchet.mts:87`) ; les fichiers générés
sont exclus (`isGovernedSourcePath`, `:38-50`) ; un mode `--staged` sert au pré-commit
(`:192`). La même bibliothèque porte des cliquets **comptés** (`parseRatchetCounts`,
`compareRatchetCounts`, `shrink-ratchet.mts:173-214`), utilisés par
`check-line-cap-ratchet.mts`, qui n'échoue que sur une hausse (`compareLineCapViolations`,
`:29-46`). En CI, le script reçoit un point de fourche figé
(`openclaw/.github/workflows/ci.yml:3424`).

**Couverture et composition réelle chez DeepSeek Harness** (`dsh/docs/testing.md`) :
couverture de lignes à 100 % par fichier sur `packages/*/*/src`, avec l'idée qu'une ligne
non couverte est souvent du code mort à supprimer (`testing.md:10`) ; « Mock only the
expensive or non-deterministic boundary (LLM adapter, network, clock); keep everything
downstream real » (`:29`) ; « Verify the world, not the self-report » (`:33`) ; tout
composant visible exige un test de composition réelle, hors unitaire, qui boote la vraie
composition et n'assert que sur ce que le modèle voit, l'état durable ou la sortie
utilisateur (`:39`).

**Transcriptions de session rejouables sans clé** (`dsh/snapshots/session/<scénario>/`,
`dsh/snapshots/AGENTS.md`) : un `session.vN.jsonl` (entrée utilisateur et réponses du
modèle enregistrées, identifiants remplacés par des jetons `{{session:1}}`, `{{cwd}}`) et
un `snapshot.yml` (profil, composition, politique d'enregistrement) ; `test:snapshot`
rejoue sans écrire, `record` ré-enregistre quand la transcription du modèle change,
`refresh` quand l'entrée reste valable (`testing.md:14`) ; « Every non-trivial model-,
protocol-, or human-visible change adds or updates a keyless recorded-session scenario in
the same PR » (`:55`) ; « Model prose and tool-result text do not prove the external
effect » : un scénario qui modifie l'espace de travail commite l'arbre attendu
(`snapshots/AGENTS.md`, § Workspace seeds).

**Ce que Pénélope a déjà pour les reprendre** : `MockProvider` garde chaque `ChatRequest`
reçue (`seen`, `mock.rs:48` ; `requests()`, `:187`), accepte un `Responder` qui choisit la
réponse d'après la requête (`:96`, `:128`) et un script de `Scripted` couvrant texte,
appels d'outils, erreurs, dépassement de contexte, images, panique (`:15-27`) ;
`ChatRequest`, `ChatResponse`, `ToolCall`, `ChatMessage` dérivent
`Serialize`/`Deserialize` (`types.rs:71-72`, `:80-81`, `:202-203`, `:291-292`) ; le
journal d'une session se relit d'un bloc (`session_events`, `event.rs:251`) et les
événements `runtime.tool`/`runtime.llm` portent arguments, résultats, modèle et coût
(`docs/runtime-events.md:33-46`, replay vers un consommateur `runtime_events.rs:88`) ;
`resilience.rs:15-22` détruit et reconstruit les services sur le même répertoire ;
`ticket_to_deploy_e2e.rs` boote la composition réelle avec `FakeConnector`
(`mcp.rs:1965`) pour les serveurs MCP. Ce qui manque : aucun scénario n'existe sous forme
de **fichier** ; les scripts sont écrits en Rust à la main
(`ticket_to_deploy_e2e.rs:273-332`) ; aucun enregistreur ne capture les réponses d'un vrai
modèle ; rien ne compare « le monde » après coup autrement que par des assertions ad hoc.

---

## 3. Règles de gel

Toutes vivent dans `penelope-archtest` (ou dans un script de `scripts/` lancé par la CI
quand il faut git ou llvm-cov), alimentées par **un fichier de budget**
`crates/penelope-archtest/budget.toml` (le crate dépend déjà de `toml`,
`penelope-archtest/Cargo.toml`), sur le modèle de la matrice CA : un test compare l'état
courant au fichier, `UPDATE_BUDGET=1 cargo test -p penelope-archtest` réécrit le fichier
**vers le bas uniquement**. Un script de CI vérifie que le fichier ne remonte jamais (R3).

```toml
# crates/penelope-archtest/budget.toml : gel de la dette (issue à créer).
# Les nombres ne montent jamais. `UPDATE_BUDGET=1 cargo test -p penelope-archtest` les
# abaisse à la valeur courante ; les remonter demande le trailer « Dérogation-budget: #N ».

[files]
ceiling = 1000        # src/**/*.rs, tests inline compris (R1)
test_ceiling = 1500   # tests/*.rs, src/**/tests.rs, modules entièrement cfg(test) (R2)
test_modules = ["crates/penelope-daemon/src/ticket_to_deploy_e2e.rs",
                "crates/penelope-daemon/src/wiki_e2e.rs"]

[files.oversized]     # liste de référence, à fbbf906 (R3) : 42 entrées
"crates/penelope-daemon/src/telegram.rs" = 13716
"crates/penelope-daemon/src/dream.rs" = 5859
# … les 40 autres lignes du §1.2

[crates]              # lignes de src/ par crate (R4)
"penelope-daemon" = 80000

[daemon]              # R5 : les 61 `pub mod` + 2 `mod` cfg(test) de lib.rs:5-67, plus `screens`
modules = ["agent", "approval_mode", "audit", "backup", "…", "workflow", "screens",
           "ticket_to_deploy_e2e", "wiki_e2e"]
impl_daemon = ["runtime.rs", "engine.rs", "runner.rs", "supervisor.rs"]

[daemon.daemon_users] # R6 : occurrences du type Daemon hors tests, 42 entrées (§1.5)
"scheduler.rs" = 26
"compaction.rs" = 21
# …

[lints]               # R7
allow_too_many_lines = 0   # posé par T5 après le premier run clippy

[ca]                  # R8 : les 71 noms de docs/ca-matrix.md ; ne peut que s'allonger
required = ["ca_2_2_penelope_home_reroots_everything", "…"]

[coverage]            # R9 : planchers posés par T17 sur la mesure, jamais devinés ; montent seulement
new_file_floor = 90   # fichiers absents du point de fourche (v1)
ignore = ["backend/macos\\.rs", "platform/src/ocr\\.rs", "platform/src/codesign\\.rs"]
[coverage.crates]
"penelope-kernel" = 0.0    # remplacé par « mesure moins 1 point » au premier run

[scenarios]           # R10 : surfaces visibles sans scénario au départ ; ne peut que rétrécir
missing = ["/commande…", "outil…", "méthode…"]
```

### R1. Plafond par fichier source : 1 000 lignes, tests inline compris

- **Périmètre** : tout `crates/*/src/**/*.rs` qui n'est pas un fichier de tests (R2).
- **Valeur** : 1 000 lignes (`raw.lines().count()`), **tests inline compris**.
- **Justification** : 80 % des fichiers sont déjà dessous (§1.1) ; c'est le seuil que le
  propriétaire et le lead ont nommé ; à largeur 100 (rustfmt par défaut), 1 000 lignes est
  la limite au-delà de laquelle un fichier ne se lit plus, il se cherche. Compter les tests
  est voulu : un test inline se lit avec son code, et la sortie est mécanique (ci-dessous).
  Une variante « 800 lignes hors tests » a été écartée : deux comptes à expliquer, et le
  repère « hors tests » dépend d'une heuristique (§1.2). Le plafond baissera après la V1
  (une ligne de `budget.toml`), pas avant.
- **Tests inline** : quand ils font déborder un fichier, ils vont dans un fichier voisin
  `src/<module>/tests.rs` déclaré `#[cfg(test)] mod tests;` : `use super::*;` y garde
  l'accès aux éléments privés, le code ne bouge pas d'une ligne. 21 des 42 fichiers de la
  liste passent sous le plafond par ce seul déplacement (§1.2).
- **Où** : `penelope-archtest`, fonction `size_violations()`, test
  `no_source_file_exceeds_the_ceiling`.
- **Message** (format `Violation`, `lib.rs:123-131`) : `[penelope-daemon]
  crates/penelope-daemon/src/media.rs:1 : plafond de taille : 1 042 lignes, plafond 1 000
  (fichier hors liste de référence) : découper, ou déplacer les tests dans media/tests.rs`.
- **Violation type** : `media.rs` (aujourd'hui < 1 000) reçoit une fonction de 60 lignes
  et passe à 1 042.

### R2. Fichiers de tests : 1 500 lignes

- **Périmètre** : `crates/*/tests/**/*.rs`, `src/**/tests.rs`, `src/**/*_tests.rs`, et les
  modules entièrement `#[cfg(test)]` listés dans `[files].test_modules`.
- **Valeur** : 1 500. Le plus gros aujourd'hui fait 737 lignes
  (`evals/tests/mcp_conformance.rs`), puis 662 (`docs.rs`) : la marge est large, et un
  fichier de tests se lit cas par cas, pas de haut en bas.
- **Où** : même fonction, `archtest` doit apprendre à parcourir `tests/` (aujourd'hui `src`
  seulement, `lib.rs:92`).
- **Message** : `[penelope-evals] crates/penelope-evals/tests/docs.rs:1 : plafond des tests
  : 1 530 lignes, plafond 1 500 : scinder le fichier de tests`.
- **Violation type** : `telegram/tests.rs` créé par T15 avec les 5 768 lignes de tests
  de `telegram.rs` dépasse 1 500 : il faut le scinder en `tests/commands.rs`,
  `tests/cards.rs`, etc. (sous-modules de `mod tests`).

### R3. Liste de référence qui ne peut que décroître (cliquet, d'après OpenClaw)

- **Règle** : `[files.oversized]` est la liste des fichiers autorisés à dépasser
  `ceiling`, chacun avec son plafond propre. Trois échecs distincts, comme dans
  `check-max-lines-ratchet.mts:235-262` :
  1. **nouveau dépassement** : un fichier absent de la liste dépasse `ceiling` (« découper,
     ou déplacer les tests ») ;
  2. **hausse** : un fichier listé dépasse son nombre, ou la liste gagne une entrée par
     rapport à la base git (« la liste ne peut que rétrécir ») ;
  3. **entrée périmée** : un fichier listé est passé sous `ceiling`, ou n'existe plus, et
     l'entrée est encore là (« retirer l'entrée, ou `UPDATE_BUDGET=1` ») ; c'est ce qui
     force la liste à rétrécir dès qu'un fichier est assaini, au lieu de laisser dormir une
     marge.
  Une baisse sans passage sous le plafond est tolérée (le nombre inscrit est une borne),
  comme dans `check-line-cap-ratchet.mts:29-46` ; `UPDATE_BUDGET=1` resserre chaque entrée
  à `min(inscrit, courant)` (l'équivalent de `--prune`, `check-max-lines-ratchet.mts:248`)
  et la routine du lot le lance dès qu'un fichier listé est touché, pour ne pas laisser de
  marge dormante.
- **Où** : (a) `archtest`, test `oversized_files_only_shrink` (cas 1, 2 sans git, 3) ;
  (b) **`scripts/check-budget.sh`**, étape du job `tests` de `ci.yml`, pour le cas 2 contre
  la base git : en PR, `git merge-base HEAD origin/<base>` (le point de fourche, pas la
  pointe qui bouge, `shrink-ratchet.mts:56-67`) ; en push, `github.event.before` ; le job
  `tests` fait un checkout sans historique (`ci.yml:30`), donc `git fetch --depth=1 origin
  <base>` d'abord. **Renommages** : `git diff --name-status --find-renames <base> --
  crates` ; une entrée dont le fichier a été renommé est acceptée sous son nouveau chemin
  avec un nombre ≤ l'ancien (`shrink-ratchet.mts:132-138`). Le script refuse toute valeur
  qui monte et toute entrée ajoutée dans `[files.oversized]`, `[daemon].modules`,
  `[daemon.daemon_users]`, `[scenarios].missing`, tout plancher de `[coverage]` qui
  baisse, sauf si un commit de la plage porte le trailer `Dérogation-budget: #<issue>`
  (`git log --format=%B <base>..HEAD`), auditable par `git log --grep`. Pas de fichiers
  générés à exclure : aucun `.rs` du dépôt n'est produit par un `build.rs`
  (`penelope-daemon/build.rs` n'embarque que du Markdown).
- **Pré-commit (facultatif)** : `scripts/check-budget.sh --staged` compare l'index à HEAD,
  comme le mode `--staged` d'OpenClaw (`check-max-lines-ratchet.mts:192`) ; la CI reste
  la seule barrière.
- **Messages** : archtest, cas 1 : `crates/penelope-daemon/src/media.rs : 1 042 lignes,
  plafond 1 000, hors liste de référence : découper, ou déplacer les tests dans
  media/tests.rs` ; cas 2 : `crates/penelope-daemon/src/telegram.rs : 13 740 lignes, la
  liste lui en accorde 13 716 : ce fichier ne peut que décroître (budget.toml
  [files.oversized])` ; cas 3 : `budget.toml [files.oversized] : entrée périmée
  crates/penelope-daemon/src/purge.rs (987 lignes, plafond 1 000) : la retirer, ou
  UPDATE_BUDGET=1 cargo test -p penelope-archtest`. Script : `budget.toml :
  "crates/penelope-daemon/src/dream.rs" passe de 5859 à 5900 par rapport à a1b2c3d ; un
  budget ne monte jamais (ajouter « Dérogation-budget: #NNN » au message de commit si c'est
  décidé)`.
- **Violations types** : un correctif ajoute 24 lignes à `telegram.rs` sans en retirer
  (cas 2) ; T15 déplace les tests de `purge.rs` et laisse son entrée (cas 3) ; une session
  renomme `telegram.rs` en `telegram/mod.rs` sans toucher au budget (cas 1 en local ; une
  fois l'entrée reportée sous le nouveau chemin, le script l'accepte comme renommage, pas
  comme ajout).

### R4. Plafond par crate

- **Règle** : `penelope-daemon/src` ≤ 80 000 lignes (77 548 à HEAD : 2 452 lignes de marge,
  soit une vingtaine de correctifs de taille courante). Aucun autre crate n'est plafonné
  au départ ; la table `[crates]` accepte des entrées.
- **Pourquoi en plus de R1 et R5** : 42 fichiers du daemon sont sous 1 000 lignes et
  pourraient chacun y monter : R1 seul laisse 20 000 lignes de croissance possible.
- **Où** : `archtest`, test `crates_stay_under_their_ceiling` ; le script R3 empêche de
  remonter le nombre.
- **Message** : `penelope-daemon/src : 80 212 lignes, plafond 80 000 (budget.toml
  [crates]) : la 0.17 ne grossit plus, la fonctionnalité va dans la branche v1`.
- **Violation type** : trois lots successifs de 900 lignes.

### R5. Liste blanche des modules du daemon

- **Règle** : toute déclaration `mod x;` (fichier séparé) dans `penelope-daemon` doit être
  dans `[daemon].modules` ; `mod tests;` est toujours autorisé (c'est la soupape de R1).
  Liste initiale : les 61 `pub mod` et 2 `#[cfg(test)] mod` de `lib.rs:5-67`, plus
  `mod screens;` (`telegram.rs:30`) : 64 noms.
- **Où** : `archtest`, regex `^\s*(pub(\(crate\))?\s+)?mod\s+([a-z_0-9]+)\s*;` sur les
  sources du daemon, test `daemon_modules_are_whitelisted`. La liste ne peut que raccourcir
  (script R3).
- **Message** : `penelope-daemon : module « notifications » déclaré dans
  crates/penelope-daemon/src/lib.rs:41, absent de la liste blanche (budget.toml
  [daemon].modules) : un nouveau module du daemon se fait dans v1, pas dans la 0.17`.
- **Violation type** : `pub mod notifications;` ajouté à `lib.rs` par un lot « petite
  fonctionnalité ».

### R6. Un module qui prend `&Daemon` est dans la liste blanche, et son couplage ne croît pas

- **Règle** : nombre d'occurrences du mot `Daemon` (hors commentaires, avant le module de
  tests, `DaemonHandle`/`DaemonTokens` exclus par la frontière de mot) par fichier du daemon
  ≤ `[daemon.daemon_users]` ; un fichier absent de la table a un budget de 0. Les blocs
  `impl Daemon` ne sont autorisés que dans `[daemon].impl_daemon` (4 fichiers, §1.5).
- **Pourquoi** : c'est la mesure du « tout passe par le daemon » que la V1 doit défaire ;
  la geler empêche qu'un 43ᵉ module s'y accroche pendant la refonte, et donne une jauge qui
  descend (42 → 4 est l'objectif V1).
- **Où** : `archtest`, test `daemon_coupling_never_grows` ; `UPDATE_BUDGET=1` abaisse.
- **Message** : `penelope-daemon : media.rs nomme le type Daemon 1 fois hors tests, budget
  0 (budget.toml [daemon.daemon_users]) : prendre &Services ou un trait, pas le daemon
  entier`.
- **Violation type** : `pub async fn transcribe(d: &Daemon, …)` ajouté à `media.rs`.

### R7. Fonctions : `clippy::too_many_lines` à 200, allows comptés

- **Règle** : `[workspace.lints.clippy] too_many_lines = "warn"` (promu erreur par le
  `-D warnings` de la CI) et `clippy.toml` `too-many-lines-threshold = 200` ; les 25
  fonctions existantes (§1.4, à confirmer par un vrai run clippy) reçoivent
  `#[allow(clippy::too_many_lines)] // gel 0.17 : <raison>` ; `archtest` compte ces allows
  ≤ `[lints].allow_too_many_lines`, qui ne monte pas.
- **Pourquoi 200 et pas 100** : 92 fonctions dépassent 100, 25 dépassent 200 ; à 100 la
  liste d'allows serait un bruit ; à 200, elle nomme exactement les monstres
  (`command` 1 500, `build_screen` 1 490, `dispatch` 1 155).
- **Où** : `Cargo.toml` racine + `[lints] workspace = true` dans les 17 manifestes +
  `clippy.toml` ; comptage dans `archtest`, test `too_many_lines_allows_never_grow`.
- **Message** : clippy : `this function has too many lines (214/200)` ; archtest :
  `26 #[allow(clippy::too_many_lines)] dans les sources, budget 25 : découper la fonction`.
- **Violation type** : une nouvelle branche de 40 lignes dans `telegram.rs:1093 command`
  ne déclenche rien (déjà en allow) ; une nouvelle fonction de 210 lignes déclenche clippy.
  R3 limite la première par la taille du fichier.

### R8. Les critères d'acceptation ne disparaissent pas

- **Règle** : chaque nom de `[ca].required` (les 71 de `docs/ca-matrix.md`) existe comme
  `fn` dans les sources ; la liste ne peut que s'allonger (le script R3 refuse une
  suppression, accepte un ajout).
- **Où** : `archtest` (réutilise `penelope_evals::ca_matrix::scan_source`, ou copie ses
  20 lignes pour ne pas dépendre d'`evals`), test `acceptance_tests_never_disappear` ; le
  seuil `>= 50` de `tests/ca_matrix.rs:11` devient `>= required.len()`.
- **Message** : `critère d'acceptation disparu : ca_12_1_ticket_to_deploy_runs_end_to_end_and_survives_restarts
  (docs/ca-matrix.md le cite, aucune fonction ne le porte) : le renommer est interdit, le
  déplacer est permis`.
- **Violation type** : une refonte sur `v1` supprime `ticket_to_deploy_e2e.rs` sans
  réécrire le test ailleurs.

### R9. Plancher de couverture par crate, cliquet vers le haut

- **Règle** : couverture de lignes par crate, mesurée par `cargo llvm-cov` sur Linux,
  ≥ `[coverage.crates].<crate>` ; les planchers ne descendent jamais ; `UPDATE_BUDGET=1`
  les remonte à « mesure moins 1 point » (arrondi au dixième inférieur). Pas 100 % par
  fichier comme DSH (`testing.md:10`) : les 17 sites `cfg(target_os = "macos")` ne sont pas
  compilés sur Linux, les tests réservés à macOS n'y tournent pas, 20 tests sont
  `#[ignore]` ; un plancher par crate posé sur la mesure réelle, jamais deviné, est ce qui
  tient. Deux compléments : un plancher **par fichier nouveau** sur `v1`
  (`[coverage].new_file_floor = 90`, fichier absent du point de fourche), dans l'esprit
  « une ligne non couverte est souvent du code mort » ; les fichiers macOS exclus par
  `--ignore-filename-regex`, listés dans `[coverage].ignore`.
- **Où** : job `couverture` dans `ci.yml` (ubuntu-latest, après `tests` : `cargo llvm-cov
  --workspace --json --output-path couverture.json` ; `llvm-tools-preview` ajouté à
  `rust-toolchain.toml:12` ; `cargo-llvm-cov` installé et mis en cache comme `cargo-deny`,
  `ci.yml:119-128`) ; `scripts/check-coverage.sh` lit le JSON, compare aux planchers, et
  applique `new_file_floor` aux chemins absents de `git ls-tree <merge-base>`. Non bloquant
  pendant une semaine de mesures (T17), puis ajouté à `needs` de `livraison`.
- **Message** : `penelope-kernel : couverture de lignes 71,2 %, plancher 73,0 %
  (budget.toml [coverage.crates]) : les lignes ajoutées ne sont pas testées, ou du code
  mort est resté` ; `crates/penelope-daemon/src/telegram/commands.rs : fichier nouveau
  couvert à 64 %, plancher 90 % (budget.toml [coverage].new_file_floor)`.
- **Violation type** : un correctif de 200 lignes dans `scheduler.rs` sans test fait passer
  le daemon de 78,4 % à 78,1 % sous un plancher de 78,2.

### R10. Test de composition réelle pour tout ce qui est visible

- **Règle** (DSH `testing.md:29`, `:33`, `:39`) : chaque commande Telegram du catalogue
  (`penelope_telegram::commands::all`, `commands.rs:35`), chaque outil natif
  (`penelope_tools::all_tools`, `spec.rs:83`) et chaque méthode RPC (`api::method`) est
  exercé par au moins un **scénario** (R11) qui boote la composition réelle
  (`Services::for_tests`, `runtime.rs:161` : store SQLite, journal, sessions, tours,
  plateforme, secrets, gateway Telegram) avec pour seuls simulés le modèle
  (`MockProvider`), le transport Telegram (`MockTransport`, `mock.rs:33`), les serveurs MCP
  (`mcp::testing::FakeConnector`, `mcp.rs:1965`) et l'horloge (`TestClock`), et dont les
  assertions portent sur le monde : ligne de `tg_outbox`, ligne du ledger `effects`,
  fichier écrit, événement du journal ; jamais sur le texte du modèle.
- **Où** : `archtest`, test `every_visible_surface_has_a_scenario` : croise les trois
  catalogues avec les surfaces déclarées dans les `scenario.toml` de
  `crates/penelope-evals/scenarios/**` ; ce qui manque au départ est inscrit dans
  `[scenarios].missing`, liste qui ne peut que rétrécir (script R3).
- **Message** : `commande Telegram /reminders sans scénario de composition
  (crates/penelope-evals/scenarios/) et absente de budget.toml [scenarios].missing`.
- **Violation type** : une nouvelle commande `/veille` livrée avec des tests unitaires du
  rendu seulement.

### R11. Un changement visible ajoute ou met à jour un scénario rejouable sans clé

- **Règle** (DSH `testing.md:55`, `snapshots/AGENTS.md`) : tout lot qui change ce que le
  modèle voit (prompt, outils exposés, format des résultats) ou ce que l'utilisateur voit
  (réponse, carte, commande) ajoute ou met à jour un scénario dans le même lot. Format
  proposé, `crates/penelope-evals/scenarios/<nom>/` :
  - `scenario.toml` : entrées (messages Telegram simulés ou tours CLI, horloge de départ,
    patchs de configuration, fichiers semés), surfaces exercées (commandes, outils,
    méthodes) ;
  - `model.jsonl` : une ligne `Scripted` sérialisée par appel de modèle, dans l'ordre,
    **enregistrée** depuis un vrai modèle (`RECORD_SCENARIO=<nom>` avec
    `OPENROUTER_API_KEY`, par un fournisseur enregistreur qui enveloppe le vrai et écrit
    chaque réponse) ou écrite à la main ; rejouée par un `Responder` (`mock.rs:96`) ;
  - `expected.jsonl` : le monde après le run, normalisé : suite d'événements (`kind`,
    `payload` réduit), textes de `tg_outbox`, lignes de `effects` (`tool`, `state`),
    candidats mémoire, arbre de fichiers de l'espace de travail ; identifiants ULID,
    horodatages et chemins remplacés par des jetons `{{session:1}}`, `{{home}}`, comme
    DSH (`session.v2.jsonl` : `{{session:1}}`, `{{cwd}}`).
  Trois modes : rejeu sans clé (`tests/scenarios.rs`, un cas par répertoire, dans
  `cargo test --workspace`) ; `UPDATE_SCENARIOS=1` régénère `expected.jsonl` quand l'entrée
  et le modèle restent valables (« refresh ») ; `RECORD_SCENARIO=<nom>` ré-enregistre
  `model.jsonl` (« record »). Chaque diff est relu avant commit.
- **Pourquoi c'est le filet de la refonte** : les scénarios sont enregistrés sur la 0.17
  **avant** la branche, puis rejoués sur `v1` à chaque fusion ; ils fixent le comportement
  observable indépendamment de la structure interne, ce qu'aucun test unitaire de
  `telegram.rs` ne fera une fois le fichier éclaté.
- **Où** : `crates/penelope-evals/src/scenario.rs` (chargeur, enregistreur, normalisation,
  comparaison) et `tests/scenarios.rs` (rejeu) ; la partie mécanique de la règle est R10
  (couverture des surfaces) ; la partie « dans le même lot » est une case du modèle de PR,
  une phrase de `CLAUDE.md`, et un contrôle du script R3 : un lot qui touche
  `crates/penelope-telegram/src/commands.rs`, `crates/penelope-tools/src/spec.rs` ou
  `crates/penelope-kernel/src/api.rs` sans toucher `scenarios/` est refusé sans le
  trailer.
- **Message** : `scénario reminders-daily : expected.jsonl diffère (effects : attendu
  [schedule_create completed], obtenu []) : le comportement a changé ; si c'est voulu,
  UPDATE_SCENARIOS=1 puis relire le diff`.
- **Violation type** : un lot change le gabarit de la carte d'approbation ; le rejeu voit
  un `tg_outbox` différent ; le lot doit régénérer `expected.jsonl` et commiter le diff.

---

## 4. La branche V1

### 4.1 Nom, création, protection

- Nom : **`v1`**. Créée depuis `main` **après** que T1 à T7 et T11 sont livrés (la branche
  hérite ainsi du gel et des filets), sur le commit du dernier tag `v0.17.x` du moment.
- Protection : rulesets GitHub (action du propriétaire, hors dépôt) sur `main` et `v1` :
  interdire la suppression et le push forcé. Pas d'obligation de PR : le flux actuel pousse
  directement sur `main` (`CLAUDE.md` § Un lot), et un statut requis ne s'applique pas à
  un push direct. Ruleset de tags `v*` : création réservée à l'application GitHub Actions ;
  le propriétaire garde le contournement, mais les sessions passent par ses droits, donc
  cette barrière n'est pas suffisante seule : voir T7.

### 4.2 CI

Une ligne : `ci.yml:9` `branches: [main]` devient `branches: [main, v1]`. Le job
`livraison` est déjà exclu par `if: github.ref == 'refs/heads/main'` (`ci.yml:141`) ; la
concurrence est par ref (`:13-14`), donc `v1` n'annule pas `main`. `pull_request` couvre
déjà les PR vers `v1` (pas de filtre de base). `signature.yml` se déclenche par chemins,
sans filtre de branche (`signature.yml:6-10`) : rien à changer.

Deux gardes en plus, dans le job `tests` (T7) :

```yaml
- name: La branche porte sa version
  run: |
    case "$GITHUB_REF" in
      refs/heads/main) grep -q '^version = "0\.' Cargo.toml ;;   # retiré à la bascule
      refs/heads/v1)   grep -q '^version = "1\.0\.0-' Cargo.toml ;;
    esac
```

et dans `release.yml`, job `verification`, avant tout : `case "$VERSION" in v1*) test
"${V1_RELEASES:-}" = 1 || { echo "::error::release 1.x interdite avant la bascule"; exit 1; } ;; esac`
avec `V1_RELEASES` variable de dépôt, absente jusqu'à la bascule. Ainsi, même un tag posé
à la main ne publie rien.

### 4.3 Version et journal

- **Version sur `v1` : `1.0.0-alpha.N`**, un bump par lot comme aujourd'hui (`make bump
  V=1.0.0-alpha.7` : `bump.sh:17` accepte le suffixe, les seize lignes se réécrivent). Pas
  de tag, jamais (§2.4 : une release `1.x` serait installée par toutes les 0.17).
- **Journal : le même `docs/progress.md`**, avec un bloc `## Version 1 (branche v1)` inséré
  en tête (avant `## Résumé`), qui reçoit les sections `### 1.0.0-alpha.N`. Le bloc 0.17
  reste dessous et continue de recevoir les `### 0.17.x` par les fusions. Un fichier
  `progress-v1.md` séparé a été écarté : il faudrait rendre `docs.rs:516-548` sensible à
  la branche, l'indexer dans `docs/README.md` (`docs.rs:107`), adapter l'extraction des
  notes de release (`release.yml`, étape « Rédiger les notes »), et fusionner les deux à la
  bascule de toute façon.
- **Le test `docs.rs:530`** apprend le suffixe : `(x, y, z, pre)` avec `pre: Option<u64>`
  et l'ordre semver (`1.0.0-alpha.3 < 1.0.0`). Sur `main`, la plus haute section reste
  `0.17.x` ; si un bloc « Version 1 » y arrivait par un rétroportage maladroit, le test
  échoue (`1.0.0-alpha.N > 0.17.x`) : c'est un garde de plus.
- À la bascule, le bloc 0.17 part dans `docs/progress-0.17.md` (archive, embarquée par
  `build.rs`, indexée dans `docs/README.md`).
- Notes de travail de la refonte : dans les issues, **pas sous `docs/`** (`build.rs` les
  embarquerait dans le binaire et Pénélope les réciterait).

### 4.4 Synchronisation avec `main`

- **Sens** : `main` → `v1` par **fusion** (`git merge --no-ff origin/main`), après chaque
  release 0.17.x (5 par jour en moyenne en septembre) et au moins une fois par jour.
  Écartés : le rebase (branche partagée et longue, un rebase casse toutes les copies de
  travail et les PR ouvertes) et le cherry-pick systématique (18 à 103 commits par jour sur
  `main`, et le merge-base se perd, donc chaque fusion suivante re-conflicte).
- **Script `scripts/sync-main.sh`** (T14) : fusion sans commit ; `Cargo.toml` et
  `Cargo.lock` conflictent à chaque fois (seize lignes de version) : prendre la version de
  `main` puis `sed "s/\"0.17.X\"/\"1.0.0-alpha.N\"/g" Cargo.toml` et `cargo update -w
  --offline` (les autres changements de dépendances de `main` sont conservés) ; `cargo test
  --workspace` ; commit « Fusion de main (v0.17.X) dans v1 ». `docs/progress.md` fusionne
  proprement tant que le bloc « Version 1 » est en tête et le bloc 0.17 dessous.
- **Règle de délai** : une fusion en conflit depuis plus de 24 h bloque tout autre lot sur
  `v1` ; c'est la seule façon d'empêcher la divergence de s'accumuler.
- **`main` ne prend que des corrections** : issues `bug`, régressions, sécurité ; toute
  fonctionnalité va dans `v1`. R4 et R5 rendent ce choix mécanique (pas de nouveau module,
  pas de croissance).
- **Sens inverse** (`v1` → `main`) : cherry-pick `-x` au cas par cas, uniquement pour une
  correction qui vaut pour du code que `v1` n'a pas encore restructuré. Jamais de section
  `1.0.0-alpha.N` dans le lot rétroporté (le test `docs` l'attrape).
- **`budget.toml` dans les fusions** : `main` et `v1` le font tous deux bouger ; en cas
  de conflit, le script T14 prend la valeur la plus stricte clé par clé (minimum des
  plafonds, maximum des planchers de couverture, intersection des listes de dette, union
  de `[ca].required`), jamais la version d'un seul côté. Pour le cliquet, la base de `v1`
  est son point de fourche puis chaque fusion (« Branches own their grandfathered debt
  from the fork », `shrink-ratchet.mts:64`) : un nettoyage fait sur `main` n'est pas
  compté comme une hausse de `v1`, et inversement.

### 4.5 Critère de bascule (`v1` devient `main`)

Conditions, toutes vérifiables sans jugement :

1. CI de `v1` verte : `tests` (Linux), `verification` (macOS, dont les tests
   `cfg(target_os = "macos")`), `dependances`, sur le dernier commit.
2. `budget.toml` de `v1` : `[files.oversized]` **vide**, `[daemon.daemon_users]` réduit aux
   fichiers de `impl_daemon`, `[lints].allow_too_many_lines` = 0.
3. Les 71 CA de `[ca].required` présents (R8), plus ceux ajoutés par T8 à T10 ; matrice
   régénérée.
4. Migration testée depuis la 0.17 courante : le test de T10 passe sur la fixture
   `penelope-0.17.<dernière>.db` régénérée à la version 0.17 finale ; puis essai réel :
   après fusion de `v1` dans `main`, `make bump V=1.0.0-rc.1` (release publiée en
   pré-release, `release.yml:243`, avec `V1_RELEASES=1`), installée **explicitement** sur
   le MBP (`penelope upgrade` par tag, `upgrade.rs:191-200` accepte un tag), démarrage
   confirmé par le mécanisme de #36, `penelope doctor` propre, 24 h de service.
5. Les suites réseau lancées une fois sur `v1` avec les mêmes clés que la ligne de base de
   T12, résultats ≥ ligne de base.
6. Test `docs` vert (pages mises à jour, décision 0011 indexée).
7. `[scenarios].missing` vide et tous les scénarios de `crates/penelope-evals/scenarios/`
   rejouent sans clé sur `v1` (R10, R11) ; les planchers de couverture de `v1` sont
   ≥ ceux de `main` au point de fourche (R9).

Séquence : gel des lots sur `main` (annonce) → dernière fusion `main` → `v1` → fusion
`v1` → `main` (commit de fusion « Version 1 ») → `make bump V=1.0.0-rc.1` → essai réel →
`make bump V=1.0.0` (release pleine : ni `v0.*` ni suffixe) → suppression de `v1`, création
d'une branche `0.17` sur le dernier tag 0.17.x pour un correctif d'urgence (release à la
main par `workflow_dispatch` de `release.yml`, le job `livraison` étant réservé à `main`).

---

## 5. Filets de non-régression

### 5.1 À garder verts pendant toute la refonte

- Les 12 suites hors réseau du §2.5, via les deux `cargo test --workspace` de la CI ;
  `docs`, `ca_matrix`, `workflow_schema`.
- Les 71 CA (R8), dont les bout-en-bout existants :
  `ticket_to_deploy_e2e.rs` (mock tracker MCP, dépôt git local, agent simulé, mock Telegram,
  redémarrage du daemon entre chaque passage, ledger vérifié en SQL `:463-469`),
  `wiki_e2e.rs` (accueil → tours → PDF → épisode → rêve → validateur du wiki),
  `tests/chat_socket.rs` (vraie socket, `chat.stream` puis `chat.send`),
  `tests/log_spans.rs` (#103), `rpc.rs:2461` (aucune méthode déclarée non servie).
- Les invariants d'architecture : `ca_3_1`, `ca_2_3`, cycles, `unsafe`.
- Le contrat de documentation (`docs.rs`) : commandes Telegram = `docs/telegram.md`, clés
  de configuration et outils = références générées d'`install-headless.md`.

### 5.2 Ce qui manque, à écrire **avant** de découper

1. **Un message Telegram simulé donne une réponse et un effet dans le ledger**, sur l'API
   publique seulement. Il n'existe pas tel quel : les cinq tests de `telegram.rs` qui
   utilisent `MockTransport` (`:7960`, `:9537`, `:9701`, `:10190`, `:10779`) sont privés au
   module et testent des cartes ou des boutons ; `ticket_to_deploy_e2e.rs` fait le tour
   complet mais à travers un workflow et des éléments `pub(crate)` (`mcp::testing`). Un
   fichier `crates/penelope-daemon/tests/telegram_e2e.rs` : `Services::for_tests`
   (`runtime.rs:161`) + `TelegramGateway` + `penelope_telegram::mock::MockTransport`
   (`mock.rs:33`) + `MockProvider` scripté (un appel d'outil sans risque, puis un texte) ;
   vérifie la ligne de `tg_outbox`, l'effet `completed` dans `effects`, les événements du
   tour ; redémarre les services et revérifie. Comme il ne voit que l'API publique, il
   survit à tous les déplacements internes de `v1`. (T8)
2. **Approbation de bout en bout** : outil à risque → carte → bouton approuver → exécution
   → ledger ; refus → transmis au modèle ; « toujours » → pas de seconde carte. (T9)
3. **Contrat RPC figé** : un fichier doré par méthode de `penelope_kernel::api::method`
   (formes JSON requête/réponse comparées par clés), pour que la CLI 0.17 et 1.0 parlent
   la même socket. (T9)
4. **Migration depuis une vraie base 0.17** : `migrations.rs:1010-1033` ne rejoue que
   `0001` puis migre ; aucune fixture de base (aucun `.db` dans le dépôt, seul
   `platform/tests/fixtures/scan.pdf` existe). Une base générée par la 0.17 avec
   conversation, souvenir, tâche planifiée, ligne d'outbox, commitée ; test : les 18
   migrations (`migrations.rs:17-91`) s'appliquent, `all_prd_tables_exist` (`:1177`), les
   lignes semées se relisent. (T10)
5. **Configuration 0.17 relue par la V1** : un `config.toml` d'exemple portant chaque clé
   de la référence générée, chargé sans erreur ni avertissement. (T10)
6. **Les CA ne disparaissent pas** : R8, liste des 71 noms figée. (T11)
7. **Ligne de base des suites réseau** : elles n'ont jamais tourné ; sans ligne de base,
   la V1 ne peut pas prouver qu'elle ne régresse pas sur le vrai modèle. (T12)
8. **Un corpus de scénarios enregistrés sur la 0.17** (R11), au moins un par commande
   Telegram, par outil natif et par méthode RPC (R10), enregistré avec le vrai modèle
   puis rejoué sans clé : la source de vérité du comportement pendant la refonte. (T18,
   T19)
9. **Une mesure de couverture** avant la branche, pour que `v1` prouve qu'elle ne perd
   pas de tests en déplaçant du code (R9). (T17)

---

## 6. Tâches

Taille : S ≤ ½ journée, M ≤ 2 jours, L > 2 jours. Chaque tâche livrée sur `main` est un
lot : code, section `### 0.17.x`, `make bump`.

| # | Tâche | Périmètre | Dépend de | Critère de fin | Taille |
|---|---|---|---|---|---|
| T1 | Budget et règles de taille (R1, R2, R3 côté test) | `archtest/src/lib.rs` (+ parcours de `tests/`, lecture de `budget.toml`, `UPDATE_BUDGET`, les trois cas d'échec de R3 : nouveau, hausse, périmé), `budget.toml` avec les 42 entrées du §1.2, `archtest/Cargo.toml` (`toml` déjà là) | rien | `cargo test -p penelope-archtest` vert à HEAD ; rouge quand `telegram.rs` gagne une ligne (essai local) ; `UPDATE_BUDGET=1` n'écrit jamais une valeur plus haute | S |
| T2 | Script de non-remontée (R3 côté CI) | `scripts/check-budget.sh` : base = merge-base en PR, `github.event.before` en push, renommages par `--find-renames`, trailer `Dérogation-budget:`, mode `--staged` facultatif ; étape dans le job `tests` de `ci.yml` avec fetch de la base | T1 | Une PR qui remonte un nombre est rouge avec le message ; la même PR avec le trailer est verte | S |
| T3 | Plafond de crate et liste blanche des modules (R4, R5) | `archtest`, `budget.toml` `[crates]`, `[daemon].modules` (64 noms) | T1 | `pub mod x;` ajouté au daemon → rouge ; +2 500 lignes dans le daemon → rouge | S |
| T4 | Couplage au `Daemon` (R6) | `archtest`, `[daemon.daemon_users]` (42 entrées), `[daemon].impl_daemon` (4) | T1 | `fn f(d: &Daemon)` ajouté à `media.rs` → rouge ; un cinquième `impl Daemon` → rouge | S |
| T5 | Lints de workspace (R7) | `Cargo.toml` `[workspace.lints]`, `[lints] workspace = true` × 17, `clippy.toml` (seuil 200), allows commentés sur les fonctions existantes (liste exacte par le run clippy), `[lints].allow_too_many_lines` | T1 | `cargo clippy --workspace --all-targets -- -D warnings` vert ; une fonction de 210 lignes nouvelle → rouge ; le compte d'allows ne monte pas | M |
| T6 | Le gel écrit là où les sessions le lisent | `CLAUDE.md` § « Gel de la dette » (règles, `UPDATE_BUDGET`, trailer, « un nouveau module va dans v1 ») ; `pull_request_template.md` (case) ; `docs/progress.md` § Routine ; `.github/workflows/README.md` corrigé (trois workflows, tests Linux, tag par la CI) ; `docs/decisions/0011-gel-0.17-et-v1.md` ; `AGENTS.md` (copie non suivie de `CLAUDE.md`) suivi ou supprimé | T1 à T4 | Test `docs` vert (0011 indexé), section et bump | S |
| T7 | Gardes de version pour deux branches | `docs.rs:530` accepte `-alpha.N` ; `ci.yml:9` `[main, v1]` + étape « la branche porte sa version » ; `release.yml` refuse `v1*` sans `V1_RELEASES` ; `upgrade.rs:137` `parse_version` renvoie `None` sur un suffixe (test `is_newer("v1.0.0-alpha.1", "0.17.58") == false`, un tag explicite reste installable) | rien | Tests verts ; `workflow_dispatch` de `release.yml` avec `tag=v1.0.0-alpha.0` échoue à la garde sans rien publier | S |
| T8 | Filet Telegram public | `crates/penelope-daemon/tests/telegram_e2e.rs`, nommé `ca_14_<n>_…`, matrice régénérée | rien | Vert sur Linux et macOS ; passe avec `Services` reconstruits ; n'importe aucun élément `pub(crate)` | M |
| T9 | Filet approbation et contrat RPC | test d'approbation (approuver, refuser, toujours) ; `tests/golden/<méthode>.json` pour chaque méthode de `api::method`, comparés par clés | T8 | Deux tests verts ; les fichiers dorés commités ; une clé retirée d'une réponse → rouge | M |
| T10 | Filet migration et configuration | générateur de fixture (`#[ignore]`, `UPDATE_FIXTURE=1`), `crates/penelope-store/tests/fixtures/penelope-0.17.<x>.db`, test de migration ; `config.toml` d'exemple complet chargé par `penelope-kernel` | rien | Trois tests verts ; taille de la fixture notée dans la section de version ; régénérée seulement par la variable | M |
| T11 | Les CA figés (R8) | `[ca].required` (71 noms), test `archtest`, seuil de `tests/ca_matrix.rs:11` relevé, script T2 refuse une suppression | T1, T2 | Renommer un `ca_*` → rouge avec le nom ; en ajouter un → vert | S |
| T12 | Ligne de base des suites réseau | Lancer une fois `ctx-recall`, `live-openrouter`, `live-telegram`, `mem-longitudinal`, `mem-bench-live` (clés du propriétaire ; `ab-hermes` si l'instance Hermes existe) ; résultats et date dans `docs/progress.md` § Suites | propriétaire | Tableau `progress.md:35-51` sans « pas encore lancées » ; échecs listés comme tels | S |
| T13 | Création de `v1` | `git branch v1 <dernier tag 0.17>` ; premier lot : bloc `## Version 1 (branche v1)` + `### 1.0.0-alpha.1`, `make bump V=1.0.0-alpha.1` ; rulesets posés par le propriétaire | T1 à T7, T11 | Run CI sur `v1` : trois jobs verts, pas de job `livraison`, aucun tag, aucune release | S |
| T14 | Synchronisation outillée | `scripts/sync-main.sh` (fusion, résolution `Cargo.*` par `sed` + `cargo update -w --offline`, tests, commit nommant le tag) ; `CLAUDE.md` § « Travailler sur v1 » (cadence, règle des 24 h, corrections seules sur `main`) | T13 | Première fusion réelle après un bump 0.17.x : seuls `Cargo.toml` et `Cargo.lock` ont conflicté, résolus par le script | S |
| T15 | Pré-découpage sans risque des 7 fichiers > 3 000 | Sur `main` : déplacer les tests inline de `telegram.rs`, `dream.rs`, `agent.rs`, `workflow.rs`, `executor.rs`, `mcp.rs`, `engine.rs` vers `<module>/tests.rs` (sous-modules si > 1 500, R2) ; aucune ligne de code déplacée ; `UPDATE_BUDGET=1` | T1, T2 | Même nombre d'attributs de test avant/après (1 775) ; CI verte ; `telegram.rs` à 7 948, budget abaissé ; fait **avant** T13 pour que les fusions ne conflictent pas sur des tests | M |
| T16 | Bascule outillée | `scripts/switch-check.sh` : vérifie mécaniquement §4.5 points 1 à 3 et 6 (`budget.toml` vide, `[ca].required`, statut CI par `gh run list`, test de migration) ; checklist des points 4 et 5 dans la décision 0011 ; procédure `V1_RELEASES` | T7, T10, T11 | Le script, lancé aujourd'hui sur `v1`, rend non-zéro et liste les critères manquants | S |
| T17 | Couverture mesurée puis plafonnée (R9) | `llvm-tools-preview` dans `rust-toolchain.toml:12` ; job `couverture` (ubuntu, `cargo llvm-cov --workspace --json`, outil en cache comme `cargo-deny`) ; `scripts/check-coverage.sh` ; `[coverage]` posé sur la première mesure moins 1 point ; non bloquant une semaine, puis dans `needs` de `livraison` | T1, T2 | Première mesure par crate dans la section de version ; une PR qui fait baisser un crate sous son plancher est rouge ; `UPDATE_BUDGET=1` ne baisse jamais un plancher | M |
| T18 | Scénarios rejouables (R11) | `crates/penelope-evals/src/scenario.rs` (chargeur, enregistreur enveloppant le vrai fournisseur, normalisation par jetons, comparaison du monde), `tests/scenarios.rs`, format `scenario.toml` + `model.jsonl` + `expected.jsonl`, `UPDATE_SCENARIOS`, `RECORD_SCENARIO` ; cinq premiers scénarios : message simple, appel d'outil, approbation, `/reminders`, compaction | T8 | Les cinq rejouent sans clé sur Linux et macOS ; l'un d'eux a été enregistré avec le vrai modèle ; changer un gabarit de carte fait échouer le rejeu avec le diff | L |
| T19 | Couverture des surfaces par des scénarios (R10) | `archtest` : croisement des catalogues (commandes `commands.rs:35`, outils `spec.rs:83`, méthodes `api::method`) avec `scenarios/**` ; `[scenarios].missing` initial ; script R3 refuse un lot qui touche un catalogue sans toucher `scenarios/` ; puis un scénario par surface manquante, par lots de cinq (chaque lot fait rétrécir la liste) | T18 | Commandes et outils couverts avant T13 ; `missing` vide à la bascule (§4.5) | L |

Ordre conseillé : T1 → T2 → T3 → T4 → T7 (cette semaine, sur `main`) ; T6 dès que les
messages d'erreur sont stabilisés ; T17 lancé tôt (une semaine de mesures avant d'être
bloquant) ; T8 puis T18 puis T19 (commandes et outils), T10, T11, T15 avant T13 ; T5 et T9
en parallèle de la création de `v1` ; T12 dès que le propriétaire fournit les clés ; T14
juste après T13 ; le reste de T19 sur `v1` ; T16 en dernier.

---

## 7. Risques et bornes

1. **Faux positifs des règles de taille.** (a) Les tables de données (`screens.rs`
   `build_screen` 1 490 lignes, `spec.rs` `all` 908, `commands.rs` `all` 352,
   `templates.rs` 294) sont longues par nature : elles sont dans la liste de référence,
   donc rien ne change tant qu'elles ne grossissent pas ; R7 leur donne un allow commenté
   « table ». (b) Un correctif légitime sur un fichier de la liste ajoute des lignes : il les
   paie (T15 dégage la marge : un test déplacé suffit) ou porte le trailer, visible dans
   `git log --grep=Dérogation-budget`, à relire chaque mois ; attendu 0 à 2 par mois.
   (c) Un reformatage : rustfmt est épinglé (`rust-toolchain.toml:11`), sans
   `rustfmt.toml` ; une montée de chaîne qui reflue du code demande une dérogation
   ponctuelle. (d) La détection de `#[cfg(test)]` est textuelle (colonne 0), comme celle de
   `forbidden_patterns` (`lib.rs:170-176`) : un `#[cfg(test)]` indenté n'est pas vu, ce qui
   rend la règle plus stricte, jamais plus laxiste. (e) `archtest` ne voit pas `tests/`
   aujourd'hui (`lib.rs:92`) : T1 l'ajoute, sinon R2 ne compte rien.

2. **Coût de deux branches.** CI doublée (9 à 16 min par run ; minutes gratuites sur dépôt
   public, la file macOS est le coût) ; fusions de plus en plus conflictuelles à mesure que
   `v1` déplace du code. Bornes : corrections seules sur `main` (R4, R5 mécaniques) ;
   T15 fait sur `main` avant la branche (les tests, moitié du volume des sept gros
   fichiers, ne conflicteront pas) ; fusion après chaque release, règle des 24 h ; script
   T14 pour les seize lignes de version. Un même `docs/progress.md` à deux blocs : la seule
   friction humaine attendue.

3. **Dérive de `main` pendant la refonte.** Le gel arrête la croissance, pas les
   changements de comportement dans les correctifs. Chaque fusion est rejouée par la suite
   complète sur `v1` ; un correctif dans `telegram.rs` alors que `v1` l'a éclaté en dix
   fichiers se reporte à la main : tenir dans la décision 0011 une table « ancien fichier →
   nouveaux fichiers », et découper par **déplacements purs** (un commit = un déplacement,
   sans changement de logique) pour que `git log --follow` et `git blame` survivent.
   Borne temporelle : si `v1` n'a pas basculé dans les six semaines, arrêter et
   re-planifier ; chaque semaine de plus renchérit toutes les fusions suivantes.

4. **Release accidentelle depuis `v1`.** `livraison` est réservé à `main` (`ci.yml:141`),
   mais un tag `v1.0.0-alpha.1` posé à la main déclenche `release.yml` (`:8`) et les
   instances 0.17 l'installeraient (`upgrade.rs:137-149`, `:191-215`). Trois gardes (T7) :
   `release.yml` refuse `v1*` sans `V1_RELEASES`, `parse_version` ignore les suffixes,
   `CLAUDE.md` l'écrit. Le ruleset de tags est une quatrième couche, pas une garantie.

5. **Le gel bloque un module d'urgence sur `main`** (sécurité) : dérogation par le trailer
   et édition du budget, auditable ; le lot suivant sur `v1` reprend la même correction.

6. **Chiffres de tests contradictoires** : 1 735 (`progress.md:11`) contre 1 775 attributs
   (20 ignorés, une partie sous `cfg(target_os = "macos")`). Le critère de bascule dit
   « CI verte sur les deux jobs », jamais un nombre.

7. **Les mesures bougent** : l'arbre de travail avait déjà 1 000 lignes de plus que HEAD
   dans le daemon (78 557 contre 77 548, `audit.rs` et `prompt_snapshot.rs` non suivis). Les
   valeurs de `budget.toml` sont à relever au commit qui pose le gel, pas copiées d'ici ;
   la marge de R4 (80 000) tient compte de ces lots en cours.

8. **Couverture : coût et bruit.** Une compilation instrumentée plus la suite complète :
   compter 1,5 à 2 fois le job `tests` (9 à 16 min mesurées), sur Linux seulement, donc
   sans les tests macOS ; un plancher posé à « mesure moins 1 point » absorbe le bruit des
   tests à horloge réelle (`tests/chat_socket.rs`), pas une baisse réelle. Une semaine non
   bloquante avant de l'ajouter à `needs` (T17), et jamais de plancher deviné.

9. **Fragilité des scénarios.** Un scénario qui compare la prose du modèle casse à chaque
   changement de gabarit : on compare le monde (événements, ledger, outbox, fichiers) et on
   normalise identifiants et dates par jetons, comme DSH (`snapshots/AGENTS.md`,
   « Committed sessions are normalization fixed points »). Un enregistrement avec le vrai
   modèle coûte quelques centimes et n'est pas déterministe : on enregistre une fois, on
   rejoue sans clé, et `RECORD_SCENARIO` n'est relancé que quand le prompt change. Le
   risque restant est l'oubli : le script R3 refuse un lot qui touche un catalogue sans
   toucher `scenarios/`.
