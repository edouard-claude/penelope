# Lot L : le cœur ne nomme plus le canal (épopée #208, T36)

Agent `l-canal`, branche `v1-l-canal`, base 1.0.0-alpha.12 (`3359b0d`).
Spécification : `design/v1/decoupage-daemon.md` §1.3 et §6 T36 ; charte
`design/v1/README.md` §3.5 (règle R8 du découpage). Préalable : T29 (passerelle au-dessus
du daemon).

## 0. Inventaire avant changement

Mesure : motif `CHANNEL_PATTERN` d'archtest sur les lignes de code (hors commentaires,
hors module de tests), crates du cœur touchées par T36 : `penelope-app`,
`penelope-conversation`, `penelope-orchestrator`, `penelope-executor`, `penelope-agent`,
`penelope-daemon`. **Baseline `[channel.allowed]` de ces six crates : 166** (sur 341 pour
toutes les crates agnostiques ; le reste est `kernel`, `store`, `ops`, `observe`, `tools`,
`vault`, `mcp-host`, hors T36).

| Fichier | Mentions | Nature | Famille T36 |
|---|---|---|---|
| `orchestrator/scheduler/origin.rs` | 30 | `place_name` (noms de chats lus en kv), `retarget` (`allowed_chats`, `Origin::Telegram`), `destination`, `target_origin` | origines |
| `daemon/supervisor.rs` | 13 | origine d'une session liée (`tg_chat_id`, T37), journal « file Telegram », `actions.purge_expired` (sans mention) | cartes, T37 |
| `executor/selfknow.rs` | 12 | commandes du canal (`penelope_telegram::commands`) ×2, résumé `[telegram]` de la configuration ×5, chemins `owner.telegram_user_id` et `telegram.` | selfknow via `Admin` |
| `app/helpers.rs` | 12 | `owner_origin_of` ×4 (T37), `deep_link` ×1, clés kv `tg.topic_name`, `tg.chat_title`, `telegram.seen_chats` | cartes (lien) |
| `daemon/runner.rs` | 11 | fusion des rafales (`Origin::Telegram`, `cfg.telegram.burst_*`), « canal Telegram indisponible », filtre `Origin::Telegram` de `deliver` | rafales, chaînes |
| `daemon/engine.rs` | 10 | `chat_session_for` (`find_by_topic`, `bind_telegram`, T37), `hooks.telegram()` | T37 |
| `app/elicitation.rs` | 10 | `Destination { chat_id, topic_id }`, `fields_from_schema`, « sur Telegram » ×5, « Telegram non configuré » | élicitation, chaînes |
| `daemon/rpc/methods/workflows.rs` | 9 | `schedule move` (`chat_id`, `topic_id`) | origines |
| `daemon/rpc/methods/approvals.rs` | 9 | `telegram.quiet_hours`, origine de session (T37) | hors T36 |
| `app/bus.rs` | 9 | `Origin::Telegram`, `telegram_chat()` | permanent jusqu'à T37 |
| `executor/.../schedules_messaging.rs` | 7 | `schedule_move` : `private` / `here` en `(chat_id, topic_id)` | origines |
| `daemon/tool_jobs.rs` | 6 | origine de session (T37) ; fichier du lot `k-jobs` | hors périmètre |
| `conversation/lib.rs` | 6 | règle de rafale de `TurnInbox` | rafales, chaînes |
| `executor/executor/mod.rs` | 5 | `elicitation_destination` (`telegram_chat()`) | élicitation |
| `daemon/runtime.rs` | 5 | `Hooks::telegram()`, `StatusReport.telegram` (T37), `tg_outbox` en SQL | cartes (hooks) |
| `app/services.rs` | 5 | `TemplateRegistry`, `ActionStore`, `templates::CATALOG`, `sample.telegram`, sous-système `telegram` | cartes |
| `orchestrator/scheduler/fire.rs`, `outcome.rs` | 1 + 1 | « Telegram non configuré », `Origin::Telegram` | chaînes, origines |
| `executor/voice.rs`, `skills_workflows.rs` | 1 + 1 | « format Telegram » (doctor), `Origin::Telegram` | chaînes |
| `app/media.rs`, `codex_scope.rs` | 1 + 1 | dossier `telegram/` du workspace, `Origin::Telegram` | chaînes |
| `agent/pipeline.rs` | 1 | `EffectKind::Telegram` (T37 ; fichier du lot `k-attempts`) | hors périmètre |

Dépendance `penelope-app → penelope-telegram` : quatre sites (`services.rs:21, 308`,
`helpers.rs:137`, `elicitation.rs:488`).

## 1. Livré

