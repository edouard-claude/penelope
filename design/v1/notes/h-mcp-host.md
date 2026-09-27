# Lot H, crate `penelope-mcp-host` (T25)

Branche `v1-h-mcp-host`, dérivée de `v1` au commit 5669220 (1.0.0-alpha.6). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §3.2, §6 (T25), §8. Préalables : T09
(`h-ports.md`), T21 (`h-app.md`), T19 (`g-rpc-mcp-doctor.md`).

## 0. Inventaire avant déplacement

| Fichier du daemon | Lignes | Ce qu'il nomme du daemon | Sortie |
|---|---|---|---|
| `mcp/mod.rs` | 143 | `runtime::Services` | `penelope_app::services::Services` |
| `mcp/connector.rs` | 185 | `executor::denied_reads`, `mcp_auth::authorization_header` | `penelope_app::helpers`, module frère |
| `mcp/gateway.rs`, `mcp/tools.rs` | 79, 301 | `executor::McpGateway`, `elicitation::Destination` | `penelope_app::ports`, `penelope_app::elicitation` |
| `mcp/admin.rs` | 527 | `ports::McpAdmin` | `penelope_app::ports` |
| `mcp/lifecycle.rs`, `mcp/render.rs` | 651, 153 | rien hors `super::*` | |
| `mcp/testing.rs` | 136 | rien ; `#[cfg(test)] pub(crate)` | public : les tests de telegram, rpc, scheduler, workflow, hermes, engine et `ticket_to_deploy_e2e` s'en servent |
| `mcp/tests.rs` | 1 198 (22 tests) | `Services::for_tests` ; `doctor::mcp_checks` (2 tests), `executor::NativeToolExecutor` et `agent::ToolExecutor` (1 test) | les trois tests qui passent par doctor ou l'exécuteur natif restent au daemon (`doctor`, `executor`) |
| `mcp_auth.rs` | 706 | `Daemon` (5 : `start`, `complete`, `callback_server`, `handle_callback`), `executor::Messenger`, `ports::{McpAdmin, Slot}`, `helpers::owner_origin_of` | `start`, `complete` prennent `&Services` ; le serveur de retour prend `AuthContext {services, messenger, mcp_admin, supervision}` |
| `mcp_auth/tests.rs` | 552 (9 tests) | `Daemon::from_services` (5) | `Services` seuls |

