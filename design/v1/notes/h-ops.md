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
