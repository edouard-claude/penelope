# V1 : le journal d'événements comme source unique de vérité de la conversation

Conception détaillée et découpage en tâches. Références : le code au 23 septembre 2026
(0.17.58 sur `main`, plus le lot #205 non commité), `docs/progress.md`, `docs/decisions/`,
les issues #205 et #206, et le dépôt DeepSeek Harness cloné en lecture seule (`dsh/` ci-dessous
désigne `scratchpad/comparatif/dsh`). Le PRD n'est pas utilisé.

Chaque affirmation sur l'état actuel cite sa preuve `chemin/fichier.rs:ligne`. Les numéros
de ligne de `purge.rs`, `agent.rs`, `state.rs`, `migrations.rs`, `tiers.rs`, `cache_audit.rs`,
`prompt_snapshot.rs` et `audit.rs` sont ceux de l'arbre de travail, lot #205 compris.

## 0. En une page

Aujourd'hui, la conversation vit dans deux magasins qui ne se connaissent pas : le journal
`events` (chaîné par hachage, purgé via `event_purges`, rejoué par le flux runtime) et
l'historique canonique `messages` (plus `message_context`, `lcm_nodes`, `artifacts`), lu par
la projection envoyée au modèle. Rien ne relie une ligne de `messages` à un événement, et
plusieurs choses que le modèle lit n'existent dans aucun des deux (prompt système avant #205,
consigne de relance, note de fusion).

