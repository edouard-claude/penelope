# Flux d'événements runtime

Le daemon expose, sur activation, un WebSocket local de lecture seule. Il sert à
observer l'activité de Pénélope, pas à converser avec elle ni à commander ses actions.
Ce n'est pas le serveur OpenClaw Gateway : Pénélope reprend son modèle de flux
d'observation sans intégrer OpenClaw. Chaque événement est écrit dans le journal
SQLite avant son émission. Une reconnexion avec `after_id`
rejoue les événements suivants ; l'ID SQLite est l'identifiant idempotent et l'horloge
séquentielle globale. Le flux reprend aussi après une perte du tampon en direct.

## Activer un consommateur

Créer un jeton aléatoire d'au moins 32 caractères, l'enregistrer avec
`penelope secret set runtime_pathlayer`, puis ajouter dans `config.toml` :

```toml
[observability]
runtime_stream_bind = "127.0.0.1:9465"

[[observability.runtime_consumers]]
name = "pathlayer"
token_secret = "runtime_pathlayer"
kinds = ["runtime.tool", "runtime.llm"]
```

Redémarrer Pénélope. Une liste de consommateurs vide désactive le serveur. Chaque
consommateur a un secret distinct et son propre filtre de types exacts ; une liste
`kinds` vide reçoit tous les types. Le bind refuse toute adresse autre que
`127.0.0.1`. Le jeton se transmet dans l'en-tête `Authorization: Bearer …` à
`ws://127.0.0.1:9465/events?after_id=0`. Un secret absent ou trop court empêche
le démarrage du flux.

## Contrat

Chaque message est un objet JSON avec `event_id`, `sequence` (même ID),
`session_seq`, `timestamp`, `kind`, `session_id`, `run_id`, `payload` et
`actuation: null`. Les réponses du client sont ignorées : aucun ordre ou arrêt de
Pénélope ne passe par ce canal. Les empreintes de la chaîne d'audit ne sont pas
exportées. Le `payload` est rédigé selon les règles communes et borné à 64 Kio ;
un contenu plus grand devient `{ "truncated": true, "bytes": … }`. La purge d'une
session s'applique au replay du journal.

`runtime.tool` contient `tool`, `args`, `result`, `ok`, `duration_ms` et
`cost_usd_estimated` (0 pour les outils natifs, `null` pour MCP sans mesure).
`runtime.llm` contient modèle, fournisseur, rôle, compteurs de tokens et coût
mesuré ou estimé. Les événements de session et HITL portent leur cycle de vie ;
les événements préexistants couvrent les tours, runs, étapes, intents, planifications
et erreurs. L'ordonnanceur émet aussi `schedule.fired` après un déclenchement réussi.

