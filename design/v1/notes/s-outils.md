# Notes de livraison : s-outils (scénarios des outils natifs, critère 7)

Branche `v1-s-outils`, sur `v1` à `99b7075` (1.0.0-alpha.17), sans rebase. Deux lots :
le premier sans toucher au moteur, le second après que l'intégrateur a ouvert
`crates/penelope-evals/src/scenario/` pour les manques relevés.

Périmètre : `crates/penelope-evals/scenarios/outils-*/`, `crates/penelope-evals/src/scenario/`
(second lot, un commit par manque), `budget.toml` par `UPDATE_BUDGET=1` seulement, ces
notes. Sortie obligatoire : une ligne par scénario dans
`crates/penelope-evals/tests/scenarios.rs` (`scenario_cases!`), sans laquelle
`every_scenario_directory_has_a_case` échoue ; `s-commandes` ajoute des lignes au même
endroit, le conflit se règle en gardant les deux listes.

## Ce qui est livré

Outils natifs sans scénario (`[scenarios].missing`, préfixe `outil:`) : **59 avant, 0
après** (31 après le premier lot). Dix-sept scénarios, tous verts, rejoués dix fois de
suite sans un octet de différence.

| Répertoire | Outils exercés | Ce que les attendus montrent |
|---|---|---|
| `outils-fichiers-et-shell` | `fs_list`, `fs_search`, `fs_edit`, `shell_exec` | lot de lectures, ligne corrigée, `rm` qui demande une carte même en mode `auto`, fichier toujours là |
| `outils-memoire` | `mem_remember`, `mem_search`, `mem_get`, `mem_neighbors`, `mem_note`, `mem_forget` | préférence retenue, retrouvée, relue par uid ; citation du propriétaire vérifiée (origine `owner`) ; oubli approuvé, recherche vide ensuite |
| `outils-skills` | `skill_propose`, `skill_search`, `skill_load`, `skill_patch` | skill écrite, retrouvée, chargée, corrigée puis rechargée |
| `outils-soi` | `time_now`, `self_docs`, `self_status`, `config_set`, `session_metadata`, `session_notes` | heure de Paris sur l'horloge de test, `config_set` destructif approuvé puis appliqué, critère coché, notes écrites puis relues |
| `outils-http-garde` | `http_fetch` | métadonnées cloud et domaine hors liste blanche refusés avant toute connexion |
| `outils-workflows` | `workflow_author`, `workflow_list`, `workflow_describe`, `workflow_plan`, `step_done`, `return_value` | workflow écrit et retrouvé, plan v1 non lancé, `step_done` et `return_value` refusés hors d'un run |
| `outils-workflow-run` | `workflow_start`, `workflow_status`, `workflow_control` | run créé avec sa session, relu `running`, annulé, relu `cancelled` ; cartes de progression au canal |
| `outils-historique` | `history_grep`, `history_expand_query` | extraits des échanges semés, numérotés, titrés, datés |
| `outils-historique-resumes` | `artifact_read`, `history_describe`, `history_expand` (et `/compact`, `fs_read`) | rapport externalisé relu par artefact, nœud LCM décrit et déplié jusqu'au message d'origine |
| `outils-question` | `ask_user` | la question rend la main, la réponse arrive au message suivant |
| `outils-planification` | `schedule_create`, `schedule_list`, `schedule_move`, `schedule_delete`, `intent_create`, `intent_list`, `intent_cancel` | rappel créé, listé, déplacé, supprimé ; intention armée, datée refusée, annulée ; inventaire vide à la fin |
| `outils-canal` | `send_message`, `send_file`, `send_voice` | envois relevés au canal simulé ; vocal refusé au-delà de `voice.max_chars` |
| `outils-git` | `git_status`, `git_diff`, `git_branch`, `git_commit`, `git_clone`, `git_push` (et `shell_exec` jusqu'au bout) | juge des commandes puis accord, dépôt préparé, diff, branche, commit refusé par le crochet `pre-commit`, clone reconnu déjà présent, push d'une option refusé |
| `outils-jobs` | `job_list`, `job_status`, `job_wait`, `job_cancel` | deux jobs lancés, listés `working`, le court attendu `completed`, le long annulé puis attendu `cancelled` |
| `outils-sous-agent` | `sub_agent_spawn` | le sous-agent lit le fichier et rend sa conclusion comme résultat d'outil |
| `outils-images` | `image_generate`, `image_inspect` | image générée envoyée au canal ; GIF d'un pixel décrit par la vision, rendu en donnée non fiable |

## Le moteur, second lot (un commit par manque)

| Commit | Changement | Attendus existants |
|---|---|---|
| `durationMs` | clé de durée de `shell_exec` normalisée en `{{ms}}` | aucun ne bouge |
| préfixes courts | tout ULID à préfixe de une à quatre lettres (`i_`, `sch_`, `tj_`, `c_`…) devient un jeton nommé | aucun ne bouge : aucun ne contenait de tel identifiant |
| notes de session | `notes/<titre>-<6 derniers caractères de la session>.md` devient `-{{session:n}}.md` | aucun ne bouge |
| orchestrateur | posé à chaque vie comme `supervisor.rs:177`, relâché à la fin de la vie | aucun ne bouge, ni `expected.jsonl` ni `surface.jsonl` |
| canal simulé | `messenger = true` dans `scenario.toml`, envois relevés en lignes `sent` ; opt-in, distinct de la passerelle Telegram simulée de `s-commandes` | aucun ne bouge |
| identifiants cités | `{{id:<préfixe>:<n>}}` dans `model.jsonl`, résolu dans les résultats d'outils de la requête reçue ; un jeton sans identifiant fait échouer le rejeu en le nommant | aucun ne bouge |
| marqueur du juge | ULID en minuscules (`<commande-…>` du juge shell) en `{{marker:n}}` | aucun ne bouge |
| durées en texte | `"durationMs": 22` dans un JSON rendu en texte, une ou deux fois échappé | aucun ne bouge |

Chaque commit porte son test unitaire (`normalise.rs`, `harness/ids.rs`, format de
`scenario.toml`). Les trois derniers ne figuraient pas dans la demande initiale : ils
sont apparus en écrivant les scénarios git et jobs.

## Choix

- Mode `auto` (`tools.approval_mode`) partout où l'on écrit ; une étape `approve` quand
  le destructif ou la carte est le sujet (`config_set`, `mem_forget`, commande composée).
- Outils à la demande appelés par leur nom ; **jamais deux lectures d'outils à la demande
  dans la même réponse** : voir le défaut ci-dessous.
- Les sorties de git traduites selon la locale ou datées (commit réussi, push) ne sont
  pas jouées : ce poste est en français, la CI Linux en anglais. Les chemins joués
  (porcelaine, diff, crochet, validation de ref, dépôt déjà présent) sont identiques
  partout.
- Un vocal qui part demande `ffmpeg`, absent peut-être de la CI : seul le refus par
  `voice.max_chars` est joué.
- Les jobs ne sont relus qu'après avoir atteint l'état relu (`job_wait` après
  `job_cancel`) : un état relu pendant une course changerait d'un rejeu à l'autre.

## Défauts relevés en écrivant les scénarios (non corrigés, hors périmètre)

1. **Promotion perdue des outils à la demande** : `penelope-executor/src/tools_on_demand.rs`,
   `touch` lit la table puis la réécrit sans verrou. Deux lectures d'outils à la demande
   dans un même lot partent en parallèle (#85) et l'une des deux promotions se perd : la
   liste d'outils du tour suivant change d'un rejeu à l'autre (vu sur `mem_get` et
   `mem_neighbors`, 3 fois sur 10). À corriger par une écriture atomique, puis un scénario
   qui garde deux lectures parallèles.
2. **L'étape `user` d'un run n'est pas exécutée dans le harnais** : le run reste `running`
   sur sa première étape et `workflow_start` rend après son attente de cinq secondes. Le
   pilote des runs n'est pas démarré par `Daemon::from_services` ; le scénario le dit.
3. **Une référence aux services survit à chaque vie**, dans tous les scénarios, déjà sur
   la base : `shut_down` attend cinq secondes puis ferme sans elle (« des références
   survivent à la vie (0 sur le daemon, 1 sur les services) »). C'est la moitié de la
   durée de la suite.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Scénarios des outils natifs (épopée #208, critère 7)

- **Chaque outil natif a un scénario rejouable sans clé** : dix-sept scénarios, un par
  famille (fichiers et shell, mémoire, skills, soi, garde http, workflows et runs,
  historique et résumés, question, planification, canal, git, jobs, sous-agent, images).
  Chacun asserte l'effet dans le monde : fichier corrigé, carte demandée, préférence
  retenue puis oubliée, rappel créé puis supprimé, commit refusé par un crochet, job
  annulé, run annulé, image envoyée. `[scenarios].missing` ne liste plus aucun outil.
- **Le moteur de scénarios** branche l'orchestrateur à chaque vie, offre un canal simulé
  (`messenger = true`, envois relevés en lignes `sent`), laisse le script citer un
  identifiant créé pendant le run (`{{id:<préfixe>:<n>}}`, échec clair s'il manque), et
  normalise les durées de `shell_exec`, les ULID à préfixe court, le marqueur du juge des
  commandes et le fichier de notes de session.
```

## Vérifications

`cargo fmt --all --check` propre ; `cargo clippy --workspace --all-targets -- -D warnings`
propre ; `cargo test --workspace` : 81 suites, 2 105 tests verts, 0 échec, 20 ignorés
(suites réseau), sortie 0, `two_replays_of_every_scenario_are_identical` compris. Les
dix-sept scénarios `outils-*` rejoués dix fois de suite, identiques ; après chaque
changement du moteur, les scénarios existants rejoués sans qu'un attendu bouge.
`UPDATE_BUDGET=1 cargo test -p penelope-archtest` après chaque lot (79 tests verts).
