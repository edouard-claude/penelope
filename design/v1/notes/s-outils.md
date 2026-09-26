# Notes de livraison : s-outils (scénarios des outils natifs, critère 7)

Branche `v1-s-outils`, sur `v1` à `99b7075` (1.0.0-alpha.17). Périmètre tenu :
`crates/penelope-evals/scenarios/outils-*/` (répertoires nouveaux), `budget.toml` par
`UPDATE_BUDGET=1` seulement, ces notes. Une seule sortie de périmètre, obligatoire :
une ligne par scénario dans `crates/penelope-evals/tests/scenarios.rs` (`scenario_cases!`),
sans laquelle `every_scenario_directory_has_a_case` échoue. `s-commandes` ajoute des lignes
au même endroit : le conflit de fusion est trivial (garder les deux listes).

Le moteur (`crates/penelope-evals/src/scenario/`) n'a pas été touché.

## Ce qui est livré

Outils natifs sans scénario : **59 avant, 31 après** (`[scenarios].missing`, préfixe
`outil:`). Huit scénarios, un par famille, tous verts, rejoués deux fois sans un octet de
différence.

| Répertoire | Outils exercés | Ce que les attendus montrent |
|---|---|---|
| `outils-fichiers-et-shell` | `fs_list`, `fs_search`, `fs_edit`, `shell_exec` | lot de lectures, ligne corrigée dans le fichier, `rm` qui demande une carte même en mode `auto`, fichier d'idées toujours là |
| `outils-memoire` | `mem_remember`, `mem_search`, `mem_get`, `mem_neighbors`, `mem_note` | préférence retenue au niveau profil puis retrouvée (score 1), citation du propriétaire vérifiée : candidate d'origine `owner` |
| `outils-skills` | `skill_propose`, `skill_search`, `skill_load`, `skill_patch` | skill écrite, retrouvée, chargée, corrigée puis rechargée avec l'étape ajoutée |
| `outils-soi` | `time_now`, `self_docs`, `self_status`, `config_set`, `session_metadata` | heure de Paris sur l'horloge de test, `config_set` destructif approuvé puis appliqué (génération 3), critère posé puis coché |
| `outils-http-garde` | `http_fetch` | point de métadonnées cloud et domaine hors liste blanche refusés avant toute connexion |
| `outils-workflows` | `workflow_author`, `workflow_list`, `workflow_describe`, `workflow_plan`, `step_done`, `return_value` | workflow écrit et retrouvé, plan en version 1 non lancé, `step_done` et `return_value` refusés hors d'un run |
| `outils-historique` | `history_grep`, `history_expand_query` | extraits des échanges semés, numérotés, avec titre et date de session |
| `outils-question` | `ask_user` | la question rend la main, la réponse arrive au message suivant, vue juste après la question |

## Choix

- Mode d'approbation `auto` (`tools.approval_mode`) dans les scénarios qui écrivent :
  personne ne répond aux cartes, sauf une étape `approve` quand le destructif est le sujet
  (`config_set`).
- Outils à la demande appelés directement par leur nom : l'exécuteur les accepte et
  archtest les compte ; `tool_call` n'ajoutait rien à ce qui est vérifié.
- `shell_exec` n'est exercé que jusqu'à sa carte : une commande qui s'exécute rend
  `durationMs`, que le normaliseur ne masque pas (voir blocages).
- `session_notes` a été retiré de `outils-soi` : le fichier de notes porte un suffixe tiré
  de l'identifiant de session (`notes/cli-xa9cdw.md`), différent à chaque rejeu.

## Les 31 outils laissés dans la liste, et pourquoi

Chaque ligne nomme le manque du moteur (numéros de la section suivante).

- `git_status`, `git_diff`, `git_branch`, `git_commit`, `git_clone`, `git_push` : préparer un
  dépôt demande `shell_exec` (1). En plus, `git_commit` rend un SHA-1 et une sortie
  traduite selon la locale, et `git_push` rend le stderr de git, lui aussi traduit : le
  scénario prévu passe par un crochet `pre-commit` qui refuse et par une branche refusée
  par `validate_ref`, seuls chemins identiques sur macOS en français et Linux en anglais.
- `intent_create`, `intent_list` : l'identifiant `i_<ULID>` n'est pas normalisé (2).
- `intent_cancel` : (2) et (5).
- `schedule_create`, `schedule_list` : pas d'orchestrateur branché, « planificateur
  indisponible ici » (3).
- `schedule_move`, `schedule_delete` : (3) et (5).
- `workflow_start`, `sub_agent_spawn`, `image_generate`, `image_inspect` : (3).
- `workflow_status`, `workflow_control` : (3) et (5).
- `send_message`, `send_file`, `send_voice` : aucun messenger dans le harnais (4) ; sans
  lui, seule l'erreur « aucun canal » est observable.
- `job_list`, `job_status`, `job_wait`, `job_cancel` : un job naît d'un `shell_exec` en
  arrière-plan (1), et son identifiant est aléatoire (5).
- `history_describe`, `history_expand` : identifiant de nœud (5).
- `artifact_read` : identifiant d'artefact (5).
- `mem_forget` : uid aléatoire (5).
- `session_notes` : nom de fichier tiré de l'identifiant de session (6).

Aucun n'est laissé pour effet réel sur la machine ou le réseau : `http_fetch` vers
l'extérieur est le seul cas, et il est couvert par sa garde.

## Ce qu'il manque au moteur (demandé à l'intégrateur)

1. `normalise.rs` : `durationMs` dans `DURATION_KEYS` (résultat de `shell_exec`).
2. `normalise.rs` : les ULID à préfixe court collé par `_` (`i_01M3…`) ne sont pas
   remplacés ; un motif générique `[a-z]{1,4}_<ULID>`.
3. `harness/lifecycle.rs` : poser l'orchestrateur à chaque vie, comme `supervisor.rs:177`.
4. Un messenger simulé qui enregistre les envois, relevés dans le monde.
5. Des identifiants d'exécution citables dans `model.jsonl` (`{{id:<préfixe>:<n>}}`,
   résolu dans les résultats d'outils de la requête reçue), les ULID étant aléatoires.
6. Un nom de fichier de notes de session stable, ou sa normalisation.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Scénarios des outils natifs (épopée #208, critère 7)

- **Huit scénarios rejouables sans clé**, un par famille d'outils : fichiers et shell,
  mémoire, skills, soi-même et session, garde http, workflows, historique, question au
  propriétaire. Chacun asserte l'effet dans le monde (fichier corrigé, carte demandée,
  préférence retenue puis retrouvée, citation vérifiée, skill corrigée, réglage appliqué
  après approbation, refus réseau avant connexion, workflow écrit et plan proposé).
- **28 outils natifs de plus ont un scénario** : `[scenarios].missing` passe de 59 à 31
  outils. Les 31 restants attendent le moteur : orchestrateur et messenger branchés dans
  le harnais, identifiants d'exécution citables par le script, deux normalisations.
```

## Vérifications

`cargo fmt --all --check` propre ; `cargo clippy --workspace --all-targets -- -D warnings`
propre ; `cargo test --workspace` : 81 suites, 2 093 tests verts, 0 échec, 20 ignorés
(suites réseau), sortie 0. Chaque scénario nouveau a été rejoué deux fois de suite après
`UPDATE_SCENARIOS=1`, sans différence. `UPDATE_BUDGET=1 cargo test -p penelope-archtest`
après chaque lot (79 tests verts).
