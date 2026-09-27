# Notes de livraison : lot B-scenarios (filets, épopée #208)

Branche `v1-b-scenarios`, dérivée de `v1` à `a9c4214`, poussée sur `origin`. Trois commits
de code (`924708a` moteur et sept scénarios, `6ff1ee2` compaction, `c6716af` sessions),
puis celui-ci pour les notes. Périmètre tenu : `crates/penelope-evals/src/scenario.rs` et
ses trois sous-modules `src/scenario/{harness,normalise,world}.rs`, `pub mod scenario;`
dans `src/lib.rs`, `tests/scenarios.rs`, quinze répertoires sous
`crates/penelope-evals/scenarios/`, `crates/penelope-evals/Cargo.toml` (trois dépendances
déjà dans le workspace, raison écrite), `Cargo.lock` par conséquence. `mock.rs` de
`penelope-llm` n'a pas été touché : le rejeu depuis un fichier n'a rien exigé de plus que
`set_responder`. Aucune clé d'API n'a été utilisée : tout est simulé.

## Le format retenu

Un scénario est un répertoire `crates/penelope-evals/scenarios/<nom>/` de quatre fichiers.
Les deux spécifications (`gel-et-outillage.md` §3 R11, `source-de-verite.md` §5 T0)
décrivent le même filet ; une seule implémentation les satisfait : un scénario porte ses
entrées, le script du modèle, et deux attendus, le monde après le run et chaque requête
vue par le modèle.

| Fichier | Rôle | Écrit par |
|---|---|---|
| `scenario.toml` | entrées ordonnées, configuration patchée, fichiers semés, serveur MCP simulé | la main |
| `model.jsonl` | une ligne par appel de modèle, dans l'ordre, vocabulaire de `Scripted` | la main, ou `RECORD_SCENARIO` |
| `expected.jsonl` | le monde après le run, normalisé | `UPDATE_SCENARIOS=1` |
| `surface.jsonl` | chaque requête reçue par le mock, rédigée et normalisée | `UPDATE_SCENARIOS=1` |

Exemple, `scenarios/crash-deux-vies/scenario.toml` :

```toml
name = "crash-deux-vies"
description = """Crash en deux vies : le modèle demande un outil MCP (lecture, idempotent), le
processus meurt pendant l'appel, après que la réponse du modèle est écrite et avant tout
résultat. Au redémarrage, la reprise remet le tour en file, l'effet en vol redevient
planifié sans demande humaine (idempotent), le tour retrouve l'appel en attente dans la
queue du transcript, l'exécute, et le modèle répond."""
pin_model = "main"

[[mcp_tools]]
server = "banc"
name = "lire"
description = "Lit un fichier du dépôt distant de test."
read_only = true
params = ["path"]
result = "rapport.txt : 42 lignes, la dernière dit « fin »."

[[steps]]
kind = "message"
text = "lis rapport.txt sur le banc et dis-moi comment il finit"
crash = "during_tool"

[[steps]]
kind = "restart"
```

`model.jsonl` du même scénario, une ligne par appel :

```json
{"tool_calls": {"text": "", "calls": [{"id": "c1", "name": "mcp__banc__lire", "arguments": {"path": "rapport.txt"}}]}}
{"text": "Le rapport se termine par « fin »."}
```

