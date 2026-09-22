# Flux d'événements runtime

Le daemon expose, sur activation, un WebSocket local de lecture seule. Chaque événement
est écrit dans le journal SQLite avant son émission. Une reconnexion avec `after_id`
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
