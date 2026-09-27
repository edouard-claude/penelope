# Lot H, crate `penelope-ops` (T28)

Branche `v1-h-ops`, dérivée de `v1` au commit c5ecc92 (1.0.0-alpha.7). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §3.2, §6 (T28). Préalables : T21
(`h-app.md`), T22 (`h-vault.md`), T25 (`h-mcp-host.md`), T29 (`h-gateway.md`).

## 0. Inventaire avant déplacement

| Module du daemon | Lignes | Ce qu'il nomme du daemon | Sortie |
|---|---|---|---|
| `doctor/` (7 fichiers + tests) | 2 355 | `helpers`, `machine`, `codex_scope`, `ports` (app) ; `mem_split`, `vault_inventory`, `embeddings`, `dream::core_overflow` (vault) ; `codex_auth`, `codex_quota`, `skill_deps`, `upgrade` (ops) ; `tool_jobs::store`, `prompt_snapshot::weight_bytes`, `history::doctor_check` (daemon) ; `mcp::stdio_profile` (mcp-host) ; `Daemon` (`embedding_check`) | voir ci-dessous |
| `upgrade.rs` + `upgrade/tests.rs` | 2 013 | `helpers`, `ports::{Handle, Slot}`, `executor::Messenger` (app) ; `mcp_auth::check_endpoint` (mcp-host) ; `crate::VERSION` ; `Daemon` (`rpc`, `change`) | `rpc` prend `&Arc<Services>` et `&Handle` |
| `hermes.rs` + `hermes/` | 1 966 | `Messenger`, `McpAdmin`, `helpers`, `bus`, `reload_skills` (app) ; `dream::vault_sync`, `vault_ops` (vault) ; tests : `Daemon::from_services`, `mcp::testing` | tests sur `Services` ; `penelope-mcp-host` en dev-dependency pour `mcp::testing` |
| `backup.rs` | 758 | `Messenger`, `helpers`, `bus` (app) ; `crate::VERSION` ; tests : `Daemon::from_services` | tests sur `Services` |
| `codex_auth.rs` | 1 040 | `Messenger`, `Slot`, `bus` (app) ; `codex_quota` (ops) ; tests : `Daemon::from_services`, `publish_config` | tests sur `Services` (`Services::publish_config` existe) |
| `codex_quota.rs` | 196 | `Messenger`, `bus` (app) | rien |
| `skill_install.rs` | 171 | `Daemon` (`install`), `reload_skills` (app), `skill_deps` (ops) | `install` prend `&Services` |
| `skill_deps.rs` | 202 | rien | rien |

Ce que doctor ne peut pas emporter (il dépendrait du daemon ou de l'hôte MCP) :

- `tool_jobs_check` (`tool_jobs::store`), `prompt_stability_check`
  (`prompt_snapshot::weight_bytes`), `history::doctor_check` : contrôles du daemon,
  appelés au milieu de `doctor::run` ; ils sortent de `run` et le daemon les ajoute dans
  la méthode RPC `doctor` (`rpc/methods/doctor.rs`).
- `mcp_checks` : lit `penelope_mcp_host::stdio_profile` ; §3.2 interdit à ops de
  dépendre de l'hôte MCP. Reste au daemon (`rpc/methods/doctor.rs`) avec ses deux tests
  (`doctor/tests/mcp_host.rs`), qui passent déjà par l'hôte et l'exécuteur natif.
- `embedding_check(&Daemon)` : prend l'`Embedder` du vault (services, `ProviderSource`,
  état), que `Daemon::embedder()` fournit.

`upgrade::fetch` refusait une adresse non HTTPS par `mcp_auth::check_endpoint` : la
règle (HTTPS, ou HTTP vers la boucle locale exacte) est recopiée en fonction privée de
`upgrade`, l'hôte MCP n'étant pas dans le périmètre du lot ; à faire descendre dans
`penelope_app::helpers` quand l'hôte MCP sera rouvert.

