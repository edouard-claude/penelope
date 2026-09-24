# Notes de livraison : lot G-telegram (passerelle Telegram en modules, épopée #208)

Branche `v1-g-telegram`, dérivée de `v1` au commit 5c0faf3. Spécification :
`design/v1/decoupage-daemon.md` §5.1, sur le modèle du découpage d'`agent.rs` (lot F,
commit 60ea09b).

## Ce qui est livré

`telegram.rs` (7 925 lignes) et `telegram/screens.rs` (2 437) deviennent
`telegram/` : 29 fichiers de code, le plus gros à 614 lignes (`approvals.rs`), aucun
au-dessus de 800. Les deux fichiers sortent de la liste de référence du gel.

| Module | Contenu |
|---|---|
| `telegram/mod.rs` (589) | struct `TelegramGateway`, `from_config`, `with_transport`, `register`, `start`, scrutation, `process_update`, `handle` |
| `cards.rs` | carte d'approbation, statut Codex, planifications, MCP, routage, `render_value` |
| `keys.rs` | clés kv, `parse_params`, modèles (`short_model`…), journaux, chats vus |
| `forms.rs`, `onboarding.rs`, `elicitation.rs` | formulaires, accueil, élicitations MCP et `impl OwnerChannel` |
| `drafts.rs`, `bursts.rs`, `media.rs` | activité et brouillons, `StopReport` ; rafales et messages retenus ; photos, documents, voix |
| `menus.rs`, `usage.rs`, `sessions_menu.rs` | menus de modèle et de suites ; `/usage` et `/budget` ; menu des sessions |
| `callbacks.rs`, `approvals.rs` | aiguillage des clics, décisions ; cartes d'approbation, d'effet, de lancement, OAuth, mémoire |
| `outbox.rs`, `delivery.rs` | `reply`, file `tg_outbox` ; `impl ChannelDelivery` et `impl Messenger` |
| `commands/mod.rs` | l'aiguillage `command` et `run_by_conversation` |
| `commands/{session,project,models,ops,memory,workflows}.rs` | une fonction par commande (`cmd_new`, `cmd_stop`…) |
| `screens/mod.rs` (550) | `Screen`, `Done`, navigation, `show_screen`, `screen_clicked`, l'aiguillage `build_screen` |
| `screens/{sessions,workflows,memory,ops}.rs`, `screens/perform.rs` | une fonction par écran (`screen_runs`…) ; les opérations |

Commits, chacun vert (tests de la passerelle, clippy, archtest) :

1. `0596b06` dérogation : plafond du daemon relevé provisoirement, entrée Daemon de `telegram/media.rs` ;
2. `f7f6047` déplacement pur de tout sauf le cœur et `command` en seize modules ;
3. `eeadee0` `command` en aiguillage, un bras par fonction ;
4. `ee014cb` le test `no_bubble_ever_shows_null` relit toute la passerelle (voir plus bas) ;
5. `0479fcd` dérogation : entrée Daemon `telegram.rs` -> `telegram/mod.rs` ;
6. `a36d085` renommage `telegram.rs` -> `telegram/mod.rs` ;
7. `6ab75a2` `perform` sort dans `screens/perform.rs` ;
8. `a419b28` `build_screen` en aiguillage, un écran par fonction ;
9. `dd29689` renommage `screens.rs` -> `screens/mod.rs` ;
10. `8c8d0a3` plafond du daemon rabattu à la mesure.

## Les choix

- **Déplacement pur** pour les commits 2, 6, 7 et 9 : vérifié par comparaison des
  multi-ensembles de lignes retirées et ajoutées, aux blancs près ; il ne reste que les
  en-têtes de modules, les `impl TelegramGateway {`, les `pub(super)` et les recollements
  de rustfmt. Les modules enfants font `use super::*;` (comme `agent/`) ; `mod.rs`
  réimporte les fonctions libres déplacées, avec leur visibilité d'origine pour
  `approval_card`, `StopReport` (`pub(crate)`), `render_value`, `parse_params` et
  `record_seen_chat` (`pub`) : aucun chemin `crate::telegram::*` ni
  `penelope_daemon::telegram::*` ne change. Cinq réimports ne servent qu'aux tests et sont
  sous `#[cfg(test)]`.
- **Visibilités** : `pub(super)` seulement là où le compilateur l'exige (vérifié en les
  retirant toutes puis en relisant les erreurs) : 43 méthodes appelées d'un module frère ou
  des tests, les fonctions libres partagées, les champs de `TextBurst`. `command` passe de
  privée à `pub(super)` (appelée par `handle` et les écrans), `perform` aussi.
