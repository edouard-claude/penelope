# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 16 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **982 tests verts**, tous hors réseau.
- `cargo clippy --workspace --all-targets -- -D warnings` : propre.
- `cargo deny check` : propre (avis, interdits, licences, sources).
- `cargo fmt --all --check` : propre.
- CI GitHub Actions (format, lint, tests, binaire, dépendances) et workflow de release
  sur tag `vX.Y.Z` avec binaire universel macOS.
- 65 tests d'acceptation nommés `ca_<section>_<n>_<nom>`, couvrant 14 sections du PRD,
  indexés dans [ca-matrix.md](ca-matrix.md), qui est généré depuis les sources.

## Étapes du §21

| # | Étape | État | Tests |
|---|---|---|---|
| 1 | `store` + `kernel` + `observe` | fait | 95 kernel, 13 store, 28 observe |
| 2 | `llm` + boucle d'agent | fait, `cli chat` non branché | 82 llm, 36 daemon |
| 3 | `context` (tuiles, ancres, niveaux 0 à 4, LCM) | fait | 73 |
| 4 | `telegram` (transport, rendu, gabarits, CTA, formulaires) | bibliothèque faite, boucle non lancée | 85 |
| 5 | `hitl` + bac à sable + `tools` | fait | 16 hitl, 64 tools, 54 platform |
| 6 | `mcp` (négociation, transports, primitives, OAuth, registre, supervision) | bibliothèque faite, superviseur non lancé | 95 + 19 conformité |
| 7 | `memory` + `skills` | fait | 97 memory, 12 skills |
| 8 | `workflow` + déclencheurs + workflows livrés | moteur fait, ordonnanceur non lancé | 74 |
| 9 | Routage par complexité, budgets, images, STT | fait côté bibliothèque ; l'ingestion Telegram qui les alimenterait ne tourne pas | inclus en llm |
| 10 | `resilience`, `upgrade`, `backup`, suites live, `ab-hermes` | résilience et sauvegarde faites ; `upgrade`, suites live et A/B non faits | 10 resilience |

## Suites du §20.1

| Suite | Réseau | État |
|---|---|---|
| `unit` | non | verte |
| `arch` | non | verte |
| `ctx-safety` | non | verte |
| `mem-learning` | non | verte |
| `mcp-conformance` | non | verte, 19 tests sur la matrice versions × transports |
| `telegram` | non | verte |
| `hitl` | non | verte |
| `workflow` | non | verte |
| `hot-reload` | non | verte, 14 tests |
| `resilience` | non | verte, 10 tests |
| `security` | non | verte, 11 tests |
| `ctx-recall`, `mem-longitudinal`, `live-openrouter`, `live-telegram`, `ab-hermes` | oui | **non écrites** : elles exigent un modèle réel, un bot réel et l'instance Hermes |

## Ce qui reste à faire

### Branché depuis la 0.1.0

- **Conversation** : boucle d'agent sur l'historique persistant, prompt T0 à T4 (âme,
  skills, instantanés mémoire, rappel), alias collant et classifieur de complexité, repli
  de modèle sur panne, streaming.
- **Exécuteur d'outils natifs** : fichiers, shell sous bac à sable, git, HTTP (contenu
  encadré comme non fiable), mémoire (écriture dans le vault), intentions, planification,
  historique, artefacts, skills, workflows (description, contrôle, écriture).
- **Approbations** : suspension du tour, reprise par un tour `resume` qui exécute l'appel
  approuvé sans redemander, refus transmis au modèle, fenêtres « session » et « toujours ».
- **Pool de runners**, bus d'événements, attente des issues sans perte.
- **Passerelle Telegram** : long polling persistant et dédupliqué, commandes, cartes
  d'approbation à boutons (double confirmation des actions destructives, refus motivé),
  brouillons `sendMessageDraft`, file d'envoi durable avec repli en texte brut, envoi de
  fichiers. Conforme à la Bot API 10.3 réelle (`draft_id` entier, `rich_message.markdown`).