| Commit | Famille | Quoi |
|---|---|---|
| `8f22a0c` | | inventaire (§0) |
| `8dbdba9` | cartes | `TemplateRegistry` et `ActionStore` quittent `Services` pour `TelegramGateway` ; port `penelope_app::channel::Cards` (`catalog`, `template`, `deep_link`) dans `Services.channel.cards` ; `workflow_known` reçoit le canal ; cartes passées par la composition au démarrage ; `purge_expired` dans une boucle `telegram.maintenance` |
| `3c7406c` | rafales | `ChannelDelivery::burst_limits` et `BurstLimits` ; `TurnInbox` et le coureur n'appliquent que les seuils du canal ; `Hooks::telegram()` retiré |
| `c086616` | origines | `ChannelDelivery::{describe_origin, destination_for}` ; `retarget(s, id, &Origin)`, `Orchestrator::schedule_move(id, &Origin)` ; `Services.channel.delivery` partagé avec `Hooks::delivery` ; nommage et autorisation dans la passerelle |
| `04128a8` | élicitation | `Destination { session_id, origin }` ; `OwnerChannel::{check_form, place}` ; `penelope-app` sans `penelope-telegram` (règle et test d'archtest) |
| `175e976` | chaînes | `Origin::is_channel` aux quatre sites qui testaient `Origin::Telegram` ; `telegram_chat` part dans la passerelle (trait `TelegramChat`) ; commandes de `self_status` par `Cards::commands` ; `penelope-executor` sans `penelope-telegram` |
| `24cef14` | dépendances | `penelope-daemon` sans `penelope-telegram` (dépendance morte) ; deux règles d'archtest |
| (suivant) | origines | correctif : sans canal, `schedule.move` vers le propriétaire reste possible (contrat doré) |

Critères de T36 :

- `penelope-app` ne dépend plus de `penelope-telegram` :
  `the_app_crate_sees_neither_the_daemon_nor_the_channel` (daemon, `penelope-telegram`,
  passerelle) ; `APP_ALLOWED_DEPS` le retire. Même chose pour `penelope-executor` et
  `penelope-daemon` (`the_daemon_does_not_depend_on_the_channel_library`).
- Baseline R8 du cœur (six crates de §0) : **166 → 74** (÷ 2,24). Total
  `[channel.allowed]` : 341 → 249. Entrées retirées : `conversation/lib.rs`,
  `daemon/runner.rs`, `app/elicitation.rs`, `app/codex_scope.rs`, `executor/mod.rs`,
  `schedules_messaging.rs`, `skills_workflows.rs`, `scheduler/origin.rs`, `fire.rs`,
  `outcome.rs`.
- Suites : passerelle 94 (dont `bursts`, `workflows`, `elicitation`, `ops`), `telegram_e2e`,
  `ticket_to_deploy_e2e`, ordonnanceur et workflows (orchestrateur 32), `rpc_golden` et
  `scenarios` verts **sans régénération**.

Ce qui reste nommé dans le cœur (74) : liaison de session et origine `Origin::Telegram`
(T37 : `supervisor.rs` 13, `engine.rs` 9, `rpc/approvals.rs` 9, `tool_jobs.rs` 6,
`helpers.rs` 9 dont `owner_origin_of`, `bus.rs` 3, `rpc/workflows.rs` 7 pour les
paramètres `chat_id`/`topic_id` de `schedule move`) ; lecture de la configuration
`[telegram]` (`selfknow.rs` 9, `runtime.rs` 4 dont `StatusReport.telegram` et
`tg_outbox`, `services.rs` 2) ; `media.rs` (dossier `telegram/` du workspace, sur
disque) ; `voice.rs` (texte de `doctor` vu par le propriétaire, gardé) ;
`agent/pipeline.rs` (`EffectKind::Telegram`, T37, fichier du lot `k-attempts`).

## 2. Choix

- **`Services.channel`** (`Channel { cards, delivery }`) plutôt qu'un port par module :
  l'ordonnanceur, l'exécuteur et `helpers::deep_link` ne tiennent que `Services`.
  `delivery` est le même `Slot` que `Hooks::delivery` du daemon (une ligne dans
  `Daemon::from_services`) : la passerelle se branche une fois, `register()` inchangé.
- **Cartes passées au démarrage.** Les workflows de l'utilisateur sont validés à leur
  chargement, avant que la passerelle n'existe : la composition passe
  `penelope_gateway_telegram::cards` (`CardsOf`) à `Daemon::new` puis
  `Services::bootstrap`. La passerelle rebranche ses propres cartes à sa construction
  (mêmes gabarits, qu'elle garde pour dessiner les cartes) : les tests qui la
  construisent sur `Services::for_tests` les ont aussi.
- **`check_form` en plus de `show`.** La spécification déplaçait `fields_from_schema`
  dans `OwnerChannel::show` ; `show` le fait (pour dessiner), mais un refus à cet
  endroit aurait changé la réponse au serveur (annulation au lieu de −32602). La
  vérification reste avant toute carte, par `OwnerChannel::check_form`.
- **`destination_for` rend l'origine.** Le canal dit à la fois si la conversation est
  autorisée et ce qu'on range (sans le `message_id` qui l'a désignée) : le cœur n'a
  pas à connaître les champs d'une origine Telegram. Sans canal branché, seule
  `owner_origin_of` est acceptée (le contrat doré l'exige).
- **Commandes de `self_status` par `Services.channel`, pas par `Admin`.** La
  spécification disait `Admin` quand `selfknow.rs` était au daemon ; l'`Admin` du daemon
  n'aurait fait que relayer le port, et `engine.rs`, qui l'implémente, est dans la liste
  de référence (+5 lignes refusées par le gel).
- **Tests déplacés, pas affaiblis.** Les noms Telegram des planifications sont vérifiés
  dans la passerelle (`schedules_are_named_and_moved_by_the_telegram_channel`) ; le
  cœur garde son plumbing sur un faux canal ; `schedule_move_sends_a_schedule_here_or_home`
  passe du daemon à l'ordonnanceur (l'orchestrateur réel y est branché), ce qui fait
  aussi baisser le daemon.

## 3. Textes qui changent

Sur Telegram, aucun. Hors Telegram (canal absent ou arrêté) :

- « aucune conversation (canal non configuré) » au lieu de « (Telegram non configuré) »
  (nom d'une destination) ; `schedule list` sans passerelle nomme ainsi toutes les
  destinations ;
- « aucun canal pour joindre le propriétaire (canal non configuré ou arrêté) »
  (élicitation) ; « aucun canal de message : canal du propriétaire non configuré »
  (planification `notify`) ; « canal du propriétaire indisponible » (carte de rafale) ;
  « aucun canal du propriétaire n'est branché » (déplacement vers un groupe).

Au modèle : `inventory.telegram_commands` devient `inventory.channel_commands` ; l'outil
`schedule_move` répond « `here` : cette conversation n'est pas celle d'un canal » ; sans
canal, la phrase d'une élicitation dirait « sur son canal » (jamais le cas : sans canal,
personne ne répond).

## 4. Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` sur macOS (en deux passes après le
correctif, évaluations à part) : 80 suites, 2 026 tests, 0 échec. Le premier passage
complet avait attrapé `rpc_golden` (`schedule.move`), corrigé sans régénérer.

