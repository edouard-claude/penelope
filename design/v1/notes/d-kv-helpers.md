# Lot D, décrocher `Daemon` : kv et helpers (T05, T06)

Branche `v1-d-kv-helpers`, dérivée de `v1` au commit 0f1c41a. Épopée #208. Spécification :
`design/v1/decoupage-daemon.md` §1.2 (cycles), §2.2 (port `kv` sur `Services`), §6 (T05,
T06), §8 (risque « clés kv »).

## 1. Ce qui est livré

### T05 : une seule famille `kv`

`Services::kv_get`, `kv_set`, `kv_delete` (`runtime.rs`) portent la requête ; les deux
familles qui la dupliquaient ont disparu : `Daemon::kv_*` (`engine.rs`) et
`workflow::kv_get` / `kv_set` (`workflow.rs`). 249 appels réécrits mécaniquement (198 sur
`Daemon`, repérés par le compilateur ; 51 sur `workflow::`). Clés et SQL identiques,
octet pour octet. `workflow::kv_delete_prefix` (privé, `LIKE`) et les
`penelope_store::kv_get` / `kv_set` utilisés dans une transaction restent tels quels : ce
sont des primitives transactionnelles, pas une troisième famille.

Critère de la spécification, vide hors tests :
`grep -rnE 'workflow::kv_|[^.]\bd\.kv_|daemon\.kv_|self\.kv_' crates/penelope-daemon/src`.

### T06 : module `helpers`

`crates/penelope-daemon/src/helpers.rs` (192 lignes) reçoit, corps inchangés :
`vault_dir`, `local_now` (conversation), `owner_origin_of` (scheduler), `round_usd`,
`set_config_path` (rpc), `shown`, `deep_link`, `topic_name_key`, `chat_title_key`,
`seen_chats`, `BOT_USERNAME_KEY`, `SEEN_CHATS_KEY` (telegram), `running_binary`,
`is_source_build` (upgrade), `last_model_key` (engine), `step_done_key` (workflow).

Préalable, dans un commit séparé du déplacement : `deep_link` et `set_config_path`
prennent `&Services` ; le corps de `Daemon::publish_config` descend sur
`Services::publish_config` (le daemon délègue, ses appelants de test ne bougent pas) ;
`scheduler::owner_origin(d)`, simple relais, est supprimé et ses onze appelants passent
`&d.services` à `owner_origin_of`.

Critères, vides hors tests : `crate::telegram::` hors `telegram*` et `supervisor.rs`
(reste `ticket_to_deploy_e2e.rs`, module de test) ; `crate::rpc::` dans `selfknow.rs`,
`engine.rs`, `dream.rs` ; `crate::engine::` dans `compaction.rs`. Cycles de §1.2 tombés :
dream → telegram, compaction → engine, selfknow → rpc, doctor / scheduler /
session_project / conversation → telegram. selfknow → upgrade passe aussi par helpers.

## 2. Mesures

| Mesure (budget.toml) | Avant (0f1c41a) | Après |
|---|---|---|
| `[daemon.daemon_users]`, somme | 261 | 254 |
| dont compaction / scheduler / rpc / telegram | 21 / 26 / 9 / 7 | 17 / 25 / 8 / 6 |
| `[channel.allowed]`, fichiers du daemon hors helpers | doctor 44, scheduler 39, dream 3, session_project 3 | 42, 33, 0 (sortie), 2 |
| `helpers.rs` dans `[channel.allowed]` | | 12 |
| `[files.oversized]` | telegram 7 967, rpc 2 518, workflow 2 915, upgrade 2 034, scheduler 1 833, engine 1 221, conversation 1 222 | 7 925, 2 456, 2 881, 2 018, 1 818, 1 179, 1 200 |

La baisse de `Daemon` est modeste : T05/T06 retirent des appels, pas des signatures.
Seules les fonctions devenues `&Services` la font descendre (quatre fonctions kv privées
de compaction, `deep_link`, `set_config_path`, `owner_origin`). Le gros du décrochage
reste T07.

## 3. Choix

- `kv_*` vit dans `runtime.rs`, à côté de `Services`, pas dans `helpers` : c'est une
  méthode du type, elle suivra `Services` dans `penelope-app` (T21).
