# Lot H (fin), crate `penelope-ops` (T28)

Branche `v1-h-ops-fin`, dérivée de `v1` au commit 8f701ff (1.0.0-alpha.11). Épopée #208.
Spécification : `design/v1/decoupage-daemon.md` §6 (T28). Suite de `h-ops.md`, qui avait
laissé au daemon `purge` et `session_ops` et relevé deux dettes : `check_endpoint` en
double, et l'ordre de la sortie de `penelope doctor`.

## 1. Ce qui est livré

| Commit | Ce qui change |
|---|---|
| `check_endpoint` dans `penelope_app::helpers` | une seule règle HTTPS (boucle locale exacte, pas d'identifiants) ; l'hôte MCP la réexporte (`pub use`), ses appels et son test ne bougent pas ; `upgrade::fetch` l'appelle ; `penelope-ops` perd sa dépendance à `url` |
| Doctor, rang des contrôles du daemon | `doctor::run_with(s, daemon)` place stabilité du prompt, historique et jobs d'outils après la rétention ; `rpc/methods/doctor.rs::run` compose `run_with` puis les serveurs MCP ; la méthode RPC l'appelle |
| Purge et `session_ops` sans `Daemon` | signatures déjà sur `&Services` ; les tests ouvrent des `Services` (session de conversation créée par le test, `Bus` neuf, `preview` appelé directement, `history verify` lu par `Services.context`) ; titre daté des forks par `penelope_app::helpers::session_label` |
| Déplacement | `purge.rs`, `purge/tests.rs`, `session_ops.rs` passent dans `penelope-ops` par `git mv`, corps inchangés (seuls les `use` de `Services`) ; réexports de transition dans `lib.rs` du daemon |

Ordre de `penelope doctor` : le test `doctor_keeps_the_order_of_alpha_7`
(`crates/penelope-daemon/src/rpc/methods/doctor/tests.rs`) compare les identifiants, de
`owner` au retour OAuth MCP, à ceux relevés sur la 1.0.0-alpha.7 (c5ecc92) :
`retention`, `prompt.stability`, `history.journal`, `tool_jobs`, `budget.day` se suivent
de nouveau, et `mcp.oauth.redirect` vient après le réseau, comme avant. Les contrôles de
l'OS, qui précèdent `owner` et varient d'une machine à l'autre, ne sont pas listés.

Tests de purge et de rétention : les dix tests de `purge/tests.rs` et les quatre de
`session_ops` passent dans la crate, dont `word_is_gone` (tables du transcript, `events.payload`, index plein texte),
`history verify` après purge et refonte, et `audit verify` (`events.verify`).

## 2. Mesures

| Mesure | Avant (8f701ff) | Après |
|---|---|---|
| `penelope-daemon/src`, lignes | 22 863 (plafond `[crates]`) | 20 831 |
| `penelope-ops/src`, lignes | 8 681 | 10 762 |
| `[daemon].modules` | `purge`, `session_ops` | sortis |
| `[channel.allowed]` | `penelope-daemon/src/purge.rs` 17, `session_ops.rs` 1 | mêmes valeurs sous `penelope-ops/src/` |

## 3. Choix

- Contrôles de doctor : la méthode RPC les insère à leur rang (deuxième voie de la
  consigne) plutôt que des ports. Les trois contrôles lisent `tool_jobs`,
  `prompt_snapshot` et `history`, modules du daemon ; un port par contrôle aurait élargi
  `penelope-app` pour trois lignes de sortie. `run_with` reçoit leurs résultats, pas
  une fonction : ils sont calculés avant les autres, ce qui ne change que l'ordre des
  lectures, pas celui de la sortie. `run(s)` reste pour les tests et les appelants
  sans daemon.
- `mcp_checks` reste au daemon (il lit `penelope_mcp_host::stdio_profile`) ; il était
  déjà ajouté après `run` à la 1.0.0-alpha.7, rien à déplacer.
- Titre des forks dans la lecture préalable de purge : `titles::label` vit dans
  `penelope-conversation`, qui n'est ni socle ni métier ; plutôt qu'une dépendance
  `ops → conversation` (hors §3.2), la forme est servie par
  `penelope_app::helpers::session_label`.
- Test de la lecture préalable : il passait par la méthode RPC ; il appelle `preview`
  directement (la méthode n'en est que le passage, sa forme est tenue par
  `rpc_golden`).
- `penelope-context` en dev-dependency de `penelope-ops`, comme l'hôte MCP : les tests
  de rétention écrivent des `conv.attempt`. La règle de dépendance ne lit que
  `[dependencies]`.

## 4. Reste et blocages

- `titles::label` (`penelope-conversation`) et `helpers::session_label` ont le même
  corps : `label` devrait déléguer à `session_label`. Non fait ici : la crate de
  conversation est modifiée par `k-steering`.
- Plafond `[crates]` du daemon : 22 863 dans `budget.toml`, 20 831 mesurées ;
  `UPDATE_BUDGET` ne le réécrit pas, à abaisser par l'intégrateur.
- La passerelle (`penelope_daemon::purge::preview`, `crate::session_ops::*`), la RPC, le
  superviseur et les évaluations passent encore par les réexports : T30.
- Aucun `ca_*` dans les fichiers déplacés : `docs/ca-matrix.md` inchangé.

## 5. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : 78 suites, 2 015 tests passés, 0 échec
(dont `penelope-archtest`, `docs`, `ca_matrix`, `rpc_golden`), sur macOS : les tests
`cfg(target_os = "macos")` sont compilés. `UPDATE_BUDGET=1` passé ;
`scripts/check-budget.sh 8f701ff` : rien ne remonte, aucune dérogation.

## 6. Notes de version (pour docs/progress.md)

#### `penelope-ops` complète : purge et sessions, ordre de `doctor` rétabli (T28)

- `purge` (purge d'une session, rétention, caviardage de la file sortante) et
  `session_ops` (fork, retour arrière, export, reconstruction) quittent le daemon pour
  `penelope-ops` ; le daemon les réexporte sous leurs anciens chemins. Leurs tests
  n'ouvrent plus de daemon.
- `penelope doctor` retrouve l'ordre de la 1.0.0-alpha.7 : stabilité du prompt,
  historique et jobs d'outils juste après la rétention, et non plus en fin de liste.
  Un test compare les identifiants.
- La règle HTTPS des adresses (OAuth MCP, téléchargement des releases) n'a plus qu'une
  copie, dans `penelope_app::helpers::check_endpoint`.
- `penelope-daemon` passe de 22 863 à 20 831 lignes.
