# Notes de livraison : lot B, filets de bout en bout (épopée #208, tâches T8 et T9)

Branche `v1-b-filets-e2e`, dérivée de `v1` à `a9c4214`, poussée sur `origin`. Périmètre
tenu : trois fichiers de tests d'intégration, 104 fichiers dorés, `docs/ca-matrix.md`
régénéré, ce fichier. Aucun fichier de `src/` touché, aucun `pub` ouvert.

## Ce qui est livré

| Commit | Contenu |
|---|---|
| `362d699` | `crates/penelope-daemon/tests/telegram_e2e.rs` : `ca_14_2_a_telegram_message_gets_an_answer_and_a_ledger_entry`. `Services::for_tests` dans un répertoire temporaire, `MockProvider` scripté (classement `low`, appel `time_now`, texte final), `TelegramGateway::with_transport` sur `MockTransport`, pool de runners réel (`runner::run_pool`), message du propriétaire par `process_update`, file d'envoi vidée par `flush_outbox`. Vérifié en base : une ligne `tg_outbox` (`sendMessage`, `sent`, chat du propriétaire, texte final), un effet `time_now` `completed` sur la session du chat, `turn.started`, `turn.finished` et `tool.result` une fois chacun, `tg_updates` traité, chaîne d'événements intacte (`events.verify`). Puis services reconstruits sur le même répertoire : `recover()` ne remet rien en file et ne signale aucun effet incertain ; même monde ; `flush_outbox` rend 0 ; le même update rejoué est ignoré ; ni le modèle ni le transport ne sont sollicités. `docs/ca-matrix.md` régénéré (72 tests, CA 14.2). |
| `11d4be1` | `crates/penelope-daemon/tests/approval_e2e.rs` : trois tests. `ca_9_4` : `fs_write` sans règle rend `AwaitingApproval`, rien n'est écrit, aucun effet ; `agent::decide_approval` (approuver une fois) puis `enqueue_resume` : le fichier existe, un seul effet `completed`, demande `approved` via `cli`, aucune règle, aucun autre tour. `ca_9_5` : refus avec raison ; à la reprise le harnais met sous les yeux du modèle « Non exécuté : le propriétaire a refusé : … », fichier absent, aucun effet, demande `denied`. `ca_9_6` : fenêtre `Session` : une règle `fs_write` bornée à la session, motif `{"path": {"$path_prefix": "notes/"}}` ; un second appel du même répertoire passe sans demande (`hits` à 1, deux effets `completed`) ; une autre session redemande. Matrice régénérée (75 tests, CA 9.4 à 9.6). |
| `81cdadb` | `crates/penelope-evals/tests/rpc_golden.rs` et `crates/penelope-evals/tests/golden/<méthode>.json`, un par constante de `method::ALL` (104, `audit.show` compris). Chaque fichier porte `params` (la requête, avec des jetons `$…` pour les identifiants tirés au vol) et la forme de la réponse : clés et types, tableaux décrits par la réunion des formes de leurs éléments, erreur par son code JSON-RPC et le type de son message. Trois tests : chaque méthode a son fichier et chaque fichier sa méthode ; chaque méthode répond avec sa forme (`UPDATE_GOLDEN=1` régénère, affiche les cas d'erreur, supprime les fichiers sans méthode) ; la comparaison voit une clé retirée, une clé ajoutée, un type changé, un booléen de requête changé. |
| (ce commit) | Ce fichier. |

Vérifications, dans le worktree, sur macOS : `cargo test -p penelope-daemon --test telegram_e2e
--test approval_e2e` vert (1 + 3 tests, moins de 2 s) ; `cargo test -p penelope-evals --test
rpc_golden` vert (3 tests, environ 5 s), deux exécutions consécutives en mode comparaison
identiques ; `cargo test -p penelope-evals --test ca_matrix` vert ; `cargo fmt --all --check`
propre ; `cargo clippy -p penelope-daemon -p penelope-evals --all-targets -- -D warnings`
propre ; `cargo test --workspace` : voir le rapport final. Linux n'a pas été exécuté ici :
la CI le dira ; les trois tests n'utilisent ni bac à sable, ni processus, ni réseau.

## Choix à connaître

- **Numéros de CA.** Le PRD n'est pas dans le dépôt : impossible de vérifier ce que dit
  son §14 point 2. La matrice avait 14.1, 14.3 à 14.6 ; le message qui obtient sa réponse
  est le critère le plus élémentaire du §14, il prend le trou, 14.2. Le §9 avait 9.1 à
  9.3 ; les trois tests d'approbation prennent 9.4 à 9.6 (approuver puis reprendre,
  refuser, fenêtre de session). À renuméroter si le PRD dit autre chose : R8 fige les noms
  existants, pas ceux-là avant leur entrée dans `[ca].required`.