`purge.rs` et `session_ops.rs` restent au daemon dans cette vague (un autre agent
modifie la purge en parallèle) : ils suivront dans une tâche ultérieure.

Consommateurs : `penelope-cli` (`doctor::render`, `upgrade::*`), la passerelle
(`crate::doctor`, `crate::upgrade`, `crate::codex_quota` par ses réexports du daemon),
le daemon (`supervisor`, `runtime`, `rpc`, `selfknow`, `engine`). Réexports de
transition dans `lib.rs` du daemon.

## 1. Ce qui est livré

Crate `crates/penelope-ops` (8 681 lignes, aucun fichier au-dessus de 810, le plus gros
étant `upgrade/tests.rs`), au-dessus de `penelope-app` et de `penelope-vault`, sous le
daemon, qui en dépend :

| Module | Origine |
|---|---|
| `doctor/` (`mod`, `coherence`, `machine`, `memory`, `models`, `secrets`, tests) | `doctor/` du daemon, par `git mv` |
| `upgrade` (+ `upgrade/boot.rs`, `upgrade/switch.rs`, tests) | `upgrade.rs`, découpé sous 800 lignes |
| `hermes` (+ `hermes/mcp.rs`, `hermes/yaml.rs`, tests) | `hermes.rs`, découpé sous 800 lignes |
| `codex_auth` (+ `codex_auth/tests.rs`), `codex_quota`, `backup`, `skill_install`, `skill_deps` | modules du même nom |

Critères de T28 :

- la crate ne dépend ni de `penelope-daemon` ni de `penelope-mcp-host` (règle
  `OPS_ALLOWED_DEPS`, test `the_ops_crate_depends_neither_on_the_daemon_nor_on_the_mcp_host`) ;
  l'hôte MCP n'est qu'une dev-dependency, pour les doubles du test d'import Hermes ;
- ses tests ne construisent pas de `Daemon` : `grep -rn Daemon crates/penelope-ops`
  ne rend que `DaemonTokens` et des commentaires ;
- `penelope-cli` importe `penelope_ops::{doctor::render, upgrade}` ;
- `docs/ca-matrix.md` régénéré : `ca_2_8` dans `crates/penelope-ops/src/upgrade/tests.rs` ;
- réexports de transition dans `lib.rs` du daemon pour les huit modules : passerelle,
  RPC, superviseur, `selfknow`, `runtime` et les tests du daemon n'ont pas bougé ;
- `embedding_check` prend l'`Embedder` du vault (qui porte le `ProviderSource`),
  fourni par `Daemon::embedder()`.

Contrôles laissés au daemon (`crates/penelope-daemon/src/rpc/methods/doctor.rs`), ajoutés
par la méthode RPC `doctor` après `doctor::run` :

- `mcp_checks` : lit `penelope_mcp_host::stdio_profile` ;
- `prompt_stability_check` (`prompt_snapshot::weight_bytes`), `tool_jobs_check`
  (`tool_jobs::store`), `history::doctor_check` : modules du daemon.

Ils étaient appelés au milieu de `run` : dans la sortie de `penelope doctor`, les trois
derniers arrivent désormais juste après les contrôles de `run`, avant ceux des serveurs
MCP. Les identifiants ne changent pas.

## 2. Mesures

| Mesure | Avant (c5ecc92) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 53 836 (plafond `[crates]`) | 45 232 |
| `penelope-ops/src`, lignes | | 8 681 |
| `[daemon].modules` | 40 | 32 |
| `[files.oversized]` | `hermes.rs` 1 210, `upgrade.rs` 1 203, `codex_auth.rs` 1 040 | sortis |
| `[daemon.daemon_users]` | `doctor/memory.rs` 1, `upgrade.rs` 3, `skill_install.rs` 2 | sortis |

## 3. Choix

- Contrôles de doctor qui lisent le daemon ou l'hôte MCP : laissés au daemon plutôt
  qu'un port de plus dans `penelope-app` ; ils sont quatre, lus par la seule méthode RPC,
  et leurs modules (`tool_jobs`, `prompt_snapshot`, `history`) sont en cours de
  modification par d'autres lots. Un port `DoctorExtras` pourra les rendre à `run` quand
  ces modules seront sortis.
