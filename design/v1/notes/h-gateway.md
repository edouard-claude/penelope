# Lot H, crate `penelope-gateway-telegram` (T29)

Branche `v1-h-gateway`, dérivée de `v1` au commit 5669220 (1.0.0-alpha.6). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §2.2 (port `Gateway`), §3.1 (la passerelle
au-dessus du daemon), §3.2, §6 (T29), §7 (R2), §8 (hooks posés après le démarrage).
Préalables : T06 (`d-kv-helpers.md`), T12 (`g-telegram.md`), T21 (`h-app.md`).

## 0. Inventaire avant déplacement

Hors de `telegram/`, le daemon citait la passerelle en quatre endroits :

| Site | Usage | Devenir |
|---|---|---|
| `lib.rs:48` | `pub mod telegram;` | retiré |
| `supervisor.rs:229` | `TelegramGateway::from_config`, `expect_owner`, `start` | `Daemon::run(gateway)`, composition dans la CLI |
| `ticket_to_deploy_e2e.rs` (cfg(test)) | `TelegramGateway::with_transport` | suit la passerelle (un test unitaire du daemon ne peut pas voir une crate au-dessus de lui) |
| `tests/telegram_e2e.rs` (filet du lot B) | `penelope_daemon::telegram::TelegramGateway` | suit la passerelle, ligne `use` seule changée |

Hors du daemon : `penelope-cli/src/commands.rs:934` (`parse_params`). Aucun autre
`crate::telegram::` hors de `telegram/`.

Ce que `telegram/` cite du daemon : 31 modules par `crate::…` (agent, rpc, runtime,
workflow, session_ops…) plus `crate::VERSION`. Tout est déjà public sauf cinq fonctions
(`agent::{server_of, without_intention, arg_pattern, arg_patterns}`,
`executor::wants_network`) et les doubles MCP `mcp::testing` (`#[cfg(test)]`), utilisés
par quatre fichiers de tests et `ticket_to_deploy_e2e`.

## 1. Ce qui est livré

| Commit | Nature |
|---|---|
| `cf37ca8`, `c38d1c2` | visibilités : cinq `pub(crate)` deviennent `pub` ; `mcp::testing` offert derrière la feature `test-util` du daemon |
| `c94e1ab` | port `penelope_app::gateway::Gateway` (`name`, `start`) ; `Daemon::run(gateway: Option<Arc<dyn Gateway>>)` ; `impl Gateway for TelegramGateway` ; composition dans la CLI |
| `7993fc3` | déplacement seul : `telegram/` (29 modules, 10 fichiers de tests), `ticket_to_deploy_e2e.rs`, `tests/telegram_e2e.rs` vers `crates/penelope-gateway-telegram` (48 fichiers, renommages à 100 % sauf la ligne `use` du filet) |
| `566cf85` | la construction (`from_config` et ses deux journaux) passe dans `penelope_gateway_telegram::compose` : `commands.rs` est dans la liste de référence et ne peut pas grossir |
| `afbb0a4` | archtest `GATEWAY_DEPENDENTS`, budget, matrice CA |
| `a440d86` | test `a_gateway_is_announced_before_mcp_and_started_after_it` |

La crate : `src/lib.rs` (52 lignes) réimporte les modules du daemon sous leur ancien
chemin (`pub(crate) use penelope_daemon::{agent, rpc, runtime, …}`), de sorte que
`crate::rpc::Rpc::new` et les 250 autres citations du code déplacé résolvent sans toucher
aux corps ; T30 réécrira ces chemins. Elle expose `TelegramGateway`, `parse_params` et
`compose`. Le daemon n'a plus de module `telegram`.

## 2. Les choix

- **Le port `Gateway` est dans `penelope-app`**, module `gateway` : il ne nomme que des
  types de la plateforme (`Arc`, `JoinHandle`), comme `ChannelDelivery`, `Messenger` et
  `OwnerChannel` qui y sont déjà ; une passerelle future qui ne dépendrait que de
  `penelope-app` pourra l'implémenter. Module à part plutôt qu'en fin de `ports.rs` :
  d'autres lots de la vague touchent `ports.rs`. `Gateway::name` garde le journal
  « Telegram non démarré » sans que le daemon écrive le nom du canal.
