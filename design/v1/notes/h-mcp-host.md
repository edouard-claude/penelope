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
