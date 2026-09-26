# T33 : le moteur hors de `Daemon` (épopée #208, clôture avant bascule)

Agent `t33-moteur`, branche `v1-t33-moteur`, base 1.0.0-alpha.17 (`9037fda`).
Cible : critère 2 de `scripts/switch-check.sh`, `[files.oversized]` vide et
`[daemon.daemon_users]` réduit aux fichiers de `[daemon].impl_daemon`
(`design/v1/decoupage-daemon.md` §6, T33).

## Livré

- **Trois ports du moteur** dans `penelope_app::engine` : `TurnIntake` (mise en file d'un
  message, d'un message avec photos, d'une relance, d'une reprise ; session de chat d'un
  canal), `SessionModels` (alias épinglé, épinglage, état du modèle d'une session),
  `Transcriber` (vocal par le rôle `stt`, description d'images par `image_describe`).
- **`Core`** (`runtime.rs`) : l'état partagé que tenait `Daemon` (services, bus, `Handle`,
  branchements, providers, compactions, runs, embeddings, boucles surveillées) et les
  méthodes qui ne font que le lire (`status`, `embedder`, `dream`, `supervision`,
  `provider_for`, `publish_config`…). Il implémente les trois ports et `Admin`.
  `Daemon` n'est plus que `{ core: Arc<Core> }` avec ses blocs `impl Daemon` (reprise,
  boucles, `run_turn`, livraison) et se lit comme son cœur (`Deref`).
- **engine.rs découpé** : 1 065 → 485 lignes. `engine/intake.rs` (101, `TurnIntake`),
  `engine/models.rs` (290, `SessionModels`, choix du modèle, frontières, classifieur),
  `engine/media.rs` (156, `Transcriber`, message d'un tour avec photos),
  `engine/admin.rs` (66, `Admin`). `execute_turn` passe sous le seuil de clippy
  (`turn_images`, `record_messages`, `origin_turn_of`, `Core::tool_executor`,
  `Core::turn_tools`) : son `allow(too_many_lines)` disparaît.
- **Consommateurs** : `Rpc` et la passerelle Telegram (`TelegramGateway::daemon`) tiennent
  `Arc<Core>` et appellent le moteur par les ports ; `workflow::context_of`,
  `compaction::context_of`, `tool_jobs::deliver_*` prennent `Core` ;
  `runtime_events::serve` et `cache_audit::stable_prefix` prennent `Services`. `lib.rs`
  ne réexporte plus `Daemon` : la composition le nomme `runtime::Daemon` (CLI,
  `penelope_gateway_telegram::compose`, tests).
- **Budget** (`UPDATE_BUDGET=1`) : `[files.oversized]` vide ; `[daemon.daemon_users]` =
  supervisor 10, runner 8, runtime 3, engine 2 (plus aucun autre fichier) ;
  `[lints].allow_too_many_lines` 20 → 19. `switch-check` : les deux lignes du critère 2
  qui concernent ce lot sont « ok ».

Commits, dans l'ordre : ports et cœur (signatures), `photo_message` en `pub(super)`
(visibilité seule), déplacement de admin, médias et modèles, extraction de la session d'un
canal en fonction (`chat_session`), déplacement de `TurnIntake`, découpage de
`execute_turn`, budget, documentation. Les deux commits de déplacement ont été vérifiés
ligne à ligne (lignes retirées = lignes ajoutées, hors en-têtes, `mod` et `impl Core {`).

## Choix

- **Un cœur plutôt qu'un renommage.** Les surfaces avaient besoin de l'état du daemon
  (services, bus, handle, branchements, providers) autant que de ses méthodes : leur
  passer une dizaine de champs un par un aurait recopié la structure dans `Rpc`, la
  passerelle et chaque contexte. `Core` sépare ce que les surfaces voient (état et ports)
  de ce qui fait tourner le processus (`Daemon`, quatre fichiers). Les méthodes du moteur
  passent par les traits : un appel de la passerelle ne dépend que de `TurnIntake`,
  `SessionModels` ou `Transcriber`.
- **`Deref` de `Daemon` vers `Core`** : les blocs `impl Daemon` et les tests lisent
  `d.services`, `d.bus` sans réécriture. `Daemon` ne porte aucun champ propre, la
  délégation est totale ; `Daemon { core }` se reconstruit d'un cœur (aide `daemon_of` des
  tests de la passerelle, qui fait tourner `runner::process`).
- **Nom des champs gardé** : `Rpc::daemon` et `TelegramGateway::daemon` sont typés
  `Arc<Core>` mais gardent leur nom, pour ne pas réécrire une centaine de lignes de la
  passerelle en parallèle des autres lots.
- **`chat_session` reste dans engine.rs** : la liaison d'une session à un chat nomme
  Telegram (`Origin::Telegram`, `bind_telegram`), et la frontière canal/cœur
  (`[channel.allowed]`) ne l'admet que dans engine.rs ; déplacer ces 9 mentions dans
  `engine/intake.rs` aurait ajouté une entrée au budget. `TurnIntake::chat_session_for`
  y délègue ; T37 rendra la liaison générique.
- Hors périmètre annoncé mais obligé : `penelope-evals` (imports `runtime::Daemon`, traits
  en portée, `Rpc::new(d.core.clone())` dans `rpc_golden`), une ligne ou deux par fichier.

## Vérifications

Scénarios (`penelope-evals --test scenarios`, 23 verts sans régénération), `rpc_golden`,
filets Telegram (`penelope-gateway-telegram` : e2e, `ticket_to_deploy_e2e`), suites du
daemon, de la CLI et de l'app verts pendant le lot ; en fin de lot `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings` et `cargo test
--workspace` (résultat dans le rapport).

## Reste et blocages

- **Plafond `[crates]` du daemon rouge** : 14 829 lignes pour 14 719. Le découpage ajoute
  les en-têtes des quatre fichiers du moteur, `Core` et son `Deref`, les impl de ports et
  les imports de traits dans les tests. `UPDATE_BUDGET` ne touche pas cette table : à
  poser par l'intégration (consigne commune).
- La passerelle ne se teste pas encore sans daemon : elle consomme les ports sur un
  `Arc<Core>` concret, que ses tests construisent par `Daemon::from_services`. Des doubles
  des trois ports demanderaient que `TelegramGateway` tienne des `Arc<dyn …>` à côté de
  l'état partagé : une étape de plus, non nécessaire à la bascule.
- `supervisor.rs` (938 lignes) reste le plus gros bloc `impl Daemon`.

## Notes de version

#### Moteur : les surfaces tiennent le cœur du daemon et passent par trois ports

Le moteur de tours expose trois ports dans `penelope_app::engine` : `TurnIntake`,
`SessionModels` et `Transcriber` (T33). Le daemon se scinde en `Core`, l'état partagé et
les ports, et `Daemon`, le processus (reprise, boucles, tours). Le RPC, la passerelle
Telegram, les contextes de l'orchestrateur et de la compaction et la livraison des jobs
d'outils tiennent le cœur ; plus aucun module du daemon hors de ses quatre fichiers de
composition ne nomme `Daemon`. `engine.rs` passe de 1 065 à 485 lignes
(`engine/intake.rs`, `models.rs`, `media.rs`, `admin.rs`) et sort de la liste de
référence du gel, qui est vide ; `execute_turn` perd son `allow(too_many_lines)`. Aucun
comportement ne change.