- **RPC** : `chat.send`, `chat.stream`, `chat.stop`, `session.switch`, `secret.set`,
  `model.route_test`, `quiet`, `tail` ; `approve` et `deny` relancent le tour.
- **CLI** : `penelope chat` (ponctuel ou interactif), `penelope secret set`.
- **Catalogue de modèles** chargé au démarrage puis toutes les 6 h ; `model list` montre
  les alias et cherche dans le catalogue.

### Encore à brancher

1. **Superviseur MCP** : démarrer les serveurs de `mcp.d/`, remplir le registre, servir
   `tool_call`, flux OAuth `paste_back` depuis Telegram, méthodes `mcp.*`.
2. **Ordonnanceur** : faire battre les schedules (cron, intervalle, `mcp_poll`) et les
   intentions.
3. **Moteur de workflows** : exécuter les runs étape par étape (`wf.run`,
   `workflow_start`, sous-agents, étapes `user` et `wait`).
4. **Rêve nocturne et digest** : consolidation des candidats, méthodes `mem.*`.
5. **Pièces jointes Telegram** : photos (vision), vocaux (transcription), documents.
6. **Compaction de niveau 3** (résumés LCM) déclenchée en fond pendant les longues sessions.

### Méthodes RPC déclarées mais non servies

Un test (`penelope-daemon`, `rpc.rs`) fixe cette liste : elle ne peut pas s'allonger en
silence.

```
session.fork  session.rewind  session.compact
mcp.list  mcp.show  mcp.add  mcp.edit  mcp.rm  mcp.enable  mcp.disable
mcp.restart  mcp.test  mcp.auth  mcp.logs
skill.rollback  wf.run  schedule.add  schedule.run_now
mem.history  mem.restore  mem.reindex  mem.forget  mem.candidates
mem.dream  mem.learned  vault.sync  vault.check
import.hermes  export  restore  store.rebuild  eval.run  upgrade
```

### Autres manques

- `penelope import hermes` (§20.2, point 3) : non implémenté.
- `penelope upgrade` : non implémenté.
- Les captures d'écran de [telegram.md](telegram.md) sont des maquettes ASCII.

## Décisions

Les écarts assumés par rapport à un « DEVRAIT » du PRD sont documentés un par un :

| # | Décision | Raison courte |
|---|---|---|
| [0001](decisions/0001-kernel-depend-de-store.md) | `penelope-kernel` dépend de `penelope-store` | Le noyau **est** la couche durable ; `store` est une infrastructure, pas un crate métier |
| [0002](decisions/0002-pas-de-sqlite-vec.md) | Pas de `sqlite-vec` | Recherche exhaustive en Rust : même sémantique, une dépendance C de moins |
| [0003](decisions/0003-validateur-json-schema-local.md) | Validateur JSON Schema maison | `$ref` borné (contenu non fiable) et annotations exposées au moteur de formulaires |
| [0004](decisions/0004-ipc-tokio-unix-socket.md) | `tokio::net::UnixListener` direct | Seul macOS est livré ; permissions `0600` explicites |
| [0005](decisions/0005-watcher-par-scrutation.md) | Surveillance par scrutation | La resynchronisation périodique est déjà le mécanisme de vérité, et elle est testable |

## Deux failles corrigées en écrivant la suite `security`

Dignes d'être notées, parce qu'elles montrent à quoi la suite sert :

- `http::check_url` acceptait `http://[::1]/`. `Url::host_str()` rend une IPv6 **entre
  crochets**, et `"[::1]".parse::<IpAddr>()` échoue : la boucle locale v6 passait donc à
  travers le filtre SSRF. Les crochets sont maintenant retirés avant l'analyse.
- `fs::resolve` acceptait `~/Library/Preferences`. Le tilde n'étant pas développé, le
  chemin était joint au workspace et créait silencieusement un répertoire nommé `~` au
  lieu de refuser. Un chemin commençant par `~` est désormais rejeté avec un message
  explicite.

Par ailleurs, `penelope-tools/src/shell.rs` existait mais n'était pas déclaré comme
module : il n'était donc ni compilé, ni testé, ni vu par clippy. Corrigé.