- **Ordre de démarrage (issue #12)**. Avant : `from_config` (sans réseau), `expect_owner`
  si configuré, superviseur MCP, puis `start`. Après : la CLI appelle `compose` (même
  `from_config`) juste avant `run` ; `run` appelle `expect_owner` si une passerelle est
  reçue, crée le superviseur MCP, puis `start`. Même ordre relatif ; seule différence,
  `from_config` tourne avant la prise de la socket et la reprise au lieu d'après : il ne
  fait que lire la configuration et le secret du jeton.
- **Preuve de l'ordre** : le test cité par la spécification,
  `an_uncertain_effect_is_pushed_then_decided_from_telegram`, ne passe pas par `run` ; il
  est vert mais ne prouve rien sur l'ordre. D'où `tests/gateway_start.rs` (daemon) : une
  passerelle factice démarrée par `run` voit le propriétaire annoncé et le superviseur MCP
  créé, puis arrête le daemon (16 s, l'arrêt attend les boucles).
- **`ticket_to_deploy_e2e` suit la passerelle** au lieu de rester au daemon (§3.2) : un
  module `#[cfg(test)]` du daemon ne peut pas utiliser une crate qui dépend du daemon.
  Déplacé tel quel, module de test de la nouvelle crate.
- **Doubles MCP** : `#[cfg(any(test, feature = "test-util"))] pub mod testing;` dans
  `mcp/mod.rs`, la passerelle active `test-util` en dev-dependency. Signalé à `h-mcp-host`
  qui sort `mcp/` : sa crate devra garder `testing` public derrière une feature, et la
  dev-dependency de la passerelle la viser.
- **Archtest** : `GATEWAY_DEPENDENTS = ["penelope-cli"]`, test
  `only_the_cli_depends_on_the_gateway` (la CLI doit la déclarer, aucune autre crate en
  `[dependencies]`). La frontière canal/cœur n'exclut plus `telegram/` du daemon
  (`is_gateway_file` retiré) : un module qui y reviendrait serait mesuré, le test du
  détecteur le vérifie. `DAEMON_DEPENDENTS` (même règle R2) n'est pas posé : hors brief.

## 3. Mesures

| Mesure | Avant (5669220) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 82 637 (plafond) | 64 364 |
| `penelope-gateway-telegram/src`, lignes | | 18 335 |
| `[daemon].modules` | `telegram`, `ticket_to_deploy_e2e` | retirés |
| `[daemon.daemon_users]` | `telegram/mod.rs` 4, `telegram/media.rs` 2 | retirés |
| `[channel.allowed]` | `supervisor.rs` 23, `lib.rs` 1 | 13, retiré |
| `[files.oversized]` `commands.rs` | 2 191 | 2 190 |

## 4. Pour l'intégrateur

- `[crates] penelope-daemon` : `UPDATE_BUDGET` ne le réécrit pas ; mesure de cette
  branche 64 364 (à recomposer avec les autres lots de la vague).
- `[files].test_modules` garde `crates/penelope-daemon/src/ticket_to_deploy_e2e.rs`, que
  `UPDATE_BUDGET` ne retire pas : le fichier est désormais
  `crates/penelope-gateway-telegram/src/ticket_to_deploy_e2e.rs` (644 lignes, sous le
  plafond des sources) ; l'entrée peut simplement disparaître.
- `scripts/bump.sh` : une ligne `version =` de plus dans `Cargo.toml` (la crate).
- Conflits attendus : `supervisor.rs` (`run`, lignes voisines de la construction MCP que
  `h-mcp-host` déplace), `Cargo.toml` et `Cargo.lock` du workspace, `budget.toml`.
- `crates/penelope-evals/tests/docs.rs:456` ne relit que `penelope-daemon/src` pour les
  clés de `docs/context.md` : vert aujourd'hui, à étendre aux crates extraites (T30).
- Commentaire périmé hors périmètre : `mcp/tests.rs:754` cite
  `telegram::tests::mcp_elicitation_is_answered_from_telegram`.

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
propres. `cargo test --workspace --no-fail-fast` sur macOS (tests `cfg(target_os =
"macos")` compilés) : 64 suites, 1 978 tests, 0 échec, dont `penelope-archtest`,
`telegram_e2e`, `approval_e2e`, `ca_matrix`, `docs`, `ticket_to_deploy_e2e` et
`an_uncertain_effect_is_pushed_then_decided_from_telegram`. Les filets Telegram du lot B
sont verts sans régénération d'attendu.
`a_burst_asks_before_answering_and_can_be_ingested` a échoué une fois sur machine
chargée (attente de 200 ms, déjà noté par le lot G) et passe en trois relances.

## 6. Notes de version (pour docs/progress.md)

#### Passerelle Telegram en crate `penelope-gateway-telegram` (T29)

- La passerelle Telegram (29 modules, 91 tests, le filet `telegram_e2e` et
  `ticket_to_deploy_e2e`) quitte `penelope-daemon` pour une crate au-dessus de lui ; le
  daemon passe de 82 637 à 64 364 lignes et n'a plus de module `telegram`.
- Le daemon ne la connaît que par ses ports : nouveau port `Gateway`
  (`penelope-app`), `Daemon::run(gateway)` au lieu d'une construction dans le
  superviseur. `penelope-cli` la compose ; archtest refuse toute autre crate qui en
  dépendrait (`GATEWAY_DEPENDENTS`).
- L'ordre de démarrage de l'issue #12 est conservé (propriétaire annoncé avant les
  serveurs MCP, passerelle démarrée après eux) et désormais testé.
- Aucun comportement visible ne change : commandes, cartes, journaux identiques.