`tool.job.started` et `tool.job.completed` encadrent un appel d'outil sorti de son tour
(un `shell_exec` ou un `sub_agent_spawn` lancé avec `background: true`). Le premier porte
`job`, `tool` et `effect` — l'identifiant de l'effet du ledger, resté `dispatching` tant
que le job tourne ; le second porte `job`, `effect` et l'état final (`completed`,
`failed` ou `cancelled`). Les deux sont attachés à la session d'origine, y compris quand
son tour est clos depuis longtemps. Le résultat, lui, ne passe pas par ces événements :
il revient dans la conversation par un tour de relance. Voir
[Jobs d'outils](install-headless.md#jobs-doutils).

`turn.started` porte, avec le modèle, les empreintes de ce que le modèle va lire :
`system_hash` (le préfixe T0 à T2) et `tools_hash` (la liste d'outils). Le texte n'est
jamais dans l'événement — il est gardé une fois, sous cette empreinte, et se relit par
`penelope audit show` (voir [Relire ce que le modèle a lu](context.md#relire-ce-que-le-modèle-a-lu)).
Pour un tour de la file, il porte aussi `turn_id` (la ligne de la file), `origin_turn`
(la requête du propriétaire, partagée par une reprise après approbation), `kind`
(`message`, `trigger`, `resume`, `nudge`) et `attempt` (la tentative, qui monte quand
un tour est rejoué après un redémarrage).

`turn.finished` ferme **chaque** tour ouvert, quelle que soit sa sortie, avec les mêmes
champs d'identité et `reason` :

| `reason` | Sortie | Champs en plus |
|---|---|---|
| `answered` | réponse finale | `iterations`, `cost_usd` |
| `awaiting_approval` | le tour attend une décision | `approval_id` |
| `cancelled` | arrêt demandé (`/stop`, bouton) | |
| `failed` | erreur du modèle, du fournisseur ou du tour | `error` |
| `budget_exceeded` | plafond de dépense atteint | `scope`, `spent_usd`, `limit_usd` |
| `loop_aborted` | détecteur de boucles | `report` |
| `calls_exhausted` | plafond d'appels au modèle du tour | `error` |
| `interrupted` | écrit au démarrage pour un tour que l'arrêt du processus a laissé ouvert ; jamais par la boucle | |

Un tour qui tombe avant d'appeler le modèle (fournisseur indisponible) est ouvert et
fermé ensemble, `turn.started` sans modèle. Un `turn.started` sans `turn.finished` de
même `turn_id` est un tour interrompu par un arrêt du processus : au démarrage suivant,
le daemon le ferme `interrupted` avant de servir, et le tour rejoué par la file ouvre sa
propre borne avec l'`attempt` suivant.

Les événements `conv.*` portent le **contenu** de la conversation, pour que le journal
se suffise (épopée #208, `design/v1/source-de-verite.md` §2.2). Chaque payload a
`"v": 1` et, pour ceux qui changent ce que le modèle lit, `surface` : `{"op":"append"}`
(un nœud de plus), `{"op":"replace","from":a,"to":b}` (une plage remplacée),
`{"op":"cut","after":a}`, `{"op":"inherit",…}` ou `{"op":"seal",…}`. Les bornes sont des
**adresses** de la surface : le `seq` de l'événement, plus l'`offset` hérité d'un fork
ou d'un scellement ; une ligne copiée d'une mère garde l'adresse qu'elle y avait.
Pendant la double écriture, les tables restent la source de lecture ; une ligne de
`messages` (et un nœud de `lcm_nodes`) cite son événement par `event_id`.

| Kind | Écrit quand | Payload |
|---|---|---|
| `conv.user` | un message utilisateur entre dans l'historique | `source` (`owner`, `merged`, `trigger`, `nudge`, `photo`), `content`, `episode`, `tokens_est` ; `turn_message_id` et `arrived_at` pour un message de la file ; `mid_turn` s'il est arrivé pendant le tour |
| `conv.assistant` | une réponse du modèle est gardée | `content`, `tool_calls`, `reasoning`, `turn`, `step`, `model`, `provider`, `upstream`, `generation_id`, `finish`, `usage`, `cost_usd`, `system_hash`, `tools_hash`, `request_hash` ; `interrupted` après un arrêt |
| `conv.tool_result` | un résultat d'outil est gardé (`append`) ; ou son corps part en artefact (niveau 1) : `replace` de ce seul nœud, qui garde son adresse | `call_id`, `tool`, `ok`, `eager`, `content`, `tokens_est` ; en remplacement, `artifact_id`, `artifact_sha256`, `original_tokens` |
| `conv.system` | le préfixe système retenu change ; le premier d'une session fille remplace celui qu'elle hérite | `hash`, `rendered` (le texte entier), `tiles`, `reason` (`first`, `cold`, `compaction`) |
| `conv.context` | le contexte volatil est figé avec un message | `target` (l'adresse du `conv.user`), `block` |
| `conv.attempt` | un appel au modèle n'a pas donné de réponse gardée (#206) ; sans `surface`, il n'entre jamais dans l'historique | `turn`, `step`, `cause` (`stream_cut` flux coupé, `before_stream` erreur avant le flux, `fallback` la suite passe au modèle de repli, `empty_answer` réponse vide), `model`, `provider`, `error`, `partial_text` et `partial_reasoning` (le début reçu, rédigé), `llm_request_id` ; `usage`, `cost_usd` (déjà comptés, pas de seconde ligne d'usage) et `retry_prompt` (la consigne que la requête suivante ajoute) pour une réponse vide. Dix au plus par tour ; chacun est aussi une ligne `tentative sans réponse` de `penelope logs --turn` |
| `conv.summary` | un résumé est publié (compaction) : `replace` de la plage qu'il couvre, précédent résumé compris quand il le prolonge ; `context.compacted` suit | `node_id`, `previous_node_id`, `summary` (le texte rendu du nœud), `anchors`, `verbatim_users`, `model`, `tokens_src`, `tokens_self`, `batches_left`, `trigger`, `idempotency_key` (republier le même travail n'écrit pas un second événement) |
| `conv.fork` | premier `conv.*` d'une session fille (`/fork`) : elle hérite de la surface de sa mère ; `session.forked` suit | `parent`, `up_to` (dernière adresse héritée), `offset` |
| `conv.rewind` | `/rewind` : `cut` après le nœud qui précède le message de coupe (0 : tout) ; `session.rewound` suit | `turns`, `archive_session` |
| `conv.import` | scellement d'une session d'avant le journal (§4.5) | `messages`, `contexts`, `lcm_active`, `digest` |

Le contenu transite donc par ce flux : un message du propriétaire, une réponse, un
résultat d'outil, un résumé. Il reste local (le bind n'accepte que `127.0.0.1`), rédigé
selon les règles communes et borné à 64 Kio comme tout payload ; un gros résultat passe
en `{ "truncated": true, "bytes": … }`. Un consommateur filtre par kind exact : il ne
reçoit aucun `conv.*` qu'il n'a pas nommé, sauf avec une liste `kinds` vide. Ces
événements sont des données personnelles, purgées comme les autres.

## Démonstration Pathlayer

L'endpoint HTTP `POST /ingest` appartient à `HttpIngestAdapter` de Pathlayer :
Pénélope y envoie `{ "source": "penelope", "payload": <événement> }` via le
[relais de 48 lignes](../scripts/pathlayer_forwarder.py). Le [récepteur
Pathlayer](../scripts/pathlayer_listen.py) traduit `runtime.tool` en `ToolEvent`
et exécute `LoopDetector` en observation passive. Installer Pathlayer depuis son
dépôt et `websockets>=14,<17` dans le même environnement Python ; le récepteur
n'a besoin que de Pathlayer.

Pour tester sur une session isolée avec quatre lectures réelles du même fichier :

```bash
export PENELOPE_EVENTS_TOKEN="$(openssl rand -hex 32)"
python scripts/pathlayer_listen.py
# dans un autre terminal, avec le même jeton :
cargo run -p penelope-daemon --example runtime_pathlayer_demo
# dans un troisième terminal :
PENELOPE_EVENTS_URL=ws://127.0.0.1:9466/events \
PENELOPE_EVENTS_CURSOR="/tmp/penelope-pathlayer-demo-$$.cursor" \
python scripts/pathlayer_forwarder.py
```

Le replay transmet les quatre appels même si le relais démarre après leur
exécution ; le récepteur affiche `LOOP` avec la confiance et le motif. Le fichier
curseur est écrit en `0600` après chaque réponse HTTP réussie. Pour une instance
normale, omettre `PENELOPE_EVENTS_URL` et utiliser le jeton configuré ; les
deux processus restent sur la même machine.
