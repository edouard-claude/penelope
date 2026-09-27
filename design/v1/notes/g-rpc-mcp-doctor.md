# Lot G, rpc.rs, mcp.rs et doctor.rs : notes de livraison

Branche `v1-g-rpc-mcp-doctor`, dérivée de `v1` au commit 5c0faf3. Épopée #208, lot G.
Spécification : `design/v1/decoupage-daemon.md` §5.5.

## 1. Ce qui est livré

Trois fichiers de la liste de référence du gel sortent de `[files.oversized]`, chacun
par un commit de déplacement pur suivi d'un commit de budget (`Dérogation-budget: #208`).

| Avant | Après | Plus gros fichier |
|---|---|---|
| `rpc.rs` 2 456 | `rpc/{mod, stream, server, tests}.rs` et `rpc/methods/{mod, ops, approvals, sessions, status_config, codex, mcp, memory, workflows}.rs` | `rpc/tests.rs` 731, code : 232 (`stream.rs`) |
| `mcp.rs` 1 958 | `mcp/{mod, connector, lifecycle, tools, admin, gateway, render}.rs` (`testing.rs` et `tests.rs` inchangés) | `lifecycle.rs` 651 |
| `doctor.rs` 2 522 | `doctor/{mod, models, secrets, machine, memory, coherence, mcp, tests}.rs` | `coherence.rs` 497 |

Chaque `mod.rs` réexporte ce que le reste du daemon nomme : aucun appelant ne change,
aucune signature publique ne change, aucun test ne change (les tests inline de `rpc.rs`
et `doctor.rs` sortent dans `tests.rs`, rustfmt a seulement replié trois lignes après la
désindentation).

## 2. Les choix

- `dispatch` de rpc (1 190 lignes sous un `#[allow(clippy::too_many_lines)]`) devient onze
  fonctions de domaine de même signature (`ops`, `approvals`, `chat`, `sessions`,
  `config`, `models`, `codex`, `mcp_admin`, `memory`, `workflows`, `skills`), chacune
  sous 200 lignes. L'aiguillage (`rpc/methods/mod.rs`) choisit le domaine sur le préfixe
  de la méthode (`mem.search` donne `mem`) ; `model.auth` va à `codex` par une garde ;
  tout le reste, méthodes sans préfixe comprises, va à `ops`. Chaque domaine finit par
  le même bras « méthode inconnue », le code d'erreur `-32601` ne change donc pas. Les
  bras sont identiques ligne pour ligne ; seuls disparaissent les séparateurs de section
  du match. L'allow disparaît : `allow_too_many_lines` passe de 26 à 25.
- Les sous-modules sont frères directs (`mcp/lifecycle.rs` plutôt que
  `mcp_host/supervisor/lifecycle.rs` du §5.5) : les structures restent dans `mod.rs`, leurs
  champs privés restent visibles de tous les sous-modules, et `pub(super)` suffit pour
  les méthodes appelées d'un frère à l'autre. Le répertoire garde le nom du module
  (`mcp/`), pas `mcp_host/` : le renommage n'est pas un déplacement.
- Imports par `use super::*;` : le compte de mentions du type `Daemon` reste exactement
  celui d'avant (8 pour rpc, 1 pour doctor), un import par fichier l'aurait fait monter.
- Visibilités élargies, toutes à `pub(super)` : `rpc::stream::write_line` ; dans mcp,
  `slot`, `ensure_live`, `connect`, `refresh_tools`, `connection_lost`, `stop_slot`, `now`,
  `now_ms`, `dir_fingerprint`, `roots_json`, `row`, `persist`, `delete_row` et les
  fonctions de `render.rs` ; dans doctor, les contrôles privés appelés par `run` ou par
  les tests.

## 3. Budget

Déplacements d'entrées à la main, même total à chaque fois : `[daemon.daemon_users]`
(rpc 8 = 4 + 2 + 1 + 1, doctor 1) et `[channel.allowed]` (rpc 18 = 9 + 9, mcp 1,
doctor 42 = 15 + 12 + 12 + 3). Le découpage fait grossir le daemon de 205 lignes (rpc
+143, mcp +33, doctor +29 : en-têtes de modules, `use super::*`, blocs `impl` et signatures
des fonctions de domaine, sans une ligne de code nouvelle). À l'intégration, le budget a été
re-mesuré en un seul commit (plafond du daemon à 82 865, après les découpages de workflow et
d'executor), les commits de budget de la branche ont été remplacés par celui-ci.

## 4. Notes de version

#### Découpage de rpc.rs, mcp.rs et doctor.rs (lot G)

- `rpc.rs` devient `rpc/` : serveur, flux et méthodes rangées par domaine ; la table de
  dispatch de 1 190 lignes devient onze fonctions de moins de 200 lignes et un aiguillage
  sur le préfixe de la méthode.
- `mcp.rs` devient `mcp/` : connecteur, cycle de vie, appels, administration, passerelle,
  rendus.
- `doctor.rs` devient `doctor/` : contrôles rangés par famille (modèles, secrets, machine,
  mémoire, cohérence, MCP).
- Aucun comportement ne change : mêmes méthodes RPC, mêmes contrôles, mêmes tests.

## 5. Blocages et reste

- Aucun blocage. `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings` et `cargo test --workspace` (57 suites) verts, dont les scénarios, `rpc_golden`,
  `docs`, `telegram_e2e` et `approval_e2e`.
- Le plafond `[crates]` touche la même ligne que le lot G workflow (commit c63c176 sur sa
  branche, +60) : à l'intégration, additionner les hausses (82 213 + 60 + 205).
- Coquille dans le message du commit 7ec3fbb : « rpc/mcp/lifecycle.rs » se lit
  `mcp/lifecycle.rs` (le budget, lui, est juste).
