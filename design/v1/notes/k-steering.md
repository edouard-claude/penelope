# Lot K : steering de la boucle (épopée #208, T12, T13, T14)

Agent `k-steering`, branche `v1-k-steering`, base 1.0.0-alpha.11 (`8f701ff`).
Spécification : `design/v1/boucle-et-outils.md` §3.4 et §5 (T12 à T14).

## Livré

| Commit | Tâche | Quoi |
|---|---|---|
| `2b44090` | T12 | port `Inbox` et `Checkpoint::BeforeModelCall` ; `request_messages` sans effet de bord |
| `35178ea` | T13 | `Checkpoint::BetweenCalls` : appels non démarrés « Non exécuté : nouveau message du propriétaire. » ; scénario `message-pendant-un-lot` |
| `dec4a5d` | T14 | `/stop` pendant un lot : « Non exécuté : arrêté par le propriétaire. » ; `Injection::Note` au tour suivant |

### T12

- `penelope-app/src/steering.rs` : `Checkpoint { BeforeModelCall, BetweenCalls }`,
  `Steer { id, text, arrived_at }`, trait `Inbox::claim(at)`.
  `Conversation::record_steer` (défaut : un message `user`).
- `penelope-conversation` : `request_messages` ne fait plus que projeter.
  `TurnInbox` (dans `lib.rs`, pour garder la clé `[channel.allowed]` du fichier) réclame
  sous le bail du tour (`absorb_pending`) et applique la règle de rafale du canal (carte,
  `turn.burst_card.<tour>`, annulation) ; `TurnInbox::for_turn` ne rend une boîte que pour
  un tour `Message`. `SessionConversation::record_steer` écrit par `append_queued`
  (une fois sous la clé de file, daté de l'arrivée, `mid_turn`). `with_merge_turn` disparaît.
- `penelope-agent/src/steering.rs` : la boucle réclame avant chaque appel au modèle, écrit
  les messages, émet `turn.merged` (même charge qu'avant) et pose la note de fusion
  (`MERGE_NOTE`, même texte, même place). `AgentLoop::with_inbox` ; le champ `inbox` est
  privé à la crate. Aucune signature publique existante ne change.
- Daemon : `engine.rs` passe `TurnInbox::for_turn(...)` à `with_inbox` (engine.rs descend
  de 1 095 à 1 093 lignes, `UPDATE_BUDGET` passé).

### T13

- Avant chaque appel à exécuter (pas avant un refus déjà décidé), la boucle réclame la boîte
  (`BetweenCalls`). Un message réclamé : les appels restants reçoivent leur résultat (un
  refus garde son texte, un appel à exécuter reçoit `NOT_RUN_NEW_MESSAGE`), le groupe est
  admis, puis le message est écrit ; le modèle est rappelé une fois, avec la note de fusion.
- Pas de réclamation quand le lot finit sur une carte d'approbation ou une boucle : le
  message attendrait sinon derrière un appel suspendu que `pending_calls` abandonnerait.
  Il reste en file pour le tour suivant, comme avant.
- Scénario rejouable `message-pendant-un-lot` : l'étape `message` accepte `steer`, écrit
  pendant l'appel MCP simulé ; le serveur simulé attend `release` au lieu de
  `pending()` (un crash ne le relâche jamais, comportement inchangé).

### T14

- `/stop` avant un appel du lot : les appels restants reçoivent `NOT_RUN_STOPPED` avant
  `Cancelled`. Même chose en tête d'itération (après le premier passage) pour les appels
  que le tour vient de demander ; au premier passage, les appels en attente appartiennent
  à une reprise et ne sont pas touchés.
- `Injection::Note` et `interruption_note` : la note est déduite du transcript (résultats
  `NOT_RUN_STOPPED` juste avant les derniers messages du propriétaire), insérée après le
  dernier message `user`, jamais écrite. `system_hash` inchangé (test).
- `close_pending` sert aussi à la clôture après le détecteur de boucles (texte inchangé).

## Tests

- `penelope-conversation` : `reading_the_conversation_absorbs_nothing_and_the_inbox_claims_once`
  (deux lectures, une absorption ; `record_steer` deux fois, une entrée).
- `penelope-agent/src/tests/steering.rs` : `a_message_during_a_batch_skips_the_calls_not_started`
  (3 lectures + 1 écriture, message pendant la 1re), `an_empty_inbox_changes_nothing`,
  `a_stop_during_a_batch_closes_the_calls_and_notes_it_next_turn`,
  `the_interruption_note_follows_only_a_stopped_batch`.
- Daemon : le test de rafale du runner réclame par `inbox.claim(BeforeModelCall)` au lieu
  de `request_messages()` (assertions inchangées).
  `a_running_turn_absorbs_a_new_message_before_the_next_model_call` garde ses assertions
  mais passe par la boucle (un exécuteur dépose le message pendant l'outil) : un test
  strictement inchangé qui n'appelle que `request_messages` contredirait le critère
  « `request_messages` sans effet de bord ».
- Scénarios : 24 verts, aucune surface existante régénérée ; seul
  `message-pendant-un-lot` est nouveau. `both_history_sources_send_the_same_requests`,
  CA 4.5 et 5.5 verts.

## Choix

- **La boucle écrit, la boîte réclame.** L'ordre de T13 (résultats avant le message)
  l'impose : une boîte qui écrirait à la réclamation poserait le message avant les
  « Non exécuté ».
- **La règle de rafale reste dans l'adaptateur** (`TurnInbox`), côté conversation : elle
  lit `cfg.telegram.burst_*` et `Origin::Telegram`, que la boucle ne peut pas nommer
  (frontière canal/cœur). T36 la passera derrière `ChannelDelivery`.
- **La note de fusion garde sa place** (après les messages système) : la déplacer en
  queue, comme le veut §3.4, change la surface de `messages-fusionnes` et le cache ; hors
  des critères de T12 à T14, laissé pour une tâche dédiée.
- **Pas de `Checkpoint::AfterBatch` ni de `Injection::{ToolResultSuffix, RequestOnly}`** :
  aucun n'a encore d'appelant.
- **La note d'interruption n'est pas écrite.** Elle se déduit du transcript à chaque
  requête : identique pour les deux sources d'historique, rien à purger. Coût : un raté
  de cache « historique » au premier appel du tour d'après, quand elle disparaît.

## Gel

- Plafond `[crates]` du daemon : rouge, 22 922 lignes pour 22 863 (le test du moteur
  réécrit par la boucle ajoute un exécuteur de test). À poser par l'intégrateur.
- Aucun fichier au-dessus de 800 lignes créé, aucun `allow` ajouté, aucune clé
  `[channel.allowed]` touchée.

## Reste

- Note de fusion en `Injection::Note` en queue (§3.4, dernier point) ; régénérer la
  surface de `messages-fusionnes`.
- T36 : la règle de rafale derrière `ChannelDelivery`.
- `/stop` quand le lot finit sur une carte : l'appel suspendu et ceux d'après restent en
  attente de la décision, comme avant.

## Notes de version (pour docs/progress.md)

#### Boucle d'agent : steering explicite (épopée #208, lot K, T12 à T14)

- Un message arrivé pendant un tour est réclamé par la boucle à des points nommés
  (`Inbox`, avant chaque appel au modèle et entre deux appels d'outils) ; lire la
  conversation n'absorbe plus rien.
- Un message arrivé pendant un lot d'outils n'attend plus la fin du lot : l'appel en
  cours finit, les suivants reçoivent « Non exécuté : nouveau message du propriétaire. »,
  et le modèle repart avec le message.
- `/stop` pendant un lot donne « Non exécuté : arrêté par le propriétaire. » aux appels
  non démarrés ; le tour suivant sait que le précédent a été interrompu, sans toucher au
  préfixe du prompt.
- Nouveau scénario rejouable `message-pendant-un-lot`.