- `upgrade::fetch` : la règle d'adresse (HTTPS ou boucle locale exacte) est recopiée en
  fonction privée plutôt que d'importer `penelope_mcp_host::auth::check_endpoint` ;
  l'hôte MCP est hors du périmètre du lot. Reste : faire descendre la fonction dans
  `penelope_app::helpers` et l'appeler des deux côtés.
- Chemins : la crate réexporte en interne (`pub(crate) use`) les modules du socle et du
  vault que les fichiers déplacés nomment par `crate::` (`helpers`, `machine`, `ports`,
  `bus`, `codex_scope`, `embeddings`, `mem_split`, `vault_inventory`, `vault_ops`) ;
  seuls `Services`, `reload_skills`, `Messenger`, `vault_sync`, `core_overflow` et les
  doubles MCP ont été réécrits. Le diff de déplacement reste limité aux `use`.
- `penelope_ops::VERSION` : `env!("CARGO_PKG_VERSION")`, la même version de workspace
  que `penelope_daemon::VERSION`.
- La crate est dans `CHANNEL_AGNOSTIC_CRATES` : les mentions de Telegram de doctor
  (propriétaire, jeton du bot, conversations de groupe) et d'upgrade suivent leurs
  fichiers dans `[channel.allowed]`, sans hausse.

## 4. Reste et blocages

- `purge.rs` et `session_ops.rs` restent au daemon dans cette vague : un autre agent
  modifie la purge en parallèle. Ils suivront dans ops (§3.2) ; `session_ops` nomme
  encore le canal (`[channel.allowed]` 1).
- Plafond `[crates]` du daemon : 53 836 dans `budget.toml`, 45 232 mesurées ;
  `UPDATE_BUDGET` ne le réécrit pas, à abaisser par l'intégrateur.
- `check_endpoint` en double (voir §3).
- Les documents de conception (`gel-et-outillage.md`, `contrat-fonctionnel.md`) citent
  encore les anciens chemins : hors périmètre.
- La passerelle et le daemon nomment encore `crate::doctor`, `crate::upgrade`,
  `crate::codex_quota`… par les réexports ; T30 les retire.

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 70 suites, 1 991 tests passés, 0 échec
(dont `penelope-archtest`, `docs`, `ca_matrix`), sur macOS : les tests
`cfg(target_os = "macos")` sont compilés. `scripts/check-budget.sh c5ecc92` : rien ne
remonte, aucune dérogation. `UPDATE_BUDGET=1` et `UPDATE_CA_MATRIX=1` passés.

## 6. Notes de version (pour docs/progress.md)

#### Crate `penelope-ops` : l'exploitation hors du daemon (T28)

- Nouvelle crate `penelope-ops`, entre `penelope-app` et `penelope-vault` d'un côté et
  le daemon de l'autre : diagnostic (`doctor`), mise à jour et retour arrière du binaire,
  sauvegarde, import d'une instance Hermes, connexion et quota de l'abonnement Codex,
  installation de skills tierces et de leurs dépendances.
- La crate ne connaît ni le daemon ni l'hôte MCP : `penelope-archtest` le vérifie. Les
  contrôles de `doctor` qui lisent le daemon (stabilité du prompt, journal, jobs
  d'outils) ou l'hôte MCP (serveurs, bac à sable) restent au daemon, qui les ajoute à la
  méthode `doctor` ; dans la sortie, ils arrivent après les autres contrôles.
- `upgrade`, `hermes` et `codex_auth` sont découpés sous 800 lignes et quittent la liste
  des fichiers trop longs ; les tests de la crate n'ouvrent pas de daemon.
- La CLI importe `doctor::render` et `upgrade` de la crate ; le daemon réexporte tout sous
  les anciens chemins. `penelope-daemon` passe de 53 836 à 45 232 lignes. `purge` et
  `session_ops` suivront.
