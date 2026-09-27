# k-stabilite : fiabilité des tests (épopée #208, lot K)

Branche `v1-k-stabilite`. Trois tests instables, la fin de vie du harnais des scénarios,
le code mort relevé par c-socle.

## 1. Trois tests instables sous charge

Méthode : chaque test en boucle, `--test-threads=1`, pendant que seize `yes > /dev/null`
occupent les huit cœurs ; puis la suite de sa crate en parallèle, sous la même charge.

| Test | Avant | Cause | Après |
|---|---|---|---|
| `tool_jobs_e2e::a_restart_during_a_job_fails_it_and_says_so_without_a_card` | 10 sur 20 rouges, effet `completed` au lieu de `failed` (ligne 309) | **produit** : `conclude` réécrivait un effet déjà clos | 50 sur 50 |
| `bursts::a_burst_asks_before_answering_and_can_be_ingested` | 1 suite sur 10 rouge, un tour créé | test : fenêtre de 40 ms, attente fixe de 200 ms | 50 sur 50, 10 suites sur 10 |
| `steering::a_message_during_a_batch_skips_the_calls_not_started` | vu rouge dans les lots | test : message déposé au bout de 100 ms | 50 sur 50 (les quatre tests de steering), 10 suites sur 10 |

- **Jobs d'outils, vraie course du produit.** Le test simule le redémarrage dans le même
  processus : le premier daemon reste vivant. La reprise du second déclare le job perdu
  (job et effet `failed`), puis tue le `sleep` orphelin par le répertoire des pid ; la
  tâche du premier voit sa commande finir et `conclude` écrivait le ledger sans condition.
  La ligne `tool_jobs` était protégée (« le premier état terminal gagne »), l'effet non.
  Correctif : `conclude` n'écrit l'effet que s'il est encore `dispatching`. Une première
  version gardait sur l'état de la ligne ; elle ouvrait une autre course (`job_cancel`
  clôt la ligne entre `forget` et la garde, l'effet restait `dispatching`, donc une carte
  `effect_unknown` au démarrage suivant) : remplacée. Le test attend la conclusion du job
  fantôme (registre vide) au lieu de lire selon la charge.
- **Rafale Telegram.** La fenêtre est glissante : c'est l'écart entre deux morceaux qui
  compte. Sous charge, deux transferts arrivaient à plus de 40 ms, la rafale se refermait
  à 5 morceaux ou moins et le reste partait en tour. Fenêtre de 1 s, carte et fiche
  source attendues par condition (bornée à 10 s).
