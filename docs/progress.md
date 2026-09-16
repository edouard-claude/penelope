# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 16 septembre 2026.

## Résumé

- 17 crates, `#![forbid(unsafe_code)]` partout, aucune dépendance circulaire.
- **924 tests verts**, tous hors réseau.
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

### Boucles de fond du daemon

C'est le manque principal. `Daemon::recover()` et le serveur RPC tournent ; rien d'autre
ne tourne en continu. Il manque, dans `penelope-daemon` :

1. **Scrutation Telegram et file d'envoi.** Le client, le rendu, les gabarits, les CTA et
   les formulaires sont complets et testés contre le mock ; aucune tâche ne les appelle en
   boucle.
2. **Superviseur MCP.** Démarrage paresseux, backoff, éviction LRU et extinction sur
   inactivité sont implémentés et testés unitairement, mais aucun n'est piloté par une
   tâche de fond.
3. **Ordonnanceur.** Cron, `mcp_poll` et déclencheurs d'événements sont implémentés ;
   personne ne les fait battre.
4. **Pool de runners.** `TurnQueue` réclame, renouvelle et libère les leases ; il n'y a
   pas de tâche qui boucle dessus, donc pas d'exécution concurrente réelle en production
   (elle l'est en test).
5. **Rêve nocturne et digest.** La consolidation est complète et testée ; son cron n'est
   pas armé.

### Méthodes RPC déclarées mais non servies

Un test (`penelope-daemon`, `rpc.rs`) fixe cette liste : elle ne peut pas s'allonger en
silence. En sont membres aujourd'hui :

```
chat.send  chat.stream  chat.stop
session.switch  session.fork  session.rewind  session.compact
secret.set  model.route_test
mcp.list  mcp.show  mcp.add  mcp.edit  mcp.rm  mcp.enable  mcp.disable
mcp.restart  mcp.test  mcp.auth  mcp.logs
skill.rollback  wf.run  schedule.add  schedule.run_now  quiet
mem.history  mem.restore  mem.reindex  mem.forget  mem.candidates
mem.dream  mem.learned  vault.sync  vault.check
import.hermes  export  restore  store.rebuild  tail  eval.run  upgrade
```

Pour presque toutes, la fonctionnalité existe déjà dans la bibliothèque correspondante et
ce qui manque est le câblage dans `rpc.rs`. Deux font exception et ne sont implémentées
nulle part : `import.hermes` et `upgrade`.

### Autres manques

- `penelope import hermes` (§20.2, point 3) : non implémenté, donc aucun rapport joint.
- Entrée image (`Content::ImageUrl`) et transcription (`Provider::transcribe`) existent
  et sont testées, mais rien ne les alimente : c'est la boucle Telegram qui reçoit les
  photos et les messages vocaux, et elle ne tourne pas.
- `penelope upgrade` : non implémenté.
- `penelope model list` reste vide : le catalogue OpenRouter est rafraîchi par une tâche
  de fond non lancée. Et `model set` ne vérifie pas que l'identifiant existe chez le
  provider.
- `penelope secret set` écrit en local, sans passer par le daemon (voulu : une
  installation neuve se configure avant le premier démarrage). La méthode RPC
  `secret.set` reste donc non servie.
- Les captures d'écran de [telegram.md](telegram.md) sont des maquettes ASCII, pas des
  captures réelles : il faut un bot de test pour les produire.

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
