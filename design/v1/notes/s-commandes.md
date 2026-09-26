# Notes de livraison : s-commandes (scénarios des commandes Telegram, épopée #208, critère 7)

Branche `v1-s-commandes`, dérivée de `v1` à `99b7075`, non rebasée. Compte de la famille
dans `[scenarios].missing` : **46 → 0 commande** (213 → 167 surfaces au total sur cette
branche, outils et méthodes RPC inchangés).

## Ce qui est livré

- **Étape `kind = "telegram"`** du moteur (autorisée par l'intégrateur), dans un fichier
  nouveau, `crates/penelope-evals/src/scenario/harness/telegram.rs`. Elle pousse une mise
  à jour du propriétaire dans `TelegramGateway::with_transport` sur un `MockTransport` :
  `text` (commande `/…` ou message ordinaire, qui devient un tour) ou `click` (le bouton
  dont le libellé contient ce texte, sur le dernier écran qui en porte un ; l'erreur
  nomme les boutons vus). Elle attend le travail détaché (plus aucun envoi pendant
  100 ms), joue les tours mis en file comme le pool de runners, vide la file d'envoi.
  L'issue de l'étape porte `screens` (méthode, texte ou légende, document, libellés des
  boutons), `reactions` (à part : elles partent de tâches détachées, leur place parmi
  les envois n'est pas reproductible) et `turns` (issues des tours joués).
- La passerelle est construite pour l'étape et relâchée à sa fin ; les slots
  `messenger` et `delivery` sont remis à leur valeur d'avant. Elle n'est pas inscrite au
  courtier d'élicitation (`Broker::attach` n'a pas de retrait : elle survivrait à la vie
  et bloquerait la fermeture). Les jetons de boutons vivent en base (`ActionStore`), un
  clic d'une étape suivante les retrouve.
- Code existant touché au plus petit : la variante `Step::Telegram` et son libellé
  (`scenario.rs`), un champ `masks` au `Spec`, la branche d'aiguillage et le champ
  `telegram` du harnais (`harness.rs`), le module déclaré. `command` est inchangé.
- **Normalisation** (`normalise.rs`) : `callback_data` devient `{{action}}` (jeton tiré
  au hasard) ; un préfixe `d_` (rêve) devient `{{dream:1}}` ; `masks` du scénario
  (expressions régulières) remplace un texte propre à l'hôte par `{{masked}}`.
- **Archtest** : une étape `telegram` dont le `text` commence par `/` exerce sa
  commande (test `a_telegram_step_exercises_its_command`) ; `penelope-evals` rejoint
  `penelope-cli` dans `GATEWAY_DEPENDENTS` (le harnais vit dans `src/`, la dépendance
  est donc normale, raison écrite dans `Cargo.toml`).
- **Sept scénarios** sous `crates/penelope-evals/scenarios/` :

| Répertoire | Commandes | Ce que les attendus montrent |
|---|---|---|
| `commandes-systeme` | /help /status /config /logs /secret /doctor /restart /upgrade | écrans ; /restart et /upgrade rollback s'arrêtent à la confirmation, sans clic |
| `commandes-sessions` | /title /new /sessions /switch /export /stop /close | titres, session fermée par /new puis rebasculée d'un clic, export en document, fermeture confirmée |
| `commandes-reglages` | /model /models /mode /projet /home /quiet /budget /usage | clic d'épinglage : le message suivant part sur `deepseek-v4-flash` (surface, requêtes, usage par modèle) ; `session.project` au journal |
| `commandes-approbations` | /approvals /policies | carte de `fs_write`, clic « Toujours pour ce répertoire », reprise, effet, fichier écrit, règle listée |
| `commandes-memoire` | /retiens /note /recall /appris /pratique /intentions /audit /oublie /mien /accueil /dream /forget | l'entrée retenue est retrouvée, puis retirée par /oublie confirmé : le second /recall est vide ; l'accueil passe à la question 2 |
| `commandes-workflows` | /wf /run /runs /resume /schedules | détail d'un workflow ; /run ouvre la préparation du plan (une réponse scriptée), rien ne s'exécute |
| `commandes-extensions` | /skills /skill /mcp /p | écrans d'état vide de l'instance de test |

## Choix

- Aucune commande laissée dans la liste. Celles qui agiraient sur la machine
  (`/restart`, `/upgrade rollback`, `/skill rollback`) sont jouées jusqu'à leur carte de
  confirmation, sans clic : l'écran est la surface visible, l'effet n'a pas lieu.
  `/upgrade` sans argument lancerait une vérification réseau en fond : il est joué avec
  `rollback`. `/upgrade install` dépend de l'emplacement du binaire (build des sources ou
  non) : écarté.