Appelants hors du module : `supervisor.rs` (construction du superviseur, serveur de
retour, rappel quotidien d'autorisation), `telegram/mod.rs` (adresse collée),
`telegram/approvals.rs` (`/mcp auth`), `rpc/methods/mcp.rs` (`mcp.auth`), `upgrade.rs`
(`check_endpoint`), `doctor/mcp.rs` (`stdio_profile`), `rpc/methods/mcp.rs`
(`ReloadReport`), les tests par `mcp::testing`. Tous gardent leur chemin par les
réexports de transition du daemon (`crate::mcp`, `crate::mcp_auth`) ; seuls les appels à
`start` et `complete` changent (`&d.services` au lieu de `d`).

Aucun cycle : le code MCP ne cite du daemon que des éléments déjà descendus dans
`penelope-app`.

## 1. Ce qui est livré

Crate `crates/penelope-mcp-host` (4 420 lignes, aucun fichier au-dessus de 981), au-dessus
de `penelope-app`, sous le daemon qui la construit :

| Module | Contenu | Origine |
|---|---|---|
| racine (`lib.rs`) | `McpSupervisor`, créneaux, `ReloadReport` réexporté | `mcp/mod.rs` |
| `connector` | `Connector`, `ProcessConnector`, `stdio_profile`, trousseau | `mcp/connector.rs` |
| `lifecycle`, `tools`, `admin`, `gateway`, `render` | cycle de vie, appels, `impl McpAdmin`, `impl McpGateway`, rendus | `mcp/` |
| `auth` | OAuth des serveurs HTTP, `AuthContext`, serveur de retour local | `mcp_auth.rs` |
| `testing` | `FakeConnector`, `server`, `tool`, `declare`, `supervisor` (public) | `mcp/testing.rs` |

- `mcp_auth` ne prend plus `Daemon` : `start` et `complete` prennent `&Services` ; le
  serveur de retour reçoit `AuthContext {services, messenger, mcp_admin, supervision}`,
  composé par `supervisor.rs` (les deux derniers champs de `Hooks` sont des `Slot` :
  branchés après le lancement de la boucle).
- `McpSupervisor: McpGateway + McpAdmin`, fixé par le test
  `the_supervisor_is_both_gateway_and_admin`.
- `supervisor.rs` construit `penelope_mcp_host::McpSupervisor` et `ProcessConnector` par
  le nom de la crate ; le reste du fichier n'a changé que pour composer `AuthContext` et
  passer `&d.services` au rappel quotidien d'autorisation.
- Réexports de transition (à retirer en T30) : `pub use penelope_mcp_host as mcp;` et
  `pub use penelope_mcp_host::auth as mcp_auth;` dans `lib.rs` du daemon. Telegram, RPC,
  doctor, upgrade, les tests et `ticket_to_deploy_e2e` gardent leurs chemins.
- Archtest : règle `penelope-mcp-host` → `MCP_HOST_ALLOWED_DEPS` (app, kernel, store,
  platform, observe, llm, mcp), test `the_mcp_host_crate_does_not_depend_on_the_daemon`,
  `the_workspace_is_discovered` la cite, entrée dans `CHANNEL_AGNOSTIC_CRATES`.

### Tests

- Dans la crate, sans `Daemon` : 20 tests du superviseur (19 venus de `mcp/tests.rs`,
  dont les deux macOS `a_real_stdio_server_runs_under_the_sandbox` et son voisin, plus
  le test de ports) et les 9 de `auth`. `real_mcp_server_handshake` reste `#[ignore]`.
- Restés au daemon, déplacés tels quels dans `doctor/tests/mcp_host.rs` : les deux qui
  appellent `doctor::mcp_checks` et `poisoned_or_changed_tools_are_flagged_and_lose_their_rules`
  (exécuteur natif, `tool_describe`). La spécification comptait « 22 + 10 » : 19 + 1 + 9
  dans la crate, 3 au daemon.

### Ordre de démarrage

Inchangé dans `supervisor.rs` : `TelegramGateway::from_config`, puis
`elicitations.expect_owner()`, puis `McpSupervisor::new` et `set_mcp`. Tests qui en
couvrent les deux côtés : `mcp_elicitation_is_answered_from_telegram`
(`telegram/tests/elicitation.rs`, propriétaire joignable avant la connexion :
l'élicitation est annoncée) et `server_requests_and_list_changes_are_handled`
(`penelope-mcp-host/src/tests.rs`, sans propriétaire : rien d'annoncé, demande annulée).
Aucun test ne joue `Daemon::run` lui-même : l'ordre des deux lignes n'est tenu que par le
commentaire de `supervisor.rs` (issue #12) ; à couvrir par T29 quand la composition passe
dans `penelope-cli`.

## 2. Mesures

| Mesure | Avant (5669220) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 82 637 (plafond) | 78 275 |
| `penelope-mcp-host/src`, lignes | | 4 420 |
| `Daemon` dans `mcp_auth` (code et tests) | 5 + 5 | 0 |
| `[daemon].modules` | `mcp`, `mcp_auth` | sortis |
| `[daemon.daemon_users]` `mcp_auth.rs` | 5 | sorti |

## 3. Choix

- La racine de la crate est l'ancien `mcp/mod.rs`, ses modules frères restent frères :
  `use super::*` et les `pub(super)` du lot G valent tels quels, le déplacement ne touche
  que les chemins vers `penelope-app`. `mcp_auth` devient `auth`.
- `start` et `complete` prennent `&Services` plutôt que le contexte : ils n'en lisent pas
  plus. Seul le serveur de retour, qui vit en fond et répond au propriétaire, reçoit
  l'`AuthContext`.
- `testing` devient public sans drapeau de fonctionnalité, comme celui de `penelope-app` :
  il ne tire aucune dépendance de test. Le `setup` des trois tests restés au daemon est
  copié (quinze lignes, `tempfile` n'est qu'une dépendance de test de la crate).
- Budget : les deux mentions du canal (`lifecycle.rs`, `auth.rs`) suivent leurs fichiers
  renommés ; `scripts/check-budget.sh 5669220` accepte sans trailer.

## 4. Reste et blocages

- `[crates]` : plafond du daemon à abaisser à 78 275 par l'intégrateur.
- Collision avec `h-gateway` : `telegram/mod.rs` et `telegram/approvals.rs` changent d'un
  argument chacun (`&self.daemon.services`, `s`) ; `supervisor.rs` : composition de
  `AuthContext` dans la boucle `mcp.oauth_callback`, chemin de construction de
  `McpSupervisor`, `&d.services` dans le rappel quotidien.
- Bissection : 65fe4ce seul a `penelope-archtest` rouge (crate hors de
  `CHANNEL_AGNOSTIC_CRATES`), 268dbcc le remet au vert.
- Le commentaire de tête de `auth.rs` dit encore « côté daemon » (déplacement seul).

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 63 suites, 1 978 tests passés, 0 échec
(dont `penelope-archtest`, `docs`, `scenarios`, `rpc_golden`, `telegram_e2e`, `hot_reload`),
sur macOS : les deux tests `cfg(target_os = "macos")` de la crate sont compilés et joués.

## 6. Notes de version (pour docs/progress.md)

#### Crate `penelope-mcp-host` : superviseur MCP et OAuth hors du daemon (T25)

- Nouvelle crate `penelope-mcp-host`, au-dessus de `penelope-app` et sous le daemon :
  le superviseur des serveurs de `mcp.d/`, son connecteur de processus et
  l'autorisation OAuth des serveurs HTTP. Elle ne dépend pas du daemon (règle
  d'archtest) ; `McpSupervisor` est `McpGateway` et `McpAdmin`.
- L'autorisation OAuth ne prend plus le daemon : ses services, et pour le serveur de
  retour local un contexte (canal du propriétaire, administration MCP, supervision).
- Le daemon réexporte l'hôte sous `mcp` et `mcp_auth` : CLI, évaluations et tests
  inchangés. `penelope-daemon` passe de 82 637 à 78 275 lignes.