La cible V1 : **un seul journal par session**, dans la table `events` existante, où chaque
contenu vu par le modèle est un événement `conv.*` avec un numéro de format et une opération de
surface (`append`, ou `replace` d'une plage). L'historique envoyé au modèle est **dérivé** du
journal par une fonction pure ; `messages`, `messages_fts`, `message_context`, `lcm_nodes` et
`prompt_snapshots` deviennent des caches reconstructibles par `penelope history reindex`. La
purge, le fork, le retour arrière et la reprise après crash s'expriment sur le journal. La
décision 0008 (rien ne bouge avant le dernier message) est respectée par construction : une
dérivation append-only ne réécrit rien, et un `replace` n'est écrit qu'à une frontière où le
cache est de toute façon perdu.

La migration se fait en quatre phases expand-contract, la V0 continuant de tourner sur `main`
à chaque étape : double écriture, commande de comparaison, bascule de lecture, retrait. Les
sessions existantes sont **scellées** par un événement d'import par session : leur préfixe
reste lu dans les tables V0, tout ce qui suit vient du journal.

```
  V0                                        V1
  ┌──────────┐   rien ne les relie   ┌──────────┐      ┌──────────────────────────┐
  │  events  │ ◄───────────────────► │ messages │      │ events (conv.* + turn.*) │ vérité
  │ (audit)  │                       │ lcm, ctx │      └────────────┬─────────────┘
  └──────────┘                       └────┬─────┘                   │ fold pur (derive)
                                          │ projection              ▼
                                          ▼                  ┌──────────────┐  caches
                                    requête modèle           │ messages/fts │  reindex
                                                             │ lcm, ctx     │
                                                             └──────┬───────┘
                                                                    ▼
                                                             requête modèle
```

## 1. État actuel

### 1.1 Le journal `events`

- Schéma : `events(id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash)`
  (`crates/penelope-store/src/migrations.rs:140-153`), `seq` unique par session depuis la
  migration 0011 (`crates/penelope-store/src/migrations.rs:912-914`).
- Écriture : `EventLog::append` calcule `prev_hash` sur le dernier événement global
  (`crates/penelope-kernel/src/event.rs:183-190`), attribue `seq = MAX(seq)+1` par session
  (`event.rs:192-203`), insère dans une transaction de l'unique thread écrivain
  (`event.rs:216-229`), puis diffuse l'événement commité aux abonnés
  (`event.rs:246`). Le verrou `live_order` garantit que l'ordre d'émission suit l'ordre des
  identifiants (`event.rs:173-175`, issue #162).
- Le corps haché couvre `session_id`, `run_id`, `seq`, `ts`, `kind`, `payload`
  (`event.rs:64-80`). Le texte canonique du payload est stocké tel quel et vérifié octet pour
  octet (`event.rs:98-130`, `event.rs:317-345`).
- Lecture : `session_events(session, from_seq)` (`event.rs:251-268`), `range(after_id, limit)`
  (`event.rs:271-287`), `verify()` qui rejoue toute la chaîne (`event.rs:290-362`).
- Ce que le daemon y écrit aujourd'hui pour une conversation : `turn.started` avec le modèle,
  `system_hash` et `tools_hash` (`crates/penelope-daemon/src/agent.rs:554-567`) ;
  `turn.finished` avec itérations et coût, **uniquement** sur la sortie « réponse finale »
  (`agent.rs:896-904`) ; `tool.result` avec le nom de l'outil, `ok` et la forme de la ligne,
  jamais le contenu (`agent.rs:1751-1767`) ; `turn.merged` (`agent.rs` n'en écrit pas, voir
  `crates/penelope-daemon/src/engine.rs:277-291` et
  `crates/penelope-daemon/src/conversation.rs:194-202`) ; `llm.retried` (`agent.rs:1038-1051`),
  `turn.empty_answer` (`agent.rs:823-840`), `llm.fallback_used` (`agent.rs:1114-1122`) ;
  `context.compaction_requested` / `_skipped` / `_failed` / `_mechanical` / `compacted`
  (`crates/penelope-daemon/src/compaction.rs:676-691`, `1150-1170`) ; `session.forked` et
  `session.rewound` avec des comptes, pas de contenu
  (`crates/penelope-daemon/src/session_ops.rs:87-95`, `235-243`) ; `audit.purge`
  (`event.rs:398-402`) ; `memory.episode_closed` (`crates/penelope-daemon/src/episodes.rs:147-156`).
- Lecteurs : le flux runtime rejoue `range()` après un curseur, rédige et borne le payload à
  64 Kio (`crates/penelope-daemon/src/runtime_events.rs:29-53`, `160-176`) ; l'export JSONL
  concatène messages puis événements de la session (`session_ops.rs:307-329`) ; le
  planificateur suit un curseur d'identifiant (`crates/penelope-daemon/src/scheduler.rs:320-345`) ;
  `store.rebuild` vérifie la chaîne (`session_ops.rs:340`).
- Trois tables de projections existent depuis la 0001 et ne sont écrites ni lues nulle part :
  `projections_session`, `projections_workflow`, `projections_approval`
  (`migrations.rs:155-173` ; `grep -rn "projections_" crates/` hors `migrations.rs` ne rend
  rien, vérifié par le lead). Ce sont les vestiges d'une intention de projections typées
  jamais réalisée. La V1 tranche : `projections_session` est réutilisée comme filigrane du
  pliage (§2.4) ; `projections_workflow` et `projections_approval` sont supprimées par la
  migration 0019 (T11), aucun test ne les nomme (`migrations.rs:1177-1225`).

### 1.2 L'historique canonique `messages` et ses satellites

- Schéma : `messages(session_id, seq, role, content JSON, tool_call_id, tool_name, tokens_est,
  ts, episode, eager, artifact_id, compacted)` avec `UNIQUE(session_id, seq)`
  (`migrations.rs:199-215`), `messages_fts` (`migrations.rs:217-219`), `message_context`
  (`migrations.rs:850-855`, issue #17), `lcm_nodes` et `lcm_edges` (`migrations.rs:221-242`),
  `artifacts` (`migrations.rs:244-261`), `messages.source_turn_id` unique (`migrations.rs:944-950`,
  issue #161), `prompt_snapshots` (`migrations.rs:964-979`, issue #205, non commité).
- `seq` des messages : `MAX(seq)+1` par session, indépendant du `seq` des événements
  (`crates/penelope-context/src/store.rs:271-277`). Deux espaces de numérotation par session,
  sans table de correspondance.

Qui écrit quoi :

| Écriture | Où | Appelé par |
|---|---|---|
| Message utilisateur, assistant, résultat d'outil | `HistoryStore::append` (`store.rs:252-304`) | `SessionConversation::record` (`conversation.rs:265-291`), déclencheurs et relances (`engine.rs:440-451`), photos (`engine.rs:523-526`) |
| Message utilisateur venu de la file, idempotent par identifiant de tour | `append_user_turn_at` (`store.rs:308-355`) | `engine.rs:428-438` (tour), `engine.rs:461-471` (messages fusionnés), `conversation.rs:180-190` (absorbés pendant le tour) |
| Corps externalisé (niveau 1) : `UPDATE` du contenu en place | `externalise` (`store.rs:643-674`) | `ContextEngine::admit_tool_group` (`crates/penelope-context/src/engine.rs:265-316`), depuis `record` pour un gros résultat (`conversation.rs:277-288`) ou `admit_tool_results` pour un groupe (`conversation.rs:295-320`, appelé en `agent.rs:1668`) |
| Contexte volatil figé | `freeze_context` (`store.rs:445-461`) | `cache_audit::freeze_volatile` (`crates/penelope-daemon/src/cache_audit.rs:203-230`), appelé en `engine.rs:569` |
| Marquage `compacted` | `mark_compacted` (`store.rs:624-640`) | `apply_summary` (`context/engine.rs:451-454`, `505-507`) et réparation à la préparation suivante (`context/engine.rs:343-351`) |
| Nœud de résumé | `Lcm::insert_leaf` / `extend` (`crates/penelope-context/src/lcm.rs:76-116`, `219-236`) | `apply_summary` (`context/engine.rs:464-503`) via `compaction::publish` (`daemon/compaction.rs:1102-1105`) |
| Copie et coupe | `copy_messages`, `truncate_from` (`store.rs:538-592`) | `fork` (`session_ops.rs:45-49`) et `rewind` (`session_ops.rs:224-230`) |
| Artefact | `put_artifact` (`store.rs:791-846`) | `admit_tool_group` |
| Prompt système rendu | `prompt_snapshot::record` (`crates/penelope-daemon/src/prompt_snapshot.rs:75-102`) | après chaque appel, hors chemin de réponse (`agent.rs:727-732`) |

Qui lit quoi :

- La projection envoyée au modèle : `projected_entries` charge les nœuds LCM actifs, puis
  `history.load(from_seq)` au-delà de leur couverture, puis `contexts_from`, et insère le
  bloc figé en tête de chaque message utilisateur (`conversation.rs:118-166`) ;
  `build_from_entries` applique les niveaux 0 et 2 sur la seule projection, répare les paires
  et assemble les tuiles (`context/engine.rs:188-261`) ; le niveau 4 réduit en le prouvant
  (`conversation.rs:254-262`, `context/compaction.rs:554-618`).
- La queue : `tail()` relit les 64 dernières entrées (`conversation.rs:329-339`) ;
  `pending_calls` y retrouve les appels sans résultat (`agent.rs:2387-2408`).
- Les outils d'historique lisent `messages` et `lcm_nodes` (`crates/penelope-daemon/src/executor.rs:1140-1202`) ;
  la relecture d'épisode lit par colonne `episode` (`episodes.rs:275`) ; l'audit du tour relit
  `messages` et `message_context` en tronquant au `msg_count` de la ligne d'usage
  (`crates/penelope-daemon/src/audit.rs:148-210`) ; la purge, le libellé de session
  (`crates/penelope-kernel/src/budget.rs:642`), le gel du volatil (`cache_audit.rs:209-218`)
  et l'exécuteur (`executor.rs:1680-1690`) font des `SELECT` directs sur `messages`.

Un troisième magasin, caché : des clés `kv` portent de l'état de conversation qui n'est ni
dans le journal ni dans `messages` : `turn.recorded.<tour>` (idempotence de l'écriture du
message, `engine.rs:409-420`, `452`), `turn.intents.<tour>` (`engine.rs:659`),
`prompt.prefix.<session>` (préfixe en attente d'un cache froid, `cache_audit.rs:232-234`, `268`),
`compaction.pending.<session>` et `compaction.cooldown.<session>` (`daemon/compaction.rs:263-271`),
`t2.snapshot.<session>.<épisode>` (`episodes.rs:412-414`). `sessions.usage_anchor` n'est
jamais écrite autrement qu'à `NULL` par le retour arrière (`crates/penelope-kernel/src/session.rs:434`,
`session_ops.rs:232-234`).

### 1.3 Où la correspondance manque

1. **Aucun contenu dans le journal.** `tool.result` ne porte que le nom, `ok` et la forme
   (`agent.rs:1754-1764`) ; il n'existe pas d'événement pour un message utilisateur, une
   réponse ou un résultat. `context.compacted` porte l'identifiant du nœud et ses bornes, pas
   le texte du résumé (`daemon/compaction.rs:1152-1166`) : le résumé n'existe que dans
   `lcm_nodes`.
2. **Des réécritures sans trace.** Le niveau 1 réécrit `messages.content` en place
   (`store.rs:666-670`) ; le marquage `compacted` est un `UPDATE` (`store.rs:634-637`) ; le
   retour arrière `DELETE` des lignes (`store.rs:586-589`) et l'événement ne garde qu'un compte
   (`session_ops.rs:238-240`) ; le fork `INSERT` des copies (`store.rs:548-557`).
3. **Ce que le modèle lit sans que rien l'enregistre.** Avant #205, le prompt système n'était
   nulle part (issue #205, constat) ; #205 le fige sous son empreinte, adressée depuis `usage`,
   `llm_requests` et `turn.started` (`prompt_snapshot.rs:8-10`). Restent hors de tout magasin :
   la consigne de relance après réponse vide, ajoutée à la requête sans être écrite
   (`agent.rs:671-677`) ; la note « un nouveau message est arrivé pendant le tour », insérée
   dans la requête seulement (`conversation.rs:86-100`) ; le partiel d'un flux coupé, perdu
   (`agent.rs:1134-1144`, issue #206).
4. **Rien n'est reconstructible.** `store.rebuild` ne reconstruit que `messages_fts` et
   l'index mémoire, puis vérifie la chaîne (`session_ops.rs:333-352`) ; `messages` et
   `lcm_nodes` n'ont pas de source. Le seul précédent de reconstruction est côté mémoire,
   `penelope mem reindex` (`crates/penelope-kernel/src/api.rs:202`,
   `crates/penelope-cli/src/commands.rs:1122`) : la commande `history reindex` de la cible
   n'a aucun précédent côté conversation.
   L'audit d'un tour est déclaré approximatif dès qu'une compaction a eu lieu
   (`audit.rs:190-196`) parce que la projection du moment n'est plus recalculable.
5. **Deux transactions là où il en faudrait une.** Un nœud écrit puis un crash avant le
   marquage laisse un état que la préparation suivante répare (`context/engine.rs:343-351`,
   test `a_crash_between_node_and_marking_is_repaired`, `context/engine.rs:990-1017`).
6. **Un tour n'est fermé que s'il répond.** `turn.finished` n'est écrit que sur
   `TurnOutcome::Answered` (`agent.rs:896-904`) : une annulation, un échec, une attente
   d'approbation ou un plafond laissent un `turn.started` sans fin, indiscernable d'un crash.
7. **Une colonne morte depuis l'origine.** Sur `main`, `llm_requests.request` existe
   (`migrations.rs:325` à HEAD) et n'est jamais renseignée : `plan()` n'insère que
   `body_hash` (`crates/penelope-llm/src/state.rs:127-131` à HEAD). C'est l'issue #205 ;
   le lot en cours retire la colonne (`migrations.rs:975`) et écrit à la place les trois
   clés `system_hash`, `tools_hash`, `request_hash` (`state.rs:145-159`). La V1 s'appuie
   sur ces clés (§2.4), jamais sur un corps recopié.

### 1.4 Ce que la purge fait déjà

- `purge_session` remplace le payload de chaque événement de la session par
  `{"purged":true}` et conserve le hash d'origine dans `event_purges` (`event.rs:365-404`,
  `migrations.rs:812-819`) ; `verify` prend le hash d'origine pour un événement purgé
  (`event.rs:331-345`) : la chaîne reste prouvable après effacement (test
  `purge_erases_content_but_keeps_chain_verifiable`, `event.rs:584-602`).
- `purge::session` efface `messages`, `messages_fts`, `message_context`, `lcm_*`, `artifacts`
  et leurs fichiers, les instantanés de prompt que cette seule session référençait, puis
  vide `llm_requests`, les tours, candidats, updates, envois, effets, approbations, tâches et
  runs (`crates/penelope-daemon/src/purge.rs:57-295`) ; la rétention ne touche jamais
  `events` ni `usage` (`purge.rs:23-24`, `316-448`).
- Le flux runtime rejoue les événements purgés avec leur marqueur (`docs/runtime-events.md`,
  section « Contrat » ; `runtime_events.rs:29-41`).

Conséquence pour la V1 : rien à inventer. Un événement `conv.*` purgé devient
`{"purged":true}` comme les autres ; la dérivation doit simplement traiter ce marqueur comme
« absent ».

### 1.5 Ce que le cache de prompt impose (décision 0008)

`docs/decisions/0008-cache-de-prompt.md` fixe cinq points ; trois contraignent la V1 :

1. Le contexte volatil est figé avec le message qu'il accompagne et projeté avec lui dans
   toutes les requêtes suivantes : `freeze_volatile` écrit `message_context` sur le dernier
   message utilisateur avant le premier envoi (`cache_audit.rs:203-230`), `projected_entries`
   le réinsère (`conversation.rs:126-147`).
2. Un préfixe T0 à T2 modifié attend un cache froid : `stable_prefix` remplace les tuiles
   construites par celles mémorisées dans `kv` tant que le dernier appel a moins de cinq
   minutes (`cache_audit.rs:239-269`). Le préfixe est **un seul** message système
   (`crates/penelope-context/src/tiers.rs:105-111`).
3. Un résumé n'est publié qu'à la fin d'un tour, jamais au milieu (`daemon/compaction.rs:711-723`,
   `docs/context.md`, section « Le cache »). Une session froide ou le premier tour d'un fork
   sont compactés avant l'appel (`daemon/compaction.rs:462-520`).

Tests qui verrouillent cela : `ca_5_3_prefix_is_byte_identical_across_turns`
(`tiers.rs:484`), `ca_5_4_each_request_extends_the_previous_one` (`cache_audit.rs:285-362`),
`the_stable_prefix_does_not_move_between_turns` (`conversation.rs:1183-1188`),
`the_volatile_tier_never_enters_the_snapshot` (`prompt_snapshot.rs:218-229`).

## 2. Cible V1

### 2.1 Les trois invariants

1. **Ce que le modèle lit est journalisé.** Chaque message d'une requête dérive d'un
   événement `conv.*` de la session (ou du préfixe scellé, §4.5) ; le prompt système
   dérive du dernier `conv.system` ; le bloc volatil dérive d'un `conv.context`. Les seules
   transformations non journalisées sont des fonctions pures de la dérivation et des
   paramètres (niveaux 0, 2 et 4, réparation des paires), et la requête enregistre quels
   niveaux ont joué. Vérifié par un test qui compare `request_messages()` à `derive(journal)`.
2. **Le journal ne se réécrit pas.** Un contenu ne change jamais en place : le niveau 1, la
   compaction, le retour arrière sont des événements qui **remplacent une plage** de la
   surface. La chaîne de hachage reste ce qu'elle est (`event.rs:64-96`).
3. **Les tables de messages sont des caches.** `messages`, `messages_fts`, `message_context`,
   `lcm_nodes`, `lcm_edges`, `prompt_snapshots` se reconstruisent depuis le journal
   (`penelope history reindex`) et se vérifient contre lui (`penelope history verify`).

### 2.2 Vocabulaire des événements de contenu

Tous les événements de contenu portent `kind = "conv.<type>"`, `session_id`, et un payload
JSON avec `"v": 1` (version de format du payload, §2.5) et, pour ceux qui produisent un
message, `"surface"`. Les noms existants (`turn.*`, `tool.result`, `context.*`) restent tels
quels : ce sont des événements d'observation, sans effet sur la surface.

```
  journal d'une session (events.seq croissant)
  ┌─────────────────────────────────────────────────────────────────────────┐
  │ turn.started ─ conv.system ─ conv.user ─ conv.context ─ conv.assistant ─ │
  │ conv.tool_result ─ conv.tool_result(replace, niveau 1) ─ conv.attempt ─  │
  │ conv.assistant ─ turn.finished ─ conv.summary(replace 1..k) ─ ...        │
  └─────────────────────────────────────────────────────────────────────────┘
        surface = fold(événements)  →  [system][nœuds dans l'ordre][+volatil du dernier user]
```

| Kind | Surface | Payload (en plus de `v`) | Remplace en V0 |
|---|---|---|---|
| `conv.system` | `append` (premier) ou `{"op":"replace","from":s,"to":s}` sur le nœud système précédent | `hash` (= `system_hash`, `cache_audit.rs:61-63`), `rendered`, `tiles` (`TileMap`, `tiers.rs:407-452`), `reason` : `first`, `cold`, `compaction` | `prompt_snapshots` + `kv prompt.prefix.*` |
| `conv.user` | `append` | `source` : `owner`, `merged`, `trigger`, `nudge`, `photo`, `import` ; `turn_message_id` (clé d'idempotence, `turn_queue.id`) ; `arrived_at` ; `content` (blocs, sérialisés comme `store.rs:975-1002`) ; `episode` ; `tokens_est` ; `mid_turn: true` pour un message absorbé pendant le tour | `messages` rôle `user`, `source_turn_id` |
| `conv.context` | aucune (log-only) | `target` : `seq` du `conv.user` visé ; `block` : le bloc `<contexte>` (`cache_audit.rs:195-197`) | `message_context` |
| `conv.assistant` | `append` | `turn`, `step`, `content`, `tool_calls`, `reasoning`, `reasoning_details`, `model`, `provider`, `upstream`, `generation_id`, `finish`, `usage` `{prompt, completion, cached, cache_write, reasoning}`, `cost_usd`, `estimated`, `llm_request_id`, `system_hash`, `tools_hash`, `request_hash`, `projection` : `{levels:[0,2], steps:[AppliedStep]}`, `interrupted: true` après `/stop`, `tokens_est` | `messages` rôle `assistant` ; les clés de `usage` restent écrites (§2.4) |
| `conv.tool_result` | `append`, ou `{"op":"replace","from":s,"to":s}` d'un seul nœud (niveau 1) | `turn`, `step`, `call_id`, `tool`, `ok`, `eager`, `content`, `tokens_est` ; en remplacement : `artifact_id`, `artifact_sha256`, `original_tokens` | `messages` rôle `tool`, `externalise` |
| `conv.attempt` | aucune | `turn`, `step`, `cause` : `stream_cut`, `before_stream`, `empty_answer`, `fallback` ; `model`, `provider`, `upstream`, `error`, `partial_text`, `partial_reasoning`, `usage`, `cost_usd`, `llm_request_id`, `retry_prompt` (consigne de relance, si la requête suivante l'ajoute) | rien (issue #206) |
| `conv.summary` | `{"op":"replace","from":a,"to":b}` | `node_id`, `previous_node_id`, `summary` (texte rendu, `render_summary`), `anchors`, `verbatim_users`, `model`, `tokens_src`, `tokens_self`, `batches_left`, `trigger` | `lcm_nodes`, `mark_compacted` ; `context.compacted` reste comme événement d'observation |
| `conv.rewind` | `{"op":"cut","after":s}` | `turns`, `archive_session` | `truncate_from` |
| `conv.fork` | `{"op":"inherit","parent":id,"up_to":s,"offset":n}` | `parent`, `up_to`, `offset` | `copy_messages` + copie des nœuds |
| `conv.import` | `{"op":"seal","messages":n,"offset":n}` | `messages`, `contexts`, `lcm_active` (nœuds actifs avec bornes), `digest` (§4.5) | rien : scelle l'historique V0 |

Deux événements d'exécution complètent le vocabulaire :

- `turn.started` gagne `turn_id`, `origin_turn`, `kind`, `attempt` (numéro de tentative du
  tour, pour un tour rejoué après crash) ; il garde `model`, `system_hash`, `tools_hash`.
- `turn.finished` est écrit sur **toutes** les sorties de `run_conversation`, avec `reason` :
  `answered`, `awaiting_approval`, `cancelled`, `failed`, `budget_exceeded`, `loop_aborted`,
  `calls_exhausted`, et deux raisons que la boucle n'émet jamais : `interrupted` (reprise
  après crash, §2.7) et `forked` (jamais : Pénélope refuse un fork pendant un tour, §2.6).

Ce que je ne reprends pas de DSH, et pourquoi :

- **`tool/call` séparé de `assistant/message`.** DSH le journalise pour distinguer, après un
  crash, un appel jamais démarré d'un appel au résultat inconnu
  (`dsh/packages/core/session/src/repair.ts:17-21`, `95-103`). Pénélope a déjà le ledger
  d'effets : `run_effect` enregistre `dispatching` puis `completed` ou `failed` par
  identifiant d'appel (`agent.rs:1674-1729`), et rejoue ou demande une décision sans
  ré-exécuter (`agent.rs:1695-1712`). Les appels restent dans `conv.assistant.tool_calls` ;
  le ledger dit s'ils sont partis.
- **`step/start` et `step/end`.** Les itérations de `run_conversation` sont numérotées dans
  les payloads (`step`), sans événement de frontière : deux événements par itération pour
  une information déjà portée par `conv.assistant`.
- **Les fermetures synthétiques de résultats d'outils après crash** (`repair.ts:125-155`). Un
  résultat inventé entrerait dans la surface et contredirait le ledger : Pénélope laisse
  l'appel en attente dans la queue dérivée et laisse `Planned::Replayed` /
  `NeedsDecision` trancher (§2.7).
- **`request/header` comme événement.** Les outils et la configuration d'appel sont déjà
  dans `llm_requests` (`crates/penelope-llm/src/state.rs:127-165`) avec `system_hash`,
  `tools_hash`, `request_hash` (#205). `conv.assistant` cite `llm_request_id` ; la liste
  d'outils reste adressée par son empreinte, comme #205 l'a tranché (`audit.rs:81-84`).
- **Le flux brut du fournisseur embarqué dans chaque message** (`assistant/message.stream`,
  `dsh/packages/core/session/src/types.ts:341-349`). Trop volumineux pour un journal haché
  vérifié en entier (`event.rs:290-362`) ; `conv.attempt` garde le partiel, pas les
  fragments.
- **`developer/message` et les projections de messages enfichables**
  (`dsh/docs/subsystems/session.md`, « Plugin-owned message projections »). Pénélope n'a pas
  de greffons tiers ; la dérivation est une fonction unique dans `penelope-context`.
- **Un fichier JSONL par session** (`dsh/docs/subsystems/persistence.md`, « The backend »).
  Le journal est déjà dans SQLite, chaîné, purgeable ligne à ligne, rejoué par le flux
  runtime ; changer de support n'apporte rien à la V1.

Ce que je reprends tel quel : le journal append-only comme seule source
(`dsh/docs/subsystems/session.md`, en-tête), la dérivation pure des messages
(`deriveMessages`, `dsh/docs/subsystems/session.md`, « Derived history »), la surface avec
`append` / `replace(startSeq, endSeq)` (`types.ts:462-464`), l'invariant « le remplacement
cite chaque nœud masqué » (`surface.ts:335-371`), l'attempt hors surface
(`agent.ts:449-459`, `471-475`), la version de format et le refus d'un événement inconnu non
marqué ignorable (`types.ts:89`, `501-511`), les projections typées avec `stateVersion`
(`dsh/docs/subsystems/session-projection.md`, `ProjectionDefinition`), la reprise après
crash sans troncature (`persistence.md`, « Crash recovery preserves an interrupted turn »),
le résumé comme nœud de remplacement et les marqueurs de compaction log-only
(`dsh/docs/subsystems/compaction.md`, tableau des événements), et les transcriptions de
session rejouables sans clé comme filet de régression (`dsh/docs/testing.md:53`,
`dsh/snapshots/session/`), transposées en T0.

### 2.3 La surface et la dérivation

**Adresse d'un nœud.** Un nœud de surface est adressé par son `seq` dérivé :
`seq = offset + events.seq`, où `offset` est le plus grand `seq` hérité par la session
(`conv.import.offset` pour une session scellée, `conv.fork.offset` pour un fork, 0 sinon).
Pour une session née après la V1 et jamais forkée, `seq` est le `seq` de l'événement. Les
nœuds hérités gardent leur adresse d'origine (les `messages.seq` V0 pour un préfixe scellé,
les adresses du parent pour un fork). La règle garantit sans collision une numérotation
croissante, avec des trous : les événements d'observation consomment des `seq` sans produire
de nœud.

**Le pliage** (`derive`, fonction pure de `penelope-context`, sans base) :

```
  entrée : préfixe hérité (nœuds déjà dérivés) + événements de la session par seq
  état   : nodes: Vec<seq>, messages: Map<seq, ChatMessage>, contexts: Map<seq, String>,
           system: Option<seq>, attempts_tail: Option<Attempt>
  pour chaque événement :
    payload {"purged":true}            → ignoré (le nœud n'existe pas ; un replace qui le
                                         citait masque simplement ce qui reste)
    kind hors conv.*                   → ignoré
    conv.* avec v inconnu              → erreur DeriveError::Format (refus, jamais silence)
    conv.system append                 → system = seq
    conv.system replace from=to=system → system = seq (le nœud 0 n'est jamais dans nodes)
    conv.user / conv.assistant /
    conv.tool_result append            → nodes.push(seq), messages[seq] = message
    conv.tool_result replace(s, s)     → messages[s] = corps externalisé ; nodes inchangé
                                         (le nœud garde son adresse : les paires appel/résultat
                                          et les pointeurs seq:N ne bougent pas)
    conv.context                       → contexts[target] = block
    conv.summary replace(a, b)         → nodes = nodes[..i(a)] ++ [seq] ++ nodes[i(b)+1..],
                                         messages[seq] = system("Résumé de la conversation
                                         antérieure (nœud X) :\n" + summary)
                                         (même texte que conversation.rs:156-159)
    conv.rewind cut(after=s)           → nodes tronqué après s ; les événements restent
    conv.fork inherit                  → seulement en tête du journal du fils : nodes et
                                         messages = dérivation du parent jusqu'à up_to
    conv.import seal                   → nodes et messages = préfixe scellé (§4.5)
    conv.attempt                       → attempts_tail = Some(...) ; effacé par le prochain
                                         conv.assistant ou turn.finished
  sortie : Surface { system, nodes, messages, contexts, attempts_tail }
```

**Vérifications du pliage** (comme `surface.ts:421-441`) : `from` et `to` d'un `replace`
doivent être des nœuds présents, dans l'ordre de surface ; un `replace` de `conv.tool_result`
ne couvre qu'un nœud, du même `call_id` ; un `replace` de `conv.system` ne couvre que le
système ; un `cut` cite un nœud présent. Une violation est une erreur de dérivation : le
journal est incohérent, on le dit (`penelope history verify`), on ne devine pas.

**De la surface à la requête** (`request_messages` V1) :

1. message système = `messages[system]` ou, sans `conv.system`, le préfixe construit par les
   tuiles (premier tour) ;
2. les nœuds dans l'ordre, chaque message utilisateur précédé de son `contexts[seq]`
   (exactement `conversation.rs:132-147`) ;
3. si un `conv.user` du tour en cours porte `mid_turn`, la note de fusion est insérée après le
   système (`conversation.rs:92-97`) ;
4. si `attempts_tail` porte `retry_prompt` et que la requête est la suivante du même tour,
   la consigne de relance est ajoutée en dernier message utilisateur (`agent.rs:673-676`) ;
5. niveaux 0, 2, réparation des paires, niveau 4 : inchangés, fonctions pures sur `Entry`
   (`context/engine.rs:188-261`) ; les `AppliedStep` sont recopiés dans le `conv.assistant`
   qui suit (`projection.steps`) ;
6. `cache_control` : inchangé (`tiers.rs:139-141`).

Les entrées `Entry` (`crates/penelope-context/src/transcript.rs:12-26`) gardent leur forme :
`seq`, `message`, `eager`, `artifact_id`, `tokens`, `episode`, `compacted` ; `compacted`
devient « masqué par un `conv.summary` », calculé par le pliage.

**Où sont les tours et les étapes.** Un tour V1 est délimité par `turn.started` et
`turn.finished` de la même session ; `step` est l'itération dans `run_conversation`
(`agent.rs:569`). La reprise après approbation (`TurnKind::Resume`) rouvre un tour avec le
même `origin_turn` (`engine.rs:475-491`) et `attempt + 1`.

### 2.4 Les projections, redéfinies comme caches

Une projection V1 est une fonction pure `apply(state, event) -> state` avec un numéro de
version (`FOLD_VERSION`), à la manière de `ProjectionDefinition.stateVersion`
(`dsh/docs/subsystems/session-projection.md`) : quand le code change la sémantique du pliage,
la version monte et les lignes en cache sont refondues, jamais réutilisées.

| Cache | Contenu dérivé | Clé de correspondance | Reconstruit par |
|---|---|---|---|
| `messages` | un nœud d'origine `append` par ligne (`seq` dérivé, rôle, contenu tel que **dernièrement** projeté, `artifact_id`, `compacted`, `episode`, `eager`, `tokens_est`, `ts` = `arrived_at` ou `ts` de l'événement) | nouvelle colonne `event_id` (NULL pour une ligne scellée) | `history reindex` |
| `messages_fts` | texte des lignes de `messages` | `msg_id` | `rebuild_fts` (existe, `store.rs:595-621`) |
| `message_context` | `conv.context` | `(session, seq)` | `history reindex` |
| `lcm_nodes` | un nœud par `conv.summary` ; `superseded_by` renseigné quand un `conv.summary` ultérieur porte `previous_node_id` | nouvelle colonne `event_id` | `history reindex` |
| `lcm_edges` | vide : `insert_condensed` n'a aucun appelant hors tests (`lcm.rs:119-201`, aucun appelant dans `crates/`) ; la table reste, dépréciée | | |
| `prompt_snapshots` | index `hash → rendered, tiles, uses` alimenté par `conv.system` | `hash` | `history reindex` (la dédup reste utile à `usage --by miss`, `prompt_snapshot.rs:132-155`) |
| `projections_session` | filigrane du pliage : `state = {"fold_version": n, "last_event": id, "offset": n, "system": seq, "nodes_len": n}` ; `last_event` sert au rattrapage | `session_id` (table existante, `migrations.rs:156-161`) | écrit par le projecteur ; `projections_workflow` et `projections_approval`, jamais utilisées, sont supprimées (T11) |

`usage` et `llm_requests` ne changent pas : la comptabilité reste une table maîtresse
(purgée comme aujourd'hui), et `conv.assistant` cite `llm_request_id`, `system_hash`,
`tools_hash`, `request_hash` pour que `audit show` retombe dessus.

**Le projecteur.** `Projector::apply(tx, event)` maintient les caches à partir des événements
`conv.*`, dans le même thread écrivain que le journal, en **deux transactions** : l'événement
d'abord (la vérité, durable contre le processus, `crates/penelope-store/src/lib.rs:1-18`),
la projection ensuite, ordonnée par le verrou `live_order`. Un crash entre les deux laisse
`projections_session.last_event` en retard ; le prochain accès à la session rattrape depuis
le filigrane (`session_events(session, from_seq)`, `event.rs:251-268`). Ce choix remplace la
réparation ad hoc `context/engine.rs:343-351` par une règle unique : le cache peut être en
retard, jamais faux, et il se rattrape seul. Une erreur du projecteur ne fait pas échouer le
tour : elle est journalisée, la ligne de filigrane porte `dirty`, et `doctor` la signale.

`penelope history reindex [--session <id>]` : efface les lignes de cache dont `event_id`
n'est pas `NULL` (les lignes scellées restent), rejoue le pliage depuis le journal, réécrit
les lignes, reconstruit `messages_fts`, pose le filigrane. Idempotent.

`penelope history verify [--session <id>]` : dérive depuis le journal et compare aux caches
(nombre de nœuds, empreinte du contenu de chaque nœud, contextes, bornes des nœuds LCM
actifs, drapeaux `compacted`, digest du préfixe scellé) ; rapport JSON, code de sortie non nul
à la première divergence, ligne dans `doctor`.

### 2.5 Versionnage du format des payloads

- Chaque payload `conv.*` porte `"v": 1`. Le pliage refuse un `v` supérieur à ce qu'il
  connaît (`DeriveError::Format`), comme DSH refuse un journal d'une génération future
  (`persistence.md`, « Format refusal »). Un `conv.*` de kind inconnu est refusé sauf
  `"ignorable": true` dans le payload (`types.ts:501-511`).
- Passage de `v` à `v+1` : une fonction pure `upgrade_payload(kind, v, payload) -> payload`
  appliquée **à la lecture**, chaînée v1→v2→v3. Le journal n'est jamais réécrit : la chaîne
  de hachage l'interdit (`event.rs:82-96`). `FOLD_VERSION` monte, les caches sont refondus.
- Les kinds d'observation (`turn.*`, `tool.result`, `context.*`) restent hors versionnage : ils
  n'entrent pas dans le pliage.
- Le contrat du flux runtime (`docs/runtime-events.md`) documente les kinds `conv.*` ; un
  consommateur filtre par kind exact (`runtime_events.rs:65`), il ne reçoit donc rien de
  nouveau sans le demander.

### 2.6 Purge, fork, retour arrière

**Purge.** Inchangée dans son mécanisme : `purge_session` remplace les payloads et garde les
hash (`event.rs:365-404`), `purge::session` efface les caches (`purge.rs:113-127`). Le pliage
ignore un payload purgé, donc `reindex` d'une session purgée donne une surface vide. Les
instantanés de prompt suivent la règle de #205 (partagés entre sessions : `purge.rs:131-151`).
Différence assumée : une session purgée **qui a des forks** perd son préfixe dans les forks
(§ fork ci-dessous) ; `penelope purge` le dit avant d'agir.

**Fork.** V0 copie les lignes et les nœuds actifs (`session_ops.rs:45-66`). V1 : la session
fille reçoit un `conv.fork {parent, up_to, offset}` comme premier événement ; sa surface
commence par la dérivation du parent jusqu'à `up_to` (nœuds, contextes, système), puis ses
propres événements. Pas de copie : la vérité du préfixe reste dans le journal du parent, le
cache `messages` de la fille se remplit au premier pliage (comme aujourd'hui, elle est
lisible immédiatement). Un fork n'est possible qu'entre deux tours (la boucle refuse un
fork pendant un tour, comme `rewind` refuse aujourd'hui, `session_ops.rs:186-188`) : pas de
fermeture synthétique `forked`. Le titre, les métadonnées, le budget, les notes suivent le
chemin V0 (`session_ops.rs:67-86`). `session.forked` reste écrit (observation).

**Retour arrière.** V1 écrit `conv.rewind {"surface":{"op":"cut","after":s}}` où `s` est le
nœud qui précède le message utilisateur de coupe (`session_ops.rs:189-198`). Les nœuds
masqués restent dans le journal et lisibles par `penelope history show --all` ; la session
d'archive V0 (`session_ops.rs:214-229`) devient un fork-par-référence jusqu'au dernier nœud,
créé fermé, pour garder l'ergonomie (`/rewind` dit où retrouver ce qui a été défait). La
règle « pas de retour avant le dernier résumé » (`session_ops.rs:208-213`) est conservée : un
`cut` ne peut pas tomber dans une plage remplacée. Le numéro du prochain nœud ne repart pas
« juste après ce qui reste » (test `rewind_archives_what_it_removes`, `session_ops.rs:422-424`)
mais continue de croître : le test s'adapte (§6).

### 2.7 Reprise après crash

Un tour ouvert est un `turn.started` sans `turn.finished` de même `turn_id`. Au démarrage,
après `TurnQueue::recover_on_boot` qui remet les tours loués en attente
(`crates/penelope-kernel/src/turn.rs:651-664`), et avant de servir la première requête :

1. pour chaque session dont le dernier tour est ouvert, écrire `turn.finished
   {reason:"interrupted", turn_id, attempt}` ; rien n'est tronqué, rien n'est inventé
   (principe de `persistence.md`, « Crash recovery preserves an interrupted turn ») ;
2. le tour rejoué par la file écrit `turn.started {attempt: n+1}` ; `pending_calls` sur la
   queue dérivée retrouve les appels sans résultat (`agent.rs:2387-2408`), et le ledger
   d'effets décide : rejoué depuis son résultat, question au propriétaire, ou exécution
   (`agent.rs:1695-1728`) ;
3. un appel dont la réponse du modèle n'a pas été journalisée (crash avant `conv.assistant`)
   est simplement absent : la requête repart du dernier nœud, comme aujourd'hui ;
4. `llm_requests` fait son propre rattrapage (`state.rs:230-244`) ; `conv.attempt` n'est pas
   écrit pour un crash (le processus est mort avant).

La rédaction du message utilisateur devient idempotente **par le journal** : `conv.user`
porte `turn_message_id` ; avant d'écrire, le tour cherche un `conv.user` de même
`turn_message_id` dans la session (index sur `json_extract(payload,'$.turn_message_id')` ou
colonne dérivée dans `messages.source_turn_id`, déjà unique, `migrations.rs:948-949`). La
clé `kv turn.recorded.<tour>` disparaît en phase 4.

## 3. Le cache de prompt

### 3.1 Démonstration : rien ne bouge avant le dernier message

Soit deux requêtes successives R1 et R2 d'une session, sans événement de remplacement entre
elles. Par construction du pliage (§2.3) :

- le message système de R2 est `messages[system]` ; sans `conv.system` entre R1 et R2, c'est
  le même événement, donc le même texte octet pour octet ;
- les nœuds de R2 sont ceux de R1 suivis des nœuds ajoutés par `append` : un `append` ne
  modifie ni l'ordre ni le contenu des nœuds existants ; un événement est immuable ;
- le bloc volatil d'un message utilisateur est `contexts[seq]`, écrit une fois par
  `conv.context` (le pliage garde la première occurrence) : le bloc d'un ancien message ne
  change pas quand un nouveau tour arrive ; le dernier message reçoit le sien avant le
  premier envoi, comme `freeze_volatile` le fait aujourd'hui (`cache_audit.rs:203-230`) ;
- la note de fusion (§2.3, point 3) est insérée juste après le système ; elle apparaît quand
  un `conv.user mid_turn` est écrit, c'est-à-dire au moment où V0 l'insère aussi
  (`conversation.rs:87-98`) : même frontière, pas de nouveau raté ;
- la consigne de relance (§2.3, point 4) est un dernier message utilisateur, après tout le
  préfixe : elle n'invalide rien devant elle, comme en V0 (`agent.rs:673-676`).

Donc R2 commence par R1 octet pour octet : c'est exactement `ca_5_4_each_request_extends_the_previous_one`
(`cache_audit.rs:339-347`), qui reste le test de référence et tourne sur la V1 en phase 3.

### 3.2 Où un `replace` peut apparaître, et pourquoi le cache ne le paie pas

| Événement | Quand il est écrit | Pourquoi le préfixe ne bouge pas « avant le dernier message » |
|---|---|---|
| `conv.summary` | `compaction::publish`, à la fin du tour ou avant l'appel d'une session froide, d'un fork, ou après un dépassement prouvé (`daemon/compaction.rs:711-723`, `462-520`, `1328-1345`) | frontière de cache déjà assumée par la V0 (`docs/context.md`, « Le cache ») : le résumé change le début de l'historique quand le cache est froid ou perdu. V1 n'ajoute aucune autre occasion. |
| `conv.tool_result` replace (niveau 1) | à l'enregistrement d'un gros résultat (`conversation.rs:277-288`) ou après l'admission d'un groupe (`agent.rs:1668`, avant la requête suivante) | le nœud remplacé n'a **jamais** été envoyé : l'admission précède la première requête qui le contient. Le pliage garde l'adresse du nœud, donc rien ne se décale. |
| `conv.system` replace | premier tour de la session, ou quand `stable_prefix` constate un cache froid (`cache_audit.rs:246-248`), ou juste après un `conv.summary` | mêmes conditions que la V0 : le préfixe en attente sort à une pause de plus de cinq minutes ou à une compaction (0008, point 3). |
| `conv.rewind` | `/rewind`, hors tour (`session_ops.rs:186-188`) | le propriétaire réécrit l'historique : le raté de cache est le prix du retour arrière, comme en V0 (`session.rewound`). |

Interaction précise d'un `replace` de compaction avec le préfixe : la requête après
publication vaut `[system][résumé (nœud k+1..)][nœuds non couverts…]`. Le message système est
inchangé (aucun `conv.system` n'est impliqué), donc chez un fournisseur qui met en cache par
bloc (marqueur `cache_control` sur le système, `tiers.rs:107-111`, `139-141`) le préfixe
système reste servi ; la partie historique est un raté, expliqué `historique` par
`miss_cause` (`cache_audit.rs:177-186`). Si un préfixe modifié attendait (`stable_prefix`),
il sort **dans le même tour** que le résumé, comme aujourd'hui `refresh_snapshot` est appelé
à la publication (`daemon/compaction.rs:1114-1116`) : un seul raté au lieu de deux.

### 3.3 Ce qui change pour le diagnostic

`miss_cause` reste calculé sur les messages réellement envoyés (`cache_audit.rs:28-54`,
`158-192`). Deux précisions deviennent possibles : la cause `historique` peut nommer
l'événement de remplacement responsable (le dernier `replace` entre les deux appels), et
`audit show` devient exact après compaction, puisque la surface au moment de l'appel se
recalcule en pliant le journal jusqu'au `seq` du `conv.assistant` (le `request_hash` logé
dans l'événement le prouve : rejouer `Fingerprint::of` sur la surface dérivée doit redonner
la même chaîne, sinon `audit show` dit `exact: false`).

## 4. Migration expand-contract

Quatre phases, chacune livrable sous forme de versions successives sur `main`, la V0 restant
le chemin de production jusqu'à la phase 3. Une clé `history.source = "tables" | "journal"`
(défaut `tables`) pilote la bascule ; elle disparaît en phase 4.

### 4.1 Phase 1 : double écriture (expand)

Chaque écriture V0 de contenu s'accompagne de son événement `conv.*`, écrit par le même
thread écrivain, dans l'ordre. Les tables V0 restent la source de lecture. `messages` et
`lcm_nodes` gagnent `event_id`. Rien ne change pour le modèle, pour le cache, pour les tests
existants : ils continuent de lire les tables.

### 4.2 Phase 2 : comparaison

`penelope history verify` dérive chaque session depuis le journal (préfixe scellé compris)
et compare aux tables. En CI, sur les bases produites par les scénarios de session
enregistrés (T0) ; sur la machine du propriétaire, par `doctor`. La phase 3 ne commence que
quand la comparaison est à zéro divergence sur tous les scénarios (compaction, fork, rewind,
niveau 1, purge, crash simulé) et que leur `surface.expected.json` n'a pas bougé depuis la
V0.

### 4.3 Phase 3 : bascule de lecture

Avec `history.source = "journal"`, `request_messages`, `tail`, les outils d'historique,
l'audit et la relecture d'épisode lisent les caches alimentés par le projecteur (qui ne lit
que le journal). En mode `tables` (défaut jusqu'à validation), une assertion de test compare
les deux projections à chaque requête (`debug_assert!` + test dédié) : si elles divergent
d'un octet, le test échoue et nomme la session. Quand la suite est verte sous les deux modes,
le défaut passe à `journal`.

### 4.4 Phase 4 : retrait du chemin direct (contract)

Les écritures V0 directes (`append`, `append_user_turn_at`, `externalise`, `mark_compacted`,
`copy_messages`, `truncate_from`, `freeze_context`) deviennent internes au projecteur ou
disparaissent ; les clés `kv turn.recorded.*`, `prompt.prefix.*`, `compaction.pending.*`
(remplacée par un `conv.summary` en attente ? non : le résumé préparé mais non publié n'est
pas un contenu vu par le modèle, il reste en `kv`) sont retirées de `EPHEMERAL_KEYS`
(`purge.rs:33-54`) quand elles ne sont plus écrites ; la clé `history.source` est retirée ;
un test d'architecture interdit tout `INSERT`/`UPDATE`/`DELETE` sur `messages`,
`message_context`, `lcm_nodes` hors du projecteur.

### 4.5 Scellement de l'historique existant

On ne peut pas fabriquer rétroactivement une chaîne de hachage : les messages V0 n'ont pas
d'événement, et en ajouter un par message maintenant produirait des maillons datés
d'aujourd'hui pour des contenus d'hier (l'alternative « un événement d'import par message »
est discutée au §7). La V1 scelle donc chaque session existante par **un** événement
`conv.import` :

```
  conv.import {
    "v": 1,
    "surface": {"op": "seal", "messages": 548, "offset": 548},
    "contexts": 112,
    "lcm_active": [{"node": "n_…", "from": 1, "to": 512, "superseded_by": null}],
    "digest": sha256( canonical_json([ lignes messages (seq, role, content, tool_call_id,
                       tool_name, artifact_id, compacted, episode, ts),
                       lignes message_context, nœuds lcm actifs ]) )
  }
```

- Écrit une fois par session ayant au moins un message, au premier démarrage après la
  migration 0019, par une étape de boot idempotente (le journal a besoin de `EventLog` et de
  l'horloge, pas d'une migration SQL). Une session sans message n'est pas scellée.
- Les lignes du préfixe sont marquées `sealed = 1` (colonne de la migration 0019) et ne sont
  jamais touchées par `reindex` ; `verify` recalcule le digest et signale toute modification
  du préfixe. Un `/rewind` qui coupe dans le préfixe ne les efface pas : il les passe à
  `sealed = 2` (masquées), la coupe s'appliquant au pliage par-dessus le préfixe ; toute
  lecture de `messages` hors de `penelope-context` filtre `sealed IS NOT 2` (correctif
  i-rewind-scelle, 26/09/2026).
- La dérivation d'une session scellée = préfixe (lignes `sealed`) puis événements après
  l'import ; `offset` = `MAX(messages.seq)` du préfixe, donc tout nouveau nœud a une adresse
  supérieure.
- Une session scellée redevient « pure journal » quand elle est purgée : la purge efface le
  préfixe comme aujourd'hui, et le `conv.import` purgé n'apporte plus rien.
- Les nœuds LCM actifs du préfixe sont listés dans l'import (bornes en adresses V0) : un
  `conv.summary` ultérieur peut les prolonger (`previous_node_id`) comme `extend` le fait
  (`lcm.rs:219-236`).

## 5. Découpage en tâches

Conventions : S = moins d'une demi-journée, M = une journée, L = deux à trois jours. Chaque
tâche est livrable seule, avec sa section dans `docs/progress.md` et son bump
(`CLAUDE.md`, « Un lot, une version, une release »). Les tâches T0 à T4 sont indépendantes
et peuvent partir en parallèle ; T0 doit être livrée avant T5. Les numéros de version ne sont
pas fixés ici : la 0.17.59 est prise par le lot #205 en cours (`audit.rs:52-53`).

```
  T0 scénarios enregistrés ──(préalable)──► T5 ; relus par T12, T14, T22
  T1 vocabulaire ─┐
  T2 append_with ─┼─► T5 double écriture messages ─► T6 system/context
  T4 turn.finished┘          │                        T7 summary
                             │                        T8 niveau 1
  T3 derive (pur) ◄──────────┤                        T9 attempts (#206)
        │                    │                        T10 fork/rewind
        │                    └───────────────────────► T18 flux runtime
        ├─► T21 seq à trous            T20 crash (T4, T5)
        └─► T11 scellement ─► T12 verify ─► T13 projector + reindex ─► T14 bascule
                                                                          │
                                                   T15 outils/audit ◄─────┤
                                                   T17 purge/rétention ◄──┤ (T9)
                                                   T22 CA ◄───────────────┘
                                                            T16 retrait ─► T19 décision, docs
```

**T0. Scénarios de session enregistrés.** (M) Le filet de régression de toute la migration,
transposé des transcriptions rejouables sans clé de DSH (`dsh/docs/testing.md:53`, section
« When a snapshot test is required » ; `dsh/snapshots/session/`, 118 scénarios, un
`session.vN.jsonl` normalisé plus un manifeste, règles dans
`dsh/packages/test-support/session-snapshot/README.md`, « Writing a snapshot suite »).
Ce qui existe déjà côté Pénélope : le `MockProvider` rejoue un script par appel (texte,
appels d'outils, erreur, dépassement de contexte, flux coupé après texte, réponse vide,
`crates/penelope-llm/src/mock.rs:15-44`) et garde chaque requête reçue (`mock.rs:186-189`) ;
`live::turn` enchaîne mise en file, réclamation et `runner::process` (`crates/penelope-evals/src/live.rs:86-106`,
mais avec une clé OpenRouter) ; `resilience::boot` simule un vrai redémarrage sur le même
répertoire (`crates/penelope-evals/tests/resilience.rs:14-22`) ; `ctx_recall` plante des
faits puis compacte, avec un vrai modèle (`crates/penelope-evals/tests/ctx_recall.rs`). Il
manque le format enregistré et la suite sans clé. Périmètre : nouvelle suite
`crates/penelope-evals/tests/sessions.rs` et un dossier `crates/penelope-evals/scenarios/<nom>/`
par scénario : `scenario.json` (messages du propriétaire, script `Scripted` par appel,
avance de `TestClock` entre les tours, commandes `/compact`, `/fork`, `/rewind`, purge,
redémarrage), et deux fichiers attendus régénérés par `UPDATE_SESSIONS=1` (convention de
`UPDATE_DOCS`, `crates/penelope-evals/tests/docs.rs:245`) : `events.expected.jsonl` (kinds et
payloads des événements de la session, identifiants, horodatages et hash remplacés par des
jetons stables, comme la normalisation DSH) et `surface.expected.json` (chaque requête vue
par `MockProvider::requests()`, rédigée). Douze scénarios initiaux : tour simple ; lectures
parallèles (#85) ; gros résultat niveau 1 et groupe (#52) ; compaction manuelle puis
prolongation ; session froide (#40) ; dépassement prouvé (`Scripted::ContextOverflow`) ;
flux coupé après texte (`MidStreamError`) ; réponse vide relancée (#206) ; messages fusionnés
(#161) ; fork ; rewind ; purge ; crash entre la réponse et le résultat d'outil (deux vies).
Dépendances : aucune. Fin : la suite passe sur la V0 telle quelle ; un déplacement volontaire
de la note de fusion fait échouer `surface.expected.json` en nommant le scénario et l'appel ;
T5 doit laisser `surface.expected.json` intact et n'ajouter à `events.expected.jsonl` que des
`conv.*` ; T12 et T14 tournent sur ces bases.

**T1. Nommer les événements de contenu.** (S) Périmètre : nouveau module
`crates/penelope-context/src/journal.rs` (types Rust des payloads `conv.*`, `Surface`,
sérialisation avec `v`, `KIND_*` constants, `upgrade_payload` identité). Dépendances : aucune.
Fin : tests de va-et-vient sérialisation pour chaque kind ; un payload `v: 2` est refusé avec
`DeriveError::Format` ; un kind `conv.inconnu` sans `ignorable` est refusé, avec `ignorable`
il est accepté.

**T2. `EventLog::append_with`.** (S) Périmètre : `crates/penelope-kernel/src/event.rs` :
`append_in(tx, draft) -> Event` (synchrone, utilisable dans une transaction) et
`append_with(draft, after: FnOnce(&Transaction, &Event) -> Result<()>)` qui exécute `after`
dans une **seconde** transaction du même thread écrivain, sous le même verrou d'ordre,
avant la diffusion. Dépendances : aucune. Fin : la chaîne reste vérifiée ; un `after` en
erreur laisse l'événement commité et renvoie l'erreur ; `live_events_follow_the_committed_log`
et `concurrent_appends_keep_a_single_chain` restent verts.

**T3. Le pliage pur `derive`.** (M) Périmètre : `crates/penelope-context/src/derive.rs` :
`derive(prefix: &Sealed, events: &[Event]) -> Result<Surface, DeriveError>`, `Surface ->
Vec<Entry>` et `Surface -> Vec<ChatMessage>` (système, contextes, note de fusion, consigne de
relance). Dépendances : T1. Fin : tests unitaires sans base : append seul = préfixe étendu
octet pour octet ; `replace` invalide (nœud absent, ordre inversé, système non couvert par
un système) refusé ; payload purgé ignoré ; kind d'observation ignoré ; `cut` puis `append` ;
`inherit` récursif sur deux niveaux ; un `conv.attempt` en fin de journal ajoute la consigne
de relance, un `conv.assistant` qui le suit l'efface.

**T4. Fermer chaque tour.** (S) Périmètre : `agent.rs` `run_conversation` (toutes les
sorties), `engine.rs` `execute_turn` (échec avant la boucle, fournisseur indisponible),
payload de `turn.started`. Dépendances : aucune. Fin : un test par variante de `TurnOutcome`
vérifie qu'un `turn.finished` de même `turn_id` suit `turn.started` avec la bonne `reason` ;
les tests de `daemon/compaction.rs` qui lisent l'ordre des kinds (`kinds(events)`,
`daemon/compaction.rs:2013`) restent verts ; `docs/runtime-events.md` liste les raisons.

**T5. Double écriture des messages.** (M) Périmètre : migration `0019_history_journal`
(`messages.event_id`, `messages.sealed`, `lcm_nodes.event_id`) ; `HistoryStore::append` et
`append_user_turn_at` écrivent `conv.user` / `conv.assistant` / `conv.tool_result` via
`append_with`, la ligne `messages` reçoit `event_id` dans la transaction `after` ; les champs
`llm_request_id`, `usage`, `request_hash`, `projection.steps` de `conv.assistant` sont
passés par `record` (signature élargie : `record(message, eager, Provenance)`).
Dépendances : T1, T2. Fin : après un tour simulé (MockProvider), chaque ligne de `messages`
a un `event_id` dont le payload redonne le même `ChatMessage` ; `upgrade_from_each_previous_version`
passe ; `queued_user_message_keeps_arrival_time_and_is_idempotent` reste vert et l'événement
porte `arrived_at`.

**T6. Journaliser le prompt système et le contexte figé.** (S) Périmètre : `cache_audit.rs`
(`freeze_volatile` écrit `conv.context` en plus de `message_context` ; `stable_prefix` écrit
`conv.system` quand le préfixe retenu diffère du dernier journalisé, avec `reason`),
`prompt_snapshot::record` alimenté par l'événement. Dépendances : T1, T2, T5. Fin : deux tours
sans rechargement → un seul `conv.system` ; skill rechargée à cache chaud → aucun ; à cache
froid → un second avec `reason: cold` ; après compaction → `reason: compaction` ;
`two_turns_without_a_reload_share_one_snapshot` reste vert.

**T7. Journaliser la compaction.** (M) Périmètre : `context/engine.rs` `apply_summary` écrit
`conv.summary` (replace des bornes, texte rendu, ancres, `previous_node_id`) via
`append_with`, la transaction `after` faisant `insert_leaf`/`extend` et `mark_compacted` ;
`lcm_nodes.event_id`. Dépendances : T1, T2, T5. Fin : `ctx_safety_level3_publication_is_idempotent`
et `recompaction_extends_the_previous_summary` restent verts ; le `conv.summary` porte le même
texte que le nœud ; republier le même travail n'écrit pas de second événement (clé
d'idempotence `SummaryJob::idempotency_key`, `context/engine.rs:81-92`, vérifiée avant
l'écriture) ; `context.compacted` continue de suivre.

**T8. Niveau 1 comme remplacement d'un nœud.** (S) Périmètre : `admit_tool_group`
(`context/engine.rs:265-316`) : artefact écrit d'abord, puis `conv.tool_result` replace du
nœud avec `artifact_id` et `artifact_sha256`, puis `externalise` dans `after`.
Dépendances : T5. Fin : `huge_tool_results_are_externalised` et
`parallel_tool_results_are_admitted_as_one_group` verts ; l'événement de remplacement cite
un `call_id` égal à celui du nœud remplacé ; l'artefact existe avant l'événement (ordre des
identifiants).

**T9. Tentatives hors surface (#206).** (M) Périmètre : `agent.rs` `call_model` (flux coupé
après texte : `conv.attempt` avec `partial_text`, message au propriétaire qui cite le début
conservé ; erreur avant flux : `conv.attempt cause before_stream` par tentative ; repli :
`cause fallback`), `run_conversation` (réponse vide : `conv.attempt cause empty_answer` avec
`retry_prompt` ; la consigne n'est plus poussée à la main, elle vient de la dérivation en
phase 3, et reste poussée à la main en phase 1 avec un test d'égalité) ; plafond de dix
tentatives par tour ; `penelope logs --turn` les montre. Dépendances : T1, T4, T5. Fin :
flux coupé après 200 caractères → tour en échec, `conv.attempt` en base, `request_messages()`
du tour suivant identique avec et sans tentative ; `/stop` inchangé (le partiel entre dans
`conv.assistant interrupted`) ; trois replis avant flux → trois tentatives, une ligne d'usage ;
`a_stream_cut_before_any_text_is_retried_then_falls_back` et
`an_empty_answer_is_retried_once_then_reported` verts.

**T10. Fork et retour arrière journalisés.** (S) Périmètre : `session_ops.rs` : `conv.fork`
premier événement de la fille (avec `offset`), `conv.rewind` avant `truncate_from` ; la copie
V0 reste. Dépendances : T1, T5. Fin : `fork_copies_then_diverges` et
`rewind_archives_what_it_removes` verts ; les deux événements sont présents avec des bornes
qui désignent des nœuds existants (vérifié par `derive`).

**T11. Scellement.** (M) Périmètre : migration 0019 (colonnes `sealed`, suppression de
`projections_workflow` et `projections_approval`), étape de boot
`history::seal_legacy(services)` dans `runtime.rs`, digest canonique, marquage `sealed`,
`conv.import` par session. Dépendances : T1, T2. Fin : une base de fixture (trois sessions,
dont une compactée et une vide) : premier démarrage → deux `conv.import` ; second démarrage →
aucun ; `derive` d'une session scellée redonne le préfixe ; `upgrade_from_each_previous_version`
vert.

**T12. `penelope history verify`.** (M) Périmètre : `crates/penelope-daemon/src/history.rs`
(nouveau), méthode RPC `history.verify`, commande CLI, ligne `doctor`. Dépendances : T0, T3,
T5 à T8, T10, T11. Fin : sur une base produite par les scénarios de la suite (tour simple,
outils, compaction, niveau 1, fork, rewind, purge), zéro divergence ; un `UPDATE messages SET
content` manuel → une divergence qui nomme la session et le nœud ; un préfixe scellé modifié →
divergence de digest.

**T13. Projecteur et `reindex`.** (M) Périmètre : `crates/penelope-context/src/projector.rs`
(`apply(tx, event)` pour chaque kind `conv.*`, filigrane dans `projections_session`,
`FOLD_VERSION`), `penelope history reindex`, rattrapage depuis le filigrane à l'ouverture
d'une session. Dépendances : T3, T11. Fin : effacer toutes les lignes non scellées puis
`reindex` → `verify` à zéro ; incrémenter `FOLD_VERSION` → refonte automatique ; un crash
simulé entre l'événement et la projection (écriture `after` qui échoue) est rattrapé au
prochain accès.

**T14. Bascule de lecture.** (L) Périmètre : `conversation.rs` (`projected_entries`, `tail`),
`agent.rs` (`pending_calls` inchangé, il lit `tail`), clé `history.source`, assertion de
comparaison V0/V1 en mode `tables`, rejouée sur les scénarios de T0. Dépendances : T0, T12,
T13. Fin : la suite complète passe
avec `history.source = journal` et avec `tables` ; `ca_5_4_each_request_extends_the_previous_one`,
`the_projection_only_reads_what_is_not_summarised`, `ctx_safety_recovery_after_crash`
verts sous les deux ; le défaut passe à `journal` dans le même lot si tout est vert.

**T15. Lecteurs secondaires.** (S) Périmètre : outils `history_*` (`executor.rs:1140-1202`),
`audit::show` (reconstitution exacte par pliage jusqu'au `seq` de l'appel, `exact: true`
après compaction), relecture d'épisode, export (`session_lines` exporte le journal et le
préfixe scellé, plus la surface dérivée). Dépendances : T14. Fin :
`a_turn_is_replayed_from_its_fingerprint` reste vert et un nouveau test le rejoue **après**
une compaction avec `exact: true` ; `export_writes_jsonl_and_rebuild_restores_search` vert.

**T16. Retrait du chemin direct.** (M) Périmètre : `HistoryStore` (méthodes d'écriture
internes au projecteur), suppression de `copy_messages`, `truncate_from`, `externalise`
publics ; clés `kv` retirées (`turn.recorded.*`, `prompt.prefix.*`) et retirées de
`EPHEMERAL_KEYS` ; clé `history.source` retirée ; test d'architecture (`penelope-archtest`)
qui interdit les écritures SQL sur les caches hors `penelope-context`. Dépendances : T14,
T15. Fin : `cargo test --workspace` vert ; le test d'architecture échoue si l'on réintroduit
un `INSERT INTO messages` ailleurs.

**T17. Purge et rétention.** (S) Périmètre : `purge.rs` (purge d'une session : caches
effacés, `conv.*` purgés par le mécanisme existant ; message d'avertissement si des forks
dérivent de la session) ; rétention : les `conv.attempt` de plus de `retention.days` sont
purgés **par payload** via `event_purges` (même mécanisme que la purge, hash conservé), pour
que le texte partiel ne survive pas à la comptabilité qu'il accompagne. Dépendances : T9,
T13. Fin : `word_is_gone` étendu à `events.payload` des `conv.*` ; `reindex` d'une session
purgée → surface vide ; rétention → `conv.attempt` anciens purgés, `verify` toujours à zéro,
`audit verify` toujours `ok`.

**T18. Flux runtime et `tail`.** (S) Périmètre : `runtime_events.rs` (rien à coder si
`bounded_redacted` suffit ; test), `docs/runtime-events.md` (catalogue `conv.*`, mention que
le contenu transite, local seulement, rédigé, borné). Dépendances : T5. Fin : un consommateur
abonné à `runtime.tool` ne reçoit aucun `conv.*` ; un consommateur sans filtre reçoit un
`conv.assistant` rédigé et borné à 64 Kio ; `public_frame_keeps_order_and_redacts_payload`
vert.

**T19. Décision et documentation.** (S) Périmètre : `docs/decisions/0011-journal-source-unique.md`
(format des 0008 et 0009 : contexte, décision, raisons, conséquences), `docs/context.md`
section « Le journal », `docs/README.md` index, `docs/install-headless.md` commandes
`history verify` et `reindex`, `UPDATE_DOCS=1 cargo test -p penelope-evals --test docs`.
Dépendances : T16. Fin : test `docs` vert ; la décision cite les issues #205, #206 et ce
document.

**T20. Reprise après crash.** (S) Périmètre : étape de boot dans `runtime.rs` après
`recover_on_boot` : `turn.finished {reason: interrupted}` pour tout tour ouvert ;
`turn.started.attempt`. Dépendances : T4, T5. Fin : base de fixture avec un `turn.started`
sans fin et un `conv.assistant` à appel sans résultat → au démarrage : un `turn.finished
interrupted` ; le tour rejoué voit l'appel en attente ; `an_uncertain_effect_marked_done_is_replayed_not_rerun`
et voisins verts.

**T21. Numérotation à trous.** (S) Périmètre : `SummaryJob::messages()` (compte les entrées
au lieu de `to - from + 1`, `context/engine.rs:95-97`), `summarizer_messages` (bornes
affichées, `context/engine.rs:108-116`), `Lcm::coverage_gaps` (déprécié ou réécrit sur les
nœuds dérivés), `history_expand` (pagination par nœud), `rewind` (coupe sur adresse de nœud).
Dépendances : T3. Fin : tests unitaires avec des adresses `1, 2, 5, 9` ; les tests
`split_*`, `summary_job_*` restent verts.

**T22. Critères d'acceptation.** (M) Périmètre : `ca_4_5_model_visible_is_logged` (pour un
tour simulé avec outils, compaction et niveau 1 : `request_messages()` égale
`derive(journal)` assemblé, octet pour octet, à chaque appel) ; `ca_5_5_replace_only_at_a_turn_boundary`
(aucun événement `replace` entre deux appels d'un même tour, hors niveau 1 sur un nœud jamais
envoyé) ; `ca_4_6_reindex_is_lossless` ; `UPDATE_CA_MATRIX=1`. Dépendances : T14. Fin : les
trois tests dans `docs/ca-matrix.md`.

Ordre de livraison proposé : T0, T1, T2, T4 → T3, T5 → T6, T7, T8, T10, T21, T18 → T9, T20
→ T11 → T12 → T13 → T14 → T15, T17, T22 → T16 → T19. Soit 23 tâches : 12 S, 10 M, 1 L.

## 6. Ce qui ne doit pas régresser

Tests et critères existants à garder verts à chaque tâche (les chemins sont ceux de
l'arbre de travail) :

Noyau et chaîne d'audit :
- `chain_is_linked_and_verifies`, `ca_4_1_detects_tampering`, `seq_is_per_session`,
  `a_duplicate_seq_is_refused_by_the_database`, `live_events_follow_the_committed_log`,
  `concurrent_appends_keep_a_single_chain`, `purge_erases_content_but_keeps_chain_verifiable`,
  `events_with_any_float_verify_after_storage` (`crates/penelope-kernel/src/event.rs:445-675`).
- `a_read_error_fails_the_append_instead_of_forging_a_genesis_link` (issue #47,
  `event.rs:469-501`) : `append_with` ne doit pas contourner cette règle.

Contexte et compaction (`ctx-safety`) :
- `ctx_safety_level0`, `_level1`, `_level2`, `_level3_publication_is_idempotent`,
  `ctx_safety_recovery_after_crash`, `ctx_safety_level4`, `a_crash_between_node_and_marking_is_repaired`,
  `recompaction_extends_the_previous_summary`, `only_a_forced_compaction_summarises_a_short_history`
  (`crates/penelope-context/src/engine.rs:707-1077`).
- `ca_5_1_level4_proves_it_fits`, `level0_only_touches_eager_tool_results`,
  `level2_degrades_until_it_fits`, `split_never_cuts_inside_a_tool_group`
  (`crates/penelope-context/src/compaction.rs:765-1003`).
- `leaf_then_condensed_builds_a_dag`, `recompaction_updates_instead_of_restarting`,
  `extension_absorbs_the_following_messages` (`lcm.rs:484-588`) : le DAG reste supporté en
  lecture même si `insert_condensed` n'est plus appelé.
- `grouping_keeps_tool_calls_with_their_results`, `repair_is_idempotent`
  (`transcript.rs:251-317`).

Cache de prompt (CA 5) :
- `ca_5_3_prefix_is_byte_identical_across_turns` (`tiers.rs:484`),
  `ca_5_4_each_request_extends_the_previous_one` (`cache_audit.rs:285`),
  `a_miss_is_explained_by_what_changed` (`cache_audit.rs:377`),
  `the_stable_prefix_does_not_move_between_turns`, `recorded_messages_come_back_in_the_request`,
  `the_projection_only_reads_what_is_not_summarised` (#55),
  `parallel_tool_results_are_admitted_as_one_group` (#52), `a_lone_tool_result_is_left_whole`,
  `huge_tool_results_are_externalised` (`conversation.rs:1015-1221`).
- `the_same_prompt_is_written_once`, `the_volatile_tier_never_enters_the_snapshot`,
  `a_reloaded_skill_names_the_index_tile`, `two_turns_without_a_reload_share_one_snapshot`
  (`prompt_snapshot.rs:198-304`) ; `a_turn_is_replayed_from_its_fingerprint`,
  `a_purged_prompt_is_announced_not_invented`, `a_secret_inside_the_prompt_never_comes_back_in_the_clear`
  (`audit.rs:296-394`).

Compaction de fond (daemon) :
- `manual_compaction_replaces_old_turns_with_a_summary`, `a_summary_ready_during_a_turn_waits_for_its_end`,
  `three_failures_compact_without_a_model_and_say_so` (#131), `a_passing_failure_is_retried_on_a_shorter_request`,
  `the_fallback_summarizer_stays_within_the_reserve`, `a_failed_summary_cools_down_until_compact_is_forced`,
  `a_turn_over_the_threshold_compacts_in_the_background`, `a_proven_overflow_compacts_then_retries_once`,
  `the_billed_prompt_size_requests_a_background_compaction` (#40),
  `budgets_do_not_block_compaction_until_the_summary_reserve_is_spent`,
  `a_cold_session_is_compacted_before_the_model_call`, et les trois tests de fidélité (#179)
  (`crates/penelope-daemon/src/compaction.rs:1394-2200`).

Historique, sessions, purge :
- `append_and_load_roundtrip`, `reasoning_survives_a_reload`,
  `queued_user_message_keeps_arrival_time_and_is_idempotent` (#161),
  `fts_finds_messages_without_accents`, `artifact_cursor_only_advances_over_returned_bytes`,
  `externalise_rewrites_the_canonical_body` (`store.rs:1162-1355`) ; ce dernier devient un
  test du projecteur en phase 4.
- `fork_copies_then_diverges`, `rewind_archives_what_it_removes`,
  `export_writes_jsonl_and_rebuild_restores_search` (`session_ops.rs:383-469`). Adaptation
  assumée pour `rewind_archives_what_it_removes` : l'assertion `seq == 3` après reprise
  (`session_ops.rs:422-424`) devient « strictement supérieur au dernier nœud restant ».
- `word_is_gone` (neuf tables, `purge.rs:759-800`), `purging_a_session_takes_the_prompts_only_it_used`,
  `retention_only_drops_prompts_nothing_points_to`, et les tests de rétention existants
  (`purge.rs:619-1100`).

Boucle d'agent :
- `an_uncertain_effect_marked_done_is_replayed_not_rerun`, `_retried_runs_once`,
  `_ignored_is_not_rerun`, `completed_effects_are_replayed_not_reexecuted`
  (`agent.rs:3041-3326`) ; `cancellation_stops_the_turn`, `a_stop_during_the_retry_wait_ends_the_turn`,
  `a_stream_cut_before_any_text_is_retried_then_falls_back`, `an_empty_answer_is_retried_once_then_reported`,
  `pending_calls_ignore_answered_and_abandoned_ones` (`agent.rs:3482-4196`).

Suites et contrats :
- Les scénarios de session enregistrés (T0) : `surface.expected.json` de chaque scénario ne
  change pas avant la phase 3, et pas d'un octet à la phase 3 ; `events.expected.jsonl` ne
  gagne que des `conv.*` et les `turn.finished` de T4.
- `penelope-evals` : `docs` (clés, commandes, index), `ca_matrix`, `hot_reload`, `security`,
  `resilience` (dont `ca_17_1`, `ca_17_4`, `ca_17_6`, `crates/penelope-evals/tests/resilience.rs:31-458`),
  `ctx_recall` (réseau, à lancer une fois avant la bascule de la phase 3).
- Le contrat du flux runtime (`docs/runtime-events.md`, `runtime_events.rs:29-53`) : les
  trames des kinds existants ne changent pas de forme.
- Le §1 (premier retour en moins de 1,5 s) : aucune écriture nouvelle sur le chemin du
  premier jeton ; `conv.assistant` est écrit là où `record` l'est déjà (`agent.rs:879`),
  `conv.system` et `conv.context` là où `kv` et `message_context` le sont (`engine.rs:569-570`).

## 7. Risques et points ouverts

Risques :

1. **Un octet de différence entre la dérivation et la projection V0 casse le cache.** C'est
   le risque principal : une note de fusion placée un cran plus loin, un contexte figé rattaché
   au mauvais message, un espace en fin de résumé. Parade : la phase 3 tourne les deux
   projections en parallèle avec assertion, et `ca_5_4` tranche. À ne pas livrer sans.
2. **Volume et vitesse de vérification.** Le contenu entre dans `events` : `audit verify`
   rejoue tout (`event.rs:290-362`), le flux runtime rejoue depuis un curseur, la base
   grossit d'à peu près la taille de `messages`. Parade : l'artefact garde les gros corps
   (les événements restent sous 25 000 tokens, `context.large_payload_tokens`), `verify
   --since`, et le poids dans `doctor`. Ordre de grandeur sur la session « Fidelatoo »
   (548 messages, issue #131) : quelques Mo.
3. **Deux sources pour les sessions scellées.** Jusqu'à leur purge, leur préfixe n'est pas
   dans le journal ; `verify` couvre le digest, mais un bug de pliage sur la jonction
   préfixe/journal (offset, nœud LCM à cheval) ne se verrait que sur ces sessions. Parade :
   fixture de base scellée dans la suite, fork et compaction testés sur elle.
4. **Numérotation à trous.** Toute arithmétique `seq + 1` cachée est une régression
   silencieuse (T21 en recense quatre : `context/engine.rs:96`, `112`, `conversation.rs:124`,
   `session_ops.rs:422-424`). Parade : test qui numérote `1, 2, 5, 9` dès T3.
5. **Le projecteur en retard.** Deux transactions au lieu d'une : entre l'événement et sa
   projection, un lecteur peut voir un cache en retard. Le rattrapage au prochain accès à la
   session couvre le crash ; pour la concurrence, le projecteur tourne sur le même thread
   écrivain sous `live_order`, donc un lecteur qui passe par le writer (toutes les écritures)
   voit l'état projeté ; un lecteur du pool peut lire un cache d'un événement en retard le
   temps d'une transaction. Aucun chemin de la boucle ne lit le cache entre les deux
   (`record` attend le retour de `append_with`).
6. **Purge et forks.** Par référence, un fork perd son préfixe quand le parent est purgé.
   C'est plus juste (RGPD : le texte purgé ne survit pas dans une copie) mais c'est un
   changement de comportement visible ; `purge` doit l'annoncer et `history verify` ne doit
   pas le compter comme divergence.
7. **Refactorisation des tests.** Beaucoup de tests fabriquent l'historique par
   `HistoryStore::append` direct (`engine.rs` tests, `session_ops.rs:372-380`,
   `compaction.rs` tests) ; en phase 4 ils doivent passer par l'API du journal. C'est le gros
   de la taille L de T14 et du M de T16.
8. **Les lots #205 et #206 en cours.** La V1 s'appuie sur `prompt_snapshots`, `RequestKeys`
   et `turn.started.system_hash` (non commités). Ils doivent être livrés avant T6 et T9 ;
   sinon T6 et T9 les absorbent, ce qui grossit les lots.

Points ouverts, à trancher par le propriétaire :

- **Un événement d'import par message plutôt qu'un scellement.** Alternative au §4.5 :
  copier chaque ligne V0 en un `conv.user`/`conv.assistant`/`conv.tool_result` daté
  d'aujourd'hui avec `imported: true` et le `ts` d'origine dans le payload. Avantage : plus de
  double source, `reindex` total dès la V1. Coût : une migration de boot longue (toutes les
  sessions), une chaîne alourdie d'autant, et des maillons dont la date de chaîne ne dit rien
  de la date du contenu. Je recommande le scellement, et l'import par message comme option
  `penelope history import --session` pour une session qu'on veut « pure ».
- **`conv.system` par session ou dédupliqué globalement.** Le texte du préfixe est écrit une
  fois par changement et par session (quelques dizaines de Ko par session). L'index
  `prompt_snapshots` reste dédupliqué. Si le volume gêne, `conv.system` peut ne porter que
  `hash` et pointer vers l'instantané, au prix de l'invariant « le journal se suffit ».
- **Journaliser les niveaux 0 et 2.** DSH journalise chaque élagage comme remplacement
  (`dsh/docs/subsystems/compaction.md`, « Tool-result pruning outcomes »). Ici ils restent des
  fonctions pures de la dérivation, enregistrées par leurs `AppliedStep`. À rouvrir si
  `audit show` doit rendre la requête dégradée sans recalcul.
- **Fusionner `tool.result` (observation) et `conv.tool_result`.** Deux événements par
  résultat d'outil. Garder les deux préserve les consommateurs du flux runtime ; les fusionner
  simplifie. Je garde les deux en V1 et propose la fusion dans une V1.1 avec un préavis dans
  `docs/runtime-events.md`.
- **Sous-agents et workflows.** `MemoryConversation` (sous-agents, `agent.rs:219-262`,
  `workflow.rs:1482`) n'écrit rien en base : hors périmètre V1, leur conclusion entre dans le
  journal du parent comme résultat d'outil. Les runs de workflow passent par
  `SessionConversation` (`workflow.rs:1313`) : couverts sans travail supplémentaire.
- **Le résumé préparé mais non publié** (`compaction.pending.*` en `kv`,
  `daemon/compaction.rs:1197-1214`) reste hors journal : il n'a pas encore été vu par le
  modèle. C'est cohérent avec l'invariant 1, mais un crash le perd comme aujourd'hui.
- **Rétention des tentatives.** Purger le payload des `conv.attempt` anciens via
  `event_purges` (T17) est la première utilisation de ce mécanisme hors purge de session ;
  `audit verify` compte alors des `purged` sur des sessions vivantes. À documenter dans la
  décision 0011.