## 5. Reste et blocages

- T37 : `Origin::Channel`, liaison de session générique (`chat_session_for`,
  `approval_origin`, `origin_of` des jobs), `EffectKind::Message`,
  `StatusReport.channel` ; les 74 mentions restantes du cœur y sont presque toutes.
- `supervisor.rs:544` (« relecture du rédacteur sur la file Telegram », journal) laissé
  pour ne pas toucher un fichier partagé.
- Plafond `[crates]` du daemon : 15 109, mesuré en dessous (le test `schedule_move` sorti
  du daemon) ; à abaisser par l'intégrateur.
- `penelope-cli` : une ligne (`Daemon::new(home, Some(penelope_gateway_telegram::cards))`).
  Hors CLI, `Daemon::new(home, None)` démarre sans cartes (workflows non vérifiés sur
  les gabarits, questions d'étape `user` par le repli « ❓ nom »).

## 6. Notes de version (pour docs/progress.md)

#### Frontière canal : le cœur ne nomme plus Telegram (épopée #208, lot L, T36)

- Les gabarits de cartes et les jetons de boutons quittent `Services` pour la
  passerelle ; le cœur lit le texte d'un gabarit, le catalogue que valident les
  workflows, les liens profonds et la liste des commandes par un port `Cards`.
- La carte de rafale, le nom des conversations où livrent les planifications, le
  contrôle des conversations autorisées et le formulaire d'une élicitation MCP sont
  décidés par la passerelle, derrière `ChannelDelivery` et `OwnerChannel`.
- `penelope-app`, `penelope-executor` et `penelope-daemon` ne dépendent plus de
  `penelope-telegram` (règles d'architecture) ; les mentions du canal dans le cœur
  passent de 166 à 74.
- Aucun texte ne change sur Telegram. Sans canal, les messages disent « canal » au lieu
  de « Telegram ».