- Dérogation au gel, trailer `Dérogation-budget: #208` sur le commit du déplacement :
  `helpers` entre dans `[daemon].modules` (le seul module nouveau, prévu par T06), et
  `helpers.rs` entre dans `[channel.allowed]` avec 12 mentions. Ces mentions ne sont pas
  nouvelles : ce sont les clés `tg.*`, `Origin::Telegram` et `telegram_user_id` qui
  étaient dans `telegram.rs` (non mesuré, passerelle) et `scheduler.rs` ; doctor,
  scheduler, dream et session_project en perdent autant (12). T36 les rendra génériques.
- Réexports de transition, à retirer en T30 : `conversation` garde
  `pub use crate::helpers::{local_now, vault_dir}` (`penelope-evals` cite
  `conversation::vault_dir`) ; `upgrade` garde `pub use crate::helpers::{is_source_build,
  running_binary}` (`penelope-cli`, `commands.rs:1240`).
- La migration ajoutait neuf caractères par appel ; rustfmt étalait les chaînes et sept
  fichiers de `[files.oversized]` dépassaient leur borne (telegram.rs de 51 lignes). Aucun
  budget relevé : liaison `let s = &….services` dans les fonctions qui appellent plusieurs
  fois le kv, liaison existante réutilisée, quatre fonctions kv privées de compaction en
  `&Services`, trois lectures par une variable. Commit à part, sans changement de
  comportement.
- Hors périmètre touché, une ligne : `crates/penelope-daemon/tests/telegram_e2e.rs:223`,
  `life.d.kv_set` devient `life.d.services.kv_set` (appel seul, attendu intact). Signalé
  au chef de lot avant de le faire, sans réponse.
- `agent.rs` et `agent/` n'appellent ni kv ni helper : rien à y laisser.
- Commentaire hérité : la doc `/// Formulaire d'étape user en cours dans un chat.` était
  posée sur `BOT_USERNAME_KEY` dans `telegram.rs` ; déplacement pur, elle a suivi. À
  corriger quand `helpers` sera relu.

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` : verts (56 suites, 0 échec, dont `scenarios`, `rpc_golden`,
`docs`, `telegram_e2e`, `approval_e2e`, `penelope-archtest`), sur macOS (les tests
`cfg(target_os = "macos")` sont compilés). Aucun attendu de test modifié.

## 5. Notes de version (pour docs/progress.md)

#### Une seule famille kv, et les helpers partagés sortent de leurs modules (T05, T06)

- `Services::kv_get`, `kv_set`, `kv_delete` remplacent `Daemon::kv_*` et
  `workflow::kv_get` / `kv_set`, qui dupliquaient la même requête ; 249 appels migrés,
  aucune clé renommée.
- Nouveau module `helpers` du daemon : clés kv du canal, `deep_link`, `seen_chats`,
  `vault_dir`, `local_now`, `owner_origin_of`, `round_usd`, `set_config_path`,
  `running_binary`, `is_source_build`, `last_model_key`, `step_done_key`. Six cycles de
  modules tombent (dream, compaction, selfknow, doctor, scheduler, session_project ne
  citent plus telegram, engine ni rpc).
- `deep_link`, `set_config_path` et l'origine du propriétaire prennent `&Services` ;
  `Services::publish_config` porte la publication de configuration.
- Gel : `helpers` est le seul module ajouté (dérogation #208) ; occurrences de `Daemon`
  261 → 254 ; telegram.rs, rpc.rs, workflow.rs, engine.rs, scheduler.rs, upgrade.rs et
  conversation.rs abaissés dans la liste de référence.

## 6. Blocages

Aucun. Trois commits intermédiaires ont été poussés avec `penelope-archtest` rouge
(ff226f1, e144592, e3eb43b : fichiers au-dessus de leur borne, vus après coup) ; 41cb3dd
les remet sous les bornes, mais lui et 340e1b2 laissent un `needless_borrow` que clippy
refuse, corrigé par d8763d2. Seule la pointe de branche est verte partout : pour
bisecter, ne retenir que 97936ba et d8763d2.