- **Steering.** Le message (et, dans le test voisin, l'arrêt) est déposé par un exécuteur
  `During` pendant l'appel nommé, au lieu d'une tâche qui dort 100 ms en pariant sur la
  position de la boucle. Sous charge, le message était pris à `BeforeModelCall`.

## 2. Fin de vie des scénarios

Personne ne gardait les services : `shut_down` comptait deux copies des services tant que
le daemon vivait (la sienne et celle du cœur), or le cœur en tient deux depuis que
`Providers` garde la sienne (T33). La condition ne pouvait jamais être vraie, chaque vie
attendait `SHUTDOWN_WAIT`. `shut_down` attend désormais que seule sa copie du daemon
reste, le lâche, puis attend que seule sa copie des services reste. Elle rend vrai quand
tout est relâché ; deux tests du harnais le tiennent (vie sans fuite, copie gardée).

Suite `scenarios` (binaire seul, même machine) : **832 s avant, 101 s après**, 62 sur 62
verts contre les mêmes attendus, aucun message « références survivantes » ni attente
sous `--nocapture`.

## 3. Code mort (relevé de c-socle)

Vérifié par `grep -rnw` sur tout le dépôt (tests, macros, `cfg(target_os)`, docs), puis
`cargo build --workspace --all-targets` sans avertissement. Un commit par crate.

Supprimées (14) : context `compaction::is_tool_group` ; llm `Fingerprint::system_hash_of`,
`MockProvider::set_models`, `StreamAccumulator::has_pending_calls`,
`TokenEstimator::messages_tokens`, `UsageAnchor::from_usage`, `LlmError::with_status` ;
mcp `McpClient::set_log_level` ; memory `MemoryIndex::with_params`,
`index::annotations_of`, `InjectionMarker::mark_all` ; tools `ToolSpec::new`,
`ToolContext::workspace` (et la structure `ToolContext`, sans aucun usage) ; workflow
`Step::is_terminal_kind`.

Gardée (1) : `penelope_tools::shell::is_read_pipeline`. Aucun appel, mais le contrat
fonctionnel (`design/v1/contrat-fonctionnel.md`, C-5.6) la nomme comme lieu de la
classification ; la retirer demande de corriger la ligne du contrat, hors de mon
périmètre. À faire avec elle.

Non traitées : les sept fonctions de `penelope-kernel` que c-socle note « tests
seulement » (`Config::quiet_range`, `KernelError::invalid_state`, `RiskClass::colour`,
`RiskClass::label_fr`, `schema::property_default`, `session::provenance_kind`,
`SessionStore::get_metadata`) : elles sont appelées par des tests, pas mortes au sens du
relevé. Aucune fonction morte ne tombait dans les fichiers de `k-corrections`.

## Budget

`penelope-daemon/src` : la garde ajoute 4 lignes ; `delivery_text` (pure fonction d'un
`ToolJob`) descend dans `penelope_executor::jobs`, commit de déplacement sans changement
de corps : le daemon repasse sous son plafond (R4). Aucune valeur de `budget.toml` à
changer (`UPDATE_BUDGET=1` n'a rien réécrit).

## Vérifications de fin

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` : verts.
- `scripts/switch-check.sh` : deux critères manquants, aucun de mon fait. Point 1 : la CI
  de `v1` sur GitHub (dernier run en échec, état distant). Point 7 : la mesure de
  couverture échoue, parce que le scénario `rpc-mise-a-jour` (et donc
  `two_replays_of_every_scenario_are_identical`) refuse de jouer `upgrade` depuis
  `target/llvm-cov-target/debug/…` : `penelope_app::helpers::is_source_build` ne
  reconnaît que `target/debug` et `target/release`. Préexistant ; à corriger dans
  `helpers.rs` (hors périmètre) ou à sauter dans la mesure.
- `scripts/coverage-check.sh -- --skip rpc_mise_a_jour --skip
  two_replays_of_every_scenario_are_identical` : toutes les crates sous leur plafond
  sauf `penelope-evals`, 491 lignes non couvertes pour un plafond de 473. La même mesure
  sur la base (`d6d6dd1`, worktree temporaire) donne **493** : l'écart vient des deux
  tests sautés, pas de ce lot, qui gagne deux lignes. Les conditions de la mesure qui a
  posé 473 ne sont pas écrites ; je ne les ai pas retrouvées.

## Incident

Pour comparer au commit de base, j'ai lancé `git stash` puis `git stash pop` sans
modification locale : le `pop` a appliqué la pile commune des worktrees, soit l'entrée
de `l-gel-fin` (`scripts/check-budget.sh`, en conflit). J'ai rétabli ce seul fichier sur
mon `HEAD` ; l'entrée `stash@{0}` de `l-gel-fin` est restée intacte dans la pile. Leçon :
jamais de `git stash` dans un worktree partagé.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Tests fiables sous charge, scénarios huit fois plus rapides (épopée #208, lot K)

- **Un job d'outil perdu au redémarrage le reste** : si sa tâche conclut quand même
  (processus orphelin tué par la reprise), l'effet du ledger n'est réécrit que s'il est
  encore `dispatching`. Avant, un effet `failed` pouvait redevenir `completed` derrière
  un job `failed`. `tool_jobs_e2e` échouait une fois sur deux sous charge.
- **Trois tests ne parient plus sur des durées** : la rafale Telegram (fenêtre de 40 ms
  dépassée sous charge), le steering (message et arrêt déposés pendant l'appel au lieu
  de 100 ms après le départ). Chacun passe 50 fois de suite sous charge.
- **La suite `scenarios` passe de 832 s à 101 s** : la fermeture d'une vie comptait mal
  les copies des services tenues par le daemon et attendait cinq secondes à chaque vie.
- **Quatorze fonctions mortes retirées** (context, llm, mcp, memory, tools, workflow),
  et la structure `ToolContext`, sans usage.
```