Et quelques lignes de son `expected.jsonl` (le tour rejoué en deux vies, l'effet
idempotent reparti, la chaîne d'audit vérifiée) :

```json
{"type":"outcome","step":2,"input":"redémarrage","recovered":{"turns_requeued":1,"effects_unknown":0,"llm_unknown":0,"runs_resumed":0,"mcp_orphans_killed":0},"drained":[{"outcome":"answered","text":"Le rapport se termine par « fin ».","iterations":1}]}
{"type":"message","session":"{{session:1}}","seq":2,"role":"assistant","text":"","tool_calls":[{"id":"c1","name":"mcp__banc__lire","arguments":{"path":"rapport.txt"}}]}
{"type":"turn","id":"{{turn:1}}","session":"{{session:1}}","kind":"message","state":"done","attempts":2,"error":null}
{"type":"effect","id":"{{effect:1}}","session":"{{session:1}}","call":"c1","kind":"mcp","tool":"mcp__banc__lire","state":"completed","attempts":2,"idempotent":true,"request":{"path":"rapport.txt"}}
{"type":"audit","ok":true,"checked":12,"purged":0}
```

Les entrées de `scenario.toml` : `message` (mis en file, réclamé, joué jusqu'à son issue ;
`crash = "during_tool"` tue le processus pendant l'appel d'outil MCP simulé), `enqueue`
(mis en file sans être joué, pour les messages fusionnés), `command` (`/compact`,
`/fork [titre]`, `/rewind [n]`, `/purge`, appelés comme les commandes Telegram et RPC les
appellent : `compaction::compact(Manual)`, `session_ops::fork`, `session_ops::rewind`,
`purge::session`), `advance_clock` (`10m`, `3h`), `restart` (services détruits puis
reconstruits sur le même répertoire, `recover()`, tours en attente joués, comme
`resilience::boot`), `approve` (première demande en attente de la session approuvée, tour
de reprise joué), `usage` (dernier appel facturé de N tokens, fixture de la session
froide) et `seed` (échanges semés dans l'historique sans appel au modèle). En tête :
`pin_model` (alias épinglé, donc pas de classifieur), `[config]` (patchs
`"chemin.pointé" = valeur`, réappliqués à chaque démarrage parce que la configuration de
test n'est pas relue du disque), `[[files]]` (semés dans le workspace par défaut),
`[[mcp_tools]]` (inscrits au registre `mcp_tools` comme les outils d'un vrai serveur, avec
`readOnlyHint`, et servis par une passerelle simulée branchée sur `hooks.mcp`).

Le monde relevé (`expected.jsonl`, une ligne par objet, `type` en tête) : `outcome` par
étape (l'issue du tour, le bilan d'une commande), `session`, `message` (rôle, texte,
appels d'outils, `compacted`, `eager`, `artifact`, `episode`), `summary` (tous les nœuds
LCM, l'ancien avec son `superseded_by` quand un résumé est prolongé), `event` (kind et
charge utile de chaque événement du journal), `turn` (état, tentatives), `effect` (outil,
état, tentatives, idempotence, arguments), `llm` (requêtes et leur état), `approval`,
`artifact`, `usage`, `outbox` (`tg_outbox`, vide tant que l'origine est la CLI), `file`
(fichiers du workspace), `audit` (`events.verify()`). La surface (`surface.jsonl`) : par
appel, le modèle, les noms des outils exposés, les réglages (`tool_choice`, `max_tokens`,
sortie structurée, replis, fournisseur collant) et chaque message (rôle, texte, appels
d'outils, `tool_call_id`), après `redact_json`.

Trois modes, lus dans l'environnement : rejeu (défaut, dans `cargo test --workspace`,
compare et échoue en nommant le scénario, le groupe de lignes ou l'appel et le message
qui divergent, puis dit `UPDATE_SCENARIOS=1 puis relire le diff`) ; `UPDATE_SCENARIOS=1`
(régénère `expected.jsonl` et `surface.jsonl`) ; `RECORD_SCENARIO=<nom>` ou `all`
(enveloppe le vrai fournisseur, réécrit `model.jsonl` puis les deux attendus). Un
`expected.jsonl` absent en rejeu est une erreur qui dit quoi faire, pas un fichier créé
en silence. Deux messages d'échec réels, provoqués pendant le développement :

```text
scénario outil-lecture : expected.jsonl diffère (effects n°1 sur 0 attendu(s), 1 obtenu(s) : attendu [absent], obtenu {"type":"effect",…,"tool":"fs_read","state":"completed",…}) : le comportement a changé ; si c'est voulu, UPDATE_SCENARIOS=1 puis relire le diff
scénario outil-lecture : surface.jsonl diffère (appel 2, message 3 : attendu {"role":"assistant","text":"autre",…}, obtenu {"role":"assistant","text":"",…}) : ce que le modèle voit a changé ; si c'est voulu, UPDATE_SCENARIOS=1 puis relire le diff
```

## Les scénarios livrés

Quinze, tous verts sur la V1 actuelle telle quelle (la 0.17.60 gelée), sans clé, en
moins de six secondes pour la suite entière. Aucun des quatorze demandés n'a été
impossible ; le quinzième est l'« approbation » de T18.

| Répertoire | Ce qu'il fixe | Appels au modèle |
|---|---|---|
| `tour-simple` | un message, le classifieur (« low », non collant), la réponse | 2 |
| `outil-lecture` | `fs_read` puis réponse, alias épinglé | 2 |
| `lectures-paralleles` | trois `fs_read` dans une réponse (#85), résultats dans l'ordre des appels | 2 |
| `niveau-1-et-groupe` | gros résultat externalisé en artefact, puis groupe admis d'un bloc (#52), `large_payload_tokens` patché à 300 | 4 |
| `compaction-puis-prolongation` | `/compact` force un lot, le second `/compact` prolonge le nœud (`superseded_by`) | 6 |
| `session-froide` | historique semé, dernier appel facturé à 300 000 tokens, pause de 10 min, compaction `resume` avant l'appel (#40), classifieur rappelé à la frontière (#82) | 3 |
| `depassement-prouve` | `context_overflow` du fournisseur, compaction `overflow`, requête repartie avec le résumé | 5 |
| `flux-coupe` | flux tombé après du texte : tour en échec explicite, rien de partiel dans l'historique, le message suivant repart | 2 |
| `reponse-vide-relancee` | réponse vide, `turn.empty_answer`, relance vue du modèle seulement (#206) | 2 |
| `messages-fusionnes` | deux messages en file plus le porteur : trois messages utilisateur distincts dans une requête (#161) | 1 |
| `fork-puis-divergence` | `/fork`, message dans le fork, l'original ne bouge pas ; le fork repasse par le classifieur | 4 |
| `rewind` | `/rewind 2`, session d'archive fermée, reprise juste après ce qui reste | 4 |
| `purge` | purge RGPD (#46) : messages, requêtes et usage lié partis, événements `purged`, chaîne vérifiable | 2 |
| `crash-deux-vies` | mort du processus pendant un appel d'outil MCP idempotent, reprise, appel retrouvé et exécuté | 2 |
| `approbation-apres-redemarrage` | `fs_write` demande l'accord, redémarrage, approbation, tour de reprise, fichier écrit | 2 |

Le crash en deux vies demande une précision. Aucun outil natif ne bloque de façon
reproductible au moment voulu (`fs_read` finit en microsecondes, `ask_user` n'attend pas
la réponse, `shell_exec` demande une carte), et tuer la tâche du tour « au bon moment »
serait une course. La suite inscrit donc un outil MCP simulé au registre (`readOnlyHint`,
classe `read`, donc idempotent et sans carte) et le sert par une passerelle branchée sur
`hooks.mcp` : en mode crash, la passerelle signale l'appel et ne répond jamais ; la
tâche du tour est tuée à cet instant précis, les services détruits. La réponse du
modèle est écrite, l'effet est en vol, aucun résultat n'existe. La deuxième vie fait
exactement ce que fait le daemon au démarrage : `recover()` remet le tour en file et
repasse l'effet idempotent en `planned` ; le tour réclamé retrouve l'appel en attente
dans la queue du transcript et l'exécute (`attempts: 2` sur le tour et sur l'effet).

## Les jetons de normalisation

Le relevé est normalisé après coup, dans `scenario/normalise.rs`, par un aller-retour
sur le JSON : identifiants ULID préfixés (`s_`, `t_`, `e_`, `a_`, `art_`, `n_`, `q_`,
`r_`) en `{{session:1}}`, `{{turn:1}}`, `{{effect:1}}`, `{{approval:1}}`,
`{{artifact:1}}`, `{{node:1}}`, `{{llm:1}}`, `{{run:1}}`, numérotés dans l'ordre du
monde (sessions par création, tours par mise en file, effets par appel, nœuds par
création) avant toute autre mention, un ULID nu en `{{ulid:1}}` ; horodatages RFC 3339
en `{{ts}}` à l'instant de départ et `{{ts+600s}}` ou `{{ts+1500ms}}` ensuite, l'horloge
de test rendant l'écart exact ; racine temporaire des services, brute et canonique
(`/private/var` sur macOS), en `{{home}}` ; hachages (clés `*hash*`, `sha256`,
`idem_key`, `fingerprint`, ou 64 hexadécimaux dans un texte) en `{{hash}}` ; durées
mesurées (`duration_ms`) en `{{ms}}`. Les mêmes jetons servent au monde et à la surface,
avec la même numérotation. Le test `two_replays_of_every_scenario_are_identical` rejoue
chaque scénario deux fois dans le même processus et compare octet pour octet ; la suite
a aussi été lancée deux fois de suite après régénération, sans un octet de différence, et
dix fois d'affilée pour chasser un aléa.

Un aléa a été vu une fois et traité : les lectures pures d'un même lot partent ensemble
(#85) et leurs événements `runtime.tool` s'écrivent dans l'ordre d'achèvement. Le relevé
relit un lot consécutif de `runtime.tool` d'une même session dans l'ordre (outil,
arguments), les numéros de séquence restant à leur place ; les effets sont relevés dans
l'ordre des appels (`step_id`), pas de leur insertion.

## Ce que le mode enregistreur attend pour être exercé

Implémenté dans `scenario/harness.rs` (`Recorder`), non exercé cette nuit : aucune clé
n'était disponible, et le brief l'excluait. Pour l'exercer :

```bash
RECORD_SCENARIO=tour-simple OPENROUTER_API_KEY=… cargo test -p penelope-evals --test scenarios tour_simple
```

L'enregistreur pose la clé dans le coffre en mémoire des services de test, construit le
jeu de fournisseurs par `penelope_llm::build_providers`, l'impose au daemon comme
fournisseur unique, et transcrit chaque `chat_stream` en une ligne de `model.jsonl`
(texte, appels d'outils, images, erreur avant flux, `context_overflow`, erreur en cours
de flux, réponse coupée par la limite de sortie, raisonnement seul), y compris les appels
du classifieur et du résumeur. Il réécrit ensuite `expected.jsonl` et `surface.jsonl`,
à relire avant commit : avec un vrai modèle, les identifiants d'appels d'outils, les
textes et le nombre d'appels changent par rapport aux scripts écrits à la main, et un
scénario qui attend un enchaînement précis (par exemple `crash-deux-vies`, qui suppose un
appel d'outil en première réponse) ne rejouera que si le modèle réel a fait ce choix.
Les modèles suivent la configuration d'exemple (`models.aliases`) ; `PENELOPE_LIVE_*`
n'est pas lu. À exercer sur `tour-simple` d'abord, puis sur un scénario par surface (T19).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Scénarios de session rejouables sans clé (lot B, épopée #208)

- **Le format** (règle R11, tâche T0) : un répertoire `crates/penelope-evals/scenarios/<nom>/`
  par scénario, avec `scenario.toml` (messages, commandes `/compact` `/fork` `/rewind`
  `/purge`, avance de l'horloge, redémarrage, crash, approbation, configuration patchée,
  fichiers semés, serveur MCP simulé), `model.jsonl` (une réponse scriptée par appel de
  modèle) et deux attendus régénérés par `UPDATE_SCENARIOS=1` : `expected.jsonl`, le
  monde après le run (sessions, messages, résumés, événements, tours, effets, requêtes
  LLM, approbations, artefacts, usage, fichiers, audit), et `surface.jsonl`, chaque
  requête vue par le modèle. Identifiants, horodatages, chemins et hachages deviennent
  des jetons (`{{session:1}}`, `{{turn:1}}`, `{{ts+600s}}`, `{{home}}`, `{{hash}}`) :
  deux rejeux sont identiques octet pour octet, un test le vérifie.
- **Quinze scénarios** verts sans clé dans `cargo test --workspace` (suite `scenarios`,
  six secondes) : tour simple, appel d'outil de lecture, lectures parallèles, niveau 1 et
  admission de groupe, compaction manuelle puis prolongation, session froide, dépassement
  prouvé, flux coupé, réponse vide relancée, messages fusionnés, fork, rewind, purge,
  crash en deux vies, approbation après redémarrage. Un diff nomme le scénario, le
  groupe de lignes ou l'appel qui divergent et dit comment régénérer.
- **`RECORD_SCENARIO=<nom>`** enveloppe le vrai fournisseur et réécrit `model.jsonl` :
  implémenté, pas encore exercé (aucune clé cette nuit).
- `penelope-evals` dépend de `toml`, `tempfile` et `async-trait`, déjà dans le workspace.
```

## Écarts par rapport au brief, à relire par l'intégrateur

- **Quatre fichiers source au lieu d'un** : `scenario.rs` (format, chargement, modes,
  comparaison) et `scenario/{harness,normalise,world}.rs`. Le moteur fait 2 200 lignes ;
  un seul fichier aurait violé la règle R1 (1 000 lignes). Les trois sous-modules sont
  le même module, découpé.
- **`tempfile` passe des `dev-dependencies` aux `dependencies`** : le harnais vit dans
  `src/`, il boote les services sur un répertoire temporaire. `Cargo.lock` suit.
- **`llm.fallback_used` dans chaque tour** : le mock renvoie dans `Started` l'identifiant
  complet avec son préfixe de fournisseur, et `call_model` le compare au modèle demandé
  sans préfixe ; chaque tour journalise donc un « repli » qui n'en est pas un. C'est le
  comportement des tests existants, relevé tel quel ; `mock.rs` n'a pas été corrigé pour
  ne pas déborder du périmètre. Le corriger (`strip_provider` dans le mock) fera bouger
  tous les `expected.jsonl` d'une ligne par tour : `UPDATE_SCENARIOS=1` et relire.
- **`turn.finished` manque après un tour en échec ou tué** (`flux-coupe`,
  `crash-deux-vies`, première vie) : c'est l'état de la 0.17 que T4 corrige ; ces deux
  attendus changeront alors, c'est voulu.
- **`memory.episode_closed` dans `compaction-puis-prolongation`** : l'heuristique de
  changement de sujet clôt l'épisode au quatrième message (« on migrera après le rejeu »),
  et la relecture de fond ne fait rien (`review_max_candidates = 0` en test). Reproductible,
  donc relevé ; mais c'est une frontière posée sur cinq mots, à regarder.
- **Fixtures de la session froide** : `seed` (échanges écrits directement dans
  `messages`, tokens déclarés, textes courts) et `usage` (ligne d'usage à 300 000 tokens
  de prompt) simulent une longue session sans jouer vingt tours. C'est ce que fait le test
  unitaire de #40 ; le scénario le dit dans sa description.
- **Vérifié sur macOS seulement** (ce poste). Aucun texte propre à la plateforme dans
  les surfaces (le prompt système ne nomme ni l'OS ni le bac à sable) ; `{{home}}` couvre
  la racine brute et canonique. La CI Linux devrait rejouer à l'identique ; si un attendu
  diverge, la cause est à lire dans le diff, pas à régénérer à l'aveugle.
- **Le contrôle « un lot qui touche `commands.rs`, `spec.rs` ou `api.rs` sans toucher
  `scenarios/` est refusé »** (R11, côté script R3) et la couverture des surfaces (T19)
  ne sont pas dans ce lot.

## Vérifications

`cargo fmt --all --check` propre ; `cargo clippy -p penelope-evals -p penelope-llm
--all-targets -- -D warnings` propre ; `cargo test -p penelope-evals --test scenarios` :
17 tests verts (15 scénarios, le test de couverture des répertoires, le double rejeu),
lancé deux fois de suite après `UPDATE_SCENARIOS=1` sans un octet de différence, puis
dix fois d'affilée.

`cargo test --workspace` (une fois, à la fin) : 50 suites, 1 784 tests verts, 0 échec, 19 ignorés (les
suites réseau), sortie 0. La suite `scenarios` a ensuite été rejouée deux fois de suite :
17 tests verts à chaque fois, aucun attendu réécrit.

## Blocages

Aucun. Reste pour l'intégrateur : rebase sur `v1` (pas fait ici, comme demandé), la
section de version et le bump, le mode enregistreur à exercer avec une clé, et les trois
points ci-dessus qui feront bouger des attendus (`mock.rs`, T4, épisode).