- **`command` et `build_screen`** : chaque bras devient une fonction de même signature
  (les paramètres inutilisés préfixés `_`) ; l'aiguillage ne fait plus que `match`. Le
  prélude de la fonction d'origine (`d`, `s`, `origin`, `rpc`, `reply_to`, `here`) est
  repris variable par variable, seulement là où le corps s'en sert : `Rpc::new` et
  `Origin::Telegram` ne sont que des constructions sans effet. Un bras qui finissait par
  une valeur la rend par la même fin (`self.reply(…)` ou `Ok(screen)`) ; un bras qui
  rendait toujours la main par `return` garde son corps, le `return Ok(())` final devenu
  `Ok(())` pour clippy (quatre bras). `command` rogne `args` avant d'aiguiller, comme avant,
  et garde le garde `"rewind" if args.is_empty()` (vers `cmd_rewind_screen`). Les
  commentaires qui précédaient un bras deviennent la doc de sa fonction.
- **Écart au §5.1** : les menus de modèle (`send_choices`, `send_model_menu`,
  `model_menu`, `workflow_choice_clicked`, `model_pin_clicked`) vont dans `menus.rs`, pas
  dans `commands/models.rs` : appelés par `callbacks.rs` et `bursts.rs`, ils auraient dû
  être `pub(in crate::telegram)` depuis un petit-enfant. Les commandes forment six familles
  au lieu de trois, pour tenir chaque fichier sous 800 lignes. `perform` garde son allow
  de too_many_lines (458 lignes, hors brief).
- **Corps réimportés** : `perform` et trois écrans appellent `super::shown`,
  `super::cancelled_note`, `super::context_line`, `super::mcp_state_icon`,
  `super::recent_log_lines` ; `screens/mod.rs` réimporte ces cinq noms pour que ces appels
  résolvent sans toucher aux corps.
- **Un test ajusté** : `no_bubble_ever_shows_null` (#115) lisait `src/telegram.rs` et
  `src/telegram/screens.rs` en dur. Depuis le commit 2 il ne voyait plus le code déplacé,
  et le renommage l'aurait fait paniquer. Il parcourt maintenant `telegram.rs` s'il existe
  et tout `telegram/` hors `tests/`, et exige plus de vingt fichiers ; l'assertion ne
  change pas. Aucun autre test n'est touché, aucun `ca_*` ne change de chemin.

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace` (57 suites, dont `scenarios`, `telegram_e2e`, `approval_e2e`,
`rpc_golden`, `docs`) : propres et verts sur `8c8d0a3`, sans modifier un attendu. Les 93
tests de la passerelle passent à chaque commit. Un échec isolé de
`a_burst_asks_before_answering_and_can_be_ingested` (attente de 200 ms sur une machine
chargée par les autres lots) ne s'est pas reproduit en trois relances ciblées.

## Gel

`UPDATE_BUDGET=1` retire `telegram.rs` et `telegram/screens.rs` de la liste de référence
et abaisse `allow_too_many_lines` de 26 à 24. Deux commits séparés portent
« Dérogation-budget: #208 » :

- `[daemon.daemon_users]` : les deux mentions du Daemon d'`enqueue_photos` et
  `store_attachment` suivent leur fonction dans `telegram/media.rs` (2) ; les quatre qui
  restent suivent le renommage en `telegram/mod.rs` (4). `telegram.rs` (6) disparaît.
- `[crates] penelope-daemon` : le découpage coûte 1 384 lignes (en-têtes, 77 signatures de
  fonctions de bras, préludes repris, aiguillages que rustfmt écrit sur quatre lignes). Le
  plafond, posé sans marge à 82 213, est relevé provisoirement puis rabattu à la mesure
  exacte, 83 597, au dernier commit. Il redescend avec l'extraction de la passerelle en
  crate (T29).

À l'intégration avec les autres lots, ce plafond s'additionne : c'est la mesure de
l'arbre fusionné qui fait foi.

## Notes de version

#### Passerelle Telegram en modules (épopée #208, lot G)

- `telegram.rs` (7 925 lignes) et `telegram/screens.rs` (2 437) deviennent `telegram/` :
  vingt-neuf modules, le plus gros à 614 lignes. Les deux fichiers sortent de la liste de
  référence du gel ; tous les chemins `crate::telegram::*` sont inchangés.
- Les 50 commandes `/…` et les 32 écrans ne sont plus deux `match` géants : une fonction
  par commande (`telegram/commands/`) et par écran (`telegram/screens/`), derrière un
  aiguillage court. Deux `#[allow(clippy::too_many_lines)]` disparaissent.
- Aucun comportement visible ne change : textes, boutons, cartes et ordre des envois sont
  ceux de la 1.0.0-alpha.3.

## Blocages

Aucun. Reste hors brief : `handle` (235 lignes), `callback` (338) et `perform` (458)
gardent leur allow de too_many_lines.
