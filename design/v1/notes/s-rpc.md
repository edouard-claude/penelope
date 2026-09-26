# Notes de livraison : scénarios des méthodes RPC (épopée #208, critère 7, R10)

Branche `v1-s-rpc`, dérivée de `v1` à `99b7075` (1.0.0-alpha.17). Périmètre tenu :
`crates/penelope-evals/src/scenario.rs` et `src/scenario/**`, douze répertoires sous
`crates/penelope-evals/scenarios/rpc-*`, `crates/penelope-archtest/src/scenarios.rs` et
ses tests, `budget.toml` par `UPDATE_BUDGET` seulement, ce fichier. Deux débords, tous
deux nécessaires : `crates/penelope-evals/tests/scenarios.rs` (un cas par répertoire, le
test `every_scenario_directory_has_a_case` l'exige) et `crates/penelope-evals/Cargo.toml`
(dépendance `penelope-mcp-host`, déjà dans le workspace, raison écrite), `Cargo.lock` par
conséquence. Aucune ligne `version =`, aucun fichier du daemon. Aucune clé d'API.

## Compte

Méthodes RPC sans scénario (`[scenarios].missing`, préfixe `rpc:`) : **108 → 4**.

| Commit | Contenu | Reste |
|---|---|---|
| `c216d51` | étape `rpc`, `[[observe]]`, `[[mcp_servers]]`, normalisation, archtest ; `rpc-sessions` | 94 |
| `e6863b1` | `rpc-config-et-modeles`, `rpc-approbations`, `rpc-planifications`, `rpc-workflows` | 62 |
| `62f5dd4` | `rpc-memoire`, `rpc-coffre-et-accueil` | 40 |
| `835309a` | `rpc-conversation`, `rpc-mcp-et-import`, `rpc-skills`, `rpc-exploitation`, `rpc-arret` | 4 |

Les quatre qui restent, une ligne chacune :

- `rpc:doctor` : le diagnostic interroge la machine réelle (service launchd, veille,
  FileVault, versions de git, npx, uvx, docker, inventaire des outils installés) et
  résout des noms en DNS (`net.api.telegram.org`, `net.openrouter.ai`) : réseau réel et
  sortie propre à chaque poste, aucun contrôle ne se rejoue.
- `rpc:mcp.auth` : démarrer l'autorisation fait la découverte OAuth du serveur HTTP,
  la terminer échange le code contre un jeton : réseau réel dans les deux cas.
- `rpc:skill.install` : la source est toujours une archive téléchargée
  (`archive_url`, `fetch`) ; seul le refus d'une source mal écrite est local.
- `rpc:upgrade` : vérifier interroge les releases GitHub, installer ou revenir en
  arrière remplace le binaire en cours d'exécution puis demande un redémarrage.

## Le format ajouté (les anciens scénarios restent valides)

```toml
[[steps]]
kind = "rpc"
method = "schedule.add"           # une méthode de api::method
params = { kind = "cron", spec = { expr = "0 9 * * 1" }, target = { type = "prompt", prompt = "…" } }
bind = "digest"                   # garde la réponse pour la suite
# params = { id = "$digest.id" }  # $session, $nom.chemin, $json:nom.chemin (texte JSON)
pick = ["/id", "/servers/*/name"] # pointeurs JSON gardés, * pour chaque élément
mask = ["/rss_mb"]                # pointeurs masqués en {{masked}}
lines = ["^# TYPE penelope_"]     # lignes gardées d'un texte de la réponse
error = true                      # l'échec est attendu ; sinon une erreur fait échouer
during = "message"                # tail : message joué pendant que le flux écoute
```

- L'appel passe par `Rpc::handle`, ou `handle_streaming` sur un tampon pour
  `chat.stream` (notifications entières) et `tail` (types d'événements vus, dans l'ordre
  de première apparition). Les tours que l'appel met en file sont joués pendant qu'il
  attend, un à un, comme le pool de runners : `chat.send`, `approve`, `deny`,
  `schedule.run_now` rendent leur réponse et le tour joué est dans `drained`.
- L'issue de l'étape (`outcome`) porte `result` ou `error` (code et message), `events`,
  `drained`, et `shutting_down` / `wants_restart` quand un drapeau du daemon est levé.
- `[[observe]]` (`name`, `sql`) : une requête en lecture après le run, une ligne
  `observed` par ligne de résultat, les colonnes JSON relues comme du JSON. C'est ce qui
  assertit l'effet d'une méthode sur une table que le relevé ordinaire ne lit pas
  (`policies`, `schedules`, `workflow_runs`, `mem_entries`, `intents`, `mcp_servers`,
  `sessions`, `kv`).
- `[[mcp_servers]]` (`name`, `tools`) : le vrai `McpSupervisor` branché sur le
  `FakeConnector` de `penelope_mcp_host::testing`, aucun processus lancé ; exclusif de
  `[[mcp_tools]]` (la passerelle simulée des scénarios de crash).
- `[[files]]` prend `root = "vault"` ou `root = "skills"` (défaut : le workspace).
- Normalisation : tout identifiant préfixé garde son préfixe comme nom de jeton
  (`sch_…` en `{{sch:1}}`, `p_…` en `{{p:1}}`) ; la version du workspace devient
  `{{version}}`, un bump ne réécrit donc aucun attendu ; `ms`, `p50_ms`, `p95_ms`,
  `snapshot_ms` deviennent `{{ms}}`.
- Archtest R10 : une étape `kind = "rpc"` exerce `rpc:<method>` (lu dans
  `scenario.toml`, jamais déclaré).

## Les scénarios, et ce que chacun assertit sur le monde après

| Répertoire | Méthodes | Ce qui est vérifié |
|---|---|---|
| `rpc-sessions` | `chat.send`, `chat.stop`, `session.*` sauf fork, rewind, compact | session neuve fermée et titrée, budget, mode, projet, alias épinglé et session courante (kv), troisième purgée |
| `rpc-config-et-modeles` | `config.*`, `secret.*`, `model.*` | réglage écrit, relu, gardé par `config.reload` ; clé inconnue, nom de secret dangereux, fournisseur mal écrit refusés ; génération montée à chaque écriture |
| `rpc-approbations` | `approvals`, `approve`, `deny`, `policies`, `policy.revoke`, `quiet` | refus avec motif (rien d'écrit), double décision refusée, accord « toujours » qui fait naître une règle, écriture suivante sans carte, règle révoquée |
| `rpc-planifications` | `schedule.*` | pause, reprise, déplacement, déclenchement dont le tour est joué, suppression, compteur de passages |
| `rpc-workflows` | `wf.*` | run démarré, paramètre inconnu refusé, plafond relevé, pause, reprise, annulation (le moteur de runs ne tourne pas dans le harnais : le run reste à `plan`) |
| `rpc-memoire` | `mem.*` sauf `mem.diff` | index d'un vault semé, oubli qui retire l'entrée sans toucher au fichier, découpage refusé, audit, rêve à blanc |
| `rpc-coffre-et-accueil` | `vault.*`, `mem.diff`, `intent.*`, `onboard.*` | intention armée par le modèle puis annulée, profil écrit par l'accueil, vu par `mem.diff`, validé par `vault.sync` |
| `rpc-conversation` | `chat.stream`, `tail`, `session.compact`, `session.fork`, `session.rewind`, `export`, `audit.*`, `history.*`, `store.rebuild`, `usage` | résumé publié, bifurcation, archive du retour en arrière, chaîne et caches vérifiés |
| `rpc-mcp-et-import` | `mcp.*` sauf `mcp.auth`, `import.hermes` | serveur ajouté, modifié, désactivé, réactivé, retiré ; import Hermes simulé puis appliqué, secret rangé, serveurs importés prêts ou en échec |
| `rpc-skills` | `skill.*` sauf `skill.install` | retour à la version sauvegardée, relu, second retour refusé |
| `rpc-exploitation` | `status`, `paths`, `metrics`, `jobs`, `backup`, `restore`, `eval.run` | état après un tour, instantané écrit, refus documentés |
| `rpc-arret` | `shutdown`, `restart` | drapeaux d'arrêt et de redémarrage |

## Points pour l'intégrateur

- **`tests/scenarios.rs`** : douze cas ajoutés en fin de `scenario_cases!`, après un
  commentaire. `s-outils` et `s-commandes` ajoutent les leurs au même endroit : conflit
  de fusion attendu, à résoudre en gardant toutes les lignes.
- **Chaque scénario attend cinq secondes à sa fermeture** (préexistant, pas introduit
  ici) : `shut_down` voit une référence aux services survivre à la vie (« des références
  survivent à la vie (0 sur le daemon, 1 sur les services) ») et attend `SHUTDOWN_WAIT`.
  `tour-simple` le montre sur la base telle quelle. La suite `scenarios` passait déjà
  à 369 s (dont le double rejeu) ; les douze scénarios ajoutés y mettent environ deux
  minutes de plus. À chercher du côté d'une tâche lancée pendant un tour qui garde un
  `Arc<Services>` sans le daemon.
- **Course dans `handle_streaming`** (daemon, hors périmètre) : la boucle saute
  `BusKind::Finished`, la vidange finale non ; le client de `chat.stream` reçoit donc
  une notification `done` une fois sur deux environ. Le harnais l'écarte ; le corriger
  dans le daemon ne fera bouger aucun attendu.
- **Couvertures minces, assumées** : `jobs` rend une liste vide (un job naît d'un
  `shell_exec` ou d'un sous-agent en arrière-plan, effet sur la machine ou course) ;
  `mem.restore` est joué dans son refus (seules les écritures de la consolidation sont
  versionnées, et `mem.forget` retire sans réécrire) ; `metrics` n'asserte que les
  trois jauges calculées à la demande, le registre étant global au processus de test.
- **git** : `vault.sync` et `mem.diff` lancent le `git` local dans le répertoire du
  test (comme `rpc_golden`) ; sortie, empreinte et nom de branche écartés par `pick`.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Les méthodes RPC rejouées par des scénarios (#208, critère 7)

- **Étape `rpc` des scénarios** : une méthode appelée comme la socket locale la sert
  (`chat.stream` et `tail` compris), les tours qu'elle met en file joués pendant
  qu'elle attend, la réponse normalisée dans le monde attendu ; `bind` et `$nom.chemin`
  enchaînent les appels, `pick`, `mask` et `lines` écartent ce qui dépend de la
  machine, `error = true` attend un refus. `[[observe]]` relève une table après le run,
  `[[mcp_servers]]` branche le vrai superviseur MCP sur un connecteur de test.
- **Douze scénarios par famille** (sessions, configuration et modèles, approbations,
  planifications, workflows, mémoire, vault et accueil, conversation, MCP et import
  Hermes, skills, exploitation, arrêt) : 104 méthodes sur 108 exercées et assertées sur
  le monde après. Restent `doctor`, `mcp.auth`, `skill.install` et `upgrade`, qui
  demandent la machine ou le réseau réels.
- La version du workspace est normalisée dans les attendus : un bump n'en réécrit
  aucun.
```

## Vérifications

Pendant l'itération : chaque scénario régénéré puis rejoué au moins neuf fois de suite
sans un octet de différence (la course de `chat.stream` a été trouvée ainsi, au
troisième rejeu) ; `cargo test -p penelope-archtest` vert après chaque
`UPDATE_BUDGET=1` ; `cargo clippy -p penelope-evals -p penelope-archtest --all-targets
-- -D warnings` propre.

Finales, une fois : `cargo fmt --all --check` propre ; `cargo clippy --workspace
--all-targets -- -D warnings` propre ; `cargo test --workspace`, lancé en deux passes au
premier plan (hors `penelope-evals` : 64 suites, 1 958 verts, 9 ignorés ;
`penelope-evals` : 17 suites, 142 verts, 11 ignorés, dont `scenarios` 35 verts en
460 s). Une première passe sans `--no-fail-fast` a vu
`a_restart_during_a_job_fails_it_and_says_so_without_a_card` (`penelope-daemon`,
`tool_jobs_e2e`) échouer une fois sous charge ; rejoué seul trois fois, puis dans la
passe complète : vert. Aucun fichier du daemon n'est touché ici : aléa préexistant, à
surveiller.