- **`process_update` plutôt que la boucle de scrutation.** `MockTransport::getUpdates`
  répond sans attendre (pas de long poll) : `poll_loop` tournerait à vide et saturerait
  un cœur pendant le test. `process_update` est exactement ce que la boucle appelle pour
  chaque update ; l'offset `tg.offset` n'est donc pas exercé.
- **Marqueur `tg.onboard.proposed`.** Sur un profil vide, le premier message déclenche
  aussi la proposition d'accueil : une carte de plus dans `tg_outbox`. Le test pose le
  marqueur par `kv_set` (comme `ticket_to_deploy_e2e.rs`) pour affirmer « exactement une
  ligne ».
- **Classifieur.** Le test Telegram le garde et scripte sa réponse (`{"complexity":"low"}`),
  comme `chat_socket.rs`. Les tests d'approbation et le contrat doré le coupent
  (`models.routing.classifier = false`) : un tour de reprise a un message vide, et une
  réponse scriptée consommée par le classifieur au mauvais moment ferait un test
  faux sans le dire.
- **`Daemon` n'a pas de `decide_approval`.** Le brief le nomme sur le daemon ; la méthode
  publique est `AgentLoop::decide_approval`, et la fonction publique
  `penelope_daemon::agent::decide_approval(&Services, id, &Decision)`, que la RPC appelle
  aussi. C'est elle que les tests utilisent.
- **`rule_created` est un marqueur.** `ApprovalStore::note_rules` écrit `"always"` dès
  qu'une règle est née, quelle que soit la fenêtre (`Session` ici). Le test vérifie la
  présence, pas la valeur.
- **`null` ne fige pas un type.** Une valeur absente (`Option` à `None`) est décrite
  `"null"` ; la comparaison accepte `"null"` face à n'importe quel type, seule la clé est
  figée. Sans cela, un champ facultatif rempli sur une machine et vide sur une autre
  (`doctor`, `session.model`) rendrait le contrat dépendant de l'environnement. Les
  tableaux sont décrits par la réunion des formes de leurs éléments pour la même raison.

## Méthodes figées dans leur cas d'erreur (24)

