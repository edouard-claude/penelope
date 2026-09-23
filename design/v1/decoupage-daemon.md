# Découpage de `penelope-daemon` : cartographie du couplage et plan d'extraction V1

Mesures prises le 23 septembre 2026 sur l'arbre de travail (branche `main`, 0.17.58, avec le
lot #205 non commité : `audit.rs`, `prompt_snapshot.rs` et +720 lignes sur `agent.rs`,
`purge.rs`, `doctor.rs`, `rpc.rs`). D'où de petits écarts avec les chiffres de l'énoncé :
78 532 lignes au lieu de 77 548, `agent.rs` 4 239 au lieu de 4 192, `rpc.rs` 2 481 au lieu
de 2 467. Toutes les lignes citées sont celles de cet arbre.

Références utilisées : le code, les `Cargo.toml`, `docs/progress.md`, `docs/README.md`,
`docs/decisions/`, `CLAUDE.md`, `.github/workflows/ci.yml`, et les issues #185, #204 via
`gh issue view`. `spec/PRD-penelope.md` n'a pas été lu.

## 0. Commandes de mesure

```bash
# tailles
find crates/penelope-daemon/src -name '*.rs' | xargs wc -l | sort -rn
# rôle : première ligne //!
for f in crates/penelope-daemon/src/*.rs; do grep -m1 '^//!' "$f"; done
# graphe interne hors tests (coupe au premier `#[cfg(test)]` en colonne 0)
#   -> scratchpad/deps.sh : occurrences de `crate::<module>` par module
# degré entrant : inversion de deps.txt (perl)
# qui a besoin de Daemon ou de Services (hors tests) :
grep -cE '&Daemon\b|Arc<Daemon>' ; grep -cE '&Services\b|Arc<Services>'
#   -> scratchpad/sigs.pl : premier paramètre de chaque `fn` (Daemon / Services / autre)
# ce que chaque gros module appelle sur `d.`/`daemon.`/`self.daemon.`
grep -oE '\b(daemon|self\.daemon|d)\.[a-z_]+(\.[a-z_]+)?' | sort | uniq -c
# ports
grep -rn 'pub trait' crates/*/src ; grep -rn 'impl .* for ' ; grep -rln 'dyn <Trait>'
# impl inhérents de Daemon hors runtime.rs
grep -n '^impl Daemon\|^impl .* for Daemon' crates/penelope-daemon/src/*.rs
# structure des fichiers géants (éléments de premier niveau et méthodes, avec lignes)
grep -n '^pub struct\|^pub enum\|^impl\|^pub fn\|^pub async fn\|^fn \|^async fn \|^    pub async fn \|^    async fn '
# séparation code / tests : ligne du premier `#[cfg(test)]`
# churn : git log --since=2026-08-23 --oneline -- <fichier> | wc -l
# consommateurs externes : grep -rhoE 'penelope_daemon::[a-z_]+(::[A-Za-z_]+)?' crates/penelope-cli crates/penelope-evals
# tests macOS : grep -rn 'cfg(target_os = "macos")' crates/penelope-daemon/src
```

Les scripts (`deps.sh`, `sigs.pl`) et la sortie brute (`deps.txt`) sont dans le scratchpad,
à côté de ce fichier.

## 1. Cartographie

### 1.1 Les 63 modules

63 modules déclarés dans `crates/penelope-daemon/src/lib.rs:5-67` (61 `pub mod`, 2
`#[cfg(test)] mod`), plus le sous-module `telegram/screens.rs` (`telegram.rs:30`).
78 532 lignes, dont 32 900 lignes de tests inclus dans les mêmes fichiers.

Colonnes : **Total / Code** (lignes totales / hors `#[cfg(test)]`), **Dépend de** (modules
`crate::x` cités hors tests, les plus fréquents), **Dépendu par** (nombre de modules qui le
citent, hors tests e2e), **Besoin** : `D` = des fonctions prennent `&Daemon` ou
`Arc<Daemon>` (nombre entre parenthèses, d'après `sigs.pl`), `S` = seulement `&Services`,
`0` = ni l'un ni l'autre.

| Module | Total / Code | Rôle (ligne `//!`) | Dépend de | Dépendu par | Besoin |
|---|---|---|---|---|---|
| telegram (+screens) | 13 716 / 7 948 (+2 435) | Passerelle Telegram (§14) : réception, commandes, approbations, brouillons, envoi | elicitation 19, agent 15, session_ops 14, titles 13, onboarding 13, upgrade 11, workflow 10, vault_ops 8, conversation 8, rpc 7, compaction 5, mcp_auth 4, ingest 4, tasks 3, media 3, executor 3, dream 3 (25 modules) | 5 (dream, doctor, scheduler, session_project, supervisor) | D (5 fn) + champ `daemon: Arc<Daemon>` (`telegram.rs:278`) : kv 43 appels, chat_session_for 18, bus 13, hooks 7, enqueue 4 |
| dream | 5 859 / 3 453 | Consolidation nocturne (§6.8), digest du matin, entretien du vault | vault_ops 13, conversation 11, vault_git 4, telegram 3, session_notes 2, scheduler 2, mem_audit 2, bus 2 (17 modules) | 7 | D (19 fn) : services 13, kv 12, provider_for 1, hooks.messenger 1 |
| agent | 4 239 / 2 146 | Boucle d'agent : un tour de bout en bout (§3.3, §4.2, §9) | cache_audit 7, prompt_snapshot 5, approval_mode 5, executor 4, budget_alert 3 | 11 | S (1 fn) |
| workflow | 4 044 / 2 912 | Moteur de workflows (§12.7), run durable étape par étape | executor 9, codex_scope 4, agent 4, scheduler 3, vision 2, conversation 2 (11) | 10 | D (17 fn) : services 15, workflows.* 9 (son propre `State` porté par Daemon), hooks.messenger 5, hooks.mcp 3, provider_for 2 |
| executor | 3 712 / 2 477 | Exécution des outils natifs et méta-outils MCP (§8.9, §11) | selfknow 6, machine 4, agent 4, workflow 3, vault_ops 3, scheduler 3, elicitation 3, vision 2, tools_on_demand 2, selfdocs 2, runtime_events 2, conversation 2 (18) | 10 | S (4 fn) |
| mcp | 3 291 / 1 953 | Superviseur MCP (§8.6) : serveurs `mcp.d/` | executor 2, elicitation 2, mcp_auth 1 | 4 | S (6 fn) |
| engine | 3 173 / 1 218 | Exécution d'un tour, de la file à la réponse (§3.3, §10.3) | compaction 5, review 4, cache_audit 4, usage_feedback 3, selfknow 3, embeddings 3, codex_scope 3, vision 2, titles 2, media 2, conversation 2, rpc 1, backup 1 (19) | 1 (compaction) | `impl Daemon` (`engine.rs:145-1179`) et `impl Admin for Daemon` (`engine.rs:83`) |
| rpc | 2 481 / 1 742 | Serveur RPC local (§2.7, §15), JSON-RPC 2.0 NDJSON | doctor 13, session_ops 9, dream 7, agent 7, codex_auth 6, scheduler 5, workflow 4, session_project 4 (33 modules) | 4 (engine, selfknow, supervisor, telegram) | D (7 fn) |
| doctor | 2 357 / 1 969 | `penelope doctor` (§2.11) | upgrade 4, telegram 2, mcp 2, embeddings 2, codex_* 6, vault_inventory, skill_deps, mem_split, machine, dream | 1 (rpc) | S (29 fn) + D (1 : `embedding_check`) |
| compaction | 2 225 / 1 373 | Compaction niveau 3 (§5.4), résumés LCM | runtime 7, codex_scope 2, scheduler 1, episodes 1, engine 1, cache_audit 1, agent 1 | 6 | D (18 fn) : `d.compaction.*` (son `State` porté par Daemon), bus.is_active 3, provider_for 2, kv 6 |
| hermes | 1 971 / 1 565 | Import d'une instance Hermes (§7, §8.7) | vault_ops 2, conversation 2, scheduler 1, dream 1, bus 1 | 1 (rpc) | D (3 fn) + S (3) |
| upgrade | 1 926 / 1 192 | Mise à jour du binaire (§2.12) | scheduler 1, mcp_auth 1 | 5 | D (3 fn) : `confirm_when_healthy`, `rpc`, `change` |
| scheduler | 1 833 / 1 146 | Ordonnanceur (§12.9, §6.9) | agent 6, bus 3, telegram 2, ingest 1, executor 1, dream 1 | 12 | D (25 fn) : services 15, kv 11, hooks.messenger 2, hooks.telegram 1, handle 2, bus 1 |
| mcp_auth | 1 246 / 690 | OAuth des serveurs MCP HTTP (§8.5) | scheduler 1 | 5 | D (5 fn) : hooks.messenger, hooks.mcp_supervisor, kv |
| ingest | 1 225 / 855 | Ingestion de documents (§6.13) | vault_ops 10, conversation 4, concepts 3, scheduler 1, media 1, agent 2 (`decide_approval`) | 4 | D (6 fn) : kv 8, hooks.messenger 1 |
| conversation | 1 222 / 687 | Transcript d'une session, assemblage du prompt (§5.2, §6.3) | session_project 10, agent 3, prompt_snapshot 2, workflow 1 (kv), session_notes 1, machine 1, episodes 1, bus 1 | 19 | S (10 fn) |
| purge | 1 185 / 533 | Purge RGPD (#46) et rétention | runtime | 3 | D (4 fn) : kv |
| codex_auth | 1 040 / 695 | Compte ChatGPT et jetons `codex` (#142) | workflow 5 (kv), codex_quota 1, bus 1 | 5 | S (14 fn) + D (2 : `refresh_loop`, `notify_disconnected`) |
| supervisor | 926 / 629 | Supervision : reprise, boucles de fond, socket, arrêt | scheduler 4, runtime 4, mcp_auth 3, workflow 2, vault_git 2, tasks 2, purge 2, mcp 2 (22) | 1 (runner via run_pool) | `impl Daemon { run }` (`supervisor.rs:90`) |
| concepts | 885 / 750 | Wiki de concepts (#22) | vault_ops 9, runtime 5, conversation 5, embeddings 1 | 7 | D (6 fn) + S (4) |
| vault_ops | 881 / 570 | Chemin d'écriture de la mémoire (§6.4, §6.10) | concepts 4, vault_inventory 1, media 1, ingest 1 | 14 | S (7 fn) |
| runtime | 821 / 677 | Composition : `Services`, `Daemon`, `Hooks` | executor 8, bus 4, mcp 3, codex_auth 3, agent 3, workflow 2, tasks 2, embeddings 2, compaction 2, elicitation 1, codex_quota 1 | 54 | définit les deux |
| elicitation | 776 / 577 | Élicitation MCP (§8.4, #12) : `Broker`, `OwnerChannel` | aucun | 4 | 0 (feuille) |
| review | 756 / 484 | Revue de fond après un tour (§6.6) | vault_ops 3, secret_shelf 1, conversation 1, codex_scope 1 | 2 | D (2 fn) : provider_for |
| backup | 756 / 522 | Sauvegarde chiffrée (#42) | conversation 1, bus 1 | 3 | D (9 fn) : kv 4, hooks.messenger 1 |
| onboarding | 748 / 701 | Entretien d'accueil (#21) | vault_ops 5, machine 1, conversation 1, bus 1 | 2 | D (12 fn) : kv 4, chat_session_for 1 |
| selfknow | 733 / 510 | `self_status` et `config_set` ; trait `Admin` | rpc 3 (`round_usd`), upgrade 2, selfdocs 2, codex_quota 2 | 3 | S (4 fn) |
| runner | 690 / 290 | Pool de runners (§3.3) | tasks 4, scheduler 2, workflow 1, agent 1 | 1 | `impl Daemon { deliver }` (`runner.rs:279`) + D (6 fn) |
| episodes | 687 / 428 | Frontières d'épisode (§6.6) | vault_ops, review, conversation, concepts, codex_scope, cache_audit | 6 | D (6 fn) : kv 8, provider_for 1 |
| ticket_to_deploy_e2e | 641 / 641 (test) | CA 12 de bout en bout | workflow 6, mcp 2, bus 2, telegram, runner | 0 | D (test) |
| vision | 575 / 444 | Vision (§10.4, #125) | media 2 | 3 | D (2 fn) : provider_for |
| machine | 566 / 379 | Ce que Pénélope sait de sa machine (#156) | workflow 2 (kv) | 6 | S (2 fn) |
| session_notes | 544 / 364 | Notes de travail d'une session (#32) | vault_ops 3, conversation 3, concepts 1 | 6 | S (10 fn) |
| voice | 484 / 424 | Réponses vocales (#41) | codex_scope 1, bus 1 | 3 | D (4 fn) : provider_for, hooks.messenger |
| session_ops | 470 / 353 | Fork, retour en arrière, export, fermeture (§4.4, §15) | session_notes 2, episodes 2, vault_ops 1 | 2 | D (6 fn) : bus 2 |
| selfdocs | 462 / 320 | Documentation embarquée (#34) ; `include!(OUT_DIR/docs.rs)` (`selfdocs.rs:14`) | aucun | 2 | 0 (feuille, mais lié à `build.rs`) |
| cache_audit | 456 / 270 | Empreinte du cache de prompt (#17) | runtime | 4 | S |
| mem_audit | 451 / 394 | Audit de la mémoire (#23) | conversation 2, vault_ops 1 | 3 | D (3 fn) : kv, hooks.mcp_supervisor |
| embeddings | 419 / 322 | Embeddings (§6.11, #11) ; `State` porté par Daemon | codex_scope 1 | 8 | D (6 fn) : provider_for, `d.embeddings` |
| runtime_events | 416 / 238 | Contrat des événements runtime | runtime | 2 | D (1 fn : `serve`) |
| bus | 407 / 343 | Bus, registre des tours actifs, attente d'une issue ; `ChannelDelivery` | agent (TurnEvent, TurnOutcome) | 20 | 0 |
| mem_split | 394 / 313 | Découpage d'une entrée (#145) | codex_scope 1 | 3 | D (2 fn) : provider_for |
| audit | 393 / 258 | Reconstitution d'une requête (#205, non commité) | prompt_snapshot | 1 | S |
| wiki_e2e | 369 / 369 (test) | Vault de référence (#29) | onboarding 4, episodes 2, dream 2 | 0 | test |
| prompt_snapshot | 349 / 179 | Instantanés du prompt système (#205, non commité) | purge | 4 | S |
| tasks | 343 / 248 | Boucles de fond surveillées (#84) | runtime | 5 | D (4 fn) : `d.tasks`, `d.handle`, clock, events |
| vault_inventory | 286 / 224 | Inventaire du vault (#15) | concepts 2, session_notes 1, conversation 1 | 5 | S + D (1) |
| media | 259 / 217 | Pièces jointes hors base (§14.4) | executor 1 | 5 | S |
| budget_alert | 238 / 138 | Alerte de budget (#20) ; `UsageWatcher` | bus 1 | 3 | D (2 fn) : hooks.messenger |
| approval_mode | 229 / 171 | Mode d'approbation (#111) | executor 1 | 3 | S |
| vault_git | 205 / 205 | Historique git du vault (#27) | conversation 3, dream 1 (`vault_sync`) | 3 | S (5) + D (1) |
| titles | 203 / 181 | Titres de session (#2) | codex_scope 1 | 3 | D (2 fn) : provider_for |
| skill_deps | 202 / 162 | Dépendances d'une skill (#146) | aucun | 2 | 0 (feuille) |
| codex_scope | 202 / 134 | Périmètre du fournisseur `codex` | bus 4 | 12 | D (2 fn) mais n'utilise que `d.services` |
| codex_quota | 193 / 136 | Jauges du plan ChatGPT ; `QuotaSink` | workflow 2 (kv), bus 1 | 6 | S (3) + D (1 : hooks.messenger) |
| session_project | 183 / 183 | Sujet de travail d'une session (#119) | runtime 2, telegram 1 (`topic_name_key`), episodes 1 | 5 | S (5) + D (1) |
| images | 179 / 124 | Génération d'images (§10.4) | runtime | 1 | D (1 fn) : provider_for |
| skill_install | 171 / 124 | Import de skills tierces (#146) | skill_deps 3 | 1 | D (1 fn) |
| secret_shelf | 152 / 120 | Secrets dans ce qui est à retenir (#37) | runtime | 3 | S |
| tools_on_demand | 109 / 72 | Outils natifs à la demande (#104) | runtime | 2 | S |
| usage_feedback | 95 / 95 | Retour d'usage de la mémoire (#105) | runtime | 2 | S |
| lib | 73 / 53 | déclaration des modules, `VERSION` | | | |

Quatre faits structurants :

0. **Deux mesures du besoin de `Daemon`, selon le motif.** Par `grep "d: &Daemon\|daemon:
   &Arc<Daemon>\|self: &Arc<Self>\|&Daemon"` (mesure du lead, reprise telle quelle) : 26 des
   63 modules prennent `&Daemon` ou `Arc<Daemon>` en paramètre. Par analyse des signatures
   (`sigs.pl`, motif `: &?Arc<Daemon>` ou `: Arc<Daemon>` compris) : 40 modules ont au
   moins une fonction dont un paramètre est `Daemon`, et 41 fichiers nomment le type hors
   tests. L'écart vient des boucles qui prennent `d: Arc<Daemon>` par valeur
   (`tasks.rs:151`, `runner.rs:14, 45`, `supervisor.rs:290-354`, `workflow.rs:417`) et
   des `d: &Arc<Daemon>` (`runner.rs:65-152`), absents du premier motif.
1. **`runtime` est cité par 54 modules sur 62**, `bus` par 20, `conversation` par 19
   (uniquement pour `vault_dir`, `conversation.rs:664-667`, trois lignes), `vault_ops` par
   14, `codex_scope` par 12, `scheduler` par 12.
2. **Le besoin de `Daemon` se réduit à cinq membres** dans la quasi-totalité des modules
   `D` : `kv_get/kv_set/kv_delete` (méthodes de `Daemon` définies dans
   `engine.rs:1130-1179`, qui n'utilisent que `self.services.store`), `provider_for`
   (`runtime.rs:397-444`), `hooks.messenger()` (`runtime.rs:337`), `handle.is_shutting_down()`
   (`runtime.rs:288`) et les trois `State` portés par `Daemon` (`runtime.rs:311-316`).
   Une fois `kv_*` posé sur `Services` et `provider_for` derrière un trait, 30 modules
   passent de `D` à `S` sans autre changement.
3. **Trois `impl Daemon` vivent hors de `runtime.rs`** : `engine.rs:145` (20 méthodes dont
   `run_turn`, `enqueue_*`, `chat_session_for`, `kv_*`), `supervisor.rs:90` (`run`),
   `runner.rs:279` (`deliver`), plus `impl Admin for Daemon` (`engine.rs:83`). La règle des
   orphelins de Rust oblige ces fichiers à rester dans la crate qui définit `Daemon`.

### 1.2 Schéma du couplage réel

```
                 penelope-cli ─────────────────────────────┐   penelope-evals
        commands.rs:1891 Daemon::new ; :713 doctor::render │   dream::run, runner::process,
        :1054 telegram::parse_params ; :1214 upgrade::*     │   vault_ops::reindex, ingest::ingest…
                                                            ▼
 ┌──────────────────────────── penelope-daemon (78 532 l.) ────────────────────────────────┐
 │                                                                                         │
 │  telegram + screens (16 151 l.) ─► 25 modules, Rpc::new(daemon) ×7, Daemon ×12 membres  │
 │       ▲ deep_link (dream.rs:3061)  ▲ topic_name_key (scheduler, conversation,           │
 │       │                            │ session_project)  ▲ shown/seen_chats (doctor)      │
 │                                                                                         │
 │  rpc (102 méthodes) ─► 33 modules      supervisor::run ─► 22 modules (boucles de fond)  │
 │  engine (impl Daemon) ─► 19 modules    runner ─► engine.run_turn, hooks.telegram        │
 │                                                                                         │
 │  executor ─► 18 modules   workflow ─► 11   dream ─► 17   scheduler ─► 6   doctor ─► 13  │
 │      │ scheduler::{create,listing,retarget}   │ dream::{digest_text,system_crons}       │
 │      │ workflow::{kv_*,step_done_key}         └── scheduler::{label,due_today} (cycle)  │
 │                                                                                         │
 │  runtime::{Services (25 champs), Daemon, Hooks, DaemonHandle}  ◄── 54 modules           │
 │  bus ◄── 20   conversation::vault_dir ◄── 19   vault_ops ◄── 14   agent (traits) ◄── 11 │
 │                                                                                         │
 │  feuilles sans dépendance interne : elicitation, selfdocs, skill_deps                   │
 └─────────────────────────────────────────────────────────────────────────────────────────┘
                                          ▼
   kernel  store  platform  observe  llm  context  memory  mcp  skills  tools  hitl  telegram  workflow
   (13 crates, toutes déclarées dans crates/penelope-daemon/Cargo.toml:21-33)
```

Cycles internes (au niveau module, tolérés aujourd'hui parce que tout est dans une crate ;
chacun devient une erreur de compilation dès qu'une frontière de crate passe entre les
deux) :

| Cycle | Preuve | Cassure |
|---|---|---|
| dream ⇄ scheduler | `dream.rs:3022` (`scheduler::label`), `:3049` (`due_today`) ; `scheduler.rs` cite `dream::digest_text`, `dream::system_crons` | passer au digest ses entrées (lignes de planification) en données |
| dream → telegram | `dream.rs:3061, 3084, 3129` (`telegram::deep_link`) | `deep_link` lit une clé kv (`telegram.rs:236-246`) : devient un helper sur `Services` |
| dream → compaction | `dream.rs:3036` (`struggling_sessions(s)`) | fonction `Services`-only sur le LCM : descend dans `penelope-context` ou devient une entrée du digest |
| vault_ops → ingest | `vault_ops.rs:195` (`ingest::index_source`) | `index_source` indexe une page source : rejoint `concepts`/vault |
| vault_git → dream | `vault_git.rs:132` (`dream::vault_sync`) | `vault_sync` (`dream.rs:3368-3395`) rejoint `vault_git` |
| compaction → engine | `compaction.rs:1239` (`engine::last_model_key`) | clé kv : helper partagé |
| selfknow → rpc | `selfknow.rs:342-352` (`rpc::round_usd`) | `round_usd` (`rpc.rs:1208`) descend |
| selfknow → upgrade | `selfknow.rs:141-145` (`running_binary`, `is_source_build`) | fonctions pures sur le binaire : `penelope-platform` |
| doctor → telegram | `doctor.rs` (`telegram::shown`, `telegram::seen_chats`) | lecteurs kv (`telegram.rs:37`, `:7923-7947`) : helpers `Services` |
| scheduler, conversation, session_project → telegram | `telegram::topic_name_key`, `chat_title_key` (`telegram.rs:7889-7897`) | constructeurs de clés kv : helpers `Services` |
| executor ⇄ agent | `executor.rs` cite `agent::without_intention` ×3, `agent::ToolExecutor` ; `agent.rs` cite `executor::wants_network` ×3 | les deux vont dans le module des ports ; `wants_network` (5 lignes sur un nom d'outil) dans `penelope-tools` |
| executor → scheduler / workflow | `executor.rs:830-890` (outils `schedule_*` → `scheduler::{create,listing,retarget}`, toutes `&Services`, `scheduler.rs:491, 977, 1012`) ; `:1552` `step_done` → `workflow::step_done_key` | étendre le port `Orchestrator` ; clé dans le module des ports |
| ingest → agent | `ingest.rs:1050, 1178` (`agent::decide_approval`) | `decide_approval` (`agent.rs:2274`) est une décision HITL, pas la boucle : rejoint les ports/app |

### 1.3 Couplage de canal hors de la passerelle

Mesure demandée par le lead, sur le modèle de `scripts/check-channel-agnostic-boundaries.mts`
d'OpenClaw (voir R8, section 7) : motifs `telegram` (insensible à la casse), `tg_`,
`chat_id`, `topic_id`, `callback_data`, hors blocs `#[cfg(test)]`, hors `telegram.rs` et
`telegram/screens.rs`.

```bash
# par fichier, après coupe au premier #[cfg(test)] en colonne 0
grep -oi 'telegram' | wc -l ; grep -o '\btg_[a-z_]*' ; grep -o '\bchat_id\b' ; grep -o '\btopic_id\b' ; grep -o 'callback_data'
# par nature, sur la concaténation des modules du daemon hors telegram (scratchpad/daemon_nontg.rs)
grep -c 'Origin::Telegram' ; grep -c 'penelope_telegram' ; grep -c '\.telegram\.' ; grep -c 'tg_outbox\|tg_updates' ; …
```

**Daemon, hors passerelle : 303 occurrences dans 31 fichiers, dont 24 dans les deux suites
e2e** (`ticket_to_deploy_e2e.rs` 23, `wiki_e2e.rs` 1), soit **279 occurrences dans 29
modules de code**.

| Fichier | Total | `telegram` | `tg_` | `chat_id` | `topic_id` |
|---|---|---|---|---|---|
| doctor.rs | 47 | 44 | 3 | 0 | 0 |
| scheduler.rs | 42 | 22 | 0 | 15 | 5 |
| supervisor.rs | 25 | 15 | 5 | 3 | 2 |
| rpc.rs | 24 | 8 | 4 | 6 | 6 |
| purge.rs | 23 | 3 | 15 | 5 | 0 |
| executor.rs | 14 | 6 | 0 | 4 | 4 |
| runtime.rs | 13 | 12 | 1 | 0 | 0 |
| selfknow.rs | 12 | 12 | 0 | 0 | 0 |
| bus.rs | 12 | 6 | 0 | 3 | 3 |
| runner.rs | 11 | 11 | 0 | 0 | 0 |
| elicitation.rs | 11 | 8 | 0 | 2 | 1 |
| engine.rs | 9 | 3 | 0 | 3 | 3 |
| conversation.rs | 6 | 6 | 0 | 0 | 0 |
| session_project.rs | 5 | 3 | 2 | 0 | 0 |
| dream.rs | 4 | 4 | 0 | 0 | 0 |
| codex_scope.rs | 3 | 3 | 0 | 0 | 0 |
| 13 autres (upgrade, media, mcp_auth, hermes, agent : 2 ; voice, session_ops, mem_audit, mcp, lib, episodes, compaction, backup : 1) | 18 | 18 | 0 | 0 | 0 |

Par famille (une ligne peut compter dans plusieurs) : commentaires 39 ; chaînes montrées au
propriétaire qui nomment Telegram 24 (`conversation.rs:222` « canal Telegram
indisponible », `runner.rs:97`, `scheduler.rs:396, 947, 990`, `executor.rs:881`) ;
`cfg.telegram.*` 25 ; `bind_telegram` / `find_by_topic` / `telegram_user_id` 19 ;
`tg_outbox` / `tg_updates` en SQL 18 (`runtime.rs:586`, `purge.rs`, `supervisor.rs`,
`rpc.rs`) ; `Origin::Telegram` 17 (`bus.rs:21-54`, `engine.rs:246`, `conversation.rs:204`,
`executor.rs:1407`, `scheduler.rs:679, 934, 943, 998, 1077`, `runner.rs:137, 282`,
`codex_scope.rs:83`) ; `crate::telegram::` 10 ; `hooks.telegram` 6 ; `penelope_telegram::`
6 (`elicitation.rs:488` convertit un schéma d'élicitation en formulaire Telegram,
`runtime.rs:21, 243`, `selfknow.rs:120, 380` listent les commandes) ; `chat_id: i64` /
`topic_id: Option<i64>` en paramètre 6 ; `callback_data` 1.

Ce qui est de la **logique** de canal, pas seulement un nom :

- `Services` porte deux magasins Telegram, `templates: Arc<TemplateRegistry>` et
  `actions: ActionStore` (`runtime.rs:48-49`, construits `runtime.rs:110-112`), lus hors
  passerelle par `scheduler.rs:391` et `workflow.rs:1951` (gabarits de cartes),
  `supervisor.rs:484` (`actions.purge_expired`), `runtime.rs:243` (catalogue des gabarits
  pour valider les workflows) ; 39 usages dans `telegram.rs` et `screens.rs`. Tant qu'ils
  sont là, `penelope-app` dépend de `penelope-telegram` ;
- la fusion des rafales, `conversation.rs:204-222` et `runner.rs:136-148`
  (`Origin::Telegram` et `cfg.telegram.burst_messages / burst_chars`) ;
- la cible des planifications, `executor.rs:386-393, 877-892` (`private` / `here`) et
  `scheduler.rs:905-1000` (`own_origin`, `target_origin`, `place_name`, `retarget` : noms
  de chats lus en kv) ;
- `chat_session_for` (`engine.rs:243-258`) qui lie une session à un chat via
  `SessionStore::find_by_topic` / `bind_telegram` ;
- `effect_kind` (`agent.rs:2531`) qui classe `send_message` / `send_file` en
  `EffectKind::Telegram`.

**Autres crates, hors `penelope-telegram` et hors daemon : 162 occurrences dans 27
fichiers.** `penelope-kernel` 94 : `session.rs` 39 (colonnes `tg_chat_id` / `tg_topic_id`
`session.rs:71-72`, `bind_telegram` 330, `unbind_telegram` 364, `find_by_topic` 378-395),
`config.rs` 32 (section `[telegram]` 144-239, `owner.telegram_user_id` 125,
`TelegramHome` 202), `turn.rs` 8 dont 4 de code (`turn.rs:103-106` normalise
`origin.channel == "telegram"`), `effects.rs` 5 (`EffectKind::Telegram` 64, 76, 469),
`api.rs` 5 dont 1 de code (`StatusReport.telegram` 419), 4 commentaires (`ids.rs:138`,
`risk.rs:41`, `schema.rs:4`, `coherence.rs:19`). `penelope-store/src/migrations.rs` 31
(tables `tg_updates` 729, `tg_outbox` 736, colonnes 186-187 et 765-777, rétention
1217-1218). `penelope-evals/src/suites.rs` 9 (noms de suites). `penelope-observe/src/redact.rs`
5 dont 2 de code (motifs de jeton 51, 53). `penelope-tools/src/spec.rs` 4 (descriptions
d'outils citant Telegram, 718, 919, 969, 972). 14 commentaires dans 12 autres fichiers.
`penelope-workflow` déclare `penelope-telegram` (`Cargo.toml:27`) sans aucun
`penelope_telegram` dans ses sources.

Taille réelle du couplage à casser pour une frontière canal / cœur : 279 (daemon) + 94
(kernel) + 6 (code de `observe` et `tools`) ≈ 380 occurrences, dont une soixantaine de
logique (rafales, cibles, `place_name`, `chat_session_for`, liaison de session, effets,
normalisation d'origine). Le reste sont des noms (configuration, SQL, chaînes,
commentaires), traités par renommage ou par liste blanche.

## 2. Ports

### 2.1 Traits frontière existants

| Trait | Défini | Implémenté par | Consommé par (`dyn`) | Rôle |
|---|---|---|---|---|
| `TurnSink` | `agent.rs:92` | `NullSink` (`agent.rs:99`), `RecordingSink` (`:118`), `BusSink` (`engine.rs:134`) | agent, engine | fragments du tour |
| `Conversation` | `agent.rs:129` | `SessionConversation` (`conversation.rs:170`), `MemoryConversation` (`agent.rs:241`) | agent | messages du modèle |
| `Compactor` | `agent.rs:157` | `OverflowCompactor` (`compaction.rs:1334`) | agent | compaction sur débordement |
| `ToolExecutor` | `agent.rs:279` | `NativeToolExecutor` (`executor.rs:2109`), `CountingExecutor`, `TimedExecutor` (tests `agent.rs:2548, 2583`) | agent, workflow, dream (test), telegram | exécution d'un appel d'outil |
| `Messenger` | `executor.rs:18` | `TelegramGateway` (`telegram.rs:7153`) + 7 `Recorder` de test | executor, telegram, scheduler, budget_alert, et via `hooks.messenger()` 16 modules | envoyer texte, fichier, carte |
| `McpGateway` | `executor.rs:113` | `McpSupervisor` (`mcp.rs:1728`), `RecordingGateway` (test) | executor | appeler un outil MCP |
| `Orchestrator` | `executor.rs:137` | `WorkflowOrchestrator` (`workflow.rs:2791`) | executor | démarrer un workflow, sous-agent, image |
| `ChannelDelivery` | `bus.rs:65` | `TelegramGateway` (`telegram.rs:6800`), `DeliveryChannel`, `BurstChannel` (`runner.rs:303, 316`) | runner, engine (`engine.rs:583`), scheduler (`:562`), conversation | livraison durable de l'issue d'un tour |
| `OwnerChannel` | `elicitation.rs:133` | `TelegramGateway` (`telegram.rs:7048`), `Recorder` (test) | elicitation (`Broker`) | montrer une élicitation MCP |
| `Connector` | `mcp.rs:44` | `ProcessConnector` (`mcp.rs:161`), `FakeConnector` (test) | mcp | démarrer un serveur MCP |
| `Admin` | `selfknow.rs:12` | `Daemon` (`engine.rs:83`), `FakeAdmin`, `TestConfigAdmin` | selfknow, executor, workflow | `self_status`, `config_set` |
| `SwitchHost` | `upgrade.rs:807` | `SystemHost` (`upgrade.rs:826`), `FakeHost` | upgrade | bascule du binaire |
| `Provider` | `penelope-llm/src/provider.rs:20` | `OpenRouterProvider`, `OpenAiCompatProvider`, `CodexProvider`, `MockProvider` | llm, agent, compaction | appel modèle |
| `TokenSource`, `QuotaSink` | `penelope-llm/src/codex.rs:45, 188` | `DaemonTokens` (`codex_auth.rs:625`), `QuotaWriter` (`codex_quota.rs:33`) | llm | jetons et quota Codex |
| `Transport` | `penelope-mcp/src/transport.rs:15` | `StdioTransport`, `HttpTransport`, `LoopbackTransport` | mcp client, daemon mcp | transport MCP |
| `Directories`, `ProcessHost`, `SecretStore`, `Sandbox`, `ServiceManager`, `PowerManager` | `penelope-platform/src/{dirs.rs:33, process.rs:471, secrets.rs:102, sandbox.rs:155, service.rs:21, power.rs:16}` | backends macOS / stub / fichier chiffré ; `doctor.rs:1983, 2003` implémente `SecretStore` pour ses tests | platform, doctor | plateforme |
| `BotTransport` | `penelope-telegram/src/api.rs:38` | `HttpTransport`, `MockTransport` | telegram.rs (`with_transport`, `telegram.rs:541`) | API Bot |
| `Clock`, `UsageWatcher` | `penelope-kernel/src/clock.rs:8`, `budget.rs:124` | `SystemClock`, `TestClock` ; `AlertWatcher` (`budget_alert.rs:28`) | kernel | horloge, budget |
| `AddressGuard`, `EventAccumulator`, `GuardInner` | tools, llm, platform | internes | | |

L'architecture hexagonale est donc déjà là dans les faits : la boucle (`AgentLoop`) ne
connaît que quatre traits, le tour est livré par `ChannelDelivery`, les serveurs MCP par
`McpGateway`, les workflows par `Orchestrator`, Telegram par `Messenger` et
`OwnerChannel`. Ce qui manque n'est pas côté « sortie » mais côté **ce que les modules
prennent sur `Daemon` sans trait**.

### 2.2 Ports manquants

| Port à créer | Ce qu'il remplace | Preuve | Consommateurs (hors tests) |
|---|---|---|---|
| `Services::kv_get / kv_set / kv_delete` (pas un trait : des méthodes sur `Services`) | `Daemon::kv_*` (`engine.rs:1130-1179`) et `workflow::kv_get / kv_set` (`workflow.rs:108-137`), deux familles pour la même table | `grep '\.kv_'` : telegram 43, scheduler 13, dream 13, engine 9, ingest 8, episodes 8, supervisor 7, compaction 6, onboarding 4, mcp_auth 4, backup 4, purge 3, screens 4 (25 modules) ; `workflow::kv_*` : codex_auth 5, runner 2, machine 2, ingest 2, executor 2, episodes 2, engine 2, codex_quota 2, selfknow, scheduler, conversation (12 modules) | tous les modules `D` de la table |
| `ProviderSource` (`provider_for`, `provider_override_active`) | `Daemon::provider_for` (`runtime.rs:397-444`), qui construit `ProviderSet` avec les jetons Codex | `grep provider_for` : engine 3, workflow 2, compaction 2, voice, vision, titles, supervisor, review, mem_split, ingest, images, episodes, embeddings, dream (15 modules) | vault, agent, dream, orchestrator, ops |
| `Supervision` (contexte de `spawn_supervised`) | `spawn_supervised(Arc<Daemon>, …)` (`tasks.rs:151-188`) qui n'utilise que `d.tasks`, `d.handle`, `d.services.clock`, `d.services.events` (`tasks.rs:134-149, 165-170, 190-199`) | telegram 3 (`spawn_supervised`), supervisor, mcp maintenance (`supervisor.rs:225`) | gateway, mcp-host, daemon |
| `Handle` (signal d'arrêt) | `DaemonHandle` (`runtime.rs:272-302`), déjà `Clone` et sans dépendance | `handle.is_shutting_down` : supervisor 4, telegram 2, scheduler 2, workflow 1, runner 1, tasks 2 | orchestrator, gateway, mcp-host |
| `McpAdmin` | `Hooks.mcp_supervisor: Option<Arc<McpSupervisor>>` (type concret, `runtime.rs:333`) | telegram 2, rpc 2, supervisor 2, workflow 1, engine 1, hermes 1, mcp_auth 1, mem_audit 1, `runtime.rs:504` (status), `doctor::mcp_checks(s, &sup)` (`rpc.rs:64-66`) | ops, gateway, daemon |
| `Messenger` explicite | `d.hooks.messenger()` (`Option`) lu dans le corps des fonctions | backup, budget_alert, codex_auth, codex_quota, compaction, dream 2, engine, hermes, ingest, mcp_auth, scheduler 2, supervisor 3, telegram 4, upgrade, workflow 5 | chaque crate extraite reçoit `Option<Arc<dyn Messenger>>` dans son contexte |
| `Orchestrator` étendu (`schedule_create / list / move / delete`) | appels directs `scheduler::{create, listing, retarget}` depuis les outils natifs (`executor.rs:830-890`) | executor | executor |
| `DigestInputs` (données, pas un trait) | `scheduler::label`, `due_today`, `compaction::struggling_sessions`, `telegram::deep_link` appelés par le digest (`dream.rs:3022-3129`) | dream | scheduler, rpc, telegram fournissent les entrées |
| `Gateway` (démarrage d'un canal) | `TelegramGateway::from_config(self.clone())` puis `gw.start()` dans `supervisor.rs:211-235` : le daemon connaît la passerelle par son type | supervisor | composition dans `penelope-cli` |
| `Cards` (rendu d'un gabarit) et `ChannelDelivery` étendu (`burst_limits`, `describe_origin`) | `Services.templates` / `Services.actions` (`runtime.rs:48-49`), la fusion des rafales (`conversation.rs:204-222`, `runner.rs:136-148`) et les noms de chats (`scheduler.rs:905-1000`) : logique de canal dans le cœur, section 1.3 | `scheduler.rs:391`, `workflow.rs:1951`, `supervisor.rs:484`, `runtime.rs:243` | agent, orchestrator, daemon |
| `TurnIntake` (`enqueue_message`, `enqueue_message_with_images`, `enqueue_retry`, `enqueue_resume`, `chat_session_for`) et `SessionModels` (`pinned_model`, `pin_model`, `session_model_view`, `select_model`), `Transcriber` (`transcribe`, `describe_images`) | méthodes de `Daemon` (`engine.rs:147-276, 696-852, 852-1054`) | telegram (chat_session_for 18, enqueue 4, transcribe 1, session_model_view 1), rpc, scheduler, supervisor, onboarding, compaction | **différé** en V1 (voir 3.3) |

Ce que `telegram.rs` appelle directement sur `Daemon` sans trait, en une ligne : `services`
(27), `kv_*` (43), `chat_session_for` (18), `bus.{cancel_session, is_active, ingests_of,
cancel_ingests, start_ingest, end_ingest, session_for_draft}` (13), `hooks.messenger` (4),
`hooks.mcp_supervisor` (2), `hooks.telegram` (1), `handle.is_shutting_down` (2),
`enqueue_message / enqueue_retry / enqueue_resume` (4), `transcribe` (1),
`session_model_view` (1), `tasks::spawn_supervised(daemon)` (3), et `Rpc::new(daemon)` (7
sites : `telegram.rs:1108, 1457, 1745, 2857`, `screens.rs:422, 1915, 2285`). S'y ajoutent
environ 80 symboles libres dans 25 modules (section 1.2, ligne telegram).

## 3. Cible V1

### 3.1 Principe

Chaque frontière ci-dessous est posée là où la mesure montre un col étroit : une poignée de
fonctions `&Services` plus un ou deux ports, jamais un module `D` à 20 membres. Les
modules qui ont besoin de `Daemon` pour autre chose que `kv`, `provider_for`, `messenger`,
`handle` restent dans le daemon (composition). Les trois `State` portés par `Daemon`
partent avec leur module et le daemon les tient en `Arc<…>`.

Deux décisions à défendre :

1. **Telegram au-dessus du daemon, pas en dessous.** Une passerelle est un adaptateur
   pilotant : elle appelle l'application. La faire passer par un port unique reviendrait à
   créer un trait de 90 méthodes (12 membres de `Daemon` + 80 fonctions libres), une
   fausse frontière. Les vraies frontières dans l'autre sens existent déjà
   (`ChannelDelivery`, `Messenger`, `OwnerChannel`). Donc `penelope-gateway-telegram`
   dépend de `penelope-daemon`, le daemon ne la connaît que par ces trois traits et par
   un port `Gateway`, et **`penelope-cli` compose** (il construit déjà le daemon,
   `commands.rs:1891-1905`). Conséquence mesurable : la passerelle peut sortir tôt, avant
   les crates métier, et le daemon perd 16 151 lignes d'un coup.
2. **Une crate `penelope-app` porte `Services` et les ports.** Toutes les fonctions
   extractibles prennent `&Services` (`sigs.pl` : 29 fn dans doctor, 27 dans dream, 14
   dans codex_auth, 10 dans conversation et session_notes…). Sans `Services` en bas, rien
   ne sort. `Services::bootstrap` (`runtime.rs:47-172`) assemble les 13 crates métier :
   `penelope-app` dépend donc des 13, comme le daemon aujourd'hui.

### 3.2 Crates proposées

| Crate | Contenu (modules actuels) | Lignes prévues | Dépendances internes autorisées |
|---|---|---|---|
| **penelope-app** (nouvelle) | `Services` + `bootstrap` + `for_tests` + `workflow_known*` + `reload_skills` + `SUBSYSTEMS` + `BUNDLED_SKILLS` (et le dossier `skills/wiki-markdown`, `runtime.rs:600`), `Handle` (ex `DaemonHandle`), `bus.rs` (Bus, Origin, ChannelDelivery), `tasks.rs` (Tasks, `spawn_supervised(&Supervision)`), `elicitation.rs` (Broker, OwnerChannel ; `Services.elicitations`, `runtime.rs:54`), module `ports` (TurnOutcome, TurnEvent, TurnSink, Conversation, Compactor, CallInfo, CallFailure, ToolExecutor, ToolEnv, Messenger, McpGateway, Orchestrator, Admin, ProviderSource, McpAdmin, Supervision, Gateway), `kv`, helpers (`vault_dir`, `local_now`, `owner_origin_of`, `default_workspaces`, `denied_reads`, clés kv Telegram, `deep_link`, `round_usd`, `set_config_path`), `decide_approval` et compagnie (`agent.rs:1929-2073, 2207-2386`), `codex_scope`, `cache_audit`, `prompt_snapshot`, `audit`, `approval_mode`, `media`, `machine` | ≈ 5 300 | les 13 crates métier (kernel, store, platform, observe, llm, context, memory, mcp, skills, tools, hitl, telegram, workflow) |
| **penelope-vault** (nouvelle) | `vault_ops`, `concepts` (+ `ingest::index_source`), `vault_inventory`, `vault_git` (+ `dream::vault_sync`), `session_notes`, `secret_shelf`, `embeddings` (+ son `State`), `usage_feedback`, `session_project`, `mem_split`, `mem_audit`, `review`, `episodes`, `dream::core_overflow`, et les fonctions de tiers et d'instantané de `conversation.rs:347-373, 559-664` | ≈ 6 100 | app + métier |
| **penelope-agent** (nouvelle) | `agent.rs` sans les ports (boucle, appel modèle, replis, effets, limites), `conversation.rs` (SessionConversation, `build_turn_prompt`), `compaction.rs` (+ son `State`), `titles`, `budget_alert` | ≈ 7 600 | app, vault + métier |
| **penelope-executor** (nouvelle) | `executor.rs` sans les ports (NativeToolExecutor, outils natifs par famille), `selfknow` (sans `Admin`), `selfdocs` (+ `build.rs`), `vision`, `images`, `voice`, `tools_on_demand` | ≈ 6 100 | app, vault + métier ; **pas** agent, **pas** orchestrator |
| **penelope-dream** (nouvelle) | `dream.rs` (sans `vault_sync`, `core_overflow`), `ingest.rs`, `onboarding.rs` | ≈ 7 800 | app, vault + métier |
| **penelope-mcp-host** (nouvelle) | `mcp.rs` (ProcessConnector, McpSupervisor), `mcp_auth.rs` | ≈ 4 500 | app + métier (mcp, platform, llm, kernel) |
| **penelope-orchestrator** (nouvelle) | `workflow.rs` (+ son `State`, WorkflowOrchestrator), `scheduler.rs` | ≈ 5 900 | app, vault, agent, executor, dream + métier |
| **penelope-ops** (nouvelle) | `doctor`, `upgrade`, `backup`, `hermes`, `codex_auth`, `codex_quota`, `skill_install`, `skill_deps`, `purge`, `session_ops` | ≈ 10 300 | app, vault (+ `McpAdmin` par port, pas de dépendance sur mcp-host) + métier |
| **penelope-daemon** (conservée) | `runtime.rs` (Daemon, Hooks, `recover`, `status`, `rss_mb`), `engine/` (impl Daemon : intake, tour, modèles, médias, Admin), `runner`, `supervisor`, `rpc/`, `runtime_events`, `ticket_to_deploy_e2e`, `wiki_e2e`, `tests/`, `examples/` | ≈ 9 200 | toutes les crates ci-dessus |
| **penelope-gateway-telegram** (nouvelle) | `telegram/` (16 modules) + `telegram/screens/` (5) + tests | ≈ 16 200 | daemon et tout ce qui est en dessous |
| **penelope-cli** | + composition de la passerelle dans la commande `daemon` | 3 227 + ≈ 40 | daemon, gateway-telegram, evals, … |
| **penelope-evals** | imports `penelope_dream::run`, `penelope_vault::…` au lieu de `penelope_daemon::…` | inchangé | daemon, dream, vault, … |

Somme ≈ 79 000 lignes (78 532 aujourd'hui, plus les traits et contextes). Le daemon passe
de 78 532 à ≈ 9 200 lignes.

Justification des frontières par le couplage mesuré :

- **app / vault** : les 13 modules de vault sont `S` ou `D` seulement par `kv`,
  `provider_for` (embeddings, review, mem_split, episodes, titles) et `hooks.messenger`
  (aucun). Ils dépendent entre eux (`vault_ops` ← 14, `concepts` ← 7) et de
  `conversation::vault_dir` (19 modules, trois lignes). Rien en dessous ne les cite hors
  `Services`.
- **agent / executor séparés** : c'est la frontière `ToolExecutor` (`agent.rs:279`), déjà
  exploitée par 38 tests de la boucle avec des exécuteurs factices (`agent.rs:2548-2600`).
  Après passage de `without_intention`, `ToolExecutor`, `CallInfo` dans `app` et de
  `wants_network` dans `penelope-tools`, les deux crates ne se citent plus.
- **dream au-dessus de vault, en dessous de orchestrator** : `dream` ← scheduler (2
  symboles), rpc, telegram, doctor, hermes, supervisor, vault_git ; `dream` → vault_ops 13,
  conversation 11, vault_git 4 ; ses seuls appels vers le haut (`scheduler::label`,
  `due_today`, `telegram::deep_link`, `compaction::struggling_sessions`) sont les entrées
  du digest (`dream.rs:3022-3129`), transformées en données.
- **orchestrator en haut de la pile métier** : `workflow.rs` construit un
  `NativeToolExecutor` (`workflow.rs:1142`) et une `AgentLoop` (`agent` 4), donc au-dessus
  d'agent et d'executor ; `scheduler.rs` déclenche le digest et les crons de `dream`, donc
  au-dessus de dream.
- **mcp-host isolé** : `mcp.rs` ne cite `executor` que pour `denied_reads` (`mcp.rs:101`,
  helper `Services`) et pour le trait `McpGateway` (`mcp.rs:1728`) ; `NativeToolExecutor::new`
  n'apparaît qu'en test (`mcp.rs:2889`). `mcp_auth` a besoin de `Messenger`, `McpAdmin`,
  `kv`.
- **ops** : `doctor` est `S` à 29 fonctions sur 30 ; ses appels vers `telegram` (2), `mcp`
  (2), `dream` (1) se réduisent à trois helpers et un port. `hermes`, `upgrade`, `backup`,
  `purge`, `session_ops`, `codex_*` sont `D` seulement par `kv` et `messenger`.
- **ce que le daemon garde** : les trois `impl Daemon` (`engine.rs:145`, `supervisor.rs:90`,
  `runner.rs:279`), `Hooks` (`runtime.rs:328`), `recover` et `status`
  (`runtime.rs:458-600`), le pool de runners, les boucles de fond
  (`supervisor.rs:172-236`), la façade RPC (102 méthodes, `rpc.rs:50`), les deux suites
  e2e qui câblent tout (`ticket_to_deploy_e2e.rs`, `wiki_e2e.rs`).

### 3.3 Ce qui est différé après V1

- Les ports `TurnIntake`, `SessionModels`, `Transcriber` : inutiles tant que la passerelle
  dépend du daemon. Ils deviennent nécessaires le jour où l'on veut tester la passerelle
  sans `Daemon` ; aujourd'hui ses 88 tests construisent un daemon complet
  (`telegram.rs:7960-7988`).
- Le registre d'outils natifs enfichable (`trait NativeTool`), qui permettrait à `vault`,
  `orchestrator`, `ops` de déclarer leurs propres outils au lieu du `match` de
  `executor.rs:480-1600`.
- `engine` en `TurnEngine` indépendant de `Daemon` (il resterait à passer `Bus` et `Hooks`
  en ports).

## 4. Ordre d'extraction

Des feuilles vers le centre, chaque étape livrable seule (un lot, une version, cf.
`CLAUDE.md`) :

```
 Phase 0  règles et outillage      T01 T02 T03 T04 T35 T38         (parallèles, sans risque)
 Phase 1  décrocher Daemon         T05 kv ─► T06 helpers ─► {T07 ProviderSource+Handle+Supervision,
                                   T08 Messenger explicite, T09 McpAdmin} ─► T10 doubles de test
                                   ─► T36 le cœur ne nomme plus le canal
 Phase 2  fichiers géants          T11 ─► T12 (telegram) ; T13 dream ; T14 agent ; T15 workflow ;
                                   T16 executor ; T17 engine ; T18 rpc ; T19 mcp ; T20 le reste
                                   (parallèles entre eux, après T05/T06 de préférence)
 Phase 3  crates                   T21 app ─► T22 vault ─► {T23 agent, T24 executor, T25 mcp-host}
                                   ─► T26 dream ─► T27 orchestrator ─► T28 ops
          passerelle               T29 gateway-telegram : possible dès T06 + T12 (elle dépend
                                   du daemon), recommandé après T21 pour citer `penelope_app`
 Phase 4  clôture                  T30 consommateurs et matrice CA ; T31 architecture.md et
                                   décision ; T32 resserrer les baselines
```

Ce qui doit exister avant chaque extraction :

| Étape | Préalables (port à créer, dépendance à casser) |
|---|---|
| T21 app | T05 (kv sur `Services`), T06 (helpers), T07, T08, T09 ; `Origin`, `TurnOutcome`, `TurnEvent` déplacés avec le bus ; `decide_approval` sorti de `agent.rs` ; `Services.elicitations` reste (`elicitation` part avec app) |
| T22 vault | T21 ; cycles vault_ops→ingest (`vault_ops.rs:195`), vault_git→dream (`vault_git.rs:132`), dream→vault (`core_overflow` `dream.rs:606`) résolus ; `embeddings::State` sorti de `Daemon` (`runtime.rs:315`) |
| T23 agent | T22 ; `compaction::State` sorti de `Daemon` (`runtime.rs:311`) ; `compaction→engine::last_model_key` (`compaction.rs:1239`) ; `wants_network` dans `penelope-tools` |
| T24 executor | T22 ; `Orchestrator` étendu (schedule_*) ; `runtime_events::bounded_redacted` (cité 2× par executor) descendu dans app ; `selfknow::Admin` dans app ; `build.rs` déplacé avec `selfdocs` |
| T25 mcp-host | T21 ; T09 ; `mcp_auth` prend `Messenger` + `McpAdmin` + kv par contexte |
| T26 dream | T22 ; `DigestInputs` ; `struggling_sessions` descendu ; `ingest→decide_approval` via app ; `ingest.rs:861` `Messenger` par contexte |
| T27 orchestrator | T23, T24, T26 ; `workflow::State` sorti de `Daemon` (`runtime.rs:313`) ; `scheduler→telegram` clés (T06) ; `hooks.telegram` (`scheduler.rs:562`) reçu en `Option<Arc<dyn ChannelDelivery>>` |
| T28 ops | T22, T25 (port `McpAdmin` seulement) ; `doctor→telegram` (T06) ; `embedding_check(d)` via `ProviderSource` |
| T29 gateway | T06 (plus aucun `crate::telegram::` ailleurs dans le daemon), T12 (fichiers < 800), port `Gateway`, composition dans `penelope-cli` (`commands.rs:1891-1905`), `parse_params` (`commands.rs:1054`) importé de la nouvelle crate |

## 5. Découpage interne des fichiers géants

Plafond visé : 800 lignes par fichier, tests compris. Première mesure, toujours payante :
sortir le `mod tests` inline dans un fichier frère (`#[cfg(test)] mod tests;` dans
`x/mod.rs`, corps dans `x/tests.rs` ou `x/tests/*.rs`). Cela suffit pour `ingest` (855
lignes de code), `conversation` (687), `mcp_auth` (690), `purge` (533), `codex_auth`
(695), `supervisor` (629), `concepts` (750), `vault_ops` (570), `runtime` (677).

### 5.1 `telegram.rs` (7 948 lignes de code, 5 768 de tests, 88 tests) et `telegram/screens.rs` (2 435)

| Module cible | Responsabilité | Lignes actuelles déplacées | Taille |
|---|---|---|---|
| `telegram/mod.rs` | struct `TelegramGateway` (277-297), `from_config` / `with_transport` (523-565), `register` / `start` / `shutting_down` (648-702), `poll_loop` (726-762), `process_update` / `handle` (784-1093), `announce_uncertain_effects` (702-726) | 277-297, 520-565, 648-1093 | ≈ 700 |
| `telegram/cards.rs` | rendu texte des cartes et listes : `shown`, `approval_card` (37-190), `codex_status_text`, notes (190-236), `context_line` (7400), `schedules_text`, `mcp_*_text`, `routing_text`, `render_value` (7563-7889) | 37-236, 7400-7427, 7563-7889 | ≈ 750 |
| `telegram/commands/session.rs` | bras `start/help`, `new`, `title`, `sessions`, `compact`, `fork`, `rewind`, `export`, `switch`, `close`, `purge`, `home`, `projet`, `mode`, `quiet`, `note`, `p` | 1113-1412, 1592-1683, 1803-1829, 1843-1977, 2503-2666 | ≈ 700 |
| `telegram/commands/models.rs` | `model`, `models`, `budget`, `usage`, `audit`, menus de modèle (`send_choices`, `send_model_menu`, `model_menu`, `workflow_choice_clicked`, `model_pin_clicked`) | 1683-1803, 1829-1843, 2117-2193, 2666-2904 | ≈ 600 |
| `telegram/commands/ops.rs` | `upgrade`, `stop`, `schedules`, `mcp`, `accueil`, `dream`, `appris`, `pratique`, `retiens`, `oublie`, `forget`, `secret`, `approvals`, `recall`, `run`, `resume`, `skill`, `logs`, `restart` | 1412-1592, 1977-2117, 2193-2503 | ≈ 700 |
| `telegram/commands/mod.rs` | l'entrée `command()` réduite à la table de dispatch (1093-1113) et la boucle finale (2572-2666) | | ≈ 150 |
| `telegram/bursts.rs` | `TextBurst` (396-447), `buffer_text` / `flush_burst` / `deliver_burst` / `ask_about_burst` (2904-3098), `Held` (337-376), `hold` / `flush_held` / `out_of_focus` (4959-5158) | | ≈ 600 |
| `telegram/media.rs` | `Album` (376-396), `photo` / `document` / `voice` (3098-3418), `enqueue_photos` / `store_attachment` / `audio_filename` (7443-7563) | | ≈ 550 |
| `telegram/usage.rs` | `usage_text` / `budget_text` (3418-3591), `send_budget_card` / `budget_clicked` (4441-4621) | | ≈ 450 |
| `telegram/callbacks.rs` | `callback` (3591-3931), `finalize_decision`, `recorded_approval_destination`, `approval_destination` (3931-4028), `retry_clicked` (6106-6140) | | ≈ 500 |
| `telegram/approvals.rs` | `send_approval_card` (4028-4238), `send_plain_approval`, `send_effect_card`, `send_launch_card` (4238-4441), `send_oauth_card`, `send_memory_card`, `send_destructive_confirm` (6140-6342) | | ≈ 700 |
| `telegram/sessions_menu.rs` | `send_sessions_menu`, `session_menu_clicked`, `send_session_actions`, `bind_chat` (4621-4959) | | ≈ 350 |
| `telegram/forms.rs` | `form_key` / `form_topic` (255-275), `send_form_step`, `form_pending`, `form_input`, `form_clicked` (5158-5465) | | ≈ 350 |
| `telegram/onboarding.rs` | `propose_onboarding` … `onboarding_clicked` (5465-5689) | | ≈ 230 |
| `telegram/elicitation.rs` | `elicitation_html` … `elicitation_clicked` (5689-6042), `impl OwnerChannel` (7048-7139) | | ≈ 450 |
| `telegram/outbox.rs` | `send_failure` (6042-6106), `reply` / `reply_or_document` / `send_long_as_document` (6342-6438), `outbox_push` / `outbox_loop` / `flush_outbox` / `push_failure_note` / `react` (6438-6653), `shorten_failure` (7139-7153) | | ≈ 550 |
| `telegram/drafts.rs` | `Activity` et `activity_for` / `tool_status` (298-337), `chat_action` / `*_activity` (573-648), `StopReport` (447-520), `draft_loop` / `spawn_draft` (6653-6800) | | ≈ 400 |
| `telegram/delivery.rs` | `impl ChannelDelivery` (6800-7028), `home_chat` (7028-7048), `impl Messenger` (7153-7400) | | ≈ 500 |
| `telegram/keys.rs` | `BOT_USERNAME_KEY` / `deep_link` (236-255), `parse_params` (7427), `normalise_model_id` / `substitute` / `short_model` / `model_pin_notice` (7509-7545), `recent_log_lines` (7629), clés et chats vus (7889-7947) ; **la plupart descendent dans app à T06** | | ≈ 250 |
| `telegram/screens/mod.rs` | `Screen`, `Done`, helpers (29-135), `nav` / `guarded` (138-218) | screens.rs | ≈ 250 |
| `telegram/screens/sessions.rs` | écrans `help`, `confirm`, `status`, `rewind`, `prompts`, `config`, `logs`, `quiet` | 425-489, 1357-1435, 1503-1626, 1790-1906 | ≈ 550 |
| `telegram/screens/workflows.rs` | `wf`, `runs`, `schedules` | 489-788 | ≈ 300 |
| `telegram/screens/memory.rs` | `forget`, `learned`, `entry`, `practices`, `practice`, `intentions`, `policies` | 1022-1357 | ≈ 340 |
| `telegram/screens/ops.rs` | `mcp`, `models`, `skills`, `skill`, `doctor`, `secrets`, `upgrade` | 788-1022, 1435-1503, 1626-1790 | ≈ 550 |
| `telegram/screens/perform.rs` | `perform` (1906-2435) | | ≈ 530 |
| `telegram/tests/*.rs` | 88 tests groupés par thème (formulaires et sujets, rafales, approbations, médias, sessions, ops, élicitation, livraison) | 7950-13716 | 8 fichiers ≈ 720 |

### 5.2 `dream.rs` (3 453 de code, 2 406 de tests, 35 tests)

| Module cible | Responsabilité | Lignes | Taille |
|---|---|---|---|
| `dream/mod.rs` | constantes, `DreamOutcome`, `Trigger`, `run` / `run_as` / `run_locked` (phases Light, REM, Deep : `dream.rs:147, 198, 211`) | 29-593 | ≈ 560 |
| `dream/candidates.rs` | `submission_order`, `contradiction`, `Item` / `Neighbour`, `nearby_batch` / `nearby_with`, `ids_for`, `is_journal`, `sort_and_plan` | 593-936 (sans `core_overflow`, qui part dans vault) | ≈ 340 |
| `dream/clash.rs` | `ask_about_clash`, `file_unanswered_clash`, `expired_journal`, `unused_entries` | 936-1101 | ≈ 170 |
| `dream/snapshot.rs` | `VaultSnapshot`, `markdown_files` | 1101-1222 | ≈ 120 |
| `dream/consolidate.rs` | `CONSOLIDATION_PROMPT`, `usage_note`, `CallOutcome`, `consolidate`, `BatchSizer`, `OutputBudget`, `write_batch`, `reasoning_fallback`, `output_cap`, `batch_event`, `consolidate_retrying`, détection de panne réseau et de délais | 1222-1996 | ≈ 780 (à couper en `consolidate.rs` et `batches.rs` si > 800) |
| `dream/apply.rs` | `target_file`, `level_of`, `today`, `apply`, `add_to_practice`, `title_of`, `mutate` / `mutate_as`, `append_dreams`, `wiki_review` | 1996-2557 | ≈ 560 |
| `dream/runs.rs` | `last_finished_start`, `record_run`, `set_phase`, `close_interrupted`, `save_stats`, `finish_run`, `last_report`, `history`, `restore`, `learned` | 2557-2851 | ≈ 300 |
| `dream/digest.rs` | `rejection_family` / `rejection_families`, `DIGEST_FAMILIES`, `night_summary`, `recent_reports`, `digest_text` (avec `DigestInputs`) | 2851-3154 | ≈ 300 |
| `dream/nightly.rs` | `system_crons`, `nightly`, `night_failed`, `failure_reported`, `last_run`, `last_failure`, `vault_check`, `vault_path` | 3154-3454 (sans `vault_sync` 3368-3395, qui part dans `vault_git`) | ≈ 280 |
| `dream/tests/{candidates,batches,apply,digest}.rs` | 35 tests | 3455-5859 | 4 fichiers ≈ 600 |

### 5.3 `agent.rs` (2 146 de code, 2 093 de tests, 38 tests)

| Module cible | Responsabilité | Lignes | Taille |
|---|---|---|---|
| `app::ports::turn` (part dans app) | `TurnOutcome`, `TurnEvent`, `TurnSink`, `NullSink`, `RecordingSink`, `Conversation`, `Compactor`, `CallFailure`, `CallInfo`, `ToolExecutor`, `TurnSpec`, `TurnRequest` | 28-361 | ≈ 330 |
| `app::approval` (part dans app) | `EFFECT_*`, `decide_uncertain_effect`, `decide_approval` (fonction libre), `pending_calls`, `call_intention`, `turn_goal`, `without_intention`, `line_shape`, `always_creates_no_rule`, `arg_patterns`, `family_of`, `arg_pattern`, `server_of`, `effect_kind` | 1929-2147, 2207-2411, 2515-2535 | ≈ 450 |
| `agent/loop.rs` | `AgentLoop`, `Step` / `Terminal` / `Pending`, `new` / `run` / `run_conversation` (sections 1 à 4, `agent.rs:574, 595, 660, 666, 881`) | 361-535 (sans 438-504), 504-924 | ≈ 600 |
| `agent/model_call.rs` | `call_model`, `retry_backoff_secs`, `with_attempts`, `sleep_unless_cancelled`, `fit_modalities`, `humanise_llm_error` | 924-1192, 2411-2515 | ≈ 380 |
| `agent/answer.rs` | `LOOP_*`, `last_result_of`, `split_choices`, `answer_after_loop`, `MemoryConversation` | 219-267, 438-504, 1192-1267 | ≈ 200 |
| `agent/pending.rs` | `resolve_pending` | 1267-1673 | ≈ 410 |
| `agent/effects.rs` | `run_effect`, `finish_call`, `record_result`, `decide_approval` / `resume_after_approval` (méthodes) | 1673-1777, 1888-1929 | ≈ 200 |
| `agent/limits.rs` | `budget_exceeded_text`, `turn_limits`, `delegation_nudge`, `TURN_CALLS`, `CALLS_EXHAUSTED` | 186-219, 498-504, 1777-1888 | ≈ 160 |
| `agent/tests/{loop,approvals,effects,fallback}.rs` | 38 tests (+ `clone_policy_tests` 2147-2207) | 2147-2207, 2535-4239 | 4 fichiers ≈ 540 |

### 5.4 `workflow.rs` (2 912 de code, 1 132 de tests, 20 tests)

| Module cible | Responsabilité | Lignes | Taille |
|---|---|---|---|
| `orchestrator/workflow/mod.rs` | constantes, `State` (47-93), `StepOutcome`, clés (`step_done_key`, `origin_key`, `brief_key`), `origin_of`, `brief_of`, `with_brief` ; `kv_*` (108-149) partent dans app | 35-220 | ≈ 190 |
| `orchestrator/workflow/start.rs` | `start_run`, `first_verdict`, `start_run_briefed`, `resolve_params` | 220-417 | ≈ 200 |
| `orchestrator/workflow/driver.rs` | `driver_loop`, `drive_all`, `admit_held`, `drive`, `drive_claimed`, `execute_with_retry`, `finish`, `refresh_spent`, budget (`budget_key`, `effective_budget`, `limit_reason`, `raise_budget`), `session_metadata` | 417-850 | ≈ 430 |
| `orchestrator/workflow/control.rs` | `control`, `answer`, `form_of` | 850-1029 | ≈ 180 |
| `orchestrator/workflow/step_ctx.rs` | `StepCtx`, `render*`, `executor`, `execute_step` | 1029-1220 | ≈ 190 |
| `orchestrator/workflow/steps/agent.rs` | `agent_step`, `run_state_line`, `record_user`, `send_approval_once`, `sub_agent_system` / `sub_agent_tools`, `SubAgentTask`, `run_sub_agent`, `extract_json`, `sub_agent_step` | 1220-1618 | ≈ 400 |
| `orchestrator/workflow/steps/shell_tool.rs` | `shell_step`, `tool_step` | 1618-1879 | ≈ 260 |
| `orchestrator/workflow/steps/user.rs` | `user_step`, `question_text` | 1879-1992 | ≈ 110 |
| `orchestrator/workflow/steps/compose.rs` | `run_child`, `parallel_step`, `workflow_step`, `wait_step`, `mcp_task_wait`, `last_event_id` | 1992-2309 | ≈ 320 |
| `orchestrator/workflow/steps/verify.rs` | `project_test_spec` … `verify_step` | 2309-2723 | ≈ 410 |
| `orchestrator/workflow/orchestrator.rs` | `progress`, `WorkflowOrchestrator`, `impl Orchestrator` | 2723-2913 | ≈ 190 |
| `orchestrator/workflow/tests/{steps,runs}.rs` | 20 tests | 2913-4044 | 2 fichiers ≈ 570 |

### 5.5 Les autres fichiers au-dessus du plafond

| Fichier | Code / tests | Découpage |
|---|---|---|
| `executor.rs` 3 712 | 2 477 / 1 235 | `executor/mod.rs` (`ToolEnv`, `NativeToolExecutor`, `new`, helpers 185-480 ; les traits 18-185 partent dans app) ; `executor/tools/{fs_shell (503-711), git_http (664-739), self (739-830), schedules_messaging (830-932), memory (932-1205), skills_workflows (1205-1453), misc (1453-1600)}.rs` ; `executor/precheck.rs` (1852-2109) ; `executor/defs.rs` (`tool_defs`, `chat_tool_defs`, `render_mcp_result`, `native_info`, 2201-2478) ; `executor/tests/*.rs` |
| `engine.rs` 3 173 | 1 218 / 1 955 | `engine/admin.rs` (`impl Admin for Daemon`, `BusSink`, 83-145), `engine/intake.rs` (`enqueue_*`, `chat_session_for`, 147-276), `engine/turn.rs` (`run_turn`, `execute_turn`, 276-650), `engine/prompt.rs` (`intents_block`, `classify`, schémas, 650-696, 1054-1219), `engine/media.rs` (`transcribe`, `photo_message`, `describe_images`, 696-852), `engine/models.rs` (`select_model` … `session_model_view`, 852-1054), `engine/tests/*.rs` |
| `rpc.rs` 2 481 | 1 742 / 739 | `rpc/mod.rs` (`Rpc`, `handle`, `call`, squelette de `dispatch` 15-50), `rpc/methods/{status_config, sessions, memory, mcp, workflows, ops, codex}.rs` (bras de `dispatch` 50-1208 et helpers 1208-1333), `rpc/stream.rs` (1333-1547), `rpc/server.rs` (`serve`, `serve_on`, 1657-1743), `rpc/tests.rs` |
| `mcp.rs` 3 291 | 1 953 / 1 338 | `mcp_host/connector.rs` (44-226), `mcp_host/supervisor/{lifecycle, tools, admin}.rs` (`McpSupervisor` 283-1707, ≈ 1 480 lignes à couper en trois), `mcp_host/gateway.rs` (1728-1804), `mcp_host/render.rs` (1804-1954), `mcp_host/tests/*.rs` (dont les deux tests macOS `mcp.rs:3149, 3257`) |
| `doctor.rs` 2 357 | 1 969 / 388 | `doctor/{mod (run 10-261, render 2001), models (261-500), secrets (557-725, 1205-1325), machine (725-930, 1035-1131), memory (854-901, 1388-1474), coherence (1325-1388, 1474-1890), mcp (1890-2001)}.rs` |
| `compaction.rs` 2 225 | 1 373 / 852 | `compaction/{mod (State, Trigger, Report, hooks de tour 143-533), summarise (537-959), publish (1093-1250), view (`context_view`, `OverflowCompactor`, `report_text` 1250-1374), fidelity (34-143)}.rs` |
| `hermes.rs` 1 971, `upgrade.rs` 1 926, `scheduler.rs` 1 833 | 1 565 / 406 ; 1 192 / 734 ; 1 146 / 687 | tests sortis, puis deux ou trois modules chacun (`scheduler/{loop (42-363), fire (363-491), state (491-1027), origin (1027-1147)}.rs` ; `upgrade/{source, switch (807-1193), boot, rpc}.rs` ; `hermes/{mod, mcp, vault, rpc}.rs`) |

## 6. Tâches fines

Taille : S = quelques heures, diff mécanique ; M = une journée ; L = plusieurs jours ou
plus de 3 000 lignes déplacées. Chaque tâche est un lot (`make bump`), livrable seule,
suite verte à chaque fois. Les déplacements se font par `git mv` et `pub use` de
transition pour que `penelope-cli`, `penelope-evals`, `tests/` et `examples/` continuent
de compiler jusqu'à T30.

| # | Titre | Périmètre | Dépend de | Critère de fin | Taille |
|---|---|---|---|---|---|
| T01 | Plafond de lignes par fichier avec baseline décroissante | `crates/penelope-archtest/src/lib.rs` | aucune | test `no_source_file_grows_past_the_cap_or_its_baseline` vert avec la baseline de la section 7 ; un fichier fictif de 801 lignes hors baseline est détecté (test du détecteur) | S |
| T02 | Règles de dépendance complètes et dépendants du daemon | `penelope-archtest/src/lib.rs` (`dependency_rules`, `dependency_violations`) | aucune | `dependency_rules()` couvre les 17 crates (8 aujourd'hui, `lib.rs:205-245`) ; test `every_crate_has_a_dependency_rule` ; `DAEMON_DEPENDENTS` et `GATEWAY_DEPENDENTS` posés | S |
| T03 | Liste blanche des fichiers qui nomment `Daemon` et des `impl Daemon` | `penelope-archtest/src/lib.rs` | aucune | baseline = 41 fichiers du daemon citant `Daemon` hors tests ; test échoue si un fichier hors liste apparaît ou si un compteur monte | S |
| T04 | Le test `docs` lit toutes les sources récursivement | `crates/penelope-evals/tests/docs.rs:443-450` (`read_dir` non récursif sur `penelope-daemon/src`) | aucune | un événement émis depuis `telegram/x.rs` ou depuis une autre crate est reconnu ; test vert | S |
| T05 | Une seule famille `kv` sur `Services` | `engine.rs:1130-1179`, `workflow.rs:108-149`, 37 modules appelants | aucune | `grep -rn 'workflow::kv_\|d\.kv_\|daemon\.kv_\|self\.daemon\.kv_' crates/penelope-daemon/src` vide hors tests ; suite verte | M |
| T06 | Helpers `Services` partagés | `conversation.rs:664-680` (`vault_dir`, `local_now`), `scheduler.rs:1066-1084` (`owner_origin` supprimé, `owner_origin_of` public), `executor.rs:212, 343`, `telegram.rs:37, 236-255, 7889-7947`, `rpc.rs:1208` (`round_usd`), `rpc::set_config_path`, `upgrade::{running_binary, is_source_build}`, `engine::last_model_key`, `workflow::step_done_key` → module `helpers` (futur app) | T05 | plus aucun `crate::telegram::` hors `telegram.rs` et `supervisor.rs:211` ; plus aucun `crate::rpc::` dans `selfknow.rs`, `engine.rs`, `dream.rs` (hors tests) ; plus de `crate::engine::` dans `compaction.rs` | M |
| T07 | Ports `ProviderSource`, `Handle`, `Supervision` | `runtime.rs:272-302, 397-444`, `tasks.rs:151-188`, modules : voice, vision, titles, review, mem_split, ingest, images, episodes, embeddings, dream, compaction, workflow, supervisor, backup, purge, onboarding, mem_audit, concepts, session_ops | T05 | `sigs.pl` : `fn_Daemon = 0` pour vault_ops, concepts, vault_inventory, vault_git, session_notes, embeddings, mem_split, mem_audit, review, episodes, titles, vision, images, voice, purge, backup, onboarding, session_ops ; `spawn_supervised` prend `&Supervision` | M |
| T08 | `Messenger` reçu explicitement | les 16 modules qui lisent `hooks.messenger()` ; chaque fonction ou contexte prend `Option<Arc<dyn Messenger>>` | T07 | `grep -rn 'hooks\.' crates/penelope-daemon/src` limité à runtime, engine, runner, supervisor, rpc, telegram, mcp (glue) | M |
| T09 | Port `McpAdmin` | `runtime.rs:328-372` (`Hooks.mcp_supervisor` devient `Arc<dyn McpAdmin>`), surface utilisée par telegram, rpc, supervisor, workflow, engine, hermes, mcp_auth, mem_audit, doctor (`mcp_checks`) à inventorier par `grep 'mcp_supervisor()' -A3` | aucune | `McpSupervisor` n'est nommé que dans `mcp.rs` et `supervisor.rs:216-221` ; suite verte | S |
| T10 | Doubles de test partagés | un `RecordingMessenger` (aujourd'hui 7 copies : `dream.rs:3854`, `budget_alert.rs:152`, `supervisor.rs:681`, `workflow.rs:2932`, `scheduler.rs:1159`, `ingest.rs:870`, `compaction.rs:1629`), un `ProviderSource` factice sur `MockProvider`, sous un module `testing` public (feature `test-util` ou module `pub` documenté) | T07, T08 | les 7 copies supprimées ; les tests des futurs modules `S` ne construisent plus de `Daemon` (`Daemon::from_services` absent de leurs tests) | S |
| T11 | Tests de `telegram.rs` en fichiers frères | `telegram.rs:7949-13716` → `telegram/tests/*.rs` (8 fichiers thématiques) ; helpers `gateway`, `settle`, `drain`, `texts` (7960-8048) dans `telegram/tests/mod.rs` | aucune | 88 tests verts ; `telegram.rs` < 8 000 lignes ; chaque fichier de tests < 800 | M |
| T12 | `telegram.rs` et `screens.rs` en modules < 800 | section 5.1 (16 + 5 modules) | T11, T06 de préférence | aucun fichier de `telegram/` > 800 ; `cargo test -p penelope-daemon telegram` vert ; `docs` vert (T04) | L |
| T13 | `dream.rs` en modules | section 5.2 | T06 (pour `deep_link`) | aucun fichier > 800 ; 35 tests verts ; `mem_bench` (evals) vert | M |
| T14 | `agent.rs` en modules, ports isolés | section 5.3 ; le module `ports` reçoit 28-361 ; `approval` reçoit 1929-2147, 2207-2411 | aucune | aucun fichier > 800 ; 38 tests verts ; `bus.rs` importe `ports::{TurnEvent, TurnOutcome}` | M |
| T15 | `workflow.rs` en modules | section 5.4 | T05 ; à séquencer avec #191-#193 (même fichier) | aucun fichier > 800 ; 20 tests + `ticket_to_deploy_e2e` verts | M |
| T16 | `executor.rs` en modules, ports isolés | section 5.5 ; `Messenger`, `McpGateway`, `Orchestrator`, `ToolEnv` (18-196) rejoignent `ports` | T14 | aucun fichier > 800 ; 30 tests verts (dont le test macOS `executor.rs:3659` sur la CI macOS) | M |
| T17 | `engine.rs` en modules | section 5.5 | T05 | aucun fichier > 800 ; 31 tests verts | M |
| T18 | `rpc.rs` par domaine | section 5.5 ; `every_declared_method_is_either_served_or_explicitly_absent` (`rpc.rs:2461`) reste le garde-fou | aucune | aucun fichier > 800 ; 23 tests verts ; `chat_socket.rs` vert | M |
| T19 | `mcp.rs` en modules | section 5.5 | T09 | aucun fichier > 800 ; 22 tests verts (2 macOS) | M |
| T20 | Les 14 autres fichiers > 800 | doctor, compaction, hermes, upgrade, scheduler (découpage) ; mcp_auth, ingest, conversation, purge, codex_auth, supervisor, concepts, vault_ops, runtime (tests sortis) | T05-T07 | plus aucun fichier du daemon > 800 hors baseline restante ; baseline archtest (T01) réduite d'autant | M |
| T21 | Crate `penelope-app` | nouvelle crate ; contenu de la section 3.2 ; `runtime.rs` garde `Daemon`, `Hooks`, `recover`, `status`, `rss_mb` et réexporte `penelope_app::{Services, …}` ; dossier `skills/` déplacé ; `Cargo.toml` avec les 13 dépendances métier et leur raison | T05-T10, T14, T16 | `cargo test --workspace` vert ; archtest (T02) connaît la crate ; `penelope-daemon` ≈ 73 000 lignes ; `penelope_daemon::runtime::Services` et `bus::Origin` restent valides pour cli, evals, `tests/`, `examples/` (réexports) | L |
| T22 | Crate `penelope-vault` | 13 modules + fonctions de tiers/instantané de `conversation.rs:347-373, 559-664` ; cycles `vault_ops.rs:195`, `vault_git.rs:132`, `dream.rs:606` résolus ; `embeddings::State` hors de `Daemon` | T21, T07 | crate sans `penelope_daemon` ; ses tests ne construisent pas de `Daemon` ; evals `vault_ops::reindex`, `review::record_candidates` importés de la nouvelle crate ; matrice CA régénérée (`episodes.rs` cité `docs/ca-matrix.md:64-65`) | M |
| T23 | Crate `penelope-agent` | `agent/`, `conversation.rs` (reste), `compaction/` (+ `State`), `titles`, `budget_alert` ; `wants_network` dans `penelope-tools` | T22, T14, T17 | 38 + 31 (engine, restent au daemon) tests verts ; `Daemon` tient `Arc<penelope_agent::compaction::State>` ; archtest vert | L |
| T24 | Crate `penelope-executor` | `executor/`, `selfknow` (sans `Admin`), `selfdocs` + `build.rs`, `vision`, `images`, `voice`, `tools_on_demand` ; `Orchestrator` étendu de `schedule_create/list/move/delete` ; `bounded_redacted` descendu | T22, T16 | crate sans dépendance sur agent ni orchestrator ; `every_native_tool_is_documented` (docs) vert ; `runtime_pathlayer_demo.rs` compile | L |
| T25 | Crate `penelope-mcp-host` | `mcp_host/`, `mcp_auth/` ; contexte `{services, messenger, mcp_admin, supervision}` pour `mcp_auth` | T21, T09, T19 | `McpSupervisor: McpGateway + McpAdmin` ; 22 + 10 tests verts ; `supervisor.rs:216-221` construit la crate | M |
| T26 | Crate `penelope-dream` | `dream/`, `ingest`, `onboarding` ; `DigestInputs` ; `struggling_sessions` descendu dans `penelope-context` ; `decide_approval` via app | T22, T13 | evals `mem_bench`, `wiki_e2e` (daemon) verts ; `scheduler.rs` cite `penelope_dream::{digest_text, system_crons}` | M |
| T27 | Crate `penelope-orchestrator` | `workflow/`, `scheduler/` ; `workflow::State` hors de `Daemon` ; `hooks.telegram` reçu en `Option<Arc<dyn ChannelDelivery>>` | T23, T24, T26, T15 | `WorkflowOrchestrator: penelope_app::Orchestrator` ; `ticket_to_deploy_e2e` vert ; `Daemon` tient `Arc<penelope_orchestrator::State>` | L |
| T28 | Crate `penelope-ops` | `doctor/`, `upgrade/`, `backup`, `hermes/`, `codex_auth`, `codex_quota`, `skill_install`, `skill_deps`, `purge`, `session_ops` | T22, T25 (port seulement), T20 | `penelope-cli` importe `penelope_ops::{doctor::render, upgrade}` (`commands.rs:713, 1203, 1214, 1734, 1869`) ; `ca_2_8` (`upgrade.rs`, `ca-matrix.md:21`) régénéré ; `rpc.rs:63-78` (doctor) compile | M |
| T29 | Crate `penelope-gateway-telegram` | `telegram/` entier ; port `Gateway` dans le daemon ; `Daemon::run(gateway: Option<Arc<dyn Gateway>>)` à la place de `supervisor.rs:211-235` ; composition et ordre (Telegram avant MCP, `supervisor.rs:209-221`) dans `penelope-cli` ; `parse_params` importé de la crate | T06, T12 (minimum) ; T21 recommandé | `penelope-daemon` sans module `telegram` ; archtest `GATEWAY_DEPENDENTS = [penelope-cli]` ; 88 tests verts dans la crate ; `an_uncertain_effect_is_pushed_then_decided_from_telegram` (`telegram.rs:8204`) vert, preuve que l'ordre de démarrage est conservé | L |
| T30 | Consommateurs et matrice | suppression des `pub use` de transition ; `penelope-evals`, `penelope-cli`, `tests/`, `examples/` sur les nouveaux chemins ; `UPDATE_CA_MATRIX=1 cargo test -p penelope-evals --test ca_matrix` ; `UPDATE_DOCS=1` si une commande ou un outil a bougé de fichier | T21-T29 | `grep -rn 'penelope_daemon::' crates/penelope-evals crates/penelope-cli` limité à `Daemon`, `VERSION`, `runner::process`, `rpc::serve` ; `docs/ca-matrix.md` à jour ; CI verte | S |
| T31 | Documentation d'architecture | `docs/architecture.md` (crates, ports, schéma cible, règles archtest), `docs/decisions/0011-decoupage-du-daemon.md`, index `docs/README.md` (test `the_index_cites_every_guide_and_decision`), section de version dans `docs/progress.md` | T21 au moins | test `docs` vert | S |
| T32 | Resserrer les baselines | `OVERSIZED_BASELINE` et `DAEMON_TYPE_ALLOWED` réduits à ce qui reste | T20, T29 | baseline daemon vide ; baseline workspace = les 34 fichiers hors daemon (section 7) | S |
| T33 (après V1) | Ports `TurnIntake`, `SessionModels`, `Transcriber` | `engine/intake.rs`, `engine/models.rs`, `engine/media.rs` ; la passerelle et `rpc` les consomment | T29 | la passerelle ne nomme plus `Daemon` que dans sa composition | M |
| T34 (après V1) | Registre d'outils natifs enfichable | `trait NativeTool` dans app ; vault, orchestrator, ops enregistrent leurs outils ; le `match` de `executor.rs:480-1600` ne garde que fs, shell, git, http | T24, T27, T28 | `penelope-executor` ne cite plus `Orchestrator::schedule_*` ; `every_native_tool_is_documented` vert | L |
| T35 | Règle R8 (frontière canal / cœur) posée avec sa baseline | `penelope-archtest/src/lib.rs` ; baseline = comptes de la section 1.3 (29 modules du daemon, 9 fichiers du kernel, `store/migrations.rs`, `observe/redact.rs`, `tools/spec.rs`) | aucune | test `channel_agnostic_surfaces_do_not_name_a_channel` vert sur la baseline ; échantillon `matches!(origin, Origin::Telegram { .. })` détecté par le test du détecteur ; toute occurrence nouvelle hors liste échoue | S |
| T36 | Le cœur ne nomme plus le canal | les 24 chaînes utilisateur rendues génériques (`conversation.rs:222`, `runner.rs:97`, `scheduler.rs:396, 947, 990`, `executor.rs:881`) ; `templates` et `actions` sortis de `Services` (`runtime.rs:48-49, 104-112`) vers la passerelle, derrière un port `Cards` pour `scheduler.rs:391` et `workflow.rs:1951`, `actions.purge_expired` (`supervisor.rs:484`) dans la maintenance de la passerelle, `workflow_known` (`runtime.rs:243`) alimenté par le port ; fusion des rafales (`conversation.rs:204-222`, `runner.rs:136-148`) derrière `ChannelDelivery::burst_limits` ; `place_name` / `retarget` / `target_origin` (`scheduler.rs:905-1000`) derrière `ChannelDelivery::describe_origin` ; `elicitation.rs:488` déplacé dans `OwnerChannel::show` ; `selfknow.rs:120, 380` via `Admin` ; `executor.rs:877-892` via `Orchestrator::schedule_move` | T06, T08 ; T29 recommandé avant | baseline R8 du daemon au moins divisée par deux (≤ 140) ; `penelope-app` sans dépendance `penelope-telegram` ; suites telegram, scheduler, workflow vertes | L |
| T37 (après V1) | Origine et liaison de session génériques | `Origin::Channel { channel, chat, thread, message }` à la place de `Origin::Telegram` (`bus.rs:19-56`, 17 sites) ; `EffectKind::Message` (`kernel/effects.rs:64`, `agent.rs:2531`) ; `SessionStore::{bind_channel, unbind_channel, find_by_binding}` (`kernel/session.rs:327-395`) sur les colonnes existantes ; `turn.rs:103-106` ; `StatusReport.channel` (`api.rs:419`) | T35, T36 | liste blanche R8 réduite à `store/migrations.rs`, `kernel/config.rs`, `observe/redact.rs` | M |
| T38 | Dépendance morte `penelope-workflow → penelope-telegram` | `crates/penelope-workflow/Cargo.toml:27` ; aucun `penelope_telegram` dans la crate | aucune | `cargo build --workspace` et `cargo test -p penelope-workflow` verts sans la dépendance ; règle R3 de `penelope-workflow` sans `penelope-telegram` | S |

38 tâches, dont 35 pour la V1 (6 S en phase 0, 7 en phase 1, 10 en phase 2, 9 en phase 3, 3
de clôture) ; T33, T34 et T37 sont différées.

## 7. Règles mécaniques à ajouter dans `penelope-archtest`

Ce que `crates/penelope-archtest/src/lib.rs` vérifie aujourd'hui :

- règles de dépendance pour **8 crates sur 17** (`dependency_rules`, `lib.rs:205-245` :
  store, kernel, observe, platform, llm, context, hitl, telegram) ; mcp, memory, skills,
  tools, workflow, daemon, cli, evals, archtest ne sont pas contraints ;
- absence de cycle entre crates (`dependency_cycles`, `lib.rs:265-295`) ;
- motifs interdits hors `penelope-platform` (`FORBIDDEN_PATTERNS`, `lib.rs:136-151`), en
  ignorant les blocs `#[cfg(test)]` et les commentaires (`lib.rs:160-175`) ;
- dépendances propres à un OS hors platform (`OS_SPECIFIC_CRATES`) ;
- `#![forbid(unsafe_code)]` dans chaque point d'entrée, `UNSAFE_ALLOWED = []`.

Ce qu'il ne vérifie pas : la taille des fichiers, le nombre de modules, qui a le droit de
nommer `Daemon`, qui a le droit de dépendre du daemon, et rien à l'intérieur d'une crate.

Règles à ajouter (chacune avec un test du détecteur, comme
`the_pattern_detector_actually_detects`, `lib.rs:439`) :

**R1. Plafond de lignes.** `pub const FILE_LINE_CAP: usize = 800;` et
`pub const OVERSIZED_BASELINE: &[(&str, usize)]` (chemin relatif à `crates/`, taille au
moment du gel). Parcours de `sources(c)` étendu à `tests/` et `examples/`. Violation si un
fichier hors baseline dépasse le plafond, si un fichier de la baseline dépasse **sa** taille
gelée, ou si un fichier de la baseline est repassé sous le plafond sans être retiré de la
liste (cliquet : la liste ne peut que se vider). Baseline initiale, daemon (23) :
telegram.rs 13 716, dream.rs 5 859, agent.rs 4 239, workflow.rs 4 044, executor.rs 3 712,
mcp.rs 3 291, engine.rs 3 173, rpc.rs 2 481, telegram/screens.rs 2 435, doctor.rs 2 357,
compaction.rs 2 225, hermes.rs 1 971, upgrade.rs 1 926, scheduler.rs 1 833, mcp_auth.rs
1 246, ingest.rs 1 225, conversation.rs 1 222, purge.rs 1 185, codex_auth.rs 1 040,
supervisor.rs 926, concepts.rs 885, vault_ops.rs 881, runtime.rs 821. Hors daemon (34) :
kernel/config.rs 2 738, cli/commands.rs 2 675, llm/provider.rs 2 048, memory/index.rs 1 622,
llm/codex.rs 1 533, memory/consolidation.rs 1 476, context/store.rs 1 379, tools/spec.rs
1 290, mcp/transport.rs 1 265, store/migrations.rs 1 260, context/engine.rs 1 214,
mcp/registry.rs 1 200, memory/vault.rs 1 163, kernel/turn.rs 1 136, context/compaction.rs
1 122, hitl/policy.rs 1 120, workflow/validate.rs 1 101, platform/backend/macos.rs 1 082,
memory/recall.rs 1 066, store/lib.rs 1 036, mcp/client.rs 1 027, platform/process.rs 1 008,
workflow/schedules.rs 1 003, tools/shell.rs 999, workflow/runs.rs 981, memory/wiki.rs 969,
tools/fs.rs 938, kernel/budget.rs 931, telegram/api.rs 913, mcp/oauth.rs 895,
telegram/templates.rs 876, llm/sse.rs 869, hitl/lib.rs 845, llm/router.rs 805.

**R2. Dépendants du daemon et de la passerelle.**
`DAEMON_DEPENDENTS = ["penelope-cli", "penelope-evals", "penelope-gateway-telegram"]`,
`GATEWAY_DEPENDENTS = ["penelope-cli"]`. Violation si une autre crate déclare
`penelope-daemon` ou `penelope-gateway-telegram` dans `[dependencies]` (les
`dev-dependencies` restent hors règle, `lib.rs:66-71`).

**R3. Une règle par crate.** `dependency_rules()` étendu aux 17 crates actuelles et aux
9 nouvelles avec les listes de la section 3.2 ; test `every_crate_has_a_dependency_rule`
qui échoue dès qu'une crate du workspace n'a pas d'entrée. Contraintes notables :
`penelope-executor` sans agent ni orchestrator ; `penelope-dream` sans orchestrator ;
`penelope-ops` sans mcp-host ; `penelope-vault` sans agent.

**R4. Qui nomme `Daemon`.** `DAEMON_TYPE_ALLOWED: &[(&str, usize)]` : fichiers autorisés
à contenir `&Daemon`, `Arc<Daemon>`, `runtime::Daemon` hors `#[cfg(test)]`, avec leur
compte gelé (baseline : les 41 fichiers du daemon qui le citent aujourd'hui hors tests, comptes de la
section 1). Cible : runtime, engine/*, runner, supervisor, rpc/*, runtime_events, les deux
e2e, et la composition de la passerelle. Même cliquet que R1.

**R5. `impl Daemon` uniquement dans la liste.** `DAEMON_IMPL_ALLOWED = ["runtime.rs",
"engine/…", "runner.rs", "supervisor.rs"]` ; détection par `^impl Daemon` et
`^impl .* for Daemon`.

**R6. Module des ports sans implémentation.** Dans `penelope-app/src/ports/`, aucune
ligne `impl <Trait> for` sauf les doubles de test sous `#[cfg(test)]` ou dans `testing/` ;
garantit que la crate des ports ne redevient pas un daemon.

**R7. Pas de `crate::telegram` ni de `Rpc::new` hors des crates prévues.** Motifs ajoutés
à une table `CRATE_LOCAL_PATTERNS` : `("penelope-daemon", "crate::telegram::")` interdit
après T29 ; `("penelope-*", "Rpc::new(")` limité à daemon et gateway.

**R8. Frontière canal / cœur.** Transposée de
`scripts/check-channel-agnostic-boundaries.mts` d'OpenClaw
(`scratchpad/comparatif/openclaw`), qui protège une liste de sources « channel-agnostic »
et y refuse, par analyse de l'AST TypeScript : un import d'un module de canal
(`channelSegmentRe` construit sur `channelIds`), un chemin de configuration
`channels.<id>`, une comparaison avec un littéral d'identifiant de canal, une affectation
`channel: "<id>"`, avec une liste `allowedViolations` par règle et par fichier, et une
règle sœur pour les textes utilisateur qui nomment un canal (`userFacingChannelNameRe`).
Sans AST en Rust, même découpage par motifs sur les lignes de code, hors `#[cfg(test)]` et
hors commentaires comme `forbidden_patterns` (`lib.rs:160-175`) :

- `CHANNEL_IDS = ["telegram"]` (à étendre le jour où un second canal existe) ;
  `CHANNEL_AGNOSTIC_CRATES = ["penelope-kernel", "penelope-app", "penelope-agent",
  "penelope-executor", "penelope-vault", "penelope-dream", "penelope-daemon"]` ;
- famille « import » : `use penelope_telegram`, `penelope_telegram::`, `crate::telegram::`,
  et `penelope-telegram` dans `[dependencies]` d'une crate protégée ;
- famille « configuration » : `.telegram.`, `cfg.telegram`, `telegram_user_id`, clés
  `"telegram.` ;
- famille « comparaison » : `Origin::Telegram`, `EffectKind::Telegram`, `"telegram" =>`,
  `== "telegram"`, `matches!(… Telegram` ;
- famille « identifiants » : `\btg_[a-z_]+`, `\bchat_id\b`, `\btopic_id\b`,
  `callback_data`, `bind_telegram`, `unbind_telegram`, `find_by_topic`, `hooks.telegram`,
  `telegram_chat(` ;
- famille « texte utilisateur » : littéral `"…Telegram…"` dans une crate protégée ;
- `CHANNEL_ALLOWED: &[(&str, usize)]` : liste blanche par fichier avec compte gelé et
  cliquet (mêmes règles que R1). Entrées permanentes, chacune justifiée dans le code :
  `penelope-store/src/migrations.rs` (l'historique SQL est immuable), `penelope-kernel/src/config.rs`
  (le schéma de configuration nomme ses canaux), `penelope-observe/src/redact.rs` (le
  caviardage doit connaître la forme des jetons), `penelope-app/src/bus.rs` pour
  `Origin::Telegram` tant que T37 n'est pas faite. Toutes les autres entrées portent la
  baseline mesurée en 1.3 et ne peuvent que décroître ;
- test du détecteur sur l'échantillon `if matches!(origin, Origin::Telegram { .. })`.

Ces règles s'écrivent avec les mêmes briques que l'existant (`crates()`, `sources()`,
`Violation`), sans dépendance nouvelle.

## 8. Risques et détection

| Risque | Preuve | Détection / parade |
|---|---|---|
| **Tests macOS non compilés en session Linux.** Quatre `#[cfg(target_os = "macos")]` dans le daemon : `executor.rs:3659` (bac à sable réseau), `mcp.rs:3149` et `:3257` (serveur stdio réel sous Seatbelt, `FAKE_PY`), `ingest.rs:972` (OCR, `#[ignore]`) ; deux blocs `cfg(target_os)` hors tests dans `runtime.rs:653, 664` (`rss_mb`). Un déplacement qui casse une signature n'est vu que par la CI macOS (`ci.yml:59-99`, `cargo test --workspace` sur `macos-14`). | `CLAUDE.md` (« la 0.17.40 a manqué sa release ainsi ») | avant de pousser : `grep -rn 'cfg(target_os = "macos")' crates/` et relecture manuelle des appels ; sur le MBP du propriétaire (Darwin), `cargo test -p <crate>` les compile réellement ; garder ces tests dans le même lot que le code qu'ils exercent |
| **Cycles entre crates.** Les 13 cycles de module de la section 1.2 deviennent des erreurs Cargo dès qu'une frontière les traverse. | tableau 1.2 | ils sont listés comme préalables (section 4) ; `cargo check` refuse les cycles ; `dependency_cycles` (archtest) les nomme ; les casser **avant** de créer la crate, dans un lot séparé |
| **Règle des orphelins.** `impl Daemon` dans `engine.rs:145`, `supervisor.rs:90`, `runner.rs:279`, `impl Admin for Daemon` (`engine.rs:83`) : impossible d'emporter ces fichiers hors de la crate de `Daemon`. | section 1.1 point 3 | ces fichiers restent dans le daemon (section 3.2) ; R5 empêche d'en créer d'autres |
| **Hooks posés après le démarrage.** `Hooks` sont des `Option` remplis dans `supervisor.rs:160-221` ; l'ordre importe : Telegram est construit avant MCP pour que `elicitations.expect_owner` soit vrai quand un serveur se connecte (`supervisor.rs:209-211`). La composition déplacée dans `penelope-cli` (T29) peut inverser l'ordre. | `supervisor.rs:209-235` | test `an_uncertain_effect_is_pushed_then_decided_from_telegram` (`telegram.rs:8204`) et `ticket_to_deploy_e2e` (câble `hooks.set_mcp`, `set_orchestrator`, `telegram`, `ticket_to_deploy_e2e.rs`) ; documenter l'ordre dans `Daemon::run(gateway)` |
| **`McpSupervisor` concret dans `Hooks`.** 10 sites lisent `hooks.mcp_supervisor()` ; tant que T09 n'est pas fait, `mcp.rs` ne peut pas sortir sans entraîner ses consommateurs. | `runtime.rs:333, 359-372` | T09 avant T25 ; R4 |
| **Churn et conflits.** 271 commits sur 30 jours ; `telegram.rs` 73, `rpc.rs` 52, `executor.rs` 45, `engine.rs` 39, `agent.rs` 37, `dream.rs` 30, `workflow.rs` 25. Lot #205 non commité sur `agent.rs`, `purge.rs`, `doctor.rs`, `rpc.rs`, `cache_audit.rs`, `conversation.rs`. Issues ouvertes sur les mêmes fichiers : #185, #191-#193 (`workflow.rs`), #204 (`agent.rs:1660-1706`, `executor.rs:1453-1474`, `mcp/tasks.rs`), #206 et #203 (`agent.rs`), #207 (`dream::vault_check`). Trois autres sessions travaillent en parallèle sur ce dépôt. | `git log`, `git diff --stat`, `gh issue list` | commits « déplacement seul » (`git mv`, aucun changement de corps) séparés des commits qui changent une signature ; T15 (workflow) et T27 après ou en accord avec #191-#193 ; T14/T23 après le merge de #205 ; un lot = une version (la serrure de `Cargo.toml`, `CLAUDE.md`) ; rebase avant chaque lot |
| **Le test `docs` lit le daemon à plat.** `docs.rs:443-450` concatène les `.rs` de `crates/penelope-daemon/src` sans descendre dans les sous-dossiers pour valider les noms d'événements ; T12 (sous-dossier `telegram/`) ou toute extraction fait échouer le test. | `docs.rs:443` | T04 en tout premier |
| **Chemins cités par la matrice CA.** `docs/ca-matrix.md:21, 46, 64, 65, 111` citent `upgrade.rs`, `cache_audit.rs`, `episodes.rs`, `ticket_to_deploy_e2e.rs` du daemon ; 5 tests `ca_*` dans le daemon (`cache_audit.rs`, `episodes.rs` ×2, `ticket_to_deploy_e2e.rs`, `upgrade.rs`). | `grep -rn 'fn ca_'` | `UPDATE_CA_MATRIX=1 cargo test -p penelope-evals --test ca_matrix` dans le lot qui déplace |
| **Ressources embarquées.** `selfdocs.rs:14` inclut `OUT_DIR/docs.rs` produit par `crates/penelope-daemon/build.rs` ; `runtime.rs:600` inclut `../skills/wiki-markdown/SKILL.md`. | | `build.rs` part avec `selfdocs` (T24), `skills/` avec `reload_skills` (T21) ; le test `bootstrap_wires_everything` (`runtime.rs:693`) et `every_native_tool_is_documented` (docs) le vérifient |
| **Tests qui construisent un `Daemon` dans des crates qui ne le connaîtront plus.** `Daemon::from_services` ou `Services::for_tests` apparaissent dans 43 fichiers ; dans vault, dream, executor, mcp-host, orchestrator, ops, les tests devront se contenter de `Services::for_tests` plus des ports factices. | section « for_tests / from_services par fichier » | T10 fournit les doubles ; critère de T22-T28 : aucun `Daemon` dans les tests des crates extraites |
| **Consommateurs externes.** `penelope-cli` (`commands.rs:713, 1054, 1203, 1214, 1734, 1869-1905, 2179`), `penelope-evals` (13 chemins : `dream::run`, `conversation::vault_dir`, `runner::process`, `bus::Origin`, `vault_ops::reindex`, `selfdocs::anchor`, `runtime::SUBSYSTEMS`, `review::record_candidates`, `ingest::ingest`, `dream::submission_order`, `dream::digest_text`, `compaction`, `agent::TurnOutcome`), `tests/chat_socket.rs`, `tests/log_spans.rs`, `examples/runtime_pathlayer_demo.rs`. | `grep -rhoE 'penelope_daemon::…'` | `pub use` de transition dans le daemon jusqu'à T30 ; `cargo test --workspace` |
| **Temps de compilation et disque.** `penelope-app` dépendra des 13 crates métier : c'est le nouveau goulot de recompilation ; 10 crates de plus grossissent `target/` (déjà > 5 Go). | Cargo.toml, mémoire de session | découpage des crates par couche pour que `cargo test -p penelope-vault` ne recompile ni agent ni orchestrator ; `cargo clean` en fin de session |
| **Le cœur reste marqué Telegram après l'extraction.** Sortir `telegram.rs` ne retire pas les 279 occurrences de canal des 29 autres modules ni les 94 du kernel (section 1.3) ; sans R8, un second canal se câblerait par de nouveaux `matches!(origin, Origin::Telegram)`. | section 1.3 | T35 pose la règle avec cliquet dès la phase 0 ; T36 traite la logique (rafales, cibles, gabarits) ; T37 généralise `Origin`, `EffectKind` et la liaison de session |
| **Événements et clés kv.** Les noms d'événements (`daemon.task_panicked`, `store.fts_rebuilt`…) et de clés kv (`cli.session`, `turn.recorded.*`) sont des chaînes dispersées ; un déplacement ne les casse pas, mais T05/T06 les concentrent : vérifier qu'aucune clé n'est renommée au passage. | `engine.rs:243-276`, `tasks.rs:142` | tests existants (`a_panicking_loop_is_restarted_and_reported`, `tasks.rs:281`) ; diff de déplacement seul |