- `/doctor` : le titre et les boutons sont gardés, le détail (contrôles de l'hôte :
  launchd, FileVault, SSH, dépendances, dérive d'horloge) est masqué. `/status` : la
  mémoire du processus et la durée de vie masquées.
- Le monde ne relève ni la mémoire (entrées du vault) ni les clés `kv` (mode, foyer,
  heures silencieuses, épinglage) : les effets sont montrés par un écran relu
  (`/mode` sans argument, second `/recall`, `/policies`) ou par ce qui suit (le modèle
  épinglé dans la surface). Les ajouter au monde est un travail de moteur, hors de ce lot.
- `/mcp`, `/p`, `/skills`, `/skill` ne montrent que l'état vide : l'instance de test n'a
  ni superviseur MCP ni skill, et le harnais n'a pas d'étape pour en semer.

## Observations à relire (non corrigées, hors périmètre)

- **Règle « Toujours pour ce répertoire » sur un chemin relatif** : pour `fs_write` sur
  `rappel.txt`, `/policies` affiche `path sous «  »` (préfixe vide). Si le préfixe vide
  vaut « partout », le bouton accorde plus que ce qu'il annonce. Attendu fixé tel quel
  dans `commandes-approbations`.
- **Une référence aux services survit à chaque vie**, y compris dans `tour-simple`
  (sans étape telegram) : `shut_down` attend ses cinq secondes par scénario. Préexistant
  sur `v1` ; c'est l'essentiel de la durée de la suite.
- `tests/scenarios.rs` : sept lignes ajoutées à `scenario_cases!`, après
  `message-pendant-un-lot` ; `s-rpc` et `s-outils` en ajoutent au même endroit (conflit
  de fusion trivial).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Les commandes Telegram ont leurs scénarios rejouables (épopée #208, critère 7)

- **Étape `telegram`** des scénarios : une commande, un message ou un clic du
  propriétaire passe par la vraie passerelle sur un transport simulé ; les écrans
  envoyés (texte, boutons), les réactions et les tours joués entrent dans les attendus.
  Les jetons de boutons deviennent `{{action}}`, et un scénario peut masquer un texte
  propre à l'hôte (`masks`).
- **Sept scénarios** couvrent les 46 commandes qui n'en avaient pas : système,
  sessions, réglages (épinglage vérifié sur l'appel suivant), approbations (carte, clic,
  reprise, règle), mémoire (retenir, retrouver, oublier), workflows, extensions.
  `[scenarios].missing` ne contient plus aucune commande.
```

## Vérifications

- `cargo test -p penelope-evals --test scenarios` : 30 verts, deux fois de suite après
  régénération, sans un octet de différence (dont `two_replays_of_every_scenario_are_identical`).
- `cargo test -p penelope-archtest` et `penelope-evals --lib` verts ; `UPDATE_BUDGET=1`
  appliqué après chaque lot.
- Fin de lot : `cargo fmt --all --check` propre, `cargo clippy --workspace --all-targets
  -- -D warnings` propre, `cargo test --workspace` : 81 suites, 2 094 tests verts,
  0 échec, 20 ignorés, sortie 0.

## Blocages

Aucun. L'étape moteur manquante a été signalée à l'intégrateur, qui a autorisé son
écriture dans ce lot.