| Méthodes | Code | Pourquoi |
|---|---|---|
| `mcp.list`, `mcp.show`, `mcp.add`, `mcp.edit`, `mcp.rm`, `mcp.enable`, `mcp.disable`, `mcp.restart`, `mcp.test`, `mcp.auth`, `mcp.logs` | -32603 | « superviseur MCP non démarré dans ce daemon » : aucun superviseur sans serveur réel (voir « Les `pub` qu'il aurait fallu »). |
| `chat.stream`, `tail` | -32603 | Répondent en flux, sur la socket seulement ; `chat_socket.rs` couvre `chat.stream`. |
| `restore`, `eval.run` | -32603 | Répondent par la marche à suivre (0.3.1). |
| `upgrade` (`rollback: true`) | -32603 | Le binaire de test n'est pas une installation ; le message cite le chemin du binaire, seul son type est figé. Toute autre forme irait sur le réseau. |
| `import.hermes` | -32000 | Répertoire absent, donné explicitement pour ne pas lire le vrai répertoire personnel. |
| `wf.run`, `wf.control` | -32000 | Workflow et run inconnus : un run réel exige l'orchestrateur et des paramètres de ticket. |
| `mem.restore` | -32000 | Historique inexistant. |
| `mem.split` | -32603 | Entrée courte : « rien à découper », sans appel au modèle. |
| `skill.install` | -32602 | Sans `source` : refusé avant le réseau. |
| `skill.rollback` | -32603 | Aucune version précédente. Observation : l'erreur remonte brute, « No such file or directory (os error 2) », sans le nom de la skill ni la marche à suivre ; matière à une petite issue, hors périmètre ici. |

Les 80 autres méthodes sont figées sur une réponse réussie, dont `chat.send` (tour complet
avec le pool de runners), `session.compact` (résumé JSON scripté), `model.set`
(`openrouter:…`), `onboard.answer` (numéro de question lu sur la séance ouverte),
`vault.sync` et `mem.diff` (vault initialisé sous git dans le répertoire temporaire :
`git` doit être dans le PATH, il l'est en CI).

## Les `pub` qu'il aurait fallu, et que je n'ai pas ouverts

- **`penelope_daemon::mcp::testing::{FakeConnector, server, tool, declare}`** sont
  `pub(crate)`. Publics, le contrat doré pourrait figer la forme **réussie** des onze
  méthodes `mcp.*` (`McpSupervisor::new(services, connector)` est public, seul le
  connecteur simulé manque). Aujourd'hui elles sont figées sur leur erreur commune, ce qui
  ne protège pas la forme de `mcp.list` ou `mcp.show` pendant la refonte. Un `pub` d'une
  ligne sur le module `testing` (ou un connecteur en mémoire public dans `penelope-mcp`)
  suffirait ; à faire dans un lot qui touche `src/`.
- Rien d'autre : `TelegramGateway::{with_transport, register, process_update,
  flush_outbox}`, `Daemon::{enqueue_message, enqueue_resume, chat_session_for, kv_set,
  recover, publish_config, set_provider_override}`, `runner::{run_pool, process}`,
  `Rpc::{new, handle}`, `agent::decide_approval` et les stores de `Services` ont suffi.

## Section de notes de version, prête pour `docs/progress.md`

Le numéro est posé par l'intégrateur (`### 1.0.0-alpha.N` dans le bloc « Version 1 » si le
lot atterrit sur `v1`, `### 0.17.x` s'il est repris sur `main`).

```markdown
### x.y.z

Les filets avant découpage (épopée #208, tâches T8 et T9 de `design/v1/gel-et-outillage.md`) :
trois tests de bout en bout sur l'API publique du daemon, qui survivent aux déplacements
internes de la refonte.

- **Telegram de bout en bout** (`tests/telegram_e2e.rs`, CA 14.2) : un message du
  propriétaire sur un transport simulé donne une réponse (`tg_outbox`), un effet `completed`
  dans le ledger, les événements du tour ; le daemon reconstruit sur le même répertoire
  retrouve tout et ne rejoue rien (ni l'outil, ni l'envoi, ni l'update).
- **Approbation de bout en bout** (`tests/approval_e2e.rs`, CA 9.4 à 9.6) : un outil à
  risque arrête le tour sur une demande ; approuver puis reprendre exécute l'outil une
  fois ; refuser transmet « Non exécuté » au modèle sans rien écrire ; « pour cette
  session » crée une règle bornée à la session, un second appel passe sans demande, une
  autre session redemande.
- **Contrat RPC doré** (`crates/penelope-evals/tests/rpc_golden.rs`) : 104 fichiers
  `golden/<méthode>.json`, un par méthode de `method::ALL`, clés et types comparés à chaque
  test ; `UPDATE_GOLDEN=1` régénère et liste les cas d'erreur figés (24, dont les onze
  `mcp.*` sans superviseur). Une clé retirée d'une réponse est rouge.
- Matrice des CA régénérée : 75 tests d'acceptation.
```

## Blocages

Aucun. Les trois suites sont vertes sur macOS ; Linux est laissé à la CI.
