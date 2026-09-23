# Inventaire fonctionnel de Pénélope 0.17.58

Contrat à conserver par la V1 : tout ce que la 0.17.x fait aujourd'hui, classé par domaine,
avec pour chaque capacité l'issue ou la version qui l'a apportée, le code qui la porte et le
test qui la prouve. Ce document est la garantie que la refonte ne perd rien.

## 0. Périmètre, sources et méthode

- Référence : commit `fbbf906` (« Documentation : actualiser le README et clarifier le flux
  runtime (#195) »), version de workspace `0.17.58` (`Cargo.toml:6`), 17 crates,
  `#![forbid(unsafe_code)]` partout.
- Sources lues : le code de `crates/`, `docs/progress.md` (88 sections de version, de 0.1.0 à
  0.17.58), `README.md`, `docs/README.md`, `docs/install-headless.md` (référence générée des
  222 clés et des 57 outils), `docs/telegram.md`, `docs/mcp.md`, `docs/workflows.md`,
  `docs/context.md`, `docs/runtime-events.md`, `docs/ca-matrix.md`, `docs/decisions/0001` à
  `0010`, `schemas/workflow.schema.json`, `.github/workflows/ci.yml` et `release.yml`,
  `Makefile`, et la liste des 173 issues fermées du dépôt `edouard-claude/penelope` (#1 à
  #194, lue par `gh issue list --state closed`). Le fichier `spec/PRD-penelope.md` n'a pas
  servi de référence : il date du jour 1.
- Index des tests : 1 783 fonctions annotées `#[test]` ou `#[tokio::test]` trouvées dans
  `crates/` (fichier `tests-index.txt` à côté de ce document, une ligne par test :
  `chemin:ligne nom`). `docs/progress.md` annonce 1 735 tests verts hors réseau ; l'écart
  vient des tests réseau ignorés (19, listés en 1.14), des tests réservés à macOS, des
  doublons de `fn main` dans les exemples de doc, et des 11 tests non commités décrits
  ci-dessous.
- Arbre de travail : le dépôt porte un lot **non commité** (issue #205, « ce que le modèle
  avait sous les yeux ») : méthode RPC `audit.show`, commande `penelope audit show`,
  modules `crates/penelope-daemon/src/audit.rs` et `prompt_snapshot.rs`, migration
  `0018_prompt_snapshots` (table `prompt_snapshots`, colonnes `system_hash`, `tools_hash`,
  `request_hash` sur `llm_requests`, colonne `request` retirée), contrôle `doctor`
  `prompt.stability`, découpe du préfixe en tuiles (`TileMap` dans `tiers.rs`) et
  `AGENTS.md` (copie de `CLAUDE.md`). L'inventaire décrit HEAD ; ce lot est signalé à part
  (section 2.11, ligne C-11.16) pour que la V1 ne l'oublie pas s'il est livré.
- Convention des tableaux : « capacité | origine (issue ou version) | code qui la porte |
  test qui la prouve ». Un test est cité par son nom (unique dans le workspace) et son
  fichier ; « aucun test » est écrit en toutes lettres ; « partiel » signifie que le mécanisme
  voisin est testé mais pas le comportement nommé. Chaque ligne porte un identifiant `C-d.n`
  pour le compte final.
- Les listes de la section 1 sont exhaustives : c'est ce que la V1 doit exposer à
  l'identique (noms, valeurs, formats).

## 1. Contrats publics à exposer à l'identique

### 1.1 RPC : JSON-RPC 2.0 sur socket Unix locale

Code : `crates/penelope-kernel/src/api.rs` (contrat), `crates/penelope-daemon/src/rpc.rs`
(service), `crates/penelope-platform/src/ipc.rs` (socket, jeton), `crates/penelope-cli/src/client.rs`.

- Enveloppe : `RpcRequest { jsonrpc: "2.0", id, method, params, auth }`, une requête par
  ligne (NDJSON) ; `auth` porte le jeton de session `{state}/rpc.token` (`0600`, tiré à
  chaque démarrage, issue #91) ; sans lui ou avec un autre, réponse `unauthorized`,
  comparaison à temps constant (`ipc::tokens_match`). `RpcResponse { jsonrpc, id, result |
  error {code, message, data} }`. Une requête sans `id` est une notification (`tail`).
- Codes d'erreur : `-32700` PARSE_ERROR, `-32600` INVALID_REQUEST, `-32601`
  METHOD_NOT_FOUND, `-32602` INVALID_PARAMS, `-32603` INTERNAL_ERROR ; propres à Pénélope :
  `-32000` NOT_FOUND, `-32001` CONFLICT, `-32003` DENIED.
- Test de couverture : `every_declared_method_is_either_served_or_explicitly_absent`
  (`rpc.rs:2461`) fixe à vide la liste des méthodes déclarées non servies ;
  `method_list_has_no_duplicates` (`api.rs:519`) ; `ca_15_1_every_command_maps_to_an_rpc_method`
  (`crates/penelope-telegram/src/commands.rs:500`) ; `every_routed_method_exists_in_the_contract`
  (`crates/penelope-cli/src/commands.rs:2430`).

Les 103 méthodes de `method::ALL` (`api.rs:242`), par famille :

| Famille | Méthodes |
|---|---|
| Daemon | `status`, `metrics`, `doctor`, `shutdown`, `restart`, `paths` |
| Conversation | `chat.send`, `chat.stream`, `chat.stop` |
| Sessions | `session.list`, `session.new`, `session.switch`, `session.close`, `session.title`, `session.fork`, `session.rewind`, `session.compact`, `session.export`, `session.purge`, `session.model`, `session.mode`, `session.project`, `session.budget` |
| Configuration | `config.get`, `config.set`, `config.status`, `config.reload` |
| Secrets | `secret.set`, `secret.list`, `secret.rm`, `secret.backend` |
| Modèles | `model.list`, `model.set`, `model.route_test`, `model.auth` |
| MCP | `mcp.list`, `mcp.show`, `mcp.add`, `mcp.edit`, `mcp.rm`, `mcp.enable`, `mcp.disable`, `mcp.restart`, `mcp.test`, `mcp.auth`, `mcp.logs` |
| Skills | `skill.list`, `skill.show`, `skill.rollback`, `skill.reload`, `skill.install` |
| Workflows | `wf.list`, `wf.show`, `wf.validate`, `wf.run`, `wf.runs`, `wf.trace`, `wf.control` |
| Planification | `schedule.list`, `schedule.add`, `schedule.rm`, `schedule.pause`, `schedule.resume`, `schedule.run_now`, `schedule.move` |
| HITL | `approvals`, `approve`, `deny`, `policies`, `policy.revoke`, `quiet` |
| Mémoire | `mem.search`, `mem.show`, `mem.history`, `mem.restore`, `mem.reindex`, `mem.forget`, `mem.candidates`, `mem.split`, `mem.dream`, `mem.learned`, `mem.signals`, `mem.audit`, `mem.retry_rejected`, `mem.diff` |
| Intentions | `intent.list`, `intent.cancel` |
| Vault | `vault.sync`, `vault.check`, `vault.lint` |
| Accueil | `onboard.next`, `onboard.answer`, `onboard.write` |
| Données | `import.hermes`, `export`, `backup`, `restore`, `audit.verify`, `store.rebuild`, `usage`, `tail`, `eval.run`, `upgrade` |

Non commité (#205) : `audit.show`.

Deux méthodes répondent par la marche à suivre au lieu d'agir (0.3.1) : `restore` (se fait
daemon arrêté par `penelope restore`) et `eval.run` (se lance depuis les sources par
`penelope eval <suite>`).

Types publiés par `api.rs` :

- `StreamEvent` (étiquette `type`, snake_case) : `delta`, `reasoning`, `tool_call`,
  `tool_result` (`ok`, `preview`), `approval` (`id`, `kind`, `subject`, `risk`), `usage`
  (`prompt`, `completion`, `cost_usd`), `done`, `error`, `log` (`level`, `target`,
  `message`), `run_update` (`run_id`, `step`, `state`). Test : `stream_events_roundtrip`
  (`api.rs:508`).
- `StatusReport` : `version`, `uptime_s`, `config_generation`, `sessions_active`,
  `runs_active`, `approvals_pending`, `mcp_ready`, `mcp_total`, `turns_queued`, `rss_mb`,
  `spent_today_usd`, `telegram`, `runners_alive`, `runners_expected` (#84), `outbox_failed`
  (#101). Test : `status_reports_a_coherent_snapshot` (`runtime.rs:703`).
- `DoctorCheck` : `id`, `label`, `ok`, `detail`, `fix` (commande proposée, jamais exécutée),
  `severity` (`info`, `warn`, `error` via `.critical()`).
- Codes de sortie CLI (`exit_code`) : `0` OK, `2` USAGE, `3` DAEMON_UNREACHABLE, `4`
  VALIDATION_FAILED, `5` DENIED, `6` NOT_FOUND, `7` DAEMON_UNRESPONSIVE (#99), `130`
  INTERRUPTED (Ctrl-C, #100), `70` INTERNAL. Test : `exit_codes_follow_the_documented_table`
  (`client.rs:267`).
- Axes de `usage` (`USAGE_AXES`, `budget.rs:129`) : `session`, `turn`, `model`, `day`,
  `role`, `provider`, `upstream`, `run`, `miss`.
- Contrôle de run (`wf.control`, `runs.rs::Control`) : `pause`, `resume`, `cancel`,
  `retry-step`, `skip-step`, `goto:<étape>`, `answer` (`choice`, `input`), `budget` (`usd`,
  `tokens`). `skip-step` et `goto` exigent une approbation (`Control::requires_approval`).
- Délais côté CLI : 15 s par défaut, sans limite pour les méthodes longues (`chat`,
  `session compact`, `mem dream`, `backup`, `upgrade`, `restore-all`, `import`, `eval`),
  `--timeout N` (0 : sans limite). Test : `each_method_has_its_limit` (`client.rs:298`).

### 1.2 CLI (`penelope`)

Code : `crates/penelope-cli/src/commands.rs` (clap), `client.rs`, `output.rs`, `main.rs`.
Options globales : `--home <dir>` (équivaut à `PENELOPE_HOME`), `--json`, `--timeout <s>`.
Tests : `the_cli_definition_is_coherent`, `global_flags_work_anywhere`,
`commands_route_to_rpc_methods`, `scalars_are_typed_from_the_command_line`
(`commands.rs:2338` à `2481`) ; rendu `output.rs` (6 tests : tables, listes verticales,
booléens en français, tableaux vides explicites).

Arbre complet des commandes (toutes les sous-commandes et options) :

- `install`, `uninstall`, `daemon`, `start`, `stop`, `restart`, `status`, `metrics`,
  `doctor`, `paths`.
- `logs [--turn <id>] [--session <id>] [--lines N=200]` : journaux JSON du jour et de la
  veille, sans daemon (#103).
- `chat [--session <id>] [message…]` : ponctuel ou interactif (`/new`, `/stop`, Ctrl-D,
  Ctrl-C = `chat.stop` et code 130, second Ctrl-C quitte).
- `onboard [partie]` (`profil`, `outils`, `style`, `limites`).
- `upgrade [--check] [--rollback] [--tag vX] [--force] [--switch]`.
- `session list | new [titre] | export <s> | close <s> | purge <s> [--yes] [--reason] |
  title <s> <titre…> | fork [--session] [--title] | rewind [n=1] [--session] | budget <s>
  [usd|off] | model [alias|auto] [--session] | compact [--session] | mode
  [ask|reads|auto|default] [--session] | project [projet|aucun] [--session]`.
- `config get | set <clé> <valeur> | status | reload | validate [fichier]` (validate sans
  daemon).
- `secret list | backend | set <nom> (valeur à l'invite ou sur stdin, jamais en argument,
  sans daemon) | rm <nom>`.
- `model list [--filter] | set <alias> <modèle> | auth [codex] [--logout] [--status]`.
- `mcp list | show <n> | add <fichier> [--name] | edit <n> <champ> <valeur> | rm <n> |
  enable <n> | disable <n> | restart <n> | auth <n> [--callback <url>] | test [n] [--file
  <toml>] | logs <n> [--lines N=50]`.
- `wf list | show <id> | validate <fichier> (sans daemon) | runs | run <id> [--param
  k=v…] | trace <run> | control <run> <op> [--choice] [--input] [--usd] [--tokens]`.
- `schedule list | add <kind> --spec <json> --target <json> [--dedup] | pause <id> |
  resume <id> | rm <id> | run <id> | move <id> (--chat <id> [--topic <id>] | --private)`.
- `mem search <q> | show <uid> | history [--uid] [--file] | restore <id> | reindex
  [--embeddings] | forget <uid> | candidates | split <uid> | audit | retry-rejected | diff
  [--since dream] | dream [--dry-run] | learned [jours=7] | signals <uid>`.
- `vault sync | check | lint`.
- `skill list | show <n> | rollback <n> | reload | install <owner/repo[@rev][:a,b]>
  [--force]`.
- `approvals`, `approve <id> [--always] [--effect done|retry]`, `deny <id> [--reason]`,
  `policies`.
- `usage [--by session|turn|model|day|role|provider|upstream|run|miss] [--session]
  [--since AAAA-MM-JJ] [--limit N=20]`.
- `audit-verify`, `backup [--push] [--full] [--media]`, `restore-all [source] [--dry-run]`,
  `restore <fichier>`, `import hermes [--path] [--dry-run] [--no-test]`, `export
  session|run|all [id]`, `store rebuild`, `eval <suite>`.
- Non commité (#205) : `audit show [--turn <id>] [--session <id>]`.

Commandes qui fonctionnent **sans daemon** (tests `*_works_without_a_daemon`,
`setting_a_secret_never_goes_through_the_rpc`, `doctor_reports_local_checks_without_a_daemon`,
`paths_works_without_a_daemon`) : `secret set`, `config validate`, `wf validate`, `paths`,
`logs`, `doctor` (contrôles locaux), `restore`, `restore-all`, `eval`, `upgrade` (mode hors
ligne quand le daemon est absent).

### 1.3 Telegram : commandes, écrans, gabarits, boutons

Code : `crates/penelope-telegram/src/commands.rs` (catalogue `all()`, `setMyCommands`,
`/help`), `crates/penelope-daemon/src/telegram.rs` (passerelle, 13 700 lignes),
`crates/penelope-daemon/src/telegram/screens.rs` (écrans), `templates.rs`, `actions.rs`,
`forms.rs`, `render.rs`, `html.rs`, `topics.rs`, `api.rs`, `mock.rs`.

Les 50 commandes du catalogue (chacune reliée à une méthode RPC, test
`ca_15_1_every_command_maps_to_an_rpc_method`) :

| Famille | Commandes |
|---|---|
| Session | `/new [titre]`, `/sessions [all]`, `/switch <id\|préfixe\|titre>`, `/title <texte>`, `/close [s]`, `/purge [s]`, `/fork`, `/rewind [n]`, `/compact`, `/export`, `/stop [tout]`, `/projet [nom\|aucun]`, `/mode [ask\|reads\|auto]`, `/home [off]` |
| Modèles | `/model [alias\|auto\|auto on\|off\|auth codex [status\|logout]\|<alias> <id>]`, `/models [filtre]`, `/budget [session <montant\|off>\|sessions\|jours]`, `/usage [turn\|model\|miss\|day]` |
| Mémoire | `/audit`, `/accueil [partie]`, `/note <texte>`, `/retiens <texte>`, `/oublie <q>`, `/recall <q>`, `/appris [jours]`, `/pratique <slug>`, `/dream`, `/intentions`, `/mien` (légende d'un document), `/forget <session>` |
| MCP | `/mcp [nom] [restart\|logs\|test\|auth <nom>]`, `/p [serveur] [prompt]` |
| Skills | `/skills`, `/skill [rollback <nom>]` |
| Workflows | `/wf`, `/run <workflow> [k=v…]`, `/runs`, `/resume <run>` |
| Planification | `/schedules [pause\|resume\|rm\|run <id>\|ici <id>]` |
| HITL | `/approvals`, `/policies`, `/quiet [HH:MM-HH:MM]` |
| Système | `/status`, `/doctor`, `/config`, `/logs [composant]`, `/restart`, `/upgrade [install\|rollback\|vX]`, `/secret [list\|rm]` |
| Aide | `/help`, `/start` (lien profond `t.me/<bot>?start=<écran>`) |

Règles du catalogue : nom en minuscules ASCII de 32 caractères au plus, description de 1 à
256 caractères (`bot_commands_payload_is_valid`), pas de doublon (`no_duplicate_commands`),
`/help` groupé par familles avec un exemple par commande (`help_groups_by_category_with_examples`),
catalogue et `docs/telegram.md` identiques dans les deux sens
(`telegram_commands_and_their_documentation_match`, `crates/penelope-evals/tests/docs.rs:194`).
Aucune commande ne répond « Usage : » : sans argument, chacune ouvre un écran
(`every_catalog_command_without_arguments_opens_a_screen`, `telegram.rs:11281`).

Les 26 gabarits livrés (`templates.rs::CATALOG`, surchargeables par `templates/<id>.toml`,
rechargés à chaud) : `answer`, `tool_approval`, `destructive_confirm`, `plan_proposal`,
`deploy_gate`, `run_card`, `run_done`, `run_blocked`, `incident`, `ticket_detected`,
`question`, `form`, `mcp_oauth_required`, `mcp_url_elicitation`, `sampling_request`,
`effect_unknown`, `skill_proposal`, `memory_proposal`, `learned`, `schedule_preview`,
`workflow_preview`, `budget_alert`, `mcp_status`, `digest`, `heartbeat`, `stopped`. Variables
de `tool_approval` : `intention`, `action`, `details`, `alerte` (un gabarit surchargé
ancien garde `outil`, `serveur`, `risque`, `arguments`, `raison`). Tests :
`the_catalog_is_complete`, `ca_14_1_every_template_renders_in_both_forms`,
`values_are_never_read_as_variables` (#129), `missing_variable_is_an_error_not_a_hole`,
`undeclared_variable_is_rejected_at_validation`, `hot_reload_overrides_builtins`,
`a_broken_template_is_reported_and_the_others_survive`, `every_button_action_is_known`.

Les actions de boutons (`actions.rs::kind`, jeton base62 de 64 octets au plus indexant la
table `tg_actions`) : `approve`, `approve_run`, `approve_always`, `deny`, `deny_reason`,
`confirm_destructive`, `run_pause`, `run_resume`, `run_cancel`, `run_trace`,
`run_retry_step`, `run_skip_step`, `regenerate`, `escalate_model`, `memorise`, `choice`,
`form_next`, `form_prev`, `form_submit`, `form_decline`, `oauth_retry`, `oauth_pasted`,
`elicit_accept`, `elicit_decline`, `elicit_cancel`, `elicit_done`, `elicit_retry`,
`schedule_enable`, `workflow_save`, `workflow_run_once`, `skill_activate`, `memory_accept`,
`memory_as_exception`, `memory_reject`, `model_pin`, `budget_raise`, `budget_stop`,
`stop_resume`, `stop_forget`, `doctor`, `effect_verify`, `effect_retry`, `effect_ignore`,
`session_switch`, `session_menu`, `session_fork`, `session_rename`, `session_close`,
`sessions_page`, `onboard_start`, `onboard_answer`, `onboard_pause`, `onboard_write`,
`onboard_cancel`, `screen`, `screen_do`, `run_command`, `say` (59 types, `action_kinds_are_unique`) ;
les jetons de navigation des écrans sont réutilisables une semaine, ceux des opérations
servent une fois. Propriétés testées :
idempotence du double clic (`ca_14_4_double_click_is_idempotent`), refus d'un autre
utilisateur (`ca_14_5_non_owner_clicks_are_refused`), expiration
(`expired_tokens_are_refused_and_purged`), jetons multi-usage
(`multi_use_tokens_can_be_clicked_repeatedly`), taille (`tokens_fit_in_callback_data`).

Boutons spéciaux : `copy_text` (`/retiens `, `/recall `, `/note `, `/title `), liens profonds
`t.me/<bot>?start=approvals|runs_stuck|audit` depuis le digest
(`the_digest_links_to_screens_once_the_bot_is_known`), boutons `url` (OAuth, liens
d'élicitation après accord).

Écrans (`screens.rs`, issue #30) : un bouton par élément, message redessiné en place,
« Précédent / Suivant » au-delà de dix éléments, écran « Confirmer / Annuler » pour tout geste
risqué (supprimer, oublier, fermer, redémarrer, installer ou annuler une mise à jour, arrêter
un run), formulaires pour les paramètres de workflow et les arguments de prompts MCP, clic
acquitté avant l'opération (#73), jamais de `null` dans une bulle (#115 :
`no_bubble_ever_shows_null`, `an_absent_value_is_shown_as_a_question_mark`).

Réactions et sujets : réactions « reçu » par message (`reactions_use_the_prd_emojis`),
`sendChatAction` toutes les 4 s pendant un tour avec l'action de l'outil en cours
(`typing`, `upload_document`, `record_voice`, `upload_photo`, #121), sujets de forum
(`topics.rs` : `Purpose`, `run_topic_name`, `session_topic_name`, `TopicStore`), foyer
`telegram.home` (#143), administrateur anonyme `1087968824` (`ANONYMOUS_ADMIN_ID`, #113),
fenêtre d'album 1,5 s (`MEDIA_GROUP_WINDOW_MS`), brouillon toutes les 700 ms
(`DRAFT_INTERVAL_MS`), téléchargement plafonné à 20 Mo (`DOWNLOAD_MAX_BYTES`).

### 1.4 Outils natifs exposés au modèle (57)

Code : `crates/penelope-tools/src/spec.rs` (`all()`, liste fixe et triée ; `ON_DEMAND` ;
`core_exposed()` ; `always_exposed(in_workflow)` ; `WHY_FIELD = "pourquoi"`),
`crates/penelope-daemon/src/executor.rs` (exécution, `precheck`, `normalise_call`),
`crates/penelope-daemon/src/tools_on_demand.rs`. Référence documentaire générée :
`docs/install-headless.md`, section « Outils natifs » (test `every_native_tool_is_documented`).

Colonnes : classe de risque (`RiskClass`, `crates/penelope-kernel/src/risk.rs`), idempotent
(un effet `unknown` peut être relancé sans HITL), réseau (contamine la provenance du tour),
portée (noyau : décrit à chaque appel de conversation ; à la demande : nommé dans le message
système, atteint par `tool_search` / `tool_describe` / `tool_call`, promu dans la session dix
tours ; workflow : réservé aux runs). Tout outil dont la classe n'est pas `read` reçoit le
champ `pourquoi` (#116).

| Outil | Classe | Idempotent | Réseau | Portée |
|---|---|---|---|---|
| `artifact_read` | read | oui | non | noyau |
| `ask_user` | read | non | non | noyau |
| `config_set` | write | non | non | à la demande |
| `fs_edit` | write | non | non | noyau |
| `fs_list` | read | oui | non | noyau |
| `fs_read` | read | oui | non | noyau |
| `fs_search` | read | oui | non | noyau |
| `fs_write` | write | non | non | noyau |
| `git_branch` | write | non | non | à la demande |
| `git_clone` | external | non | oui | à la demande |
| `git_commit` | write | non | non | à la demande |
| `git_diff` | read | oui | non | à la demande |
| `git_push` | external | non | oui | à la demande |
| `git_status` | read | oui | non | à la demande |
| `history_describe` | read | oui | non | à la demande |
| `history_expand` | read | oui | non | à la demande |
| `history_expand_query` | read | non | non | à la demande |
| `history_grep` | read | oui | non | noyau |
| `http_fetch` | external | non | oui | noyau |
| `image_generate` | external | non | oui | à la demande |
| `image_inspect` | read | oui | non | à la demande |
| `intent_cancel` | write | oui | non | à la demande |
| `intent_create` | write | non | non | à la demande |
| `intent_list` | read | oui | non | à la demande |
| `mem_forget` | destructive | non | non | à la demande |
| `mem_get` | read | oui | non | à la demande |
| `mem_neighbors` | read | oui | non | à la demande |
| `mem_note` | write | non | non | noyau |
| `mem_remember` | write | non | non | à la demande |
| `mem_search` | read | oui | non | noyau |
| `return_value` | read | oui | non | workflow |
| `schedule_create` | write | non | non | à la demande |
| `schedule_delete` | write | oui | non | à la demande |
| `schedule_list` | read | oui | non | à la demande |
| `schedule_move` | write | oui | non | à la demande |
| `self_docs` | read | oui | non | à la demande |
| `self_status` | read | oui | non | noyau |
| `send_file` | write | non | non | à la demande |
| `send_message` | write | non | non | noyau |
| `send_voice` | read | oui | non | à la demande |
| `session_metadata` | read | oui | non | à la demande |
| `session_notes` | read | oui | non | à la demande |
| `shell_exec` | write | non | non | noyau |
| `skill_load` | read | oui | non | à la demande |
| `skill_patch` | write | non | non | à la demande |
| `skill_propose` | write | non | non | à la demande |
| `skill_search` | read | oui | non | à la demande |
| `step_done` | read | oui | non | workflow |
| `sub_agent_spawn` | write | non | non | noyau |
| `time_now` | read | oui | non | noyau |
| `workflow_author` | write | non | non | à la demande |
| `workflow_control` | write | non | non | à la demande |
| `workflow_describe` | read | oui | non | à la demande |
| `workflow_list` | read | oui | non | à la demande |
| `workflow_plan` | write | non | non | à la demande |
| `workflow_start` | write | non | non | à la demande |
| `workflow_status` | read | oui | non | à la demande |

Comptes : 57 outils, 39 à la demande, 2 réservés aux workflows, 16 dans le noyau (la
documentation dit 17, chiffre de #104 ; le test `the_core_is_small_and_on_demand_tools_exist`
borne la liste sous 20 définitions avec les trois méta-outils). Classes : 31 `read`, 20
`write`, 5 `external`, 1 `destructive`. Deux outils de classe `read` demandent une
approbation par nature : `ask_user` (question au propriétaire) et `send_voice` (envoi), et
`shell_exec` prend la classe `read` idempotente quand la commande est une lecture (#111).

Règles transverses des outils :

- `shell_exec` : `command` (une commande par appel), `cwd`, `network: true` (devient
  `external`), `output: "full"`, délai `tools.shell_timeout` ; `is_read_command` classe
  les lectures ; `NETWORK_OFF_NOTE` sur un échec réseau sans réseau (#106).
- Lectures parallèles (`PARALLEL_SAFE`, `agent.rs:374`, par quatre) : `fs_read`,
  `fs_list`, `fs_search`, `git_status`, `git_diff`, `time_now`, `schedule_list`,
  `mem_search`, `mem_get`, `mem_neighbors`, `intent_list`, `history_grep`,
  `history_describe`, `history_expand`, `history_expand_query`, `artifact_read`,
  `skill_search`, `workflow_list`, `workflow_describe`, `workflow_status`, `self_status`,
  `self_docs`, `tool_search`, `tool_describe`. Tout le reste est une barrière.
- Plafonds : 24 appels au modèle par tour (`TURN_CALLS`), `MAX_FAMILIES_PER_CLICK = 3`
  règles par « Toujours », `EXPECTED_ARGS_MAX_CHARS = 1500`, `LOOP_STOP_NOTE` laissé dans
  la conversation.
- `git_clone` accepte `https://`, `ssh://`, `git://`, `file://`, `git@hôte:owner/repo.git`,
  `owner/repo` (GitHub) ; refuse un chemin local ; réutilise un clone d'origine équivalente
  (#160).
- Tests transverses (`spec.rs`) : `every_prd_tool_is_present`, `the_list_is_sorted_and_stable`,
  `no_duplicate_names`, `risk_classes_follow_the_prd_table`,
  `network_tools_are_marked_for_contamination`, `read_tools_are_idempotent`,
  `workflow_only_tools_are_hidden_outside_runs`, `every_schema_is_a_valid_object_schema`,
  `required_fields_are_enforced`, `the_core_is_small_and_on_demand_tools_exist`,
  `rare_tools_are_found_by_what_they_do`.

### 1.5 Méta-outils MCP et déclaration `mcp.d`

Code : `crates/penelope-mcp/src/registry.rs` (`meta_tools()`, `RegisteredTool`,
`ToolRegistry`, `qualified_name`, `fts_query`), `config.rs` (`ServerConfig`),
`protocol.rs`, `client.rs`, `transport.rs`, `supervisor.rs`, `tasks.rs`, `oauth.rs`,
`crates/penelope-daemon/src/mcp.rs` (superviseur), `mcp_auth.rs`, `elicitation.rs`.

- Les trois méta-outils, dans cet ordre (test `meta_tools_are_the_three_of_the_prd`) :
  `tool_search` (mots-clés, FTS puis repli lexical, noms, descriptions courtes, risque ;
  outils natifs à la demande d'abord sous le serveur `natif`, `search_can_be_scoped_to_a_server`),
  `tool_describe` (schémas complets, 20 outils au plus, schéma résumé au-delà de
  `mcp.schema_max_bytes`, `oversized_schema_is_summarised`), `tool_call` (`name` requis,
  `args` objet libre ou `args_json`, validation contre le schéma avant l'envoi,
  `arguments_are_validated_before_the_call`, #110). Nom qualifié `mcp__<serveur>__<outil>`
  (`qualified_names_are_normalised`, `long_names_are_truncated_with_a_hash`).
- Ensemble collant : `mcp.sticky_set_max` (30) outils promus à une frontière de compaction
  seulement (`promotion_waits_for_the_compaction_boundary`, `sticky_set_is_bounded`) ;
  `eager_schemas` en T1 sous `mcp.eager_total_max_bytes` (`eager_schemas_respect_the_global_cap`).
- Empreinte par outil (description, schéma, annotations) et date de première vue (table
  `mcp_tools`, migration `0014`) ; changement silencieux après `tools/list_changed` :
  règles « Toujours » révoquées, événement `mcp.tool_changed`, propriétaire prévenu (#92).
- Versions de protocole (`protocol.rs::VERSIONS`) : `2024-11-05`, `2025-03-26`,
  `2025-06-18`, `2025-11-25`, `2026-07-28` (préférée). Codes : `-32020` HEADER_MISMATCH,
  `-32021` MISSING_REQUIRED_CLIENT_CAPABILITY, `-32022` UNSUPPORTED_PROTOCOL_VERSION,
  `-32042` URL_ELICITATION_REQUIRED, `-32002` ressource introuvable (legacy).
- Déclaration `mcp.d/<nom>.toml` (`ServerConfig`, `deny_unknown_fields`) : `name`,
  `transport` (`stdio` | `http` | `sse` | `auto`), `enabled`, `command`, `args`, `cwd`,
  `env` (placeholders `${SECRET:…}`), `url`, `headers`, `client_id`, `client_secret`
  (référence `${SECRET:…}` seulement, #174), `scopes`, `lazy_start` (true),
  `idle_timeout` (`10m`), `timeout` (`30s`), `max_concurrency` (4), `eager_schemas`
  (false), `protocol` (`2026-07-28`), `sandbox_profile` (`mcp-stdio`), `roots`,
  `sampling_budget_tokens` (20 000), `elicitation_timeout` (`10m`), `tool_policy`
  (`auto` | `ask` | `ask_twice` | `deny`), `tool_risk`, `timeout_per_tool`. Tests :
  `stdio_config_validates`, `http_config_requires_a_url`,
  `confidential_client_requires_a_secret_reference`, `auto_transport_picks_by_field`,
  `invalid_names_and_protocols_are_rejected`, `per_tool_timeout_overrides`,
  `parse_single_server_file`, `parse_multi_server_file`, `a_broken_file_does_not_stop_the_others`,
  `write_then_reload_roundtrip`, `secrets_are_resolved_in_headers_and_env`,
  `missing_secret_is_an_explicit_error`, `unknown_policy_override_is_rejected`.
- Un champ liste (`args`, `scopes`, `roots`) accepte une valeur seule (#138 :
  `a_list_field_takes_a_single_value`).
- États d'un serveur (`supervisor.rs::ServerState`, affichés par `/mcp` et `mcp list`) :
  `configured` (démarre au premier appel), `connecting`, `ready`, `degraded`, `failed`,
  `disabled` ; `auth_required` pour OAuth. Backoff 1 s à 5 min, 8 échecs, éviction LRU
  au-delà de `mcp.max_processes`.

### 1.6 Clés de configuration (222)

Code : `crates/penelope-kernel/src/config.rs` (structures `Owner`, `Telegram`,
`TelegramHome`, `Providers`, `Codex`, `OpenRouter`, `OpenRouterRouting`, `LocalProvider`,
`Models`, `Routing`, `Budget`, `Context`, `Memory`, `Promotion`, `Intents`, `Mcp`,
`McpPolicy`, `Runners`, `Sandbox`, `Observability`, `RuntimeConsumer`, `Tools`,
`Workflows`, `Upgrade`, `Voice`, `Backup`, `Retention`) ; référence générée dans
`docs/install-headless.md` entre `reference:config:debut` et `reference:config:fin`
(test `every_configuration_key_is_documented`, `docs.rs:356`). Une clé « sans effet dans
cette version » est acceptée mais pas lue : la V1 peut la garder inerte, pas la refuser.

- `[owner]` : `telegram_user_id = 0` (0 : canal fermé, configuration invalide tant qu'il
  vaut 0), `timezone = "Indian/Reunion"`, `language = "fr"`.
- `[telegram]` (20) : `token = "${SECRET:telegram_bot_token}"`, `mode = "polling"`
  (`webhook` non servi), `topics = true` (sans effet), `rich_messages = false`,
  `quiet_hours = "22:00-07:00"`, `api_base = "https://api.telegram.org"`,
  `poll_timeout_s = 50`, `rate_per_chat_per_s = 1.0`, `text_limit = 4096` (sans effet),
  `caption_limit = 1024` (sans effet), `max_fragments = 3` (la table dit « sans effet »
  mais 0.17.27 l'a branché pour le digest : `should_send_as_document`),
  `draft_interval_ms = 700` (300 au moins), `webhook_url = ""` (sans effet),
  `allow_groups = false` (sans effet depuis 0.17.4), `allowed_chats = []`,
  `text_group_window_ms = 2000`, `burst_messages = 5`, `burst_chars = 20000`,
  `home.chat = 0`, `home.topic = 0`.
- `[providers.openrouter]` (18) : `api_key = "${SECRET:openrouter_api_key}"`,
  `base_url = "https://openrouter.ai/api/v1"`, `request_retries = 3`,
  `stream_idle_timeout = "120s"`, `catalog_refresh = "6h"`,
  `referer = "https://github.com/edouard-claude/penelope"`, `title = "Penelope"`,
  `categories = "personal-agent"`, `routing.allow_fallbacks = true`, `routing.order = []`,
  `routing.data_collection = "allow"`, `routing.require_parameters = false`,
  `routing.zdr = false`, `routing.sort = ""`, `routing.only = []`, `routing.ignore = []`,
  `routing.quantizations = []`, `enabled = true`.
- `[providers.local]` (7) : `kind = "openai_compat"`, `base_url = "http://127.0.0.1:8080/v1"`,
  `api_key = ""`, `enabled = false`, `models = []`, `stream_idle_timeout = "120s"`,
  `context_window = 32768`.
- `[providers.codex]` (13) : `enabled = false`, `base_url = "https://chatgpt.com/backend-api/codex"`,
  `issuer = "https://auth.openai.com"`, `client_id = "app_EMoamEEZ73f0CkXaXp7hrann"`,
  `originator = "codex_cli_rs"`, `client_version = "0.149.0"`, `stream_idle_timeout = "120s"`,
  `request_retries = 3`, `reasoning_summary = "auto"`, `verbosity = "medium"`,
  `quota_alert_ratio = 0.8`, `quota_stop_ratio = 0.95`, `models = ["gpt-6-astra",
  "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5", "gpt-5.4"]`.
- `[providers.extra.<nom>]` (7) : `kind`, `base_url`, `api_key`, `enabled`, `models`,
  `stream_idle_timeout`, `context_window`.
- `[models.aliases]` (9) : `embedding = "openrouter:openai/text-embedding-3-small"`,
  `fast = "openrouter:deepseek/deepseek-v4-flash"`, `image = "openrouter:google/gemini-3.1-flash-image"`,
  `main = "openrouter:deepseek/deepseek-v4-pro"`, `reasoning = "openrouter:z-ai/glm-5.2"`,
  `stt = "openai_compat:whisper-default"`, `summarizer = "openrouter:deepseek/deepseek-v4-flash"`,
  `tts = "openai_compat:mlx-community/Voxtral-4B-TTS-2603-mlx-4bit"`,
  `vision = "openrouter:google/gemini-3.1-flash-image"`. Préfixes de fournisseur admis
  (`PROVIDER_PREFIXES`, `config.rs:261`) : `openrouter`, `openai_compat`, `local`, `codex`.
- `[models.roles]` (11) : `chat_default = "main"`, `classifier = "fast"`,
  `code = "reasoning"`, `compaction = "summarizer"`, `embedding = "embedding"`,
  `image_describe = "vision"`, `image_generate = "image"`, `image_locate = "vision"`,
  `memory_review = "fast"`, `stt = "stt"`, `tts = "tts"`. Table à clés libres (`MAP_PATHS`) :
  une entrée nouvelle est acceptée par `config set` (#128).
- `[models.routing]` (7) : `classifier = true`, `low = "fast"`, `medium = "main"`,
  `high = "reasoning"`, `sticky = true`, `fallback.main = ["fast"]`,
  `fallback.reasoning = ["main"]` ; `models.locate_frame = "auto"` (`auto` | `pixels` |
  `per_mille`, `LOCATE_FRAMES`).
- `[budget]` (8) : `daily_usd = 20.0`, `session_usd = 5.0`, `run_usd = 5.0`,
  `alert_ratio = 0.8`, `turn_checkpoint_usd = 1.0`, `show_turn_cost_usd = 0.5`,
  `delegate_after_calls = 10`, `compaction_reserve_usd = 0.5`.
- `[context]` (12) : `compaction_threshold = 0.7`, `tail_ratio = 0.025`,
  `tail_min_tokens = 10000`, `tail_max_tokens = 25000`, `min_tail_user_messages = 2`,
  `max_tool_result_share = 0.25`, `large_payload_tokens = 25000`,
  `max_prompt_tokens = 120000`, `model_thresholds.<nom>`, `background_compaction_margin = 0.1`,
  `cooldown_ms = [60000, 300000, 900000]`, `auto_title = true`.
- `[memory]` (37) : `vault_path = "{data}/vault"`, `vault_git_autocommit = "15m"`,
  `vault_git_remote = ""`, `profile_budget_tokens = 600`, `core_budget_tokens = 1200`,
  `project_budget_tokens = 800`, `recall_budget_tokens = 1000`, `recall_timeout_ms = 150`,
  `trigger_threshold = 0.72`, `max_injected_per_turn = 3`, `half_life_days = 180.0`,
  `dedup_cosine = 0.92` (sans effet), `dedup_jaccard = 0.9`, `episode_idle = "2h"` (sans
  effet : la valeur est codée), `episode_topic_shift = 0.35` (sans effet : codée),
  `review_max_candidates = 5`, `dream_batch = 40`, `dreaming_cron = "30 3 * * *"`,
  `dream_retry_wait = "2m"`, `consolidation_reasoning = "auto"`,
  `consolidation_reasoning_tokens = 16000`, `digest_cron = "0 8 * * *"`,
  `promotion.ecart_min_occurrences = 3`, `promotion.ecart_min_sessions = 3`,
  `promotion.ecart_min_days = 2`, `promotion.fact_min_recalls = 2` (ignoré depuis 0.14.0),
  `promotion.fact_min_importance = 8` (ignoré), `promotion.preference_min_sessions = 2`
  (ignoré), `promotion.max_retire_ratio = 0.2`, `promotion.contested_confidence = 0.5`
  (sans effet), `promotion.contested_min_observations = 4` (sans effet),
  `intents.cooldown = "24h"`, `intents.fire_budget = 3`, `intents.expiry = "90d"`,
  `intents.max_per_turn = 3`, `prune_episodic_days = 180` (sans effet),
  `expire_ecart_days = 90`.
- `[mcp]` (21) : `registry_mode = "lazy"` (sans effet), `max_processes = 24`,
  `default_timeout = "30s"` (sans effet), `oauth_redirect_mode = "paste_back"`,
  `public_callback_url = ""`, `cimd_url = ""`, `preferred_protocol = "2026-07-28"` (sans
  effet), `idle_timeout = "10m"` (sans effet), `max_concurrency_per_server = 4` (sans
  effet), `sticky_set_max = 30`, `schema_max_bytes = 8192`, `eager_total_max_bytes = 65536`,
  `callback_port = 7777`, `callback_host = "127.0.0.1"` (`localhost` accepté, #159),
  `policy.read = "auto"`, `policy.write = "ask"`, `policy.destructive = "ask_twice"`,
  `policy.external = "ask"`, `policy.unknown = "ask"`, `restart_backoff_max = "5m"` (sans
  effet), `max_failures = 8`.
- `[runners]` (3) : `count = 4`, `lease_ttl = "60s"`, `heartbeat = "15s"` (au plus la
  moitié de `lease_ttl`, #43).
- `[sandbox]` (6) : `default_profile = "workspace-write"` (`read-only` | `workspace-write`
  | `full`), `allow_full_for = []`, `allow_keychain_for = []`, `workspaces = []`
  (canonicalisés, #164), `shell_network = false` (une configuration qui porte `true` le
  garde), `deny_read = ["~/.ssh", "~/.aws", "~/.gnupg", "~/.config/gh", "~/.netrc",
  "~/.kube", "~/.docker/config.json", "{data}/penelope.db", "{data}/secrets.enc",
  "{data}/mcp.d", "{config}", "{state}"]`.
- `[observability]` (6) : `otlp_endpoint = ""` (sans effet), `prometheus = "127.0.0.1:9464"`
  (sans effet, les métriques se lisent par `penelope metrics`), `log_retention_days = 14`,
  `log_level = "info"` (`PENELOPE_LOG` l'emporte), `runtime_stream_bind = "127.0.0.1:9465"`,
  `runtime_consumers = []` (chacun : `name`, `token_secret`, `kinds`).
- `[tools]` (10) : `shell = ""`, `shell_timeout = "120s"`, `http_allowlist = []`,
  `http_block_private_ips = true`, `loop_detector_repeats = 3`, `max_output_bytes = 262144`,
  `approval_mode = "reads"` (`ask` | `reads` | `auto`, `APPROVAL_MODES`), `shell_allow = []`,
  `shell_allow_network = []`, `inventory_extra = []`.
- `[workflows]` (3) : `workspace_retention_days = 7`, `max_depth = 3`,
  `default_max_iterations = 40` (sans effet).
- `[upgrade]` (8) : `channel = "stable"` (sans effet), `base_url = ""`,
  `minisign_pubkey = ""`, `health_timeout = "60s"` (sans effet : 60 s codées),
  `heartbeat_daily = true` (sans effet), `codesign_identity = ""`,
  `codesign_identifier = "io.github.edouard-claude.penelope"`, `install_dir = "~/.local/bin"`.
- `[voice]` (3) : `tts_voice = "fr_female"`, `max_chars = 1500`, `reply_in_kind = false`.
- `[retention]` (2) : `days = 90`, `memory_history_days = 30` (0 : rien n'est effacé).
- `[backup]` (7) : `git_remote = ""` (vide : celui du vault), `cron = "0 4 * * *"`,
  `keep_daily = 7`, `keep_weekly = 4`, `keep_monthly = 12`, `include_media = false`,
  `max_push_bytes = 104857600`.

Garanties de la configuration (tests dans `config.rs`) : chaque mutation est une génération
immuable publiée par `ArcSwap` (`ca_4_3_generations_are_monotonic`,
`snapshot_is_frozen_for_the_reader`, `stale_apply_result_cannot_overwrite_newer`) ; mutations
sérialisées et sans perte (`concurrent_mutations_never_lose_a_write`,
`a_reload_racing_a_mutation_leaves_a_consistent_state`, `atomic_write_replaces_file`, #45) ;
édition en place de la seule clé changée (`a_mutation_edits_only_the_changed_key`,
`a_removed_map_entry_leaves_the_file`, `the_first_file_is_short_and_reads_back`, #76) ;
clés inconnues tolérées et nommées (`unknown_sections_and_keys_are_tolerated_and_named`)
mais refusées par `config set` (`unknown_key_is_rejected`) ; fichiers des versions publiées
relus (`files_written_by_released_versions_still_load`, fixtures dans
`crates/penelope-kernel/tests/fixtures`) ; clés à redémarrage (`RESTART_ONLY_PATHS` :
`store.path`, `rpc.socket`, `telegram.token`, `observability.runtime_stream_bind`,
`observability.runtime_consumers` ; `restart_only_paths_are_limited`) ; moments d'effet de
`config_set` : appel suivant (`sandbox.*`, `tools.*`, `models.aliases.*`, `models.roles.*`
hors `chat_default`, `budget.*`), tour suivant (`owner.language`, `models.roles.chat_default`,
`models.routing.*`), redémarrage (`RESTART_ONLY_PATHS`) ; secrets et identité du
propriétaire refusés par `config_set` (`config_set_guards`, `selfknow.rs:718`).
Contradictions (`coherence.rs`, `contradictions()`, `new_refusals()`) : refus nommé pour un
rôle ou palier vers un alias absent, un repli vers soi-même, `budget.alert_ratio` hors
]0, 1[, une adresse privée dans `tools.http_allowlist` bloquée par
`http_block_private_ips`, `runners.heartbeat` au-delà de la moitié de `lease_ttl`
(`a_heartbeat_slower_than_half_the_lease_is_rejected`), un préfixe de fournisseur inconnu
(`an_unknown_provider_prefix_is_refused_by_name`), un hôte de rappel OAuth hors bouclage
(`the_oauth_callback_host_must_be_a_loopback_name`), un ratio codex hors [0, 1]
(`codex_quota_ratios_are_checked`) ; avertissement pour un plafond de session au-delà du
jour, un alias vers un fournisseur désactivé, une action destructive moins protégée qu'une
écriture, un déclencheur pendant `telegram.quiet_hours`
(`self_cancelling_settings_are_named`, `the_sample_configuration_is_coherent`).

### 1.7 Événements du journal (`events`, chaîne hachée)

Code : `crates/penelope-kernel/src/event.rs` (`EventDraft::new(kind, payload)`,
`.session()`, `.run()`, `EventLog::append`, `verify`, `subscribe`, `GENESIS`,
`compute_hash_from_text`), `canonical.rs` (JSON canonique, SHA-256). Chaque événement porte
`kind`, `payload` rédigé, `session_id`, `run_id`, `seq` par session (unique par la base,
migration `0011`), `prev_hash`, `hash`. Une purge remplace le payload et garde le hachage
(`audit.purge`).

Types d'événements relevés dans les appels à `EventDraft::new` (75 types), par famille :

- Tours et conversation : `turn.started`, `turn.finished`, `turn.merged` (#161),
  `turn.empty_answer`, `turn.loop_aborted`, `tool.result`, `message.received`,
  `session.closed`, `session.forked`, `session.rewound`, `approval.decided`.
- Contexte : `context.compaction_requested`, `context.compaction_skipped`,
  `context.compaction_failed`, `context.compacted` (avec `evidence`, #179),
  `context.compaction_mechanical` (#131).
- Modèles : `llm.fallback_used`, `llm.retried` (#50), `llm.codex_scope_fallback` (#142),
  `llm.provider_connected`, `llm.provider_disconnected`, `llm.provider_tokens_revoked`.
- Mémoire : `memory.episode_closed`, `memory.episode_ingested`, `memory.dreamed`,
  `memory.dream_batch` (`reasoning`, `reasoning_starved`, `judged`), `memory.dream_retry`,
  `memory.dream_failed`, `memory.clash_unanswered`, `memory.proposal_applied`,
  `memory.split_proposed`, `memory.split_applied`, `memory.index_gap` (#15),
  `document.ingested`, `intent.fired`.
- MCP : `mcp.auth_requested`, `mcp.authorized`, `mcp.task.completed`, `mcp.tool_changed`
  (#92).
- Planification : `schedule.fired`, `schedule.notified`, `schedule.failed`,
  `schedule.final_not_repeated` (#133), `run.done`.
- Workflows : `workflow.started`, `workflow.step`, `workflow.question`,
  `workflow.control`, `workflow.finished`, `workflow.subgroup_exited`,
  `workflow.budget_raised` (#136), `workflow.workspace_large` (#177).
- Exploitation : `daemon.recovered`, `daemon.task_panicked` (#84), `store.backup`,
  `store.retention`, `store.rebuilt`, `store.fts_rebuilt` (#158), `backup.done`,
  `audit.purge`, `config.contradiction`, `import.hermes`, `skill.installed`,
  `upgrade.installed`, `upgrade.confirmed`, `upgrade.rolled_back`,
  `upgrade.rollback_failed`, `voice.sent`, `telegram.card_degraded` (#129).
- Flux runtime (#162), écrits dans le journal avant émission : `runtime.tool`,
  `runtime.llm`, `runtime.session.created`, `runtime.session.state`,
  `runtime.approval.requested`, `runtime.approval.decided`.

Journal applicatif (`tracing`) à part : lignes JSON par jour, `mcp_tool_suspicious`, forme
des lignes shell (`simple`, `liste`, `composee`) sans la commande (#150).

### 1.8 Flux runtime (WebSocket local de lecture seule)

Code : `crates/penelope-daemon/src/runtime_events.rs` (`StreamConsumer`, `public_frame`,
`bounded_redacted`), configuration `observability.runtime_stream_bind` et
`runtime_consumers` (`name`, `token_secret`, `kinds`). Contrat (`docs/runtime-events.md`) :
désactivé sans consommateur ; bind refusé hors `127.0.0.1`
(`runtime_stream_requires_loopback_and_per_consumer_token`) ; jeton d'au moins 32 caractères
dans le magasin de secrets, `Authorization: Bearer …`, `ws://127.0.0.1:9465/events?after_id=0`
(`websocket_rejects_unauthenticated_clients`, `stream_does_not_bind_without_a_valid_stored_secret`) ;
message JSON `event_id`, `sequence`, `session_seq`, `timestamp`, `kind`, `session_id`,
`run_id`, `payload` (rédigé, borné à 64 Kio, sinon `{ "truncated": true, "bytes" }`),
`actuation: null` ; replay depuis `after_id` puis direct, ordre des identifiants SQLite,
reprise après perte du tampon (`websocket_replays_then_streams_filtered_committed_events`,
`public_frame_keeps_order_and_redacts_payload`, `public_frame_bounds_legacy_payloads_too`) ;
empreintes d'audit jamais exportées ; les réponses du client sont ignorées. `runtime.tool`
porte `tool`, `args`, `result`, `ok`, `duration_ms`, `cost_usd_estimated` ; `runtime.llm`
modèle, fournisseur, rôle, tokens, coût. Démonstration : `scripts/pathlayer_forwarder.py`,
`scripts/pathlayer_listen.py`, `crates/penelope-daemon/examples/runtime_pathlayer_demo.rs`.

### 1.9 Schéma de workflow (`<id>.workflow.json`)

Fichier : `schemas/workflow.schema.json` (JSON Schema 2020-12, sous-ensemble validable par
`penelope-kernel::schema`). Tests : `the_schema_itself_is_well_formed`,
`every_bundled_workflow_satisfies_the_published_schema`,
`the_schema_rejects_what_the_validator_rejects`, `the_schema_accepts_the_shapes_the_prd_describes`
(`crates/penelope-evals/tests/workflow_schema.rs`). Validateur de référence :
`crates/penelope-workflow/src/validate.rs` (`penelope wf validate`, sans daemon).

- Racine : `metadata` (requis), `entryStep` (requis), `settings`, `startCondition`,
  `steps` (requis, au moins un), `additionalProperties: false`.
- `metadata` : `id` (slug `[a-z0-9-]`, 64 max, égal au nom du fichier), `name`,
  `description` (non vides), `version` (`1.0.0`), `color` (`#3b82f6`), `parameters`
  (`id`, `label`, `type` : `string` | `number` | `integer` | `boolean` | `enum`,
  `required`, `default`, `description`), `platforms` (`macos`, `linux`, `windows`),
  `config` (libre).
- `settings` : `maxIterations` (40), `budget` (`maxUsd` 5, `maxTokens` 2 000 000 en
  tokens facturés, `maxWallMs` 7 200 000 ; au moins un non nul), `concurrency`
  (`maxConcurrent` 1, `admission` : `parallel` | `hold` | `coalesce` | `drop`),
  `workspace` (`ephemeral` | `persistent:<nom>`), `tracker`, `deploy_workflow`, `forms`
  (un JSON Schema d'objet par identifiant).
- `step` : `id`, `type` (`agent`, `sub_agent`, `shell`, `tool`, `user`, `parallel`,
  `workflow`, `wait`, `verify`), `name`, `phase` (`plan`, `build`, `verification`,
  `waiting`, `deploy`, `done`), `subGroup`, `transitions` (`goto`, `condition`, `tag`),
  `budget`, `retry` (`max`, `backoffMs`), `timeoutMs`, `model` (alias, jamais un
  identifiant brut), `agentId`, `subAgentType`, `prompt`, `nudgePrompt`, `tools`,
  `outputSchema`, `command` (chaîne ou table par OS `unix` | `macos` | `linux` |
  `windows`), `cwd`, `successExitCodes` ([0]), `network` (false), `tool`
  (natif ou `mcp__<serveur>__<outil>`), `args`, `template`, `choices`, `input`
  (`none` | `text` | `form:<id>`), `children` (`sub_agent`, `shell`, `tool` seulement),
  `maxConcurrency`, `workflowId`, `params`, `on` (exactement une forme : `event`, `cron`,
  `duration_ms`, `mcp_task` chaîne `serveur:tâche` ou `{server, task}`), `criteriaKey`
  (`criteria`), `verifier`, `checks` (`shell`, `tool`, `project_tests`), `quote` (#154).
- Cibles terminales : `$done`, `$blocked` ; aucun identifiant d'étape ne commence par `$`.
- Conditions (`conditions.rs`) : `always`, `step_result` (`result`), `metadata_all_match`,
  `metadata_any_match` (`key`, `field`, `value`), `metadata_all_in` (`values`),
  `output_match` (`path` + `equals` | `in` | `regex`), `all`, `any` (`of`), `not` (`cond`).
  Vacuité vraie pour `metadata_*` sur liste vide (`empty_lists_satisfy_all_conditions`).
- Variables de gabarit : paramètres déclarés, `{{workdir}}`, `{{run.id}}`, `{{now}}`,
  `{{os}}`, `{{arch}}`, `{{reason}}`, `{{criteriaList}}`, `{{criteriaCount}}`,
  `{{pendingCount}}`, `{{modifiedFiles}}`, `{{stepOutput.…}}`, `{{steps.<étape>.…}}`,
  `{{brief}}`, `{{metadata.…}}`, `{{params.…}}`. Dans une `command`, les valeurs sont citées
  par le moteur (`a_path_with_a_space_stays_one_argument`,
  `quoting_is_never_doubled_and_survives_an_apostrophe`).
- Workflows livrés (`crates/penelope-workflow/src/bundled.rs`) : `build-verify`, `review`,
  `ticket-to-deploy`, `deploy-generic` ; un fichier utilisateur de même identifiant les
  remplace (`user_files_override_bundled`).
- Contrat des critères (`session_metadata`, `MetadataOp` : `set`, `update`, …) : `id`
  unique, `text` ou `label`, `status` parmi `pending`, `completed`, `passed`, `failed`
  (`metadata_criteria_flow`, `criteria_follow_their_contract`) ; `session_metadata.project`
  (`dir`, `test_command`) et `session_metadata.verification` (`dir`, `test_command`,
  `prerequisites`, `evidence` : `{kind, ref, sha}` avec `pr`, `ci`, `tdd_green`, `tdd_red`,
  #167).
- Plan révisable (`crates/penelope-workflow/src/plan.rs`, #186) : `Plan` (`goal`,
  `steps` typés par `Phase`, `version`, `gate` : `PlanGate`, `history`), `revise`,
  `restore`, `approve` ; persisté par `PlanStore` ; un bouton périmé ne peut approuver une
  autre version (`a_stale_button_or_revision_cannot_approve_another_version`).

### 1.10 Formats du vault (wiki Markdown)

Code : `crates/penelope-memory/src/vault.rs` (`Annotations`, `VaultEntry`, `parse_entries`,
`Practice`, `Level`, `DIRECTIVE_PREFIXES`, `block_id`, `migrate_legacy_line`), `wiki.rs`
(`note_type`, `touch`, `Resolver`, `lint`, `rename_note`, `append_log`, `ATTACHMENTS_DIR`,
`LOG_FILE`), `ingest.rs` (`SOURCES_DIR`, `INBOX_DIR`, `INGESTIBLE`, `PASSAGE_CHARS`,
`render_source`, `parse_source`), `edit.rs`, `crates/penelope-kernel/src/frontmatter.rs`.

- Fichiers et niveaux (`Level::from_path`) : `AGENTS.md`, `SOUL.md` (instruction),
  `profil.md` (profil), `memoire.md` (cœur), `projets.md` et tout autre `.md` (cure,
  dont `notes.md`, `pratiques/`, `concepts/`, `notes/<titre>-<id>.md`), `journal/`
  (épisodique), `DREAMS.md` et `.dreams/` (revue). Injectés d'office : instruction, profil,
  cœur, projet. Sept niveaux : `instruction`, `profil`, `coeur`, `projet`, `cure`,
  `episodic`, `revue`.
- Autres dossiers : `sources/<slug>.md` (fiches de documents, sections `## Résumé`,
  `## Contenu`, `## Original`, `## Concepts`, propriété `source`, `![[nom.pdf]]`),
  `attachments/` (originaux), `inbox/` (dépôt, `inbox/refusés/`), `accueil/accueil-AAAA-MM-JJ.md`,
  `audits/audit-AAAA-MM-JJ.md`, `archive/`, `concepts/<slug>.md`, `concepts/_a-definir.md`,
  `index.md`, `log.md` (ajout seul, `## [AAAA-MM-JJ] <op> | <titre>`), `.gitignore`.
- Propriétés YAML : `type` (`journal`, `source`, `concept`, `profil`, `memoire`,
  `accueil`, `audit`, `revue`, `session`, `pratique`, …), `created`, `updated`
  (`AAAA-MM-JJ`), `aliases`, `tags` (listes ; liste en ligne découpée hors guillemets,
  #48), `date` (journal), `source`.
- Entrée : `- texte <!-- annotations --> ^uid` ; identifiant de bloc final visé par
  `[[note#^uid]]` ; annotations en commentaires : `importance`, `depuis`, `expire`
  (`AAAA-MM-JJ`, 14 jours par défaut et 90 au plus pour un état passager), `sensible: oui`,
  `remplace: <uid>` (supersede), `projet`, `declencheurs`, `quand` (prédicats des
  pratiques), `confiance`, `origine`.
- Directives de profil : préfixes `Toujours`, `Jamais`, `Préférer`, `Éviter`.
- Pratique (`type: pratique`) : défaut, exceptions (`quand:` obligatoire), écarts observés
  (jamais injectés), statut dérivé (`PracticeStatus`), confiance = f(succès,
  contradictions).
- Provenance (`provenance.rs`, table `mem_provenance`) : `origin` parmi `owner`, `agent`,
  `untrusted`, `system` ; seuls `owner` et `agent` sont promouvables et injectables ;
  `source` (document, question d'accueil, session), `supersedes`.
- Uniques dans tout le vault : noms de fichiers (sinon wikilink par chemin), identifiants
  de bloc ; lint (`LintReport`) : liens non résolus, blocs absents, orphelines, impasses,
  alias et noms en double, identifiants invalides ou dupliqués, propriétés mal typées,
  entrées expirées, contradictions proposées.
- Bornes : `MAX_ENTRY_CHARS = 300` (`mem_remember`), `TEMPORAL_DAYS = 30`,
  `MAX_TEXT_CHARS = 2 000 000` (ingestion), `PASSAGE_CHARS = 1 200`, formats ingérables
  `pdf`, `docx`, `html`, `htm`, `md`, `markdown`, `txt`, OCR 50 pages.
- Skill livrée `wiki-markdown` (`crates/penelope-daemon/skills/wiki-markdown/SKILL.md`,
  `the_wiki_markdown_skill_is_bundled`) ; skills : `SKILL.md` avec frontmatter `name`,
  `description`, `version`, `allowed_tools`, `activation`, `sub_agent`, `declencheurs`,
  `requires` (`pip:`, `npm:`, `bin:`), portées `bundled` < `user` < `workspace`, table de
  portage `TOOL_MAP` (`Read` → `fs_read`, `Write` → `fs_write`, `Edit`/`MultiEdit`/
  `NotebookEdit` → `fs_edit`, `Bash`/`BashOutput` → `shell_exec`, `Glob`/`Grep` →
  `fs_search`, `WebFetch`/`WebSearch` → `http_fetch`).

### 1.11 Base SQLite : tables et migrations

Code : `crates/penelope-store/src/migrations.rs` (`MIGRATIONS`), `lib.rs` (`Store`,
écrivain unique sur son thread avec 8 Mio de pile, pool de lecture, `write_durable`,
`backup_to`, `integrity_report`), `pool.rs`, `vector.rs`.

- 17 migrations à HEAD, appliquées de manière idempotente depuis chaque version
  (`migrations_apply_from_empty`, `migrations_are_idempotent`,
  `upgrade_from_each_previous_version`, fixtures) : `0001_init`, `0002_indexes`,
  `0003_event_purges`, `0004_usage_attribution`, `0005_single_chat_binding` (#10),
  `0006_prompt_cache` (#17), `0007_retry_origin_rejections` (#24), `0008_memory_flags`
  (#25), `0009_schedule_origin_session` (#39), `0010_retention` (#46),
  `0011_events_seq_unique` (#47), `0012_candidate_deferrals` (#59), `0013_memory_seen`
  (#86), `0014_mcp_tool_fingerprint` (#92), `0015_memory_usage_reset` (#105),
  `0016_turn_merge` (#161), `0017_clone_source` (#160, révoque les règles `git_clone`
  sans motif : `a_legacy_unbounded_clone_rule_is_revoked_on_upgrade`). Non commitée :
  `0018_prompt_snapshots` (#205).
- 55 tables à HEAD (`all_prd_tables_exist`) : `approval_requests`, `artifacts`,
  `config_generations`, `dream_runs`, `effects`, `embeddings_cache`, `episodes`,
  `event_purges`, `events`, `intent_vec`, `intents`, `kv`, `lcm_edges`, `lcm_nodes`,
  `leases`, `llm_requests`, `mcp_resources_cache`, `mcp_servers`, `mcp_tasks`,
  `mcp_tools`, `mcp_tools_fts`, `mcp_tools_vec`, `mem_candidates`, `mem_entries`,
  `mem_flags`, `mem_fts`, `mem_history`, `mem_links`, `mem_provenance`, `mem_signals`,
  `mem_vec`, `message_context`, `messages`, `messages_fts`, `oauth_clients`,
  `oauth_state`, `policies`, `projections_approval`, `projections_session`,
  `projections_workflow`, `schedules`, `schema_migrations`, `seen_items`, `sessions`,
  `skills`, `subsystem_apply_results`, `tg_actions`, `tg_outbox`, `tg_render_mode`,
  `tg_topics`, `tg_updates`, `turn_queue`, `usage`, `workflow_runs`, `workflow_step_log`,
  `workflows` (plus `prompt_snapshots` non commitée).
- Clés `kv` de travail (familles, purgées par la rétention) : `turn.*`, `prompt.prefix.<session>`,
  `wf.*` (`wf.<quoi>.<run>.<étape>.<itération>`, `wf.brief.<run>`, `wf.origin.<run>`,
  `wf.plan.go`, `wf.approval_sent.<id>`, `wf.held.<run>`), `tg.*` (`tg.offset`,
  `tg.form.<chat>.<sujet>`, `tg.held.<session>`, `tg.card.<clé>`,
  `tg.approval_destination.<id>`, `tg.burst.<id>`, `tg.new_session.<session>`,
  `tg.onboard.<chat>`, `tg.bot_username`, `tg.topic_name.<chat>.<sujet>`,
  `tg.upgrade.last_check`), `session.tools.<session>` (#104), `budget.daily.<jour>`,
  `codex.oauth`, `codex.quota`, `codex.refresh`, `codex.installation_id`, `dream.lock`,
  `dream.failed_nights`, `dream.failed_reason`, `machine.inventory`, `retention.last`,
  `backup.last`, `backup.cron.last`, `upgrade.health`, `mcp.oauth.pending.<state>`,
  `mcp.oauth.client.<issuer>`, `mcp.oauth.notified.<nom>`, `memory.usage_since`.
- Vecteurs : `f32` little-endian en BLOB, cosinus en Rust, 0.0 si dimensions différentes
  (décision 0002 ; `roundtrip`, `cosine_bounds`, `mismatched_dimensions_are_not_similar`).
- SQLite embarqué ≥ 3.50 (`the_bundled_sqlite_is_recent_enough`, #158).

### 1.12 Métriques (`penelope metrics`, texte Prometheus)

Code : `crates/penelope-observe/src/metrics.rs` (`register_default_metrics`, `render`,
`LATENCY_BUCKETS_MS`). Séries : `penelope_turns_total` (par issue),
`penelope_turn_duration_ms`, `penelope_tool_calls_total` (par outil),
`penelope_llm_requests_total`, `penelope_llm_tokens_total`, `penelope_llm_cost_usd_total`,
`penelope_mcp_request_duration_ms`, `penelope_mcp_servers_ready`,
`penelope_approvals_pending`, `penelope_effects_unknown`, `penelope_compactions_total`
(par déclencheur et issue), `penelope_compaction_missing_evidence_total` (#179),
`penelope_rss_bytes`, `penelope_store_writer_panics_total` (#44, compteur du store lu par
`doctor`). Tests : `counters_and_gauges_render`, `histogram_quantiles_are_monotonic`,
`labels_are_escaped`, `metrics_are_readable_over_rpc`.

### 1.13 Contrôles `doctor`

Code : `crates/penelope-daemon/src/doctor.rs` (`run`, une fonction par contrôle),
contrôles CLI locaux dans `crates/penelope-cli/src/commands.rs` (`daemon`, `config.file`),
plateforme dans `crates/penelope-platform/src/lib.rs::doctor`. Chaque échec porte une
commande corrective proposée, jamais exécutée. Identifiants relevés :

- CLI : `daemon` (critique, code 7 si muet), `config.file`.
- Base et journal : `db`, `audit`, `store.writer` (#44), intégrité à deux avis avec trois
  verdicts (#158), `effects` (effets incertains en attente), `retention.last`, `clock`.
- Configuration : `owner` (critique), `config.unknown` (#76), `config_coherence` (#16),
  `budget.day` (#79), `telegram.home` (#143), `telegram.allowed_chats` et conversations
  refusées récentes (#113), `telegram_forms` (#149), `sandbox.deny_read` (#68),
  `sandbox.shell_network` (#106), `shell_lines` (#150), `models.tools` (décision 0009),
  `reasoning_effort` et `dream_power` (#152), `memory.size` (#145), `vault_index` (#15),
  `vault_git` (#27), embeddings (#11), `schedules` (`last_error`, #39), `workflows`,
  `skills`, `skills.requirements` (#146), `machine.inventory` et `machine.missing` (#156).
- Secrets : `secret_roundtrip` (8 Ko, #148 et #157), `stored_secrets` (#134),
  `logs_secrets` (#26), `redactor` (#153), secrets attendus (`openrouter_api_key`,
  `telegram_bot_token`, `codex.oauth`).
- Fournisseurs et réseau : `provider.codex`, `provider.codex.identity` (avertissement
  permanent), `provider.codex.scope`, `provider.codex.quota`, hôtes joignables
  (`openrouter.ai`, `api.telegram.org`, `chatgpt.com`, `auth.openai.com`).
- MCP (`mcp_checks`) : serveurs en panne, secrets manquants, profil confiné sans lecture
  refusée (#89), trousseau ouvert par serveur (#122), URL de rappel OAuth effective
  (`mcp.oauth.redirect`, #159).
- Exploitation : `binary_signature` (#28), `install_mode` (#33), `upgrade_pending` (#36),
  boucles relancées ou finies (#84), sauvegarde (âge > 48 h, taille, durée, #42 et #77),
  ffmpeg et phrase d'essai vocale (#41), `macos.sleep`, `macos.filevault`,
  `macos.remote_login`, `macos.sandbox`, `macos.keychain`, `macos.power_source`,
  dépendances et disque (`doctor_reports_dependencies_and_disk`).
- Non commité : `prompt.stability` (#205).

Tests : `doctor_covers_the_expected_checks` (attend `owner`, `db`, `audit`, `clock`,
`effects`, `workflows`, `skills`, `reasoning_effort`, `dream_power` et, non commité,
`prompt.stability`), `a_missing_owner_is_critical_with_a_fix`,
`database_and_audit_are_healthy_on_a_fresh_install`, `pending_unknown_effects_are_surfaced`,
`unknown_config_keys_are_named_not_fatal`, `retention_is_reported_with_the_kept_content`,
`doctor_reports_the_reasoning_share_of_the_consolidation`,
`a_token_left_in_a_log_is_reported`, `a_service_launching_a_build_output_is_flagged`,
`an_upgrade_that_never_booted_is_reported`, `missing_rg_suggests_the_homebrew_formula_name`,
`image_roles_do_not_need_tool_calling`, `an_altered_read_is_not_blamed_on_a_locked_keychain`,
`an_altered_read_only_says_hex_when_it_is_hex`, `a_locked_keychain_is_still_told_to_unlock`,
`rendering_marks_failures`, `doctor_names_what_is_broken_and_how_to_fix_it` (MCP),
`doctor_reports_the_codex_provider`, `doctor_says_when_there_is_no_backup_yet`,
`doctor_reports_local_checks_without_a_daemon`.

### 1.14 Suites d'évaluation (`penelope eval <suite>`)

Code : `crates/penelope-evals/src/suites.rs` (`all_suites`, `cargo_filter`), tests
`crates/penelope-evals/tests/*.rs`. Hors réseau (12) : `unit`, `arch`, `ctx-safety`,
`mem-learning`, `mem-bench` (modèle simulé, rapport `banc-memoire.md` joint aux releases),
`mcp-conformance`, `telegram`, `hitl`, `workflow`, `hot-reload`, `resilience`, `security`.
Réseau (6, écrites, jamais rejouées par la CI) : `ctx-recall`, `mem-longitudinal`,
`mem-bench-live`, `live-openrouter`, `live-telegram`, `ab-hermes` (variables
`OPENROUTER_API_KEY`, bot de test, `PENELOPE_AB_HERMES_CMD`). Tests ignorés par défaut
(19) : `a_scanned_pdf_is_read_by_ocr`, `a_scanned_pdf_is_read_by_vision`,
`a_secret_round_trips_through_the_keychain`, `keychain_holds_a_long_secret_and_forgets_it`,
`seatbelt_enforces_denied_reads_on_this_mac`, `seatbelt_opens_the_keychain_only_when_declared_on_this_mac`,
`seatbelt_closes_unix_sockets_on_this_mac`, `real_mcp_server_handshake`,
`a_tool_is_called_with_structured_arguments`, `an_image_is_generated`,
`reasoning_is_exposed_when_requested`, `streaming_answers_with_usage_and_cost`,
`the_catalog_lists_the_configured_models`, `the_bot_sends_edits_reacts_and_cleans_up`,
`long_messages_are_split_under_the_api_limit`, `facts_survive_a_level_3_compaction`,
`fourteen_days_of_conversations_become_scoped_rules`, `mem_bench_live_consolidation`,
`penelope_does_at_least_as_well_as_hermes_for_no_more_cost`. Tests launchd réels sous
`PENELOPE_LAUNCHD_TESTS=1` (`crates/penelope-platform/tests/launchd_relay.rs`).
Matrice des critères d'acceptation générée (`docs/ca-matrix.md`, 71 tests `ca_*`, 14
sections, `UPDATE_CA_MATRIX=1`).

### 1.15 Secrets, variables d'environnement, chemins

- Noms de secrets attendus : `openrouter_api_key`, `telegram_bot_token`,
  `backup_passphrase` (#42), `codex.oauth` (#142), `runtime_<consommateur>` (flux runtime),
  jetons OAuth MCP par serveur, secrets référencés par `${SECRET:nom}` dans `config.toml`
  et `mcp.d` (et `${env:VAR}` à l'import Hermes), secrets rangés par la mémoire
  (`<contexte>-<empreinte>`). Un nom est un slug (`validate_secret_name`,
  `secret_names_accept_what_the_project_actually_uses`). Backends : trousseau macOS
  (`security` par stdin hexadécimal, découpe au-delà de 4 000 octets par ligne,
  `penelope.<nom>#n`, marque `penelope-chunks:v1:`), fichier chiffré
  (`PENELOPE_SECRETS=file`, `PENELOPE_PASSPHRASE` ou `PENELOPE_MASTER_KEY_FILE`,
  permissions `0600`), mémoire (tests).
- Variables d'environnement : `PENELOPE_HOME`, `PENELOPE_SECRETS`, `PENELOPE_PASSPHRASE`,
  `PENELOPE_MASTER_KEY_FILE`, `PENELOPE_LOG`, `PENELOPE_SERVICE=1` (sous launchd, plus de
  copie stderr), `PENELOPE_LAUNCHD_TESTS=1`, `PENELOPE_MINISIGN_PUBKEY` (build),
  `MINISIGN_PUBLIC_KEY` et `MINISIGN_SECRET_KEY` (CI), `SIGN_IDENTITY`, `SIGN_IDENTIFIER`,
  `INSTALL_DIR` (Makefile), `OPENROUTER_API_KEY`, `PENELOPE_AB_HERMES_CMD` (évaluations),
  `HERMES_HOME`, `UPDATE_DOCS=1`, `UPDATE_CA_MATRIX=1`, `PENELOPE_EVENTS_TOKEN`,
  `PENELOPE_EVENTS_URL`, `PENELOPE_EVENTS_CURSOR` (scripts Pathlayer), `HF_HUB_OFFLINE`
  (serveur vocal, documentaire).
- Chemins macOS (`crates/penelope-platform/src/dirs.rs`, `macos_directories_follow_the_prd_table`) :
  données `~/Library/Application Support/Penelope` (`penelope.db`, `secrets.enc`, `vault/`,
  `skills/`, `workflows/`, `templates/`, `mcp.d/`, `mcp-data/<nom>/`, `media/photos`,
  `media/generated`, artefacts, `backups/`), configuration `…/Penelope/config/config.toml`,
  état `…/Penelope/state` (`rpc.sock`, `rpc.token`, `runs/<run>`, `mcp-pids`,
  `upgrade.json`, `upgrade/relay/reloader.log`, workspaces de planification), journaux
  `~/Library/Logs/Penelope` (`0700`, fichiers `0600`, `daemon.err.log`), cache
  `~/Library/Caches/Penelope` (lecteur OCR compilé) ; `PENELOPE_HOME` ou `--home` déplace
  tout (`ca_2_2_penelope_home_reroots_everything`). Service `~/Library/LaunchAgents/com.penelope.daemon.plist`
  (`SERVICE_LABEL`), relais `com.penelope.daemon.reloader`, PATH complété (Homebrew,
  `~/.local/bin`, Docker, nvm). Placeholders `{data}`, `{config}`, `{state}`, `~`.

## 2. Comportements garantis, par domaine

Chemins abrégés : `daemon/` = `crates/penelope-daemon/src/`, `kernel/` =
`crates/penelope-kernel/src/`, `llm/` = `crates/penelope-llm/src/`, `context/` =
`crates/penelope-context/src/`, `memory/` = `crates/penelope-memory/src/`, `hitl/` =
`crates/penelope-hitl/src/`, `tools/` = `crates/penelope-tools/src/`, `mcp/` =
`crates/penelope-mcp/src/`, `tg/` = `crates/penelope-telegram/src/`, `wf/` =
`crates/penelope-workflow/src/`, `platform/` = `crates/penelope-platform/src/`, `observe/`
= `crates/penelope-observe/src/`, `store/` = `crates/penelope-store/src/`, `skills/` =
`crates/penelope-skills/src/`, `evals/` = `crates/penelope-evals/`. Un test sans chemin est
dans le fichier de la colonne « code ».

### 2.1 Conversation et boucle d'agent

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-1.1 | Un tour résout d'abord les appels d'outils en attente puis appelle le modèle ; premier passage et reprise suivent le même chemin | 0.1.0 | `daemon/agent.rs` (module, `resolve_pending`), `daemon/engine.rs` | `a_plain_answer_finishes_in_one_iteration` (agent.rs:2690), `a_message_is_answered_and_the_transcript_persists` (engine.rs:1482) |
| C-1.2 | Tout effet non `read` est planifié dans le ledger avant exécution ; un effet terminé est rejoué, jamais ré-exécuté (clé d'idempotence indépendante de l'ordre des arguments) | 0.1.0, §4.2 | `kernel/effects.rs` (`EffectLedger`, `EffectSpec::idem_key`), `daemon/agent.rs` | `completed_effects_are_replayed_not_reexecuted` (agent.rs:3292), `completed_effect_is_replayed_not_reexecuted`, `idem_key_is_argument_order_independent` (effects.rs), `ca_17_2_a_completed_effect_is_replayed_not_reexecuted` (evals/tests/resilience.rs:160) |
| C-1.3 | Un outil soumis à approbation suspend ce tour seulement ; la reprise (`TurnKind::Resume`) exécute l'appel approuvé sans redemander | 0.1.0 | `daemon/agent.rs`, `kernel/turn.rs` | `write_tools_suspend_the_turn_for_approval`, `an_approved_call_runs_on_resume_without_asking_again` (agent.rs:2850, 2880), `an_approval_suspends_then_a_resume_turn_finishes_the_work` (engine.rs:3025), `an_approval_card_click_resumes_the_turn` (telegram.rs:9642) |
| C-1.4 | Un refus (avec raison) est transmis au modèle, pas au harnais | 0.1.0 | `daemon/agent.rs` | `a_denied_call_is_reported_to_the_model` (agent.rs:3170), `a_refusal_with_a_reason_reaches_the_model` (telegram.rs:10053) |
| C-1.5 | Détecteur de boucles (`tools.loop_detector_repeats`, alternances, ordre des arguments) : le tour s'arrête, répond sans outil avec l'erreur exacte et deux ou trois suites en boutons, laisse `LOOP_STOP_NOTE` dans la conversation | #19, #31 | `tools/loops.rs` (`LoopDetector`), `daemon/agent.rs` (`LOOP_STOP_NOTE`, `LOOP_DEFAULT_CHOICES`) | `loop_detector_aborts_the_turn`, `a_stopped_loop_still_answers_with_the_real_error_and_choices`, `choices_are_split_from_the_answer` (agent.rs), `identical_calls_warn_then_abort`, `alternation_is_detected`, `argument_order_does_not_hide_a_loop`, `window_is_bounded` (loops.rs), `a_stopped_loop_answer_offers_choices_that_become_messages` (telegram.rs:11185) |
| C-1.6 | Un appel invalide (balisage, schéma, nom inconnu) est refusé avant politique et approbation, avec les paramètres attendus ; compte dans la garde de boucle (`observe_invalid`) | #110, #117 | `daemon/executor.rs` (`precheck`), `tools/lib.rs` (`expected_args`, `close_names`), `tools/loops.rs` | `invalid_calls_are_refused_before_any_approval_card`, `explained_errors_keep_the_loop_guard_and_tool_call_rules_stay_bounded` (engine.rs), `invalid_arguments_are_rejected_before_running`, `a_missing_native_argument_comes_back_with_the_expected_parameters`, `tool_call_accepts_any_arguments_and_unknown_names_get_suggestions` (executor.rs) |
| C-1.7 | Une erreur d'outil revient au modèle | 0.1.0 | `daemon/agent.rs`, `tools/error.rs` (`ToolError::for_model`) | `tool_errors_go_back_to_the_model` (agent.rs:3325), `messages_are_actionable` (error.rs) |
| C-1.8 | Plafond de 24 appels au modèle par tour, reprises comprises ; message `CALLS_EXHAUSTED` et bouton « Continuer (24 appels de plus) », coût du tour, suggestion de délégation | #19, #139 | `daemon/agent.rs` (`TURN_CALLS`, `CALLS_EXHAUSTED`) | `the_call_cap_spans_resumptions_and_suggests_delegating` (agent.rs:3631), `a_turn_out_of_calls_offers_to_continue` (telegram.rs:8889) |
| C-1.9 | Point de contrôle « ce tour a coûté X $, je continue ? » à chaque `budget.turn_checkpoint_usd`, coût affiché au-delà de `show_turn_cost_usd`, rappel de délégation tous les `delegate_after_calls` appels | #19 | `daemon/agent.rs` | `a_costly_turn_asks_before_going_on` (agent.rs:3545) ; rappel de délégation : partiel (couvert par C-1.8) |
| C-1.10 | Budget atteint : arrêt avant l'appel, message qui nomme la clé (`budget.session_usd`, `daily_usd`, `run_usd`), la dépense et le plafond | #4 | `daemon/agent.rs`, `kernel/budget.rs` | `exceeded_budget_stops_before_calling_the_model`, `the_budget_message_names_the_key_of_the_scope_reached` (agent.rs) |
| C-1.11 | Liste blanche d'outils par contexte (sous-agent, étape, skill) ; préfixes acceptés | 0.2.8 | `tools/lib.rs` (`is_allowed`), `daemon/agent.rs` | `tools_outside_the_allowlist_are_refused` (agent.rs:3700), `allowlists_support_prefixes` (tools/lib.rs) |
| C-1.12 | Flux : deltas, raisonnement, appels et résultats d'outils, usage, fin, erreur poussés sur le bus et vers `chat.stream` / `tail` | 0.1.0 | `daemon/agent.rs` (`emit`), `daemon/bus.rs`, `daemon/rpc.rs` | `deltas_are_streamed_to_the_sink` (agent.rs:3725), `chat_stream_sends_deltas_then_the_final_answer` (daemon/tests/chat_socket.rs:72), `a_late_waiter_still_gets_the_outcome`, `an_early_waiter_is_woken` (bus.rs) |
| C-1.13 | Panne avant flux : nouvelles tentatives 1 s, 2 s, 4 s (`request_retries`, `llm.retried`), puis replis d'alias (aussi avec OpenRouter) ; `/stop` interrompt l'attente ; l'échec final compte les essais | #5, #50, 0.2.2 | `daemon/agent.rs` (`RETRY_AFTER_MAX_SECS`, `STREAM_RETRY_SECS`), `llm/router.rs` (`fallback_chain`, `should_fallback`) | `a_transient_failure_falls_back_to_the_next_model`, `a_transient_error_before_the_stream_is_retried_with_openrouter`, `repeated_connection_timeouts_say_how_many_attempts_were_made`, `a_stop_during_the_retry_wait_ends_the_turn` (agent.rs), `ca_10_3_fallback_chain_is_used_on_transient_failure` (router.rs) |
| C-1.14 | Flux coupé avant tout texte : même modèle après 2 s puis repli ; après du texte : échec dit, bouton « Réessayer » | #5 | `daemon/agent.rs`, `daemon/telegram.rs` | `a_stream_cut_before_any_text_is_retried_then_falls_back` (agent.rs:4080), `a_failed_turn_offers_a_retry_button` (telegram.rs:8981) |
| C-1.15 | Réponse vide : une relance puis diagnostic (`turn.empty_answer`) | 0.2.2 | `daemon/agent.rs` | `an_empty_answer_is_retried_once_then_reported` (agent.rs:4128) |
| C-1.16 | Annulation (`/stop`, Ctrl-C, client de flux parti) descend jusqu'aux outils (`execute_cancellable`), aux lots de lectures et aux sous-agents (jeton enfant) ; un sous-agent qui s'arrête ne touche pas au parent | #57, #100 | `daemon/agent.rs`, `llm/provider.rs` (`CancelToken::child`) | `cancellation_stops_the_turn`, `stop_interrupts_a_whole_batch_of_reads` (agent.rs), `a_cancelled_turn_stops_its_sub_agent` (workflow.rs:3905), `a_child_token_follows_its_parent_but_not_the_other_way` (provider.rs), `a_stream_client_that_leaves_cancels_its_turn` (chat_socket.rs:167), `cancelling_a_session_reaches_its_turn` (bus.rs) |
| C-1.17 | Lectures pures consécutives exécutées par quatre (`PARALLEL_SAFE`), écritures et appels approuvés en barrière, résultats enregistrés dans l'ordre, un échec n'annule pas les autres | #85 | `daemon/agent.rs` (`PARALLEL_READS`, `PARALLEL_SAFE`) | `reads_requested_together_run_together_and_keep_their_order`, `a_write_between_reads_is_a_barrier` (agent.rs:2729, 2773) |
| C-1.18 | Résultats d'appels parallèles admis en groupe sous le budget (petits entiers, gros en artefacts) | #52 | `daemon/conversation.rs`, `context/compaction.rs` (`level1_admission`) | `parallel_tool_results_are_admitted_as_one_group`, `a_lone_tool_result_is_left_whole` (conversation.rs) |
| C-1.19 | `dispatching` et `completed` d'un effet non idempotent sont écrits en commit durable (`synchronous=FULL`, `fullfsync`) ; les lectures ne paient rien | #75 | `store/lib.rs` (`write_durable`), `daemon/agent.rs`, `kernel/effects.rs` | `only_non_idempotent_effects_pay_a_durable_commit` (agent.rs:2943), `a_durable_write_runs_under_full_sync_and_restores_normal` (store/lib.rs), `dispatch_and_completion_are_durable_planning_is_not` (effects.rs) |
| C-1.20 | Effet incertain après un arrêt : une seule demande `effect_unknown` par effet, poussée sur Telegram à la reprise, carte « C'est fait / Relancer / Ignorer », décision passée au ledger (`resolve_unknown`) avant la reprise, jamais de règle | #83, §17.3 | `daemon/agent.rs` (`EFFECT_DONE`, `EFFECT_RETRY`, `EFFECT_IGNORE`), `kernel/effects.rs` (`UnknownDecision`), `daemon/runtime.rs` | `an_uncertain_effect_marked_done_is_replayed_not_rerun`, `an_uncertain_effect_retried_runs_once`, `an_uncertain_effect_ignored_is_not_rerun`, `a_request_without_arguments_never_creates_a_rule` (agent.rs), `an_uncertain_effect_is_pushed_then_decided_from_telegram` (telegram.rs:8204), `recovery_requeues_and_asks_instead_of_retrying` (runtime.rs), `ca_17_3_an_uncertain_effect_asks_instead_of_retrying`, `an_idempotent_tool_may_be_retried_without_a_human` (resilience.rs), `ca_4_2_dispatching_becomes_unknown_without_retry`, `unknown_resolution_paths` (effects.rs) |
| C-1.21 | « Toujours » crée une règle révocable bornée à l'appel (famille, répertoire, remote et branche, hôte, clé) ; « Pour cette session » borne à la session ; la première décision gagne ; par `tool_call`, la règle porte sur l'outil visé | #67, #110, #111 | `daemon/agent.rs` (`decide_approval`), `hitl/policy.rs` | `always_decision_creates_a_revocable_rule`, `a_session_window_creates_a_rule_bound_to_the_session`, `a_second_decision_does_not_win`, `an_always_rule_is_bounded_to_the_call_it_was_granted_for`, `a_quoted_query_url_is_not_chaining_and_its_family_applies` (agent.rs), `clone_always_rule_is_limited_to_the_source_origin`, `approving_clone_always_creates_only_a_scoped_rule` (agent.rs:2152, 2167) |
| C-1.22 | Le réseau est accordé à une famille de commandes, jamais au shell entier ; une règle sans réseau ne le donne pas | #106 | `daemon/agent.rs`, `hitl/policy.rs` (`PolicyRule::matches`) | `network_is_granted_to_a_command_family_never_to_the_shell` (agent.rs:3772), `a_command_asking_for_network_is_an_external_action` (executor.rs:3627) |
| C-1.23 | Un modèle sans vision reçoit une mention à la place des images de l'historique | 0.2.7 | `daemon/agent.rs` | `blind_models_get_a_mention_instead_of_images` (agent.rs:4208) |
| C-1.24 | Choix du modèle : message trivial sans classifieur ; classification et vecteur de rappel en parallèle ; modèle épinglé par session (`/model`) ; collant hors `low`, revu aux frontières (cache froid 5 min, compaction, épisode) ; classifieur coupé ramène tout sur `main` | #74, #82, 0.2.2 | `daemon/engine.rs`, `llm/router.rs` (`is_trivial`, `route_deterministic`, `StickyModel`) | `trivial_messages_skip_the_classifier`, `ca_10_1_sticky_model_survives_until_a_boundary`, `a_pinned_model_beats_sticky_and_classifier_but_not_images` (router.rs), `the_sticky_model_is_revisited_at_boundaries`, `disabling_the_classifier_brings_every_session_back_to_main`, `classifications_are_parsed_even_with_surrounding_text` (engine.rs), `model_buttons_pin_the_session_then_give_it_back_to_the_router` (telegram.rs:10217) |
| C-1.25 | Routage image strict : seule une demande explicite d'image part sur l'alias `image` ; une photo jointe part sur `vision` même sur une session épinglée | #81 | `llm/router.rs` (`looks_like_image_request`) | `only_an_explicit_image_request_goes_to_the_image_model`, `image_attachment_routes_to_vision`, `image_request_routes_to_image_model` (router.rs), `a_report_request_never_reaches_the_image_model` (engine.rs:1375) |
| C-1.26 | Les messages en attente d'une même session et origine sont absorbés par le tour porteur (transcript séparé, heure de réception, déduplication gardée) ; un tour en vol absorbe avant chaque appel au modèle ; `turn.merged` ; réponse au dernier message ; une photo n'est pas absorbée | #161 | `kernel/turn.rs` (`TurnQueue::claim`, migration 0016), `daemon/engine.rs`, `daemon/runner.rs` | `claim_merges_pending_messages_of_one_origin`, `claim_keeps_sessions_origins_and_resume_priority_separate`, `a_live_turn_absorbs_pending_messages_and_recovery_keeps_them`, `a_message_after_completion_remains_a_separate_turn`, `a_photo_message_is_not_absorbed_as_text`, `cancelling_a_merged_turn_cancels_every_original_message` (turn.rs), `claimed_messages_keep_separate_user_entries_and_arrival_times`, `a_running_turn_absorbs_a_new_message_before_the_next_model_call` (engine.rs), `a_merged_reply_targets_the_last_telegram_message` (runner.rs) |
| C-1.27 | Rejouer un tour ne duplique pas le message utilisateur ; un `update_id` rejoué ne crée pas de second tour | 0.1.0, §17 | `daemon/engine.rs`, `kernel/turn.rs` (`dedup_key`) | `replaying_a_turn_does_not_duplicate_the_user_message` (engine.rs), `dedup_key_prevents_double_enqueue` (turn.rs), `replayed_updates_do_not_duplicate_turns` (resilience.rs) |
| C-1.28 | Pool de `runners.count` runners, bail `lease_ttl` battu toutes les `heartbeat` avec jeton de clôture (`WHERE holder = ?`), `LeaseLost` pour l'évincé, runner vivant tenu en mémoire (bail expiré sur écrivain gelé non repris), priorité des tours, `recover_on_boot` | #43, §3.2, §3.3 | `kernel/turn.rs` (`TurnQueue`), `daemon/runner.rs` | `an_evicted_runner_loses_its_lease_and_writes_nothing`, `heartbeat_prevents_reclaim`, `ca_3_2_four_sessions_run_concurrently`, `ca_3_3_expired_lease_is_reclaimed`, `a_live_runner_keeps_its_turn_when_the_writer_was_frozen`, `priority_is_respected`, `recover_on_boot_releases_everything` (turn.rs), `a_runner_that_lost_its_lease_delivers_nothing`, `the_pool_answers_queued_turns_and_waiters_get_the_outcome` (runner.rs), `ca_17_7_a_frozen_writer_does_not_duplicate_a_turn_in_flight` (resilience.rs) |
| C-1.29 | Un tour qui panique échoue proprement : runner vivant, battement arrêté, verrou rendu, échec livré ; la panique porte son emplacement | #84, #130 | `daemon/runner.rs`, `daemon/tasks.rs` | `a_panicking_turn_neither_kills_its_runner_nor_locks_its_session` (runner.rs:567) |
| C-1.30 | Titre de session automatique (3 à 6 mots, modèle `fast`, `context.auto_title`), jamais par-dessus un titre manuel | #2 | `daemon/titles.rs` | `titles_are_cleaned_and_bounded` (titles.rs), `sessions_get_a_readable_title` (telegram.rs:13563) ; « jamais remplacé » : partiel |
| C-1.31 | Sessions : `fork` (copie transcript, métadonnées, résumés, notes, budget), `rewind` (échanges mis de côté dans une session d'archive), `export` JSONL (session, run, all ; session inconnue refusée), `close` (tour arrêté, file vidée), `title`, `list`, `new` | 0.3.1, #72 | `daemon/session_ops.rs`, `daemon/rpc.rs` | `fork_copies_then_diverges`, `rewind_archives_what_it_removes`, `export_writes_jsonl_and_rebuild_restores_search` (session_ops.rs), `sessions_can_be_created_and_listed` (rpc.rs), `exporting_an_unknown_session_says_so`, `after_a_fork_only_the_fork_answers` (telegram.rs) |
| C-1.32 | Genres de session (`chat`, `workflow_run`, `sub_agent`, `scheduled`, `heartbeat`) ; seules les sessions `chat` produisent des candidats promouvables | §6.5 | `kernel/session.rs` (`SessionKind`) | `background_sessions_do_not_promote` (session.rs:700), `ca_6_7_background_sessions_produce_nothing` (provenance.rs) |
| C-1.33 | Sous-agents `sub_agent_spawn` : contexte neuf, outils restreints, retour structuré, jeton enfant, périmètre codex hérité, `outputSchema` validé en deux tentatives | 0.2.8, #57, #142 | `daemon/agent.rs` (`spawn_sub_agent`), `daemon/workflow.rs` | `a_cancelled_turn_stops_its_sub_agent` (workflow.rs), `the_subscription_only_serves_the_owner` (engine.rs:2460) ; validation `outputSchema` en deux tentatives : partiel |
| C-1.34 | Outils natifs à la demande : noyau + 3 méta-outils à chaque appel ; un outil décrit ou appelé rejoint la liste de la session au tour suivant et la quitte après dix tours ; workflows et sous-agents gardent la liste complète | #104 | `daemon/tools_on_demand.rs`, `tools/spec.rs` (`ON_DEMAND`, `search_on_demand`) | `a_discovered_tool_is_forgotten_after_idle_turns` (tools_on_demand.rs:83), `a_rare_native_tool_is_found_and_called_like_a_direct_one`, `chat_tool_definitions_are_the_core_plus_what_the_session_found`, `tool_definitions_hide_workflow_only_tools_in_chat` (executor.rs), `a_turn_offers_the_core_then_what_the_session_discovered` (engine.rs:1591) |
| C-1.35 | Champ `pourquoi` sur tout outil non-lecture ; la carte l'affiche en tête, sinon le message du propriétaire ; retiré des arguments MCP et de l'empreinte de boucle ; budget de schémas sous 3 000 tokens | #116, #150 | `tools/spec.rs` (`WHY_FIELD`, `spec()`), `daemon/telegram.rs` | `an_approval_card_says_the_intention_first` (telegram.rs:9087) ; plafond de 3 000 tokens : partiel (asserté dans `the_core_is_small_and_on_demand_tools_exist`) |
| C-1.36 | Relecture de fond après un tour substantiel (`memory_review`, au plus `review_max_candidates`) ; un accord court après une proposition est relu avec la proposition ; secrets rangés, journal du jour | §6.6, #108 | `daemon/review.rs` | `only_substantial_or_corrective_turns_are_reviewed`, `a_short_agreement_is_reviewed_only_after_a_proposal`, `candidates_are_typed_bounded_and_filtered`, `a_secret_in_a_candidate_is_shelved_and_referenced`, `a_review_notes_candidates_and_the_journal`, `an_agreement_turns_the_proposal_into_an_owner_decision` (review.rs) |
| C-1.37 | Une intention armée revient dans le contexte du message qui la réveille (une fois par tour, cooldown, budget de tirs) ; une intention datée est redirigée vers un déclencheur | 0.2.5 | `daemon/engine.rs`, `memory/intents.rs`, `daemon/executor.rs` | `an_armed_intent_comes_back_with_the_message_that_mentions_it` (engine.rs:2938), `a_dated_intent_is_redirected_to_a_schedule` (executor.rs:3267), `ca_6_10_intent_fires_respects_cooldown_and_expires` (intents.rs) |
| C-1.38 | Notes de travail `session_notes` : fichier `vault/notes/<titre>-<id>.md` à sections fixes, bloc borné en T4 à chaque tour, copie au fork, décisions récoltées par le rêve une fois | #32, #61 | `daemon/session_notes.rs` | `sections_round_trip_in_order`, `notes_survive_compaction_are_copied_by_fork_and_harvested_once` (session_notes.rs), `decisions_from_working_notes_become_candidates`, `a_decision_noted_in_a_scheduled_session_is_not_recorded` (dream.rs) |
| C-1.39 | Clé d'API manquante : message actionnable, pas de panique | 0.1.0 | `daemon/engine.rs`, `llm/lib.rs` | `a_missing_key_fails_with_an_actionable_message` (engine.rs:3083), `missing_secret_is_an_auth_error` (llm/lib.rs) |
| C-1.40 | `penelope chat` : ponctuel ou interactif (`/new`, `/stop`, Ctrl-D) ; Ctrl-C = `chat.stop` (file vidée) et code 130 ; second Ctrl-C quitte ; client parti = tour annulé ; `tail` et Telegram non concernés | #100 | `crates/penelope-cli/src/commands.rs`, `daemon/rpc.rs` | `a_stream_client_that_leaves_cancels_its_turn` (chat_socket.rs) ; Ctrl-C interactif et second Ctrl-C : aucun test |
| C-1.41 | Chaque exécution d'outil natif, MCP ou shell est journalisée en `runtime.tool` (arguments, résultat, durée, coût estimé), y compris dans les workflows | #162 | `daemon/executor.rs` | `every_native_tool_execution_is_in_the_runtime_log` (executor.rs:2860) |
| C-1.42 | Réponse vocale `send_voice` : texte rendu lisible, découpe en phrases, `tts` jamais replié sur le modèle de conversation, WAV assemblés, OGG/Opus par ffmpeg, `sendVoice`, texte trop long renvoyé pour résumé, échec en texte avec la raison, `voice.sent` | #41 | `daemon/voice.rs`, `platform/audio.rs` | `speech_text_keeps_what_is_said_out_loud`, `long_texts_are_cut_between_sentences`, `wav_parts_are_joined_with_their_duration`, `the_tts_model_never_falls_back_to_the_chat_model` (voice.rs), `a_wav_becomes_an_ogg_opus_voice_note` (audio.rs), `a_spoken_answer_arrives_as_a_voice_note`, `a_failed_synthesis_falls_back_to_text` (telegram.rs) |
| C-1.43 | Génération d'images `image_generate` (modalités `image` + `text`, `data:` décodé, enregistré sous `media/generated`, envoyé) | 0.2.8 | `daemon/images.rs` | `generated_images_are_written_and_counted`, `data_urls_are_decoded_with_their_type` (images.rs), `an_image_is_generated` (live, ignoré) |
| C-1.44 | Vision `image_inspect` : `describe`, `read`, `locate` (rôle `image_locate`, repère `auto` déduit de la famille et des valeurs, points ramenés en pixels, points douteux refusés, `model_frame`), résultat encadré comme donnée | #125, #128 | `daemon/vision.rs` | `the_describe_prompt_is_unchanged_and_locate_imposes_no_language`, `points_are_read_from_the_usual_answers`, `a_per_mille_model_read_as_pixels_is_refused`, `the_frame_is_deduced_and_checked`, `tasks_parse_and_pick_their_role` (vision.rs), `an_element_is_located_on_a_screenshot` (engine.rs:1816) |
| C-1.45 | `self_status` (version, modèle du tour et routage, configuration sans secret, coûts, file, chemins, machine, quota codex, dernière sauvegarde, contexte réel) et inventaire (`workflows`, `skills`, `tools`, `mcp`, `commands`, `schedules`, `install`, `limits`, `inventory`) ; `self_docs` (`list`, `search`, `read` paginé, `limits`, lien GitHub au tag) ; `config_set` à chaud, double confirmation pour les réglages sensibles, secrets et identité refusés | #34, 0.2.2 | `daemon/selfknow.rs`, `daemon/selfdocs.rs` | `the_report_names_the_model_the_routing_and_the_machine`, `full_config_masks_inline_secrets`, `config_set_guards`, `the_inventory_and_the_prompt_know_the_workflows` (selfknow.rs), `every_doc_file_is_embedded`, `anchors_follow_github`, `search_finds_the_workflow_subgroups_section`, `the_list_carries_each_page_role_from_the_index`, `the_interface_method_is_taught_where_the_agent_reads` (selfdocs.rs), `penelope_reports_her_own_model_and_state`, `config_set_asks_twice_for_sensitive_settings_even_with_an_always_rule` (engine.rs), `config_set_reports_the_actual_application_time`, `config_set_workspaces_applies_to_the_next_tool_call_and_turn`, `config_set_reports_canonical_and_missing_workspaces` (executor.rs) |
| C-1.46 | Inventaire de la machine : dix-sept binaires connus plus `tools.inventory_extra`, versions, connexion des forges (`gh`, `glab` par hôte), Docker joignable ; une ligne stable en T1 (ni version, ni date), réflexes de routage seulement pour ce qui est connecté, remarque sur `http_fetch` vers une forge connectée, skills annotées `binaires_manquants`, passe au démarrage, toutes les heures et à chaque `doctor` | #156, #182 | `daemon/machine.rs` (`KNOWN`, `REFLEX`, `NEEDS_LOGIN`) | `the_line_names_what_is_connected_and_routes_only_to_that`, `the_line_survives_an_upgrade_but_not_a_logout`, `the_line_says_the_sandbox_and_the_network`, `a_login_line_is_read_in_both_formats`, `a_connected_gitlab_host_survives_another_hosts_failure`, `a_fetch_to_a_connected_forge_is_remarked`, `only_binary_requirements_are_checked_here`, `the_inventory_is_stored_and_read_back` (machine.rs), `the_machine_inventory_reaches_the_model_and_keeps_the_prefix` (selfknow.rs) ; passe horaire : partiel |
| C-1.47 | Skills : chargées au démarrage et relues quand le contenu change (empreinte de contenu), `skill_search` (recherche et suggestion), `skill_load` (portage, note de commandes collées, dossier absolu), `skill_propose` / `skill_patch` (approbation `skill_proposal`, secret refusé), `skill_rollback`, `skill_reload`, portées bundled/user/workspace, index T1 | §7, #63, #118, #146 | `skills/lib.rs`, `skills/install.rs`, `daemon/supervisor.rs` (`skills_tick`) | `parses_a_valid_skill`, `ca_7_2_invalid_skills_are_rejected_explicitly`, `workspace_overrides_user_which_overrides_bundled`, `ca_7_1_new_skill_is_available_after_reload`, `broken_skill_does_not_hide_the_others`, `search_and_suggest`, `index_line_is_short`, `ca_7_3_rejected_proposal_is_never_written`, `proposal_with_a_secret_is_refused`, `write_then_rollback`, `rendered_proposal_reparses`, `flat_md_files_are_also_scanned` (skills/lib.rs), `a_skill_dropped_by_scp_is_picked_up_by_the_maintenance_pass`, `skills_reload_only_when_their_content_changes` (supervisor.rs), `ca_7_4_a_dropped_skill_is_available_without_restart`, `an_invalid_skill_does_not_break_the_registry`, `scopes_override_in_the_right_order` (hot_reload.rs), `a_skill_whose_examples_glue_commands_is_told_at_load`, `the_portage_note_translates_tools_and_names_the_directory` (install.rs) |
| C-1.48 | Périmètre codex : l'abonnement ne sert que les tours du propriétaire et leurs sous-agents ; planification, rêve, compaction, relecture, classifieur, embeddings, stt, tts, titre, workflows se replient sur l'alias OpenRouter avec `llm.codex_scope_fallback` ; `model set` refuse un alias de fond vers `codex:` | #142, décision 0010 | `daemon/codex_scope.rs`, `daemon/rpc.rs` | `a_background_model_falls_back_outside_the_subscription`, `background_roles_are_named` (codex_scope.rs), `the_subscription_only_serves_the_owner` (engine.rs), `a_background_alias_cannot_aim_at_the_subscription` (rpc.rs:1816) |
| C-1.49 | Quota codex : jauges lues à chaque réponse, alerte une fois par fenêtre à `quota_alert_ratio`, retrait à `quota_stop_ratio` (`RateLimited` avant l'appel), message qui distingue quota et panne, affichage `/budget`, `model list`, `self_status` | #142 | `daemon/codex_quota.rs`, `llm/codex.rs` | `the_gauge_reads_at_a_glance`, `the_alert_tells_a_quota_from_a_failure`, `the_worst_window_decides` (codex_quota.rs), `the_plan_gauge_alerts_once_per_window` (engine.rs:2527), `a_spent_quota_steps_aside_before_calling`, `quota_is_read_from_headers_and_events` (codex.rs) |
| C-1.50 | Une session Telegram liée par chat et sujet ; une seule session au focus écrit dans le chat ; sessions en fond retenues (`hold`) et délivrées au retour | #10, #112 | `daemon/engine.rs` (`bind_chat`), `daemon/telegram.rs` | `telegram_chats_get_their_own_bound_session` (engine.rs:3106), `background_sessions_hold_their_replies_until_switched_back`, `a_left_session_keeps_working_and_answers_on_return` (telegram.rs), `telegram_topic_lookup` (session.rs) |
| C-1.51 | Le tour interrompu (arrêt du daemon) est remis en file une seule fois ; les requêtes LLM sont classées selon l'endroit du crash | §17.1, §17.4 | `daemon/runtime.rs` (reprise), `llm/state.rs` | `ca_17_1_a_turn_interrupted_mid_flight_is_requeued_once`, `ca_17_4_llm_calls_are_classified_by_where_the_crash_happened`, `double_planning_inside_one_process_is_rejected` (resilience.rs), `ca_17_1_a_turn_is_requeued` (evals/src/ca_matrix.rs) |
| C-1.52 | Résultats MCP rendus en texte ; contenu binaire jamais dans le transcript | 0.2.4 | `daemon/executor.rs`, `daemon/mcp.rs` | `mcp_results_render_their_text_blocks` (executor.rs), `binary_content_never_enters_the_transcript` (mcp.rs) |

### 2.2 Contexte et compaction

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-2.1 | Cinq tuiles : T0 identité (`SOUL.md`, `HARNESS_RULES`, politique), T1 index (skills, méta-outils, une ligne par serveur MCP, outils à la demande nommés, index des workflows borné à 2 000 caractères, ligne machine, schémas `eager`), T2 (`AGENTS.md`, instantanés profil, cœur, projets), T3 historique, T4 volatil ; préfixe T0 à T2 identique octet pour octet d'un tour à l'autre, un seul message système, ordre déterministe, aucun schéma MCP dans le préfixe | §5.2, #17 | `context/tiers.rs` (`Tiers`, `TiersBuilder`, `HARNESS_RULES`, `WORKFLOWS_INDEX_CHARS`) | `ca_5_3_prefix_is_byte_identical_across_turns`, `index_order_is_deterministic`, `prefix_is_one_single_system_message`, `harness_rules_are_always_present`, `mcp_tool_schemas_are_never_in_the_prefix`, `memory_snapshot_lands_in_t2`, `assemble_without_volatile_has_no_trailing_system` (tiers.rs), `the_stable_prefix_does_not_move_between_turns`, `soul_and_mcp_servers_enter_the_prefix` (conversation.rs), `the_stable_prefix_only_moves_when_the_tiers_change` (hot_reload.rs) |
| C-2.2 | T4 (date locale, état du run, rappel mémoire, intentions, notes) placé en tête du dernier message utilisateur et figé avec lui (`message_context`) pendant toutes les itérations du tour et les tours suivants | #17 | `context/tiers.rs` (`assemble`, `volatile_header`), `daemon/conversation.rs` | `assemble_puts_volatile_into_the_last_user_message`, `volatile_stays_with_the_user_message_during_tool_iterations`, `volatile_header_formats` (tiers.rs), `recorded_messages_come_back_in_the_request` (conversation.rs) |
| C-2.3 | Marqueur `cache_control` en fin de T2 et sur les trois derniers messages (fournisseurs Anthropic via OpenRouter) | #17 | `context/tiers.rs`, `llm/provider.rs` | `cache_markers_follow_system_plus_three` (tiers.rs), `cache_marker_emits_cache_control` (provider.rs) |
| C-2.4 | Le raisonnement accompagne les appels d'outil, jamais les réponses finales ; il survit à un rechargement | #17 | `llm/provider.rs` (`to_openai_body`), `context/store.rs` | `reasoning_goes_back_only_with_tool_calls` (provider.rs), `reasoning_survives_a_reload` (context/store.rs) |
| C-2.5 | Un préfixe modifié (souvenir, skill, serveur MCP, promotion d'outil) attend un cache froid (pause > 5 min) ou une compaction ; un tour en cours garde son instantané | #17, décision 0008 | `daemon/conversation.rs`, `mcp/registry.rs` (`apply_promotions`), clé `prompt.prefix.<session>` | `promotions_wait_for_a_compaction_boundary`, `a_running_turn_keeps_its_frozen_snapshot` (hot_reload.rs), `promotion_waits_for_the_compaction_boundary` (registry.rs), `ca_6_14_a_profile_write_waits_for_the_next_episode` (episodes.rs) |
| C-2.6 | Empreinte de chaque requête (système, outils, messages, hachage chaîné) et cause probable d'un raté : premier appel, pause, préfixe, outils, modèle, historique réécrit, fournisseur amont ; `usage --by miss` | #17 | `daemon/cache_audit.rs`, `kernel/budget.rs` (`miss_label`) | `ca_5_4_each_request_extends_the_previous_one`, `a_miss_is_explained_by_what_changed` (cache_audit.rs) |
| C-2.7 | Niveau 1 : un résultat au-delà du budget de groupe (`min(fenêtre × part, large_payload_tokens)`) part en artefact dès son arrivée, transcript à 60 % tête et 40 % queue avec `artifact_read(…)` ; admission idempotente ; les groupes se partagent le budget | #8, #52 | `context/compaction.rs` (`level1_admission`, `externalised_body`, `tool_group_budget`), `context/store.rs` (`externalise`) | `level1_keeps_small_groups_untouched`, `level1_externalises_only_the_oversized_result`, `externalised_body_carries_the_pointer`, `a_huge_result_is_externalised_even_on_a_huge_window`, `tool_group_budget_is_a_capped_share_of_the_window` (compaction.rs), `externalise_rewrites_the_canonical_body` (store.rs), `ctx_safety_level1` (engine.rs), `huge_tool_results_are_externalised` (conversation.rs), `web_pages_and_long_listings_stay_small_in_context` (executor.rs) |
| C-2.8 | Niveaux 0 et 2 : dans la requête seulement, vieux résultats volatils remplacés par un pointeur `history_expand(…)`, puis réduits à 200 + 100 caractères, puis à un pointeur, du plus ancien au plus récent ; la queue et les entrées récentes sont protégées ; les paires restent valides | §5.4 | `context/compaction.rs` (`level0_micro`, `level2_degrade`, `plan_levels`) | `level0_only_touches_eager_tool_results`, `level0_protects_recent_entries`, `level2_degrades_until_it_fits`, `plan_levels_follows_the_prd_triggers` (compaction.rs), `ctx_safety_level0`, `ctx_safety_level2` (engine.rs) |
| C-2.9 | Niveau 3 : résumé de fond par le rôle `compaction` quand la projection estimée ou le prompt réellement facturé atteint le seuil moins la marge ; publication à la frontière de tour (mis de côté si le tour court), travail périmé refusé, idempotent ; lot minimal 2 000 tokens sauf `/compact` ; lots quand la fenêtre du résumeur est petite (12 par passe), longs messages échantillonnés | 0.2.6, #40 | `daemon/compaction.rs`, `context/engine.rs` (`SummaryJob`, `MIN_SUMMARY_TOKENS`, `SUMMARIZER_PROMPT`) | `ctx_safety_level3_publication_is_idempotent`, `only_a_forced_compaction_summarises_a_short_history`, `summary_job_batches_when_summarizer_window_is_too_small`, `summary_job_key_is_stable`, `huge_messages_are_sampled_for_the_summarizer`, `background_compaction_flag_trips_ten_points_early`, `short_session_has_nothing_to_summarise` (engine.rs), `a_summary_ready_during_a_turn_waits_for_its_end`, `a_turn_over_the_threshold_compacts_in_the_background`, `the_billed_prompt_size_requests_a_background_compaction`, `manual_compaction_replaces_old_turns_with_a_summary` (daemon/compaction.rs), `compact_summarises_the_chat_session_and_reports_back` (telegram.rs:11388) |
| C-2.10 | Résumé structuré à neuf sections (objectif, contraintes, fait, en cours, bloqué, décisions, fichiers, prochaines étapes, contexte critique), sortie JSON stricte quand le modèle la sait, 4 000 caractères par section, au moins une section clé remplie sinon rejet ; ancres (120 au plus, extraites du texte entier, jamais paraphrasées) et derniers messages du propriétaire verbatim | 0.2.6 | `context/compaction.rs` (`SUMMARY_SECTIONS`, `validate_summary`, `render_summary`, `select_verbatim_users`), `context/anchors.rs` | `summary_validation_normalises_the_model_output`, `summary_validation_rejects_empty_or_foreign_output`, `response_format_requires_every_section`, `sections_only_strips_mechanical_blocks`, `summary_schema_and_render`, `verbatim_users_are_most_recent_first_under_budget` (compaction.rs), `extracts_paths`, `extracts_sha_but_not_plain_numbers`, `extracts_tickets_and_prs`, `extracts_urls_without_trailing_punctuation`, `extracts_error_lines`, `extracts_ulids_and_uuids`, `extraction_is_deterministic_and_deduplicated`, `nothing_is_paraphrased`, `merge_prefers_recent_anchors_under_the_cap`, `extract_many_respects_the_cap` (anchors.rs), `anchors_survive_in_the_node` (lcm.rs) |
| C-2.11 | Re-compaction : le même nœud est prolongé (couverture, tokens, ancres fusionnées) au lieu d'être empilé ; LCM en DAG, couverture sans trou, profondeur illimitée, rien d'effacé | 0.2.6 | `context/lcm.rs` (`Lcm`, `coverage_gaps`), `context/engine.rs` | `recompaction_extends_the_previous_summary`, `a_crash_between_node_and_marking_is_repaired`, `ctx_safety_recovery_after_crash` (engine.rs), `leaf_then_condensed_builds_a_dag`, `coverage_gaps_are_detected`, `full_coverage_has_no_gap`, `recompaction_updates_instead_of_restarting`, `extension_absorbs_the_following_messages`, `depth_is_unlimited`, `describe_unknown_node_is_none` (lcm.rs) |
| C-2.12 | Seuils : `compaction_threshold`, marge de fond dix points plus tôt, `max_prompt_tokens` qui plafonne seuil, budget d'outils et queue, seuil abaissé sur fenêtre courte, queue jamais au-delà du quart du seuil de fond, seuil par modèle, réserve de réponse ; table par fenêtre générée et vérifiée | #18, #40, #107 | `context/compaction.rs` (`CompactionParams`, `reserved_output`, `headroom_threshold`, `tail_budget`) | `max_prompt_tokens_caps_the_thresholds`, `background_threshold_is_ten_points_lower`, `short_windows_get_a_lower_threshold_and_a_smaller_tail`, `tail_budget_is_clamped`, `natural_split_leaves_a_short_history_alone`, `split_keeps_at_least_two_user_messages_in_the_tail`, `split_never_cuts_inside_a_tool_group` (compaction.rs), `model_threshold_override` (config.rs), `the_context_page_follows_the_code` (docs.rs), `max_prompt_tokens_compacts_a_huge_window_in_the_background` (daemon/compaction.rs) |
| C-2.13 | Session froide (pause > cache) ou premier tour d'un fork au-delà du seuil : résumé avant l'appel au modèle | #40 | `daemon/compaction.rs` | `a_cold_session_is_compacted_before_the_model_call` (daemon/compaction.rs:2130) |
| C-2.14 | Les plafonds de session et de run n'arrêtent pas les résumés ; au plafond du jour, réserve `budget.compaction_reserve_usd` | #40 | `daemon/compaction.rs` | `budgets_do_not_block_compaction_until_the_summary_reserve_is_spent` (daemon/compaction.rs:2081) |
| C-2.15 | Échec du résumeur : délai 120 s + 1 s par millier de tokens (420 s max), relance sur un lot trois fois plus court, alias de repli une fois dans la réserve (prix connu exigé), cooldown persisté 60 s, 5 min, 15 min levé par `/compact`, au troisième échec compaction sans modèle (`context.compaction_mechanical`, message au propriétaire avec coût par tour, `/status` et digest le disent) | 0.2.6, #131 | `daemon/compaction.rs`, `context/compaction.rs` (`Cooldown`) | `a_passing_failure_is_retried_on_a_shorter_request`, `the_fallback_summarizer_stays_within_the_reserve`, `a_failed_summary_cools_down_until_compact_is_forced`, `three_failures_compact_without_a_model_and_say_so` (daemon/compaction.rs), `cooldown_escalates_then_clears` (compaction.rs) |
| C-2.16 | Niveau 4 : réduction sans modèle avec preuve locale de taille (corps vidés puis groupes retirés, système et dernier message gardés) ; dépassement prouvé par le fournisseur : compaction immédiate (attente d'un résumé en route jusqu'à 200 s) puis une seule relance | §5.4, 0.2.6 | `context/compaction.rs` (`level4_emergency`, `proof_of_fit`), `context/engine.rs` (`emergency`), `daemon/compaction.rs` | `ca_5_1_level4_proves_it_fits`, `proof_rejects_broken_pairs` (compaction.rs), `ctx_safety_level4` (engine.rs), `a_proven_overflow_compacts_then_retries_once` (daemon/compaction.rs) |
| C-2.17 | La projection ne relit que ce qui n'est pas résumé (`seq > covered_to`), queue lue en `ORDER BY seq DESC LIMIT 64` | #55 | `daemon/conversation.rs` | `the_projection_only_reads_what_is_not_summarised` (conversation.rs:1015) |
| C-2.18 | Fidélité observée : `context.compacted` compte les `TODO:`, `À faire:`, `- [ ]` et identifiants des messages utilisateur absents du contexte final (trois exemples de 80 caractères), sans changer le résumé ; métrique agrégée | #179 | `daemon/compaction.rs` | `fidelity_observation_reports_missing_evidence_without_rewriting_summary`, `fidelity_observation_counts_automatically_preserved_evidence`, `fidelity_observation_bounds_samples_and_ignores_other_roles` (daemon/compaction.rs) |
| C-2.19 | Historique relisible : `history_grep` (FTS sans accents, ponctuation tolérée, `scope` session ou all, suggestion `all`), `history_expand_query` (mots significatifs, toutes sessions, titre et date), `history_describe` / `history_expand` (manifeste, contenu paginé), artefacts paginés sans couper l'UTF-8 | #1, 0.3.2 | `context/store.rs` (`HistoryStore`, `significant_terms`, `sanitise_fts`, `Artifact`) | `fts_finds_messages_without_accents`, `fts_query_with_punctuation_does_not_fail`, `fts_sanitiser_quotes_terms_and_drops_operators`, `artifact_cursor_only_advances_over_returned_bytes`, `artifact_read_never_splits_utf8`, `append_and_load_roundtrip`, `queued_user_message_keeps_arrival_time_and_is_idempotent`, `payload_description_is_typed`, `kind_guessing` (store.rs), `past_sessions_are_found_with_their_title_and_date` (executor.rs:3193) ; `history_expand` / `history_describe` par l'outil : partiel |
| C-2.20 | Paires appel/résultat réparées (résultat orphelin retiré, appel sans résultat stubbé), groupes jamais séparés, tête et queue d'un long texte | §5.4 | `context/transcript.rs` | `grouping_keeps_tool_calls_with_their_results`, `valid_sequence_is_recognised`, `orphan_result_is_removed`, `call_without_result_gets_an_error_stub`, `repair_handles_partial_group`, `repair_is_idempotent`, `stub_preserves_protocol_fields`, `head_tail_splits_long_text`, `head_tail_returns_whole_short_text` (transcript.rs) |
| C-2.21 | Estimation de tokens par message (code plus dense), calibrée sur l'usage facturé, ancre invalidée quand le transcript change, image au coût appris | §5 | `llm/tokens.rs` | `prose_estimate_is_in_a_plausible_range`, `code_is_denser_than_prose`, `calibration_converges_towards_reality`, `calibration_is_bounded`, `calibration_ignores_degenerate_samples`, `message_tokens_include_tool_calls`, `images_use_the_learned_cost`, `anchor_accounts_for_previous_completion`, `usage_state_tracks_delta_and_ratio`, `anchor_is_invalidated_when_transcript_changes`, `unanchored_waits_for_proof_unless_over_window`, `fingerprint_changes_with_content` (tokens.rs) |
| C-2.22 | Un fait donné avant compaction est retrouvé après (suite réseau) | §5 | `evals/tests/ctx_recall.rs` | `facts_survive_a_level_3_compaction` (réseau, ignoré) |
| C-2.23 | Événements et observabilité de la compaction : `context.compaction_requested` (taille), `context.compaction_skipped` (raison), `context.compaction_failed`, `context.compacted`, INFO dans les journaux, `penelope_compactions_total` ; `/status`, `/budget`, `self_status` donnent taille réelle, seuils, dernière compaction | #40 | `daemon/compaction.rs` | partiel : assertés au passage dans `a_turn_over_the_threshold_compacts_in_the_background` et `the_billed_prompt_size_requests_a_background_compaction` ; aucun test dédié aux événements `skipped` et `failed` |

### 2.3 Mémoire

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-3.1 | Le vault est un wiki Markdown valide à tout instant : propriétés YAML typées, identifiants de bloc `^uid` (ajoutés automatiquement, migration des uid anciens), wikilinks résolus par nom, chemin puis alias, noms uniques, `log.md` en ajout seul, originaux dans `attachments/`, renommage qui réécrit les liens, dossiers cachés intacts | #29 | `memory/vault.rs`, `memory/wiki.rs`, `kernel/frontmatter.rs`, `daemon/wiki_e2e.rs` | `entries_use_block_ids`, `uids_are_added_automatically`, `annotations_roundtrip`, `sections_are_tracked`, `missing_annotations_are_neutral`, `wiki_links_are_extracted` (vault.rs), `touch_sets_properties_without_rewriting_the_rest`, `wikilinks_resolve_by_name_then_path`, `renaming_a_note_rewrites_every_link`, `log_lines_are_greppable_and_appended` (wiki.rs), `rendered_properties_are_valid_yaml`, `parses_scalars_lists_and_body`, `parses_dash_lists`, `booleans_and_quotes`, `crlf_is_tolerated`, `comments_are_ignored`, `render_roundtrip`, `unterminated_frontmatter_reports_the_line`, `malformed_line_reports_its_number` (frontmatter.rs), `a_full_simulated_journey_leaves_a_valid_markdown_wiki` (wiki_e2e.rs:94) |
| C-3.2 | Une liste en ligne de frontmatter garde une virgule entre guillemets | #48 | `kernel/frontmatter.rs` | `an_inline_list_keeps_a_comma_inside_a_quoted_value` (frontmatter.rs), `an_inline_alias_list_with_a_comma_resolves_its_links` (wiki.rs) |
| C-3.3 | Sept niveaux déduits du chemin ; quatre injectés d'office ; directives de profil à préfixe imposé | §6.3, §6.4 | `memory/vault.rs` (`Level`, `DIRECTIVE_PREFIXES`) | `levels_from_paths`, `directives_have_the_required_prefixes` (vault.rs) |
| C-3.4 | Annotations `depuis`, `expire`, `sensible`, `remplace`, `importance`, `projet` lues et rendues ; entrées expirées retirées la nuit ; `sensible` est un marqueur qui n'empêche pas l'injection | #25, #37 | `memory/vault.rs` (`Annotations`), `memory/consolidation.rs`, `daemon/dream.rs` | `annotations_roundtrip` (vault.rs), `the_grid_updates_journals_and_ages_the_memory` (dream.rs:5407), `temporal_sensitive_and_self_facts_are_recognised` (quality.rs) |
| C-3.5 | Pratiques (règles défaisables) : défaut, exceptions avec `quand`, écarts jamais injectés, statut et confiance dérivés, rappel contextuel (type de tâche, projet actif) avec le défaut et les exceptions satisfaites, sections indexées avec leur vrai type | §6.1, #58 | `memory/vault.rs` (`Practice`, `When`, `PracticeRecall`), `memory/recall.rs` (`CurrentContext`, `classify_task`), `daemon/conversation.rs` | `ca_6_1_defeasible_rule_recall`, `practice_parses_all_sections`, `unknown_predicate_is_not_satisfied_but_may_be_listed`, `deviations_are_never_injected`, `exception_without_when_is_reported_and_ignored`, `practice_render_roundtrip`, `a_rendered_practice_reads_back_identically`, `confidence_formula`, `when_parsing_and_rendering`, `when_evaluation`, `when_compatibility_and_intersection` (vault.rs), `practice_recall_is_context_dependent`, `practice_not_triggered_is_not_injected`, `task_classification` (recall.rs), `a_practice_is_recalled_with_its_default_and_never_its_deviations`, `practice_sections_are_indexed_with_their_own_type`, `an_entry_of_the_open_project_ranks_first` (conversation.rs) |
| C-3.6 | Index hybride : FTS5 (mots courts ignorés, préfixes) et vecteurs (cosinus), fusion RRF, facteurs de récence (demi-vie relue à chaud), importance, projet actif (LRU de quatre), confiance, usage ; filtres (niveau, type, projet, slug, épisodique, `source`, parties de pratiques) appliqués **avant** la coupe aux 200 ; `retire` retire de la recherche sans effacer la ligne ; liens indexés ; provenance écrite une fois et jamais relevée | §6.11, #86, #87, décision 0002 | `memory/index.rs` (`MemoryIndex::search`, `SearchFilter`, `rrf`, `decay`, `usage_factor`), `store/vector.rs` | `scoring_factors_follow_the_prd_table`, `rrf_rewards_being_in_both_lists`, `upsert_and_get`, `provenance_is_written_once_and_never_upgraded`, `fts_search_finds_entries`, `vector_search_complements_fts`, `active_project_entries_rank_higher`, `episodic_is_excluded_unless_explicitly_requested`, `retire_removes_from_search_but_keeps_the_row`, `links_are_indexed`, `age_is_computed_from_dates_and_timestamps`, `fts_query_drops_short_words` (index.rs), `ingested_passages_do_not_evict_memories_from_recall`, `an_old_curated_entry_is_still_recalled`, `a_recent_entry_ranks_before_an_old_equivalent`, `the_half_life_setting_changes_the_order`, `project_keys_are_normalised`, `active_projects_are_lru_bounded_to_four` (recall.rs) |
| C-3.7 | Rappel automatique (voie 1) borné à `recall_timeout_ms` (150 ms), jamais bloquant, seuil sur la pertinence seule (`trigger_threshold`), au plus `max_injected_per_turn`, jamais d'épisodique ni de passage de document ; escalade sémantique quand la phrase montre une intention de rappel et que la voie 1 est faible ; vecteur du message sous 1,5 s sinon lexical | §6.12, #11, #86 | `memory/recall.rs` (`Recall`, `RecallParams`, `shows_recall_intent`), `daemon/embeddings.rs` | `ca_6_12_recall_never_blocks`, `path1_injects_triggered_entries_under_budget`, `path1_never_injects_episodic_or_low_score`, `escalation_requires_intent_and_weak_path1`, `recall_intent_detection` (recall.rs), `a_memory_entry_is_found_by_a_synonym` (embeddings.rs) |
| C-3.8 | Instantanés T2 (profil, cœur, projets) sous budgets de tokens, triés par importance, hachage stable, figés par épisode ; entrées servies d'office jamais rappelées deux fois ni comptées « jamais rappelées » ; filtrés par le sujet de travail de la session (`/projet`, déduit du sujet Telegram, du titre ou du premier message) | §6.3, #62, #119 | `memory/recall.rs` (`Snapshots`), `daemon/conversation.rs`, `daemon/session_project.rs` | `snapshots_hash_is_stable`, `snapshot_block_respects_budget_and_importance` (recall.rs), `the_injected_memory_follows_the_session_subject`, `an_injected_entry_is_never_recalled_twice` (conversation.rs), `ca_6_14_a_profile_write_waits_for_the_next_episode` (episodes.rs) |
| C-3.9 | Provenance : classification conservatrice (`owner`, `agent`, `untrusted`, `system`), contamination du tour par un outil réseau ou `openWorldHint`, marqueur d'écho (un souvenir rappelé n'est jamais ré-extrait), filtre de session, encadrement du contenu non fiable | §6.5 | `memory/provenance.rs` (`classify`, `TurnContamination`, `InjectionMarker`, `session_allows_candidate`, `frame_untrusted`) | `classification_is_conservative`, `promotion_and_injection_rights`, `turn_contamination_lifecycle`, `open_world_mcp_tool_contaminates`, `ca_6_6_recalled_memory_is_never_re_extracted`, `echo_detection_ignores_punctuation_and_case`, `ca_6_7_background_sessions_produce_nothing`, `workflow_sessions_promote_only_after_an_owner_decision`, `interactive_sessions_promote_everything`, `untrusted_content_is_always_framed`, `provenance_builders` (provenance.rs), `untrusted_content_is_never_promotable` (security.rs) |
| C-3.10 | Candidats typés (`fait`, `preference`, `correction`, `ecart`, `decision`), cinq par tour au plus, groupés par sujet et signature de contexte (Jaccard), sessions et jours comptés, échos jamais enregistrés, sessions de fond muettes, états et expiration, trois reports puis rejet, détection de correction et de règle énoncée | §6.6, #59 | `memory/candidates.rs` (`CandidateStore`, `group`, `looks_like_correction`, `stated_as_a_rule`) | `at_most_five_candidates_per_turn`, `grouping_counts_sessions_and_days`, `duplicates_in_the_same_session_and_day_collapse`, `different_context_signatures_are_different_groups`, `memory_echoes_are_never_recorded`, `background_sessions_record_nothing`, `state_transitions_and_expiry`, `three_deferrals_reject_the_candidate`, `correction_detection`, `rule_phrasing_detection`, `jaccard_similarity`, `subject_keys_ignore_stop_words` (candidates.rs) |
| C-3.11 | Épisodes : clôture après 2 h d'inactivité, trois messages consécutifs hors sujet (similarité lexicale, décision 0006) ou `/new` ; relecture unique (résumé au journal, candidats) ; instantané T2 figé par épisode | 0.4.0 | `daemon/episodes.rs` | `ca_6_15_two_idle_hours_close_the_episode_and_ingest_it`, `three_messages_off_topic_open_a_new_episode`, `lexical_similarity_follows_the_topic`, `a_summary_is_read_from_the_json_answer`, `ca_6_14_a_profile_write_waits_for_the_next_episode` (episodes.rs) |
| C-3.12 | Consolidation nocturne (`dreaming_cron`) : verrou `dream.lock`, phases Light, REM, Deep, portes déterministes (non fiable et système exclus avant tout prompt, vague ignoré), opérations `add_entry`, `replace_entry`, `supersede_entry`, `retire_entry`, `add_exception`, `update_exception`, `record_ecart`, `update_default` (toujours une proposition), `link`, `create_entity`, `noop` validées (uid connu, pratique connue, `quand` valide, plafond de retrait par fichier, édition manuelle reporte) puis appliquées ligne par ligne, pré-images `mem_history`, `DREAMS.md`, commit git, `--dry-run`, passe vide sans effet | 0.2.9, §6.8 | `daemon/dream.rs`, `memory/consolidation.rs` (`Operation`, `Consolidation`), `daemon/vault_git.rs` | `untrusted_candidates_never_reach_the_model_and_vague_ones_are_ignored`, `a_dream_is_committed_in_the_vault_history`, `the_quality_gate_shapes_the_promoted_memory`, `corrections_become_exceptions_of_an_existing_practice` (dream.rs), `update_default_is_always_a_proposal`, `ca_6_8_manual_edit_defers_the_operation`, `unknown_uid_is_rejected`, `retire_ratio_is_capped_per_file`, `ca_6_13_forbidden_content_is_blocked_in_consolidation`, `exception_without_valid_when_is_rejected`, `phases_progress`, `ca_6_11_empty_pass_is_a_noop`, `operations_schema_validates_shape`, `untrusted_candidates_never_reach_the_model` (consolidation.rs) |
| C-3.13 | Grille de tri à cinq critères (durable, utile, précis, introuvable ailleurs, endossé) : le modèle argumente, le code place (ignoré, journal avec `expire`, mémoire durable) ; souvenirs proches fournis (embeddings sinon lexical) ; `supersede` pose `remplace` et `depuis` ; un texte déjà en mémoire n'est jamais ajouté deux fois ; une règle dictée une fois passe dès la première nuit ; `endossé` remplace la confirmation manuelle | #37, #24 | `memory/grid.rs` (`Verdict`, `Placement`, `journal_expiry`), `daemon/dream.rs` | `the_grid_decides_from_the_criteria`, `journal_expiry_is_bounded`, `verdicts_operations_and_noops_are_read_leniently` (grid.rs), `a_stated_rule_is_promoted_once_with_history_and_review`, `a_rule_dictated_by_the_owner_is_promoted`, `the_grid_updates_journals_and_ages_the_memory`, `replaying_a_written_batch_adds_nothing_twice` (dream.rs) |
| C-3.14 | Contradictions : polarité en tête de phrase, sujet commun par Jaccard (0,4) et similarité d'embedding (0,80), bornes de longueur, `fait` et `écart` ne contredisent rien ; sans contexte distinct la contradiction devient une question ; avec contexte distinct, une exception ; carte `memory_proposal` à trois boutons (Remplacer, Exception, Ignorer), question non reposée le lendemain, rangée dans `DREAMS.md` et `mem candidates` | #145, §6.8 | `memory/consolidation.rs`, `daemon/dream.rs`, `daemon/ingest.rs` | `ca_6_4_contradiction_without_distinct_context_asks`, `contradiction_with_distinct_context_is_an_exception`, `unrelated_statements_are_not_contradictions`, `a_file_is_not_a_rule_and_two_unrelated_directives_do_not_clash` (consolidation.rs), `a_distant_neighbour_does_not_clash` (dream.rs), `the_three_buttons_of_a_clash_card_decide` (ingest.rs:1072) |
| C-3.15 | Lots de consolidation : `dream_batch`, sortie coupée rejouée sur un lot plus petit sans repartir à 40, taille qui retient ce qui a tenu, coupure à deux ou moins n'accuse pas la taille, candidat seul coupé repris une fois à sortie doublée, passe de lots d'un candidat arrêtée après huit, estimation de sortie apprise par candidat, budget de raisonnement séparé et relevé sur « raisonnement plein », kill switch `consolidation_reasoning = "off"`, sortie utile seule dimensionne, lot écrit un par un (snapshot relu entre lots), passe interrompue reprise, délai d'appel dérivé du budget (4 à 15 min), notre délai distingué d'une coupure réseau (sonde TCP), trois tentatives par lot, `memory.dream_batch` et `memory.dream_retry` par tentative, `memory.dream_failed` et message au foyer, nuits blanches comptées | #59, #127, #135, #140, #152 | `daemon/dream.rs` (`OutputBudget`, `BatchSizer`, `call_timeout`, `network_stall`) | `many_candidates_are_consolidated_in_bounded_batches`, `a_truncated_consolidation_retries_with_a_smaller_batch`, `a_verbose_candidate_does_not_leave_the_pass_one_by_one`, `a_starved_batch_raises_the_reasoning_budget_instead_of_shrinking`, `batches_are_written_one_by_one_and_survive_a_failure`, `switching_reasoning_off_sends_the_kill_switch`, `a_network_stall_replays_the_batch_instead_of_giving_up`, `the_output_budget_ignores_what_was_spent_thinking`, `a_lone_cut_is_retried_with_twice_the_output`, `a_pass_of_lone_lots_stops_and_says_so`, `batch_sizes_remember_what_held`, `a_heavy_head_does_not_shrink_every_later_batch`, `a_cut_teaches_the_output_estimate`, `a_pass_does_not_restart_from_the_full_batch_after_a_cut`, `a_passing_error_is_retried_on_its_batch`, `the_call_deadline_follows_the_budget_it_was_given`, `our_own_deadline_is_not_a_network_cut`, `a_failed_night_keeps_what_it_wrote_and_is_said_once` (dream.rs) |
| C-3.16 | État d'un candidat décidé après l'écriture : opération refusée, écriture en échec ou candidat sans opération restent en attente avec raison ; promu dès l'entrée écrite | #60, #127 | `daemon/dream.rs` | `a_rejected_operation_leaves_its_candidate_pending`, `a_durable_candidate_without_any_operation_stays_pending` (dream.rs) |
| C-3.17 | Porte de qualité : texte tronqué, phrase incomplète ou sujet absent rejetés ; au-delà de 300 caractères scindé en phrases ; état passager vers `projets.md` avec `expire` ; données client, financières ou de sécurité marquées `sensible` ; faits sur la configuration de Pénélope refusés ; `mem_remember` refuse au-delà de `MAX_ENTRY_CHARS` ; `mem_note` sans borne | #25, #145 | `memory/quality.rs`, `daemon/vault_ops.rs` | `truncated_or_subjectless_texts_are_rejected`, `a_long_paragraph_is_split_into_facts`, `temporal_sensitive_and_self_facts_are_recognised` (quality.rs), `a_memory_entry_holds_one_fact` (vault_ops.rs:868), `quality_gate_shapes_what_gets_promoted` (consolidation.rs) |
| C-3.18 | Secrets dans un candidat rangés dans le magasin sous un nom tiré du contexte et d'une empreinte, la mémoire garde `${SECRET:nom}` ; une référence traverse la détection et la rédaction ; un numéro de carte est refusé avec son fragment masqué ; un nombre collé à un identifiant n'est pas une carte à l'écriture | #37, #132 | `daemon/secret_shelf.rs`, `observe/redact.rs` (`secret_fragment`, `luhn`) | `names_follow_the_context_and_the_value`, `references_are_listed_once` (secret_shelf.rs), `an_artifact_id_passes_and_a_card_refusal_names_its_fragment`, `secrets_and_injections_never_enter_the_vault` (vault_ops.rs), `a_number_inside_an_identifier_is_not_a_card` (redact.rs) |
| C-3.19 | Chemin d'écriture unique (`vault_ops`) : `mem_remember`, `mem_note`, `mem_forget` (ligne retirée, entrée retirée de l'index), filtre de secrets et d'injections, écriture optimiste (relecture, réapplication ligne à ligne, report si la ligne a changé), écriture atomique, index mis à jour, migration d'un vault antérieur sans perdre la provenance, `reindex` ajoute les uid manquants et indexe les notes manuscrites, garde la provenance et les signaux | §6.10, #29 | `daemon/vault_ops.rs`, `memory/edit.rs`, `memory/index.rs` | `remembered_entries_land_in_the_vault_and_the_index`, `secrets_and_injections_never_enter_the_vault`, `forgetting_removes_the_line_and_retires_the_entry`, `a_concurrent_hand_edit_is_merged_or_the_operation_deferred`, `a_legacy_vault_is_migrated_without_losing_provenance`, `reindex_adds_missing_uids_and_indexes_hand_written_notes` (vault_ops.rs), `memory_tools_write_through_the_vault` (executor.rs), `appending_goes_under_its_section_and_keeps_the_rest`, `replacing_removing_and_linking_touch_one_line`, `a_callout_entry_is_removed_whole`, `a_whole_entry_is_replaced_in_place` (edit.rs), `ca_6_9_reindex_keeps_provenance_and_signals`, `forget_session_retires_its_entries` (index.rs), `memory_write_path_refuses_forbidden_content` (security.rs) |
| C-3.20 | Digest du matin (`digest_cron`, foyer) en une bulle : appris (compte, cinq exemples), fichiers touchés, questions en attente, Cœur au-delà du budget, entrées trop longues, jamais rappelées, motifs d'écart par famille, dépense de la veille, agenda, planifications en échec, nuits sans promotion, échec de compaction des 24 h, score d'audit le lundi ; au-delà de `max_fragments` en document | #109, #145 | `daemon/dream.rs` (`DreamReport::render_brief`), `daemon/telegram.rs` | `a_night_is_told_in_three_numbers_and_grouped_reasons`, `quiet_nights_are_said_and_the_digest_stays_short`, `the_morning_digest_is_short_and_readable` (dream.rs), `a_scheduled_digest_is_delivered_once` (telegram.rs:9466) ; score d'audit du lundi et envoi en document : partiel |
| C-3.21 | Retour d'usage : un souvenir servi compte comme rappelé, utile seulement si la réponse reprend un mot distinctif (deux pour un long) ; tour rejoué non recompté ; `usage_factor` borné [0,85 ; 1,2] ordonne sans franchir la pertinence ; hors conversation, date seule ; retrait proposé après 60 jours sans rappel et dix apparitions (`mem_signals.seen`) ; `mem signals` ; migration 0015 | #62, #105, #86 | `daemon/usage_feedback.rs`, `memory/index.rs` (`usage_factor`, `Signals`), `memory/recall.rs` (`used_in_answer`) | `usage_factor_is_bounded_and_rewards_useful_recalls_only`, `usage_orders_equal_matches_without_beating_a_better_one`, `signals_accumulate` (index.rs), `a_memory_is_useful_when_the_answer_uses_what_it_brought`, `only_entries_that_had_their_chance_are_proposed_for_retirement` (recall.rs), `a_recalled_memory_counts_as_useful_only_when_the_answer_uses_it` (engine.rs), `usage_signals_are_evidence_for_the_grid_never_a_gate` (dream.rs), `memory_signals_are_readable` (rpc.rs), `usage_counters_restart_from_zero_once` (migrations.rs) |
| C-3.22 | Intentions : temporelles redirigées vers un déclencheur, événementielles armées avec déclencheurs (mots vides retirés), cooldown, budget de tirs, expiration, trois par tour, préfiltre vectoriel des paraphrases, annulation | §6.9 | `memory/intents.rs` (`IntentStore`, `classify_intent`, `extract_triggers`) | `temporal_and_eventual_intents_are_distinguished`, `triggers_are_extracted_without_stop_words`, `ca_6_10_intent_fires_respects_cooldown_and_expires`, `expiry_marks_intents_expired`, `at_most_three_intents_per_turn`, `vector_prefilter_catches_paraphrases`, `cancel_disarms`, `eligibility_rules`, `lexical_score_is_a_proportion` (intents.rs) |
| C-3.23 | Ingestion de documents : PDF page par page (décompression bornée, panique rattrapée), DOCX, HTML sans scripts, Markdown, texte ; formats refusés ou vides ; slug lisible ; fiche `sources/<slug>.md` d'origine `untrusted` (sauf `/mien`), passages indexés en `source`, exclus du rappel automatique et encadrés à la lecture ; secrets et cartes masqués ; original conservé ; même contenu reçu deux fois reprend la fiche ; réindexation garde la provenance ; résumé et au plus cinq propositions `memory_proposal` écrites une seule fois après « Tout » ; boîte de dépôt `inbox/` vidée ; OCR Vision pour un PDF sans texte (macOS, 50 pages) ; `document.ingested` | 0.2.7, 0.4.0 | `memory/ingest.rs`, `daemon/ingest.rs`, `platform/ocr.rs` | `pdf_text_is_extracted_page_by_page`, `garbage_pdf_is_an_error_not_a_panic`, `docx_paragraphs_tabs_and_entities_survive`, `html_keeps_text_and_drops_scripts`, `unsupported_or_empty_documents_are_refused`, `slugs_are_readable_and_safe`, `source_files_round_trip_and_keep_their_origin`, `passages_respect_paragraphs_and_the_target_size` (memory/ingest.rs), `the_vault_inbox_is_ingested_then_emptied`, `a_scanned_pdf_is_read_by_ocr` (ignoré), `reindexing_keeps_documents_untrusted`, `an_approved_proposal_is_written_once`, `proposals_pass_the_memory_write_filter`, `a_plain_text_answer_still_gives_a_summary` (daemon/ingest.rs), `output_is_split_into_pages`, `a_scanned_pdf_is_read_by_vision` (ignoré, ocr.rs), `a_document_is_ingested_proposed_and_answered`, `mien_marks_a_document_as_written_by_the_owner` (telegram.rs) |
| C-3.24 | Ingestion interruptible : registre des ingestions par session sur le bus, jeton d'annulation traversant `ingest`, `summarise`, `chat_stream` ; `/stop` les compte, `/stop tout` les annule | #155 (0.17.40) | `daemon/bus.rs` (`start_ingest`, `end_ingest`, `cancel_ingests`), `daemon/ingest.rs` | partiel : `stop_promises_only_what_it_did` (telegram.rs:11138) ; aucun test dédié à l'annulation d'une ingestion en cours |
| C-3.25 | Wiki de concepts : pages `concepts/<slug>.md` tirées des documents (définition, alias, sources), dédoublonnées par les mots puis le sens, liens `[[slug]]` depuis les sources et la mémoire, `concepts/_a-definir.md`, `index.md`, outil `mem_neighbors` | #22 | `daemon/concepts.rs` | `two_sources_sharing_a_term_meet_on_a_concept_page`, `names_compare_without_accents_or_plurals` (concepts.rs) ; `mem_neighbors`, `_a-definir.md` et `index.md` : partiel |
| C-3.26 | Embeddings des entrées, intentions et outils MCP calculés en fond (après tour, réindexation, inscription d'outils), cache par contenu, `mem reindex --embeddings`, ancien défaut basculé au chargement, `doctor` et `self_status` disent hybride ou lexical | #11 | `daemon/embeddings.rs`, `kernel/config.rs` (`DEFAULT_EMBEDDING_MODEL`, `LEGACY_EMBEDDING_MODEL`) | `a_memory_entry_is_found_by_a_synonym` (embeddings.rs:347) ; rattrapage de fond, bascule du défaut : partiel |
| C-3.27 | Accueil : neuf questions écrites dans `accueil/accueil-AAAA-MM-JJ.md` avant d'être posées, reprise après pause, récapitulatif validé avant d'écrire `profil.md` (directives) et `memoire.md` (provenance vers la question), parties rejouables (`profil`, `outils`, `style`, `limites`), inventaire machine proposé pour `outils`, proposé au premier message d'un profil vide | #21, #156 | `daemon/onboarding.rs` (`Part`) | `answers_become_directives` (onboarding.rs:707), `onboarding_writes_the_profile_from_the_answers`, `an_empty_profile_proposes_onboarding_once` (telegram.rs) |
| C-3.28 | Audit sur 100 : barème fixe v2 sur cinq axes (propriétaire, portée, savoir-faire, autonomie, qualité avec le lint), une prochaine action par axe, historique `audits/`, écart depuis le précédent | #23, #29 | `daemon/mem_audit.rs` (`Axis`, `best_next`) | `an_onboarding_raises_the_score_and_changes_the_next_action` (mem_audit.rs:406) |
| C-3.29 | Inventaire du vault : contenu hors index nommé (`vault check`, `doctor`, journal, `memory.index_gap`) ; une recherche vide rappelle le périmètre ; exclusions documentées | #15 | `daemon/vault_inventory.rs` | `content_outside_the_index_is_reported` (vault_inventory.rs:234) |
| C-3.30 | Lint du wiki (`vault lint`, rêve, audit) : liens non résolus, blocs absents, orphelines, impasses, alias et noms en double, identifiants invalides ou dupliqués, propriétés, entrées expirées, contradictions proposées | #29 | `memory/wiki.rs` (`lint`, `LintReport`) | `lint_reports_the_graph_problems` (wiki.rs:870) |
| C-3.31 | Historique git du vault : dépôt créé au démarrage si autocommit, `.gitignore`, commit périodique, commit par rêve, push si remote, avertissement `doctor` et digest, `mem diff [--since dream]` | #27 | `daemon/vault_git.rs`, `platform/process.rs` (`git_sync_repo`, `git_commit_push`) | `a_dream_is_committed_in_the_vault_history` (dream.rs) ; commit périodique, push et `mem diff` : aucun test |
| C-3.32 | `mem split <uid>` : découpage d'une entrée longue en faits courts (puces, listes numérotées, phrases), données financières et administratives écartées, second essai sur réponse vide ou trop courte, échec expliqué sans carte, carte `memory_proposal` qui remplace l'entrée acceptée, `memory.split_proposed` / `split_applied` | #145, #184 | `daemon/mem_split.rs` | `facts_are_read_as_bullets`, `an_apology_or_intro_is_never_a_memory_fact`, `a_long_bullet_is_split_at_sentence_boundaries`, `splitting_a_long_bullet_keeps_dots_inside_urls`, `financial_and_administrative_details_are_not_proposed`, `a_silent_or_sparse_model_answer_requires_another_attempt` (mem_split.rs), `an_accepted_split_replaces_the_catch_all_entry` (ingest.rs) |
| C-3.33 | Commandes de mémoire : `mem history` (pré-images par uid ou fichier), `mem restore <id>`, `mem candidates` (attente et questions), `mem learned <jours>`, `mem retry-rejected` (migration 0007), `mem forget`, `/appris`, `/pratique`, `/oublie`, `/forget <session>`, `/retiens`, `/note`, `/recall` | 0.2.9, #24 | `daemon/rpc.rs`, `daemon/vault_ops.rs`, `daemon/telegram.rs` | partiel : `a_stated_rule_is_promoted_once_with_history_and_review` (pré-images), `forget_session_retires_its_entries` (index.rs), `three_deferrals_reject_the_candidate` ; `mem restore`, `mem learned`, `mem retry-rejected`, `/appris`, `/pratique`, `/oublie`, `/retiens`, `/note`, `/recall` : aucun test dédié |
| C-3.34 | Procédure candidate proposée comme skill seulement après deux exécutions réussies dans deux sessions distinctes | #178 | `memory/consolidation.rs` (`ProcedureCandidate`) | `procedure_candidate_becomes_a_proposal`, `procedure_requires_two_distinct_sessions` (consolidation.rs) |
| C-3.35 | Écarts : promotion en exception après `ecart_min_occurrences`, `ecart_min_sessions`, `ecart_min_days` ; correction bornée à son contexte ; écart jamais promu expiré après `expire_ecart_days` | §6.2, §6.3 | `memory/consolidation.rs` | `ca_6_2_ecart_promotion_thresholds`, `ecart_needs_a_single_day_span_of_two`, `ca_6_3_correction_scoping` (consolidation.rs) |
| C-3.36 | Banc d'essai de la mémoire : `mem-bench` (modèle simulé, seuils exacts, rapport joint aux releases), `mem-bench-live`, `mem-longitudinal` (14 jours, score ≥ 85 %, aucune promotion non fiable) | #37, 0.4.0 | `evals/src/mem_bench.rs`, `evals/tests/mem_bench.rs`, `mem_longitudinal.rs` | `mem_bench_simulated_consolidation_keeps_what_it_should` ; `mem_bench_live_consolidation`, `fourteen_days_of_conversations_become_scoped_rules` (réseau, ignorés) |
| C-3.37 | Skills tierces : `skill install owner/repo[@rev][:a,b]`, archive HTTPS sans `git`, dossier entier copié, refus avant écriture (remontée, lien symbolique, taille, `SKILL.md` invalide), frontmatter complété (`version`, `allowed_tools` déduit), dépendances `requires` listées jamais installées, `doctor skills.requirements`, `--force` | #146 | `skills/install.rs`, `daemon/skill_install.rs`, `daemon/skill_deps.rs` | `a_source_reads_owner_repo_ref_and_skills`, `a_skill_is_installed_with_all_its_files`, `an_archive_never_writes_outside_the_skill_directory` (install.rs), `the_report_names_what_is_installed_and_what_is_missing` (skill_install.rs), `a_requirement_is_read_or_refused`, `what_is_missing_is_said_once_with_its_command` (skill_deps.rs) |

### 2.4 HITL : approbations, règles, modes

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-4.1 | Classes de risque `read`, `write`, `destructive`, `external`, `unknown` ; déduction depuis les annotations MCP (destructif gagne, `openWorldHint` rend externe, absence = inconnu) ; politique par défaut `read` auto, `write` ask, `destructive` ask_twice, `external` ask, `unknown` ask ; la configuration (`tool_risk`, `tool_policy`) l'emporte toujours sur les annotations | §8.10, §11 | `kernel/risk.rs` (`RiskClass`, `classify_annotations`, `PolicyDecision`, `PolicyWindow`), `hitl/policy.rs` (`PolicyEngine::default_for`), `tools/lib.rs` (`effective_risk`) | `read_only_is_read`, `read_only_but_open_world_is_external`, `destructive_wins`, `no_annotation_is_unknown`, `explicit_write_is_write`, `decisions_roundtrip` (risk.rs), `defaults_follow_the_prd_table`, `annotations_never_override_configuration` (policy.rs), `risk_overrides_win_over_annotations` (tools/lib.rs), `tool_annotations_drive_risk_classification` (mcp_conformance.rs) |
| C-4.2 | Fenêtres `once`, `run`, `session`, `always` ; seule `always` crée une règle visible et révocable (`/policies`, `policy.revoke`) ; une règle plus spécifique (`tool` > `server` > `global`) gagne ; fenêtre `run` révoquée à la fin du run ; `once` ne rejoue jamais | §8.10, §9.3 | `hitl/policy.rs` (`PolicyRule`, `RuleScope`, `in_window`) | `ca_9_3_always_rule_applies_then_is_revocable`, `more_specific_rule_wins`, `run_window_is_scoped_to_that_run`, `once_window_never_matches_later`, `window_revocation_on_run_end` (policy.rs) |
| C-4.3 | Motifs d'arguments des règles : `$cmd_prefix` (famille de commande, frontière de mot, refus de tout enchaînement), `$path_prefix` (chemin normalisé, casse réelle du volume, lien symbolique sortant refusé, chemin relatif résolu dans le workspace), `$origin` (schéma et hôte de clone, formes scp et GitHub) ; motifs imbriqués ; jamais contournables | #67, #160, #164 | `hitl/policy.rs` (`CMD_PREFIX_OP`, `PATH_PREFIX_OP`, `ORIGIN_OP`, `command_matches`, `family_covers`, `describe_pattern`) | `pattern_operators_cannot_be_tricked`, `nested_argument_patterns`, `clone_origin_pattern_understands_scp_and_github_shortcuts`, `path_rule_uses_workspace_filesystem_case_and_refuses_symlink_escape`, `evaluation_resolves_relative_path_rule_in_workspace` (policy.rs) |
| C-4.4 | Un seul lexer de ligne de commande : mots, apostrophes littérales, guillemets doubles littéraux sauf `$`, backtick et `\` ; une liste `a && b && c` d'étapes nommables est une liste (une famille par étape, trois règles par clic au plus, couverture seulement si chaque étape est couverte) ; tube vers une lecture pure garde la famille de la tête ; affectations d'environnement anodines gardent le programme, celles qui détournent l'interpréteur (`PATH`, `IFS`, `DYLD_*`, `GIT_SSH_COMMAND`, …) laissent la ligne composée ; `;`, `\|\|`, `(`, `)`, redirections, substitutions, échappements, sauts de ligne, `!`, guillemets non fermés, `sh -c`, `sudo`, `env`, `xargs`, `timeout`, `python3 -c`, `node -e` restent composés ; `cd`, `export`, `set` et les lectures n'ont besoin d'aucune règle | #141, #150, #111 | `hitl/cmdline.rs` (`list`, `pipeline`, `simple`, `family`, `is_read`, `sets_up_shell`, `needs_no_rule`, `why_composed`) | `quoted_operators_are_characters_not_chaining`, `chaining_outside_quotes_still_composes`, `a_pipe_into_pure_reads_keeps_the_family_of_its_first_stage`, `leading_assignments_keep_the_program_unless_they_hijack_it`, `a_family_is_a_simple_line_without_assignment`, `unusual_shapes_never_panic`, `a_chain_of_ands_is_a_list_of_steps`, `everything_but_and_stays_composed`, `an_ampersand_in_quotes_is_not_a_chain`, `pipeline_still_refuses_a_list` (cmdline.rs), `an_and_list_gets_a_rule_per_family_and_stops_asking`, `a_composed_line_records_that_no_rule_was_written`, `a_quoted_query_url_is_ruled_by_its_family` (engine.rs) |
| C-4.5 | Familles autorisées d'avance (`tools.shell_allow`, `shell_allow_network`), lues par le même découpage ; une liste n'est couverte que si chaque étape l'est | #111, #150 | `daemon/approval_mode.rs` (`declared_allow`) | `a_declared_family_covers_a_list_only_when_every_step_is_covered` (approval_mode.rs:195), `a_declared_family_covers_a_quoted_query_url` (engine.rs:2428) |
| C-4.6 | Modes de session `ask` (tout demander, même une lecture du shell), `reads` (défaut `tools.approval_mode`), `auto` (tout sans demande sauf destructif, politique imposée par un serveur MCP et commande que `may_destroy` ne peut juger) ; `/mode`, `session mode`, `session.mode` ; préparer un plan n'exige rien, le lancer si | #111, #186 | `daemon/approval_mode.rs`, `tools/shell.rs` (`may_destroy`) | `preparing_a_plan_needs_no_prior_approval_but_starting_does` (approval_mode.rs:225), `the_approval_mode_is_set_from_telegram` (telegram.rs:13174), `the_approval_mode_and_useless_rules_are_readable` (rpc.rs:2354), `shell_commands_are_classified_before_asking` (engine.rs:2178), `destructive_or_opaque_commands_are_spotted` (shell.rs) |
| C-4.7 | Demandes d'approbation (`ApprovalStore`, kinds `tool_call`, `mcp_sampling`, `mcp_elicitation`, `workflow_gate`, `plan_proposal`, `effect_unknown`, `skill_proposal`, `memory_proposal`, `config_change`, `budget_exceeded`, `mcp_admin`, chacun avec son gabarit) : la première décision gagne, expiration à 24 h qui reprend le tour en le disant au modèle, rappels à T+1 h et T+6 h avec boutons neufs dans la conversation d'origine, raison de refus gardée pour le modèle, marqueur de règle, demandes non urgentes regroupées pendant les heures calmes pour le digest, `effect_unknown` et `budget_exceeded` jamais silencés, cycle de vie sur le flux runtime | §9, #97 | `hitl/lib.rs` (`ApprovalKind`, `ApprovalState`, `Decision`, `due_reminders`), `daemon/supervisor.rs` | `ca_9_1_first_decision_wins`, `ca_9_2_expiry_blocks_then_can_resume`, `reminders_fire_at_one_and_six_hours`, `denial_reason_is_kept_for_the_model`, `always_records_a_rule_marker`, `quiet_requests_are_batched_for_the_digest`, `urgent_kinds_are_never_silenced`, `every_kind_maps_to_a_template`, `approval_lifecycle_is_in_the_runtime_log` (hitl/lib.rs), `pending_approvals_are_reminded_at_one_and_six_hours`, `an_expired_approval_resumes_its_turn` (supervisor.rs), `the_reminder_of_a_go_template_command_is_delivered` (telegram.rs) |
| C-4.8 | Double confirmation (`destructive_confirm`) pour un geste destructif ; `config_set` sur bac à sable, providers, Telegram, outils et politiques demande deux fois même avec une règle « Toujours » ; secrets et identité refusés | §14.5, 0.2.2 | `daemon/engine.rs`, `daemon/selfknow.rs`, `daemon/telegram.rs` | `config_set_asks_twice_for_sensitive_settings_even_with_an_always_rule` (engine.rs:2779), `config_set_guards` (selfknow.rs), `workflow_budget_and_destructive_cards_keep_the_topic` (telegram.rs:9922) |
| C-4.9 | Carte d'approbation : intention d'abord (`pourquoi` ou message du propriétaire), action exacte en bloc de code ou outil et valeurs sur une ligne, ligne de qualificatifs (réseau, sortie complète, répertoire, serveur MCP, classe, politique), alerte réseau, « Toujours » qui nomme sa portée (familles, réseau), aucun bouton à la place de « Toujours » quand aucune règle n'est possible (raison et sortie nommées), `rule_created = always` seulement si une règle a été écrite, valeurs secrètes masquées avec leur compte, carte non rendable envoyée en texte brut avec `telegram.card_degraded`, toute forme de commande (multi-lignes, heredoc, guillemet non fermé, vide, `{{…}}`) atteint sa carte sans panique | #116, #129, #130, #134, #141, #150 | `daemon/telegram.rs`, `tg/templates.rs` (`substitute` en un passage) | `an_approval_card_says_the_intention_first`, `an_mcp_approval_card_names_its_server_as_a_qualifier`, `a_command_with_go_templates_reaches_its_approval_card`, `every_command_shape_reaches_its_card_without_panic`, `the_always_button_says_when_no_rule_is_possible`, `stored_and_sent_secrets_are_masked_and_checked`, `an_unrenderable_card_is_sent_plain_and_recorded` (telegram.rs), `always_on_a_multiline_heredoc_resumes_without_panic`, `a_copied_key_is_stored_masked_and_executed_whole` (engine.rs) |
| C-4.10 | La carte, le clic, « Déjà tranché », la seconde confirmation et la carte de budget reviennent dans le chat et le sujet de la session ; destination persistée (`tg.approval_destination.<id>`) et relue après redémarrage ; repli sur l'origine du run puis la session ; raisons de refus isolées par sujet | #143, #165 | `daemon/telegram.rs` | `workflow_approval_confirmation_stays_in_the_cards_topic`, `workflow_budget_and_destructive_cards_keep_the_topic` (telegram.rs:9701, 9922) |
| C-4.11 | Vérification des arguments avant la politique et la carte (`precheck`), par `tool_call` compris, pour les outils natifs, MCP et les étapes `tool` ; un refus revient au modèle avec les paramètres attendus | #117 | `daemon/executor.rs` (`ToolExecutor::precheck`) | `invalid_calls_are_refused_before_any_approval_card` (engine.rs:1911), `invalid_clone_source_is_rejected_before_approval_even_via_tool_call` (executor.rs:2484), `an_invalid_mcp_call_is_explained_without_reaching_the_server` (executor.rs:3405) |
| C-4.12 | Règles inutiles signalées (`rule_note` : famille issue d'une commande composée ou d'une affectation, lecture déjà libre, jamais utilisée depuis une semaine) et retirables d'un bouton ; une famille créée est vérifiée à l'écriture (relue comme écrite, sinon pas de règle) | #111, #141 | `hitl/policy.rs`, `daemon/rpc.rs`, `daemon/telegram/screens.rs` | `the_approval_mode_and_useless_rules_are_readable` (rpc.rs) ; vérification à l'écriture : `always_after_a_cd_rules_the_real_command`, `an_and_list_gets_a_rule_per_family_and_stops_asking` (engine.rs) |
| C-4.13 | `approvals`, `approve <id> [--always] [--effect done\|retry]`, `deny <id> [--reason]`, `policies` en RPC et CLI ; `/approvals` renvoie une demande bloquée | 0.1.0, #83, #129 | `daemon/rpc.rs` | `approvals_flow_over_rpc` (rpc.rs:2297) ; renvoi par `/approvals` : aucun test |
| C-4.14 | Les règles « Toujours » d'un outil MCP sont révoquées quand sa description, son schéma ou ses annotations changent en silence ; propriétaire prévenu | #92 | `daemon/mcp.rs`, `mcp/registry.rs` (`fingerprint`) | `poisoned_or_changed_tools_are_flagged_and_lose_their_rules` (mcp.rs:2865) |
| C-4.15 | Un `cd <workspace> &&` en tête est relevé en `cwd` avant toute décision (garde de boucle, classement, carte, « Toujours », exécution), y compris dans les étapes de workflow et par `tool_call` ; hors workspace ou suivi d'un autre enchaînement, la ligne reste composée | #123 | `daemon/executor.rs` (`normalise_call`), `tools/shell.rs` (`split_cd_prefix`) | `a_cd_into_the_workspace_is_the_working_directory`, `always_after_a_cd_rules_the_real_command` (engine.rs), `a_cd_into_a_workspace_becomes_the_cwd` (executor.rs:3060), `a_single_cd_prefix_is_split_from_the_command` (shell.rs) |
| C-4.16 | Les demandes sans arguments (budget, effet incertain) ne créent jamais de règle ; leurs cartes n'affichent ni « Toujours » ni « Pour cette session » | #83 | `daemon/agent.rs` (`decide_approval`) | `a_request_without_arguments_never_creates_a_rule` (agent.rs:3141) |
| C-4.17 | `quiet` (heures calmes `telegram.quiet_hours`, `/quiet`) : réglage à chaud, plage à cheval sur minuit, contradiction signalée avec un déclencheur planifié | 0.1.0, #16 | `daemon/rpc.rs`, `kernel/config.rs` (`TimeRange`), `daemon/doctor.rs` | `quiet_hours_cross_midnight` (config.rs), `quiet_requests_are_batched_for_the_digest` (hitl/lib.rs) ; méthode `quiet` : aucun test dédié |

### 2.5 Outils natifs : fichiers, shell, HTTP, git, bac à sable

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-5.1 | Catalogue fixe et trié, classes conformes, requis imposés, schémas d'objet valides, outils réseau marqués, lectures idempotentes, outils de workflow cachés en conversation, table documentaire générée | §11, #104 | `tools/spec.rs` | `every_prd_tool_is_present`, `the_list_is_sorted_and_stable`, `no_duplicate_names`, `risk_classes_follow_the_prd_table`, `network_tools_are_marked_for_contamination`, `read_tools_are_idempotent`, `workflow_only_tools_are_hidden_outside_runs`, `every_schema_is_a_valid_object_schema`, `required_fields_are_enforced` (spec.rs), `every_native_tool_is_documented` (docs.rs), `workflow_only_tools_are_refused_in_chat` (executor.rs) |
| C-5.2 | Workspaces : `resolve` vérifie le chemin demandé et sa forme réelle (plus long préfixe existant canonicalisé) ; lien symbolique sortant refusé même pour un fichier à créer ; lien interne et workspace lié acceptés ; casse réelle du volume ; tilde refusé ; sans workspace tout est refusé ; racines relues à chaque appel (`config_set sandbox.workspaces` vaut dès l'appel suivant), racines d'un workflow stables, sous-agent restreint jamais élargi ; refus qui cite les racines vivantes ; défaut `default_workspaces` | #66, #163, #164, §2.5 | `tools/fs.rs` (`resolve`), `tools/shell.rs` (`default_workspaces`), `daemon/executor.rs` | `a_symlink_out_of_the_workspace_is_refused`, `resolve_rejects_paths_outside_the_workspace`, `resolve_uses_the_filesystems_case_rules`, `resolve_without_workspace_denies_everything` (fs.rs), `config_set_workspaces_applies_to_the_next_tool_call_and_turn`, `config_set_reports_canonical_and_missing_workspaces`, `a_workspace_refusal_lists_the_live_roots`, `files_are_written_edited_and_read_inside_the_workspace` (executor.rs), `ca_2_5_workspace_write_blocks_outside_writes` (security.rs), `workspace_paths_are_canonicalised_at_load_and_mutation` (config.rs) |
| C-5.3 | `fs_read` en flux (offset sauté sans le garder, total compté sous 8 Mio, saut au-delà de 64 Mio refusé, ligne tronquée à 64 Kio, binaire refusé, lignes numérotées) ; `fs_search` ligne à ligne (fichiers > 32 Mio ignorés et nommés, regex invalide refusée, glob) ; `fs_list` trié et borné ; longue liste en artefact avec résumé par dossier | #94, #8 | `tools/fs.rs` (`ReadCaps`, `read_with`, `search_with`, `matches_glob`) | `read_paginates_and_numbers_lines`, `the_head_of_a_big_file_is_read_without_scanning_it`, `a_far_offset_is_bounded`, `a_single_huge_line_is_truncated`, `search_skips_and_names_oversized_files`, `read_refuses_binary_files`, `list_is_sorted_and_bounded`, `search_finds_matches_with_line_numbers`, `search_rejects_invalid_regex`, `glob_matching` (fs.rs), `web_pages_and_long_listings_stay_small_in_context` (executor.rs) |
| C-5.4 | `fs_write` (diff rendu, LF, point de reprise git si le workspace est un dépôt) et `fs_edit` (portion unique exigée, portion absente signalée, `replace_all`), verrous par fichier | 0.1.0 | `tools/fs.rs` (`write`, `edit`, `unified_diff`, `FileLocks`), `tools/git.rs` (`checkpoint`) | `write_creates_then_reports_a_diff`, `edit_requires_a_unique_match`, `edit_reports_a_missing_portion`, `file_locks_serialise_writes_to_the_same_path`, `diff_handles_insertions_and_deletions`, `files_are_written_in_lf` (fs.rs), `checkpoint_is_none_outside_a_repo` (git.rs) |
| C-5.5 | `shell_exec` : commandes interdites et préfixes de privilège refusés (aussi dans un tube), environnement filtré sans jeton (`SHELL_EXTRA_ENV` : PATH, HOME, langue, agent SSH, emplacements de configuration), délai `shell_timeout` puis `SIGTERM` et `SIGKILL` sur le groupe (`kill -s SIG -- -pgid`), aucun orphelin, étiquette PID ramassée au démarrage suivant, sorties lues en continu sous plafond tête et queue sans couper l'UTF-8, annulation en moins de deux secondes, code de sortie et flux rendus, profil Seatbelt selon la configuration, `deny_read` effectif, réseau par appel, note réseau sur un échec qui y ressemble | #65, #68, #106, #57 | `tools/shell.rs` (`check_command`, `ExecOptions`, `truncate`, `profile_for`, `profile_with_denied_reads`, `looks_like_network_failure`, `NETWORK_OFF_NOTE`), `platform/process.rs` (`forbidden_commands`, `PRIVILEGE_PREFIXES`, `UnixProcessHost`) | `the_shell_env_never_carries_tokens`, `a_denied_path_is_unreadable_under_the_sandbox`, `a_network_failure_is_told_apart_from_a_command_failure`, `a_timed_out_command_leaves_no_process_behind`, `a_huge_output_is_capped_while_reading`, `a_cancelled_command_is_terminated_quickly`, `forbidden_commands_are_refused`, `ordinary_commands_pass`, `sudo_in_a_pipeline_is_caught`, `truncation_keeps_head_and_tail`, `short_output_is_untouched`, `truncation_never_splits_utf8`, `profiles_follow_the_configuration`, `timeout_is_enforced`, `exit_code_and_streams_are_reported` (shell.rs), `shell_network_is_granted_per_call_under_the_sandbox` (executor.rs, Seatbelt), `spawn_and_terminate_a_child`, `environment_is_filtered`, `reap_orphans_removes_stale_pid_files` (process.rs) |
| C-5.6 | Lectures du shell classées (`is_read_command`, programmes connus appelés par leur nom, sans option qui écrit) et parties sans demande en mode `reads` ; formes inhabituelles jamais paniquantes | #111, #130 | `tools/shell.rs` (`is_read_command`, `is_read_pipeline`), `hitl/cmdline.rs` | `read_commands_are_told_apart_from_the_rest`, `unusual_command_shapes_never_panic` (shell.rs), `read_tools_run_without_approval` (agent.rs:2709) |
| C-5.7 | Sorties de tests filtrées (`cargo`, `go`, `npm`, `pnpm`, `yarn`, Jest, Vitest, `pytest`, `make test` : résumé et échecs seulement, sortie complète en artefact), autre commande en échec longue : tête, queue et lignes d'erreur, sortie entière sous 60 lignes, `output: "full"` | #32, #130 | `tools/test_output.rs` (`Runner`, `digest`, `MIN_LINES`, `MAX_CHARS`) | `a_failing_output_of_any_length_is_digested_without_panic`, `test_runners_are_recognised`, `a_cargo_run_keeps_only_the_failures_and_the_summary`, `go_jest_and_pytest_failures_are_extracted`, `other_long_failures_keep_head_tail_and_error_lines` (test_output.rs), `a_failing_command_of_41_lines_is_returned_whole`, `a_test_run_is_digested_and_its_full_output_kept_as_an_artifact` (executor.rs) |
| C-5.8 | `http_fetch` : schémas `http` et `https` seulement, allowlist (sous-domaines, `tools.http_allowlist`), adresses privées, locales, IPv6 entre crochets et métadonnées (`METADATA_HOSTS`) refusées, chaque saut de redirection résolu une fois et revérifié, connexion épinglée sur les adresses vérifiées (rebinding DNS impossible), repli sur une seconde adresse vérifiée, corps lu par morceaux jusqu'à `max_bytes`, `Content-Length` au-delà de vingt fois la limite refusé, HTML rendu en texte lisible (brut en artefact), remarque vers une forge connectée | #64, #93, #156 | `tools/http.rs` (`check_url`, `is_blocked_ip`, `host_allowed`, `AddressGuard`), `tools/html.rs` (`to_text`, `looks_like_html`) | `the_connection_goes_to_the_checked_address`, `a_redirect_to_a_rebinding_name_is_refused`, `a_name_with_several_checked_addresses_falls_back`, `a_redirect_to_a_private_address_is_refused`, `a_redirect_outside_the_allowlist_is_refused`, `a_huge_body_is_read_up_to_the_cap_only`, `an_oversized_content_length_is_refused_before_reading`, `private_addresses_are_blocked`, `public_addresses_are_allowed`, `metadata_endpoints_are_refused`, `localhost_and_internal_are_refused`, `non_http_schemes_are_refused`, `allowlist_matches_subdomains`, `empty_allowlist_allows_public_hosts`, `host_allowed_is_exact_or_subdomain`, `resolution_check_rejects_private_targets` (http.rs), `a_page_becomes_readable_text`, `links_around_blocks_never_split_a_character`, `html_is_recognised_by_type_or_by_its_first_bytes` (html.rs), `ssrf_is_blocked_including_after_redirects` (security.rs), `a_fetch_to_a_connected_forge_is_remarked` (machine.rs) |
| C-5.9 | Outils git : références validées (pas d'option injectée), noms de branche slugifiés et bornés, `git_status`, `git_diff`, `git_commit` (message vide refusé, commit vide toléré), `git_branch`, `git_clone` (sources acceptées, chemin local refusé avant tout `git`, réutilisation d'un clone équivalent même imbriqué, `file://` explicite rapporte sa source, règle bornée à `$origin`) | 0.1.0, #160 | `tools/git.rs` (`normalize_clone_url`, `validate_ref`, `branch_name_for`) | `ref_validation_blocks_option_injection`, `branch_names_are_slugified_and_bounded`, `status_reports_branch_and_files`, `commit_then_diff`, `empty_commit_is_not_an_error`, `branch_creation_and_checkout`, `commit_refuses_an_empty_message`, `clone_sources_accept_remotes_and_expand_a_github_shortcut`, `clone_reuses_an_existing_workspace_repo_with_an_equivalent_origin`, `clone_searches_the_workspace_even_when_destination_is_nested`, `clone_rejects_local_paths_before_starting_git`, `an_explicit_file_url_clones_and_reports_the_actual_source` (git.rs), `git_refs_cannot_smuggle_options` (security.rs) |
| C-5.10 | `tool_call` vers un outil natif suit le chemin d'un appel direct (nom effectif, classe, politique, carte) ; `expected_args` bornés et lisibles ; noms proches sur nom inconnu ; `render` préfère le champ lisible | #104, #110 | `tools/lib.rs` (`validate_args`, `call_markup`, `expected_args`, `close_names`, `render`), `daemon/executor.rs` | `expected_arguments_are_readable_and_bounded`, `unknown_names_get_close_suggestions`, `arguments_are_validated_against_the_spec`, `render_prefers_the_readable_field`, `empty_readable_field_falls_back_to_json`, `outcome_carries_error_text_for_the_model` (tools/lib.rs), `mcp_meta_tools_are_read_only_but_calls_carry_the_target_risk`, `a_rare_native_tool_is_found_and_called_like_a_direct_one` (executor.rs) |
| C-5.11 | `schedule_move` (`to: "here"` ou `"private"`, sous approbation) ; `schedule_create` sous approbation ; `schedule_list` avec `destination` | #124, 0.2.5 | `daemon/executor.rs` | `schedule_move_sends_a_schedule_here_or_home` (executor.rs:3006) |
| C-5.12 | `session_metadata` (contrat des critères, `project`, `verification`) et `session_notes` passent sans approbation | #137, #167 | `daemon/executor.rs`, `kernel/session.rs` | `criteria_follow_their_contract` (executor.rs:2954), `metadata_criteria_flow`, `metadata_on_an_unknown_session_fails` (session.rs) |
| C-5.13 | `workflow_author` refuse un brouillon invalide en renvoyant à la section de `docs/workflows.md` | #34 | `daemon/executor.rs`, `daemon/selfdocs.rs` (`workflow_doc_for`) | `an_invalid_workflow_draft_points_to_its_documentation` (executor.rs:2579) |
| C-5.14 | `workflow_plan` : plan durable et révisable, gate « vas-y », `workflow_start` refusé depuis une conversation Telegram ; `workflow_start` attend le premier verdict (5 s) et signale `blocked` ou `failed` | #186, #154 | `daemon/executor.rs`, `wf/plan.rs` | `workflow_plan_is_durable_revisable_and_gates_telegram_start` (executor.rs:2638) ; attente du premier verdict : aucun test |
| C-5.15 | `history_expand_query` cherche dans toutes les sessions et cite titre et date | #1 | `daemon/executor.rs`, `context/store.rs` | `past_sessions_are_found_with_their_title_and_date` (executor.rs:3193) |
| C-5.16 | `time_now`, `send_message`, `send_file`, `ask_user` (appel direct en conversation), `artifact_read` par l'outil | 0.1.0 | `daemon/executor.rs` | `send_message` planifié : `a_repeated_final_answer_is_recognised` (scheduler.rs) ; `artifact_read` : `artifact_cursor_only_advances_over_returned_bytes` (store.rs) ; `time_now`, `send_file`, `ask_user` en conversation : aucun test |
| C-5.17 | Bac à sable Seatbelt : profils `readonly`, `workspace-write`, `mcp-stdio`, `full` (jamais imposé), refus par défaut, écriture au workspace et au temporaire, `deny_read` rendu après les autorisations et trousseau fermé (`mach-lookup com.apple.SecurityServer`), sockets Unix fermées même réseau ouvert (sauf DNS et agent SSH), trousseau ouvert seulement au profil qui le déclare, chemins liés autorisés sous leur forme réelle, guillemets échappés, profil passé en argument (`sandbox-exec -p`), aucun fichier écrit, échec fermé sur les autres systèmes, dossier `$TMPDIR/penelope-sandbox` nettoyé au démarrage | #68, #89, #90, #91, #106, #122 | `platform/sandbox.rs` (`Profile`, `seatbelt_profile`, `is_within`, `normalise`), `platform/backend/macos.rs` | `open_network_still_closes_unix_sockets`, `the_keychain_is_closed_to_every_enforced_profile`, `the_keychain_opens_only_for_a_profile_that_declares_it`, `denied_reads_come_after_the_allow_and_close_the_keychain`, `symlinked_workspaces_are_allowed_by_their_real_path`, `profile_kinds_roundtrip`, `full_profile_is_not_enforced`, `seatbelt_denies_by_default_and_allows_workspace`, `seatbelt_readonly_has_no_write_rule`, `seatbelt_escapes_quotes_in_paths`, `network_toggle`, `path_containment_rejects_traversal`, `normalise_resolves_dots` (sandbox.rs), `the_profile_goes_inline_and_no_file_is_written`, `seatbelt_enforces_denied_reads_on_this_mac`, `seatbelt_opens_the_keychain_only_when_declared_on_this_mac`, `seatbelt_closes_unix_sockets_on_this_mac` (macos.rs, les trois derniers ignorés), `sandbox_failure_is_closed_not_open` (security.rs) |
| C-5.18 | Effets typés par outil (`tool`, `mcp`, `shell`, `telegram`, `fs`, `git`, `http`) et serveur déduits du nom | §4.2 | `daemon/agent.rs`, `kernel/effects.rs` (`EffectKind`) | `effect_kinds_and_servers_are_derived_from_names` (agent.rs:4197) |
| C-5.19 | `image_inspect` sur une photo reçue (chemin dans le message) ou une capture du workspace | #125 | `daemon/executor.rs`, `daemon/vision.rs` | `an_element_is_located_on_a_screenshot` (engine.rs) |
| C-5.20 | Pièces jointes : photos sous `media/photos`, fichier non ingérable dans le premier workspace (`<workspace>/telegram/`) ou en artefact (texte), noms qui ne sortent pas de leur dossier, images reconnues par octets magiques et taille lue dans les en-têtes | 0.2.7 | `daemon/media.rs` | `image_sizes_are_read_from_headers`, `images_are_recognised_by_their_magic_bytes`, `attachment_names_cannot_escape_their_directory` (media.rs), `other_files_become_attachments_the_agent_can_reach` (telegram.rs) |

### 2.6 MCP : client, registre, OAuth, supervision, élicitation

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-6.1 | Cinq versions de protocole ; négociation `server/discover` (2026-07-28) puis `initialize` avec la version préférée puis la meilleure version commune annoncée par `-32022`, jamais de boucle ; une réponse `isError` ou silencieuse à la sonde retombe sur `initialize` ; deux serveurs de versions différentes derrière une URL ; capacités par version (sortie structurée, élicitation, icônes, élicitation URL, tâches, streamable HTTP, batching, journalisation par `_meta`, reprise de session) | §8.1, #9, 0.2.4 | `mcp/client.rs` (`connect`, `Negotiated`), `mcp/protocol.rs` (`ProtocolVersion`, `client_capabilities`, `client_info`, `request_meta`) | `negotiates_stateless_core`, `falls_back_to_initialize_and_takes_the_server_version`, `an_is_error_result_to_discover_falls_back_to_initialize`, `a_silent_discover_probe_on_stdio_falls_back_to_initialize`, `unsupported_version_picks_the_best_common`, `no_common_version_is_an_explicit_error`, `stateless_requests_carry_meta`, `initialized_notification_only_in_legacy_mode` (client.rs), `versions_are_ordered_and_parseable`, `capability_gates_follow_the_table`, `client_capabilities_grow_with_the_version`, `request_meta_carries_version_and_trace` (protocol.rs), `ca_8_1_every_version_and_transport_negotiates`, `ca_8_2_mixed_versions_behind_the_same_url`, `capabilities_and_extensions_are_stored` (mcp_conformance.rs), `a_server_confused_by_the_probe_is_retried_with_initialize` (mcp.rs) |
| C-6.2 | Transports `stdio` (groupe de processus propre, profil `mcp-stdio`, `ExitInfo` avec code ou signal, durée de vie et dernière ligne d'erreur), Streamable HTTP (session par en-tête, SSE lu au fil de l'eau pour les requêtes du serveur, en-têtes miroirs du corps en 2026 : `MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name` = nom d'outil, de prompt ou URI encodée `=?base64?…?=`, version négociée seule dès 2025-06-18, erreur JSON-RPC d'un 4xx conservée), SSE historique ; `auto` choisit par les champs ; l'attente du propriétaire ne compte pas dans le délai ; réponses appariées par identifiant, réponse orpheline ignorée | §8.2, #114, #126 | `mcp/transport.rs` | `mirrored_headers_follow_the_body`, `unsafe_header_values_use_the_base64_sentinel`, `a_json_rpc_error_in_a_4xx_keeps_its_code`, `a_server_that_exits_at_once_says_how`, `writing_to_a_dead_server_says_how_it_died`, `a_server_killed_by_a_signal_says_so`, `stderr_lines_are_kept_with_the_exit_code`, `pending_matches_responses_by_id`, `unmatched_response_is_dropped_without_panic`, `rpc_error_becomes_an_mcp_error`, `waiting_for_the_client_does_not_count_against_the_timeout`, `sse_server_requests_are_published_before_the_stream_ends`, `sse_events_are_split_on_blank_lines`, `sse_payload_extraction`, `loopback_records_calls_and_answers`, `loopback_publishes_notifications` (transport.rs), `a_2026_tool_call_mirrors_its_name_in_the_headers`, `a_header_mismatch_keeps_its_json_rpc_code`, `earlier_versions_send_only_the_negotiated_version` (mcp_conformance.rs), `a_stdio_server_dead_at_start_says_why` (mcp.rs), `real_mcp_server_handshake` (ignoré) |
| C-6.3 | Déclarations `mcp.d/*.toml` : validées (fichier invalide signalé, pas fatal), rechargées à chaud au démarrage et à chaque changement, réécrites par l'administration (`add`, `edit`, `rm`, `enable`, `disable`), secrets résolus dans `headers` et `env` seulement à l'usage | §8.7, 0.2.4 | `mcp/config.rs`, `daemon/mcp.rs` | tests de `config.rs` listés en 1.5, `changes_in_mcp_d_are_picked_up_live`, `administration_rewrites_the_declaration` (mcp.rs), `ca_8_7_adding_an_mcp_server_is_hot`, `mcp_server_files_are_loaded_from_disk` (hot_reload.rs), `mcp_servers_are_administered_over_rpc` (rpc.rs:1971) |
| C-6.4 | Superviseur : découverte au démarrage seulement si les outils sont inconnus ou la déclaration a changé, démarrage paresseux au premier appel, arrêt après `idle_timeout`, backoff exponentiel avec gigue ≤ 20 % jusqu'à 5 min, `failed` après 8 échecs jusqu'à `mcp restart`, éviction LRU des inactifs au-delà de `max_processes` (non paresseux jamais évincés), démarrage ardent selon la déclaration, quantiles de latence et taux d'erreur, connexion perdue comptée et reconnexion au prochain appel, orphelins tués au démarrage (`state/mcp-pids`), entretien `mcp.maintenance` supervisé | 0.2.4, #84 | `mcp/supervisor.rs` (`ServerState`, `Backoff`, `evict_lru`, `next_state`, `ServerMetrics`, `should_start_eagerly`), `daemon/mcp.rs` | `backoff_grows_then_caps`, `backoff_exhausts_after_eight_failures`, `jitter_stays_within_twenty_percent`, `lifecycle_transitions`, `usable_states`, `idle_servers_are_stopped_first`, `process_cap_evicts_least_recently_used`, `non_lazy_servers_are_never_evicted`, `metrics_quantiles`, `eager_start_rules` (supervisor.rs), `servers_are_discovered_and_their_tools_answer_calls`, `known_tools_do_not_start_a_lazy_server_at_boot`, `a_broken_server_backs_off_then_fails_until_restarted`, `a_lost_connection_is_counted_and_the_next_call_reconnects` (mcp.rs), `reap_orphans_removes_stale_pid_files` (process.rs) |
| C-6.5 | Registre paresseux : trois méta-outils, recherche FTS par mots-clés avec préfixes et repli, portée par serveur, remplacement d'un serveur qui retire ses anciens outils, ensemble collant borné promu à la compaction, schémas `eager` sous plafond, descriptions courtes tronquées, `describe` borné | §8.9 | `mcp/registry.rs` | `search_finds_tools_by_keyword`, `search_can_be_scoped_to_a_server`, `replacing_a_server_removes_its_old_tools`, `promotion_waits_for_the_compaction_boundary`, `sticky_set_is_bounded`, `oversized_schema_is_summarised`, `eager_schemas_respect_the_global_cap`, `fts_query_uses_prefix_or`, `short_form_truncates_long_descriptions`, `meta_tools_are_the_three_of_the_prd`, `qualified_names_are_normalised`, `long_names_are_truncated_with_a_hash`, `arguments_are_validated_before_the_call` (registry.rs), `the_model_reaches_mcp_tools_through_the_supervisor` (engine.rs:2864) |
| C-6.6 | Politique par outil : `tool_policy` et `tool_risk` de la déclaration, `mcp.policy` par classe, `deny` gagne toujours, `eager_schemas` | §8.10 | `daemon/mcp.rs`, `tools/lib.rs` | `tool_policy_and_eager_schemas_come_from_the_declaration` (mcp.rs:3006), `unknown_policy_override_is_rejected` (config.rs) |
| C-6.7 | Sortie structurée (`structuredContent`) validée contre `outputSchema` quand la version le permet, `isError` transmis tel quel au modèle, tout type de contenu lu (`text`, `image`, `audio`, `resource`), résultat `cacheable` lu en 2026, résultat sans type = complet | §8.4 | `mcp/client.rs`, `mcp/protocol.rs` (`ToolResult`, `ContentBlock`) | `structured_output_is_validated_against_the_schema`, `valid_structured_output_passes_through` (client.rs), `tool_result_parses_every_content_kind`, `is_error_is_preserved_for_self_correction`, `cacheable_result_is_read`, `missing_result_type_means_complete` (protocol.rs), `structured_output_is_validated_when_the_version_supports_it`, `execution_errors_come_back_to_the_model`, `cacheable_results_are_read_on_2026`, `unknown_tools_produce_a_clean_error` (mcp_conformance.rs) |
| C-6.8 | Primitives : outils paginés, ressources et gabarits, `resources/read`, abonnements (ou notifications en legacy), prompts, complétions, journalisation (`_meta` ou `logging/setLevel` selon la version), sonde de santé par version, `listen_changes` opt-in en 2026, annulation, `tasks/get` et `tasks/result`, primitive absente non fatale | §8.3 | `mcp/client.rs` | `tools_list_is_paginated`, `missing_primitive_is_not_a_failure`, `listen_changes_opts_in_on_2026`, `legacy_server_uses_notifications_not_subscribe`, `log_level_uses_meta_or_set_level_depending_on_version`, `health_probe_depends_on_version` (client.rs), `resources_and_templates_are_reachable`, `prompts_and_completions_follow_the_version`, `change_subscriptions_follow_the_version`, `logging_uses_meta_or_set_level`, `health_probe_works_on_every_version` (mcp_conformance.rs) |
| C-6.9 | Tâches MCP longues : table `mcp_tasks`, `poll_interval` croissant (2 s à 1 min), reprise au redémarrage (terminales non reprises), états de fournisseur tolérés, `mcp.task.completed`, disponibles dès 2025-11-25 | 0.4.0, §8.6 | `mcp/tasks.rs` (`TaskStore`), `daemon/tasks.rs` | `create_poll_and_complete`, `ca_8_6_tasks_survive_a_restart`, `terminal_tasks_are_not_recovered`, `state_parsing_accepts_provider_variants`, `poll_interval_grows_and_caps` (tasks.rs), `tasks_are_only_available_from_2025_11_25` (mcp_conformance.rs), `a_long_mcp_task_is_awaited_until_it_completes` (workflow.rs) |
| C-6.10 | OAuth 2.1 : `WWW-Authenticate` analysé, ressource protégée (RFC 9728) puis serveur d'autorisation (RFC 8414 et OpenID, deux documents essayés), enregistrement CIMD > pré-enregistré > DCR (RFC 7591), PKCE S256, `resource` (RFC 8707), `state`, `iss` vérifié (RFC 9207), consentement incrémental (portées fusionnées sans doublon), portées depuis la déclaration, le défi 401 puis `scopes_supported`, jetons rafraîchis avant expiration avec marge, réponse sans `access_token` en erreur, client confidentiel `client_secret_basic` ou `client_secret_post` selon les métadonnées (basic par défaut), secret jamais persisté et enregistré au rédacteur, endpoints en HTTPS ou boucle locale exacte, hôte de rappel configurable, demande valable dix minutes et à usage unique, `paste_back` (adresse collée, avec ou sans `http://`) ou serveur local `127.0.0.1:7777`, jetons dans le magasin, `mcp.auth_requested`, `mcp.authorized`, carte `mcp_oauth_required`, message qui nomme la commande quand aucun enregistrement n'est possible | 0.3.0, #6, #159, #174, #188 | `mcp/oauth.rs`, `daemon/mcp_auth.rs` | `pkce_is_s256_and_url_safe`, `www_authenticate_parsing`, `protected_resource_url_follows_rfc9728`, `discovery_tries_both_documents`, `authorize_url_carries_pkce_and_resource`, `callback_parsing_accepts_full_url_or_query`, `callback_validation_accepts_matching_state_and_issuer`, `invalid_issuer_is_refused`, `invalid_state_is_refused`, `provider_error_is_surfaced`, `registration_preference_order`, `dcr_body_picks_application_type`, `token_bodies_include_resource_indicator`, `confidential_client_method_follows_server_metadata`, `tokens_expiry_and_refresh_margin`, `token_response_without_access_token_is_an_error`, `incremental_scopes_merge_without_duplicates`, `as_metadata_requires_endpoints`, `cimd_document_is_self_describing` (oauth.rs), `authorization_code_flow_with_paste_back_and_refresh`, `confidential_client_uses_secret_post_for_exchange_and_refresh`, `confidential_client_uses_secret_basic_for_exchange_and_refresh`, `confidential_client_secret_is_registered_for_redaction`, `basic_authorization_value_is_redacted_even_in_a_generic_structured_field`, `endpoints_must_be_https_unless_local`, `scopes_fall_back_to_those_advertised_by_the_resource`, `a_server_without_registration_names_the_command_to_run`, `the_callback_host_is_configurable` (mcp_auth.rs), `ca_8_3_oauth_discovery_and_pkce`, `ca_8_3_invalid_issuer_is_rejected`, `ca_8_3_incremental_consent_and_registration`, `ca_8_3_paste_back_flow_over_telegram` (mcp_conformance.rs), `unauthorized_exposes_missing_scopes` (error.rs) |
| C-6.11 | Requêtes du serveur : `roots/list` (racines déclarées, jamais le home), `ping`, `sampling/createMessage` refusé et non annoncé, `elicitation/create` annoncée seulement si Telegram est configuré, `notifications/tools/list_changed` suivi, `_meta` 2026 (`io.modelcontextprotocol/clientCapabilities`, `clientInfo`, `logLevel`) | 0.2.4, #12 | `daemon/mcp.rs`, `mcp/protocol.rs` | `server_requests_and_list_changes_are_handled` (mcp.rs:2795), `refused_requests_are_not_announced` (protocol.rs) |
| C-6.12 | Élicitation : confirmation (Accepter, Refuser, Annuler), formulaire depuis `requestedSchema` (enums titrés `oneOf`/`anyOf`, multi-sélection, défauts), lien (domaine, adresse entière, alerte Punycode, ouverture après accord, fin par `notifications/elicitation/complete`), délai `elicitation_timeout` (délai de l'outil suspendu), rappel à mi-délai, annulation avec bouton « Relancer », requêtes invalides rejetées, carte dans la conversation d'origine (`Broker::scope`) sinon au foyer, sans canal la demande est annulée avec sa raison, MRTR `input_required` relancé avec `inputResponses` et `requestState` (quatre fois), `-32042` attend les liens puis retente une fois, le résultat dit au modèle qui a répondu | #12, #143 | `daemon/elicitation.rs`, `mcp/client.rs` (`retry_tool`) | `without_a_channel_the_request_is_cancelled_and_says_why`, `the_owner_answers_or_the_request_expires`, `an_accepted_link_waits_for_its_completion`, `invalid_requests_are_rejected`, `a_request_goes_back_to_the_calling_conversation`, `a_pending_request_is_reminded_once_then_cancelled` (elicitation.rs), `mcp_elicitation_is_answered_from_telegram`, `mcp_links_and_mrtr_elicitations_from_telegram` (telegram.rs), `mrtr_input_required_is_surfaced` (mcp_conformance.rs) |
| C-6.13 | Serveurs stdio confinés : `deny_read` comme le shell (répertoire de données et `roots` restent lisibles), trousseau fermé sauf `sandbox.allow_keychain_for` (effet au prochain appel sur un serveur déjà lancé), `allow_full_for` pour `sandbox_profile = "full"`, erreur qui parle du trousseau complétée par le réglage, `mcp list` colonne « trousseau », `mcp show` `keychain`, un vrai serveur sous Seatbelt en CI macOS | #89, #122 | `daemon/mcp.rs`, `platform/sandbox.rs` (`mcp_stdio`, `with_keychain`) | `a_stdio_server_cannot_read_what_the_shell_cannot`, `only_a_declared_server_reaches_the_keychain`, `a_keychain_failure_names_the_sandbox_not_the_secret`, `the_keychain_setting_takes_effect_on_a_running_server`, `a_server_keeps_its_own_directories_readable`, `a_real_stdio_server_runs_under_the_sandbox` (mcp.rs, le dernier macOS), `mcp_list_says_which_servers_reach_the_keychain` (cli commands.rs) |
| C-6.14 | `mcp test [nom\|--file]` : connexion, négociation, liste des outils, appel d'un outil en lecture sans argument requis (`list`, `get`, `whoami` d'abord) ; faute de protocole = échec, refus métier = note ; `-32020` en conversation passe le serveur en `degraded` et le dit au modèle ; `mcp logs` rend stderr puis la fin du processus (sortie vide dite) | #114, #126 | `daemon/mcp.rs` | `mcp_test_really_calls_a_read_tool` (mcp.rs:2161) ; `degraded` sur `-32020` et `mcp logs` : partiel |
| C-6.15 | Tool poisoning : descriptions et schémas rendus encadrés comme non fiables avec l'alerte du détecteur, description retirée d'un schéma `eager` suspect, `mcp_tool_suspicious` journalisé, carte d'approbation qui signale un outil suspect | #92 | `mcp/registry.rs` (`describe`, `flags`), `daemon/mcp.rs` | `fingerprints_and_exposed_descriptions` (registry.rs:1061), `poisoned_or_changed_tools_are_flagged_and_lose_their_rules` (mcp.rs) |
| C-6.16 | Administration : `mcp.*` en RPC, `penelope mcp …`, `/mcp` (détail, redémarrer, tester, journal, activer, autoriser), `doctor` nomme les pannes et corrections, `self_status` section `mcp`, une ligne par serveur dans le prompt | 0.2.4 | `daemon/rpc.rs`, `daemon/telegram/screens.rs`, `mcp/lib.rs` (`server_summary_line`) | `mcp_servers_are_administered_over_rpc` (rpc.rs), `mcp_servers_are_visible_and_restartable_from_telegram`, `an_mcp_server_is_restarted_from_its_menu` (telegram.rs), `doctor_names_what_is_broken_and_how_to_fix_it` (mcp.rs), `summary_line_has_no_schema` (mcp/lib.rs) |
| C-6.17 | Rappel quotidien des serveurs en attente d'autorisation (`mcp.oauth.notified.<nom>`) | 0.3.0 | `daemon/mcp_auth.rs`, `daemon/supervisor.rs` | aucun test |
| C-6.18 | Erreurs MCP classées (réessayables ou non), codes JSON-RPC mappés, portées manquantes exposées sur 401 | §8 | `mcp/error.rs` | `retry_classification`, `unauthorized_exposes_missing_scopes`, `rpc_codes_are_mapped` (error.rs) |
| C-6.19 | Client pré-enregistré Slack documenté (manifeste, `callback_host = localhost`, portées du manifeste), limites : `Mcp-Param-*` non couverts | #159 | `docs/mcp.md` | partiel : `the_callback_host_is_configurable`, `a_server_without_registration_names_the_command_to_run` (mcp_auth.rs) |

### 2.7 LLM : fournisseurs, routage, flux, machine d'état

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-7.1 | Trois fournisseurs (`openrouter`, `openai_compat` / `local` / `providers.extra`, `codex`) assemblés en `ProviderSet` par préfixe ; un modèle `codex:` n'est jamais servi par un autre ; préfixe inconnu refusé par son nom ; secrets résolus à la frontière ; secret manquant = erreur d'authentification lisible ; préférences de routage minimales par défaut | #142, 0.2.2 | `llm/lib.rs` (`build_providers`, `CodexAccess`), `llm/provider.rs` (`ProviderSet`), `kernel/config.rs` (`PROVIDER_PREFIXES`) | `provider_set_routes_by_prefix`, `providers_resolve_secrets_at_the_boundary`, `missing_secret_is_an_auth_error`, `routing_value_is_minimal_by_default` (lib.rs), `a_codex_model_never_falls_back_to_another_provider` (provider.rs), `an_unknown_provider_prefix_is_refused_by_name` (config.rs), `model_ids_default_to_openrouter` (telegram.rs) |
| C-7.2 | OpenRouter : corps avec `session_id` (routage collant, cache) et `models` (replis serveur, `llm.fallback_used`), préférences de fournisseur (`order`, `allow_fallbacks`, `data_collection`, `require_parameters`, `zdr`, `sort`, `only`, `ignore`, `quantizations`), fournisseur amont précédent épinglé en tête d'`order` pendant 10 min sauf ordre imposé (identifiant depuis `GET /models/{id}/endpoints`), en-têtes `HTTP-Referer`, `X-OpenRouter-Title`, `X-OpenRouter-Categories`, coût facturé `usage.cost` (BYOK compris) préféré au catalogue, `Retry-After` honoré une fois, erreurs typées par `error_type`, refus (`refusal`) non silencieux, fournisseur amont conservé, `reasoning` transmis (effort, `enabled: false`), embeddings, transcription avec coût, catalogue `GET /models` | 0.2.2, #17, #152 | `llm/provider.rs` (`OpenRouterProvider`, `endpoint_slugs`, `to_openai_body`), `llm/sse.rs`, `llm/types.rs` | `openrouter_body_carries_session_and_fallbacks`, `the_previous_upstream_is_pinned_unless_routing_is_configured`, `rate_limits_keep_the_retry_after_hint`, `billed_cost_wins_over_the_catalog`, `reasoning_effort_is_forwarded`, `openrouter_transcription_reports_its_cost`, `a_documented_stream_is_decoded_end_to_end`, `collect_stream_builds_a_response`, `collect_stream_propagates_errors` (provider.rs), `usage_chunk_carries_the_billed_cost`, `byok_cost_adds_the_upstream_inference`, `refusals_are_not_silent`, `documented_mid_stream_error_chunk_keeps_type_and_provider` (sse.rs), `openrouter_error_bodies_are_classified_by_their_canonical_type`, `mid_stream_errors_keep_their_type`, `retryable_classification`, `context_length_error_is_detected_whatever_the_status` (types.rs), `parses_openrouter_catalog` (catalog.rs) |
| C-7.3 | Décodeur SSE : événements découpés (CRLF, commentaires), caractère multi-octets coupé reconstitué à tout offset (octets invalides remplacés), deltas accumulés, appels d'outils fragmentés ou parallèles reconstitués et émis une fois, arguments invalides gardés bruts pour autocorrection, `reasoning_details` fusionnés dans l'ordre et lus une fois, `usage` avec cache et raisonnement, erreur mi-flux, images générées collectées, trames de debug inoffensives | #80, 0.2.2 | `llm/sse.rs` (`SseDecoder`, accumulateur) | `decoder_splits_events`, `decoder_handles_crlf_and_comments`, `a_character_split_at_any_byte_is_rebuilt`, `tool_arguments_survive_transport_cuts`, `random_packet_sizes_give_the_same_text`, `invalid_bytes_are_replaced_not_held`, `accumulates_text_deltas`, `reassembles_fragmented_tool_calls`, `reasoning_details_are_merged_by_index_in_order`, `openrouter_reasoning_is_read_once_not_twice`, `two_parallel_tool_calls_are_kept_apart`, `tool_calls_are_emitted_once`, `invalid_arguments_are_kept_raw_for_self_correction`, `usage_reads_cached_and_reasoning`, `mid_stream_error_is_surfaced`, `generated_images_are_collected`, `debug_and_usage_frames_with_empty_choices_are_harmless` (sse.rs) |
| C-7.4 | Flux muet coupé après `stream_idle_timeout` (tout octet remet le compteur), délai global 30 min, un flux lent qui parle n'est jamais coupé, annulation immédiate même pendant un silence, délai de connexion sur le client OpenAI-compatible | #51, 0.2.2 | `llm/provider.rs` (`DEFAULT_STREAM_IDLE`, `with_stream_idle`) | `a_mute_stream_is_cut_on_the_idle_timeout`, `a_slow_but_talking_stream_is_never_cut`, `cancelling_a_silent_stream_does_not_wait_for_the_next_byte`, `cancel_token_propagates` (provider.rs), `cancellation_stops_the_stream` (mock.rs) |
| C-7.5 | Endpoint OpenAI-compatible : `stream_options.include_usage`, fenêtre lue dans `GET /models` (`context_length`, `max_model_len`, `n_ctx`) sinon `context_window`, transcription multipart (whisper.cpp), synthèse `POST /audio/speech` (JSON, audio en retour), embeddings, `providers.extra.<nom>` | #53, #41, 0.2.2 | `llm/provider.rs` (`OpenAiCompatProvider`, `DEFAULT_LOCAL_WINDOW`, `parse_embeddings`) | `the_openai_body_asks_for_usage_in_the_stream`, `a_local_stream_ending_with_usage_feeds_the_token_count`, `a_local_model_window_comes_from_the_endpoint_or_the_configuration`, `local_whisper_transcription_uses_the_openai_multipart_form`, `local_speech_posts_json_and_returns_audio_bytes` (provider.rs) |
| C-7.6 | Corps OpenAI : contenu chaîne pour le texte, jamais de tableau vide, images en parties, arguments d'outils sérialisés en chaîne, résultat d'outil avec `id` et `name`, `cache_control`, raisonnement seulement avec les appels d'outil | 0.2.2, #17 | `llm/provider.rs` (`to_openai_body`) | `empty_content_is_never_an_empty_array`, `body_uses_string_content_for_plain_text`, `images_use_the_parts_form`, `tool_calls_are_serialised_with_string_arguments`, `tool_result_message_carries_id_and_name`, `cache_marker_emits_cache_control`, `reasoning_goes_back_only_with_tool_calls` (provider.rs) |
| C-7.7 | Codex : dialecte Responses (items `message`, `function_call`, `function_call_output`, `reasoning`, `store: false`, `include: ["reasoning.encrypted_content"]`, `prompt_cache_key` = session), raisonnement chiffré réinjecté, même identité sur toutes les requêtes (`originator`, `User-Agent`, `x-codex-installation-id`), 0 $ connu (`estimated = false`), 401 rafraîchi puis rejoué une fois, quota lu dans les en-têtes et l'événement `codex.rate_limits`, retrait avant l'appel, 429 `usage_limit_reached` avec heure de retour, `usage_not_included`, `context_length_exceeded`, fermeture sans `response.completed` = coupure, catalogue avec fenêtres et outils sans prix, repli sur la liste embarquée | #142, décision 0010 | `llm/codex.rs` (`CodexProvider`, `TokenSource`) | `every_request_carries_the_same_identity`, `a_subscription_call_costs_nothing_and_says_so`, `a_401_is_refreshed_once_and_replayed_once`, `a_spent_quota_steps_aside_before_calling`, `the_body_speaks_the_responses_dialect`, `encrypted_reasoning_and_tool_results_go_back`, `the_stream_yields_text_a_tool_call_and_usage`, `a_stream_cut_before_completion_is_an_error`, `failures_carry_their_cause`, `a_quota_429_says_when_to_come_back`, `quota_is_read_from_headers_and_events`, `the_catalog_has_windows_tools_and_no_price` (codex.rs) |
| C-7.8 | Connexion Codex par code d'appareil (`model auth codex`, `--status`, `--logout`, `/model auth codex`), jetons sous `codex.oauth` (jamais `~/.codex/auth.json`), `refresh_token` rotatif à usage unique sérialisé par verrou et écrit avant usage, réutilisation = déconnexion définitive et propriétaire prévenu une fois, rafraîchi avant expiration ou après huit jours, jetons masqués, révocation si le rangement échoue, un seul compte à la fois, `providers.codex.enabled` activé à la connexion | #142, #148 | `daemon/codex_auth.rs`, `daemon/rpc.rs` | `the_device_code_flow_stores_a_grant`, `a_missing_device_code_says_what_to_do`, `refresh_is_json_rotates_once_and_a_reuse_disconnects`, `a_reused_refresh_token_disconnects_for_good`, `tokens_carry_the_account_the_plan_and_their_expiry`, `refresh_keeps_or_rotates_the_refresh_token`, `a_token_is_refreshed_before_it_dies_or_after_eight_days` (codex_auth.rs), `only_one_chatgpt_account_at_a_time`, `doctor_reports_the_codex_provider` (rpc.rs) |
| C-7.9 | Routeur : décision déterministe (trivial, image, modèle épinglé, modèle ou rôle d'une étape), classifieur (`CLASSIFIER_PROMPT`, schéma validé, raisonnement réduit, sortie structurée), paliers `low`, `medium`, `high`, collant jusqu'à une frontière, chaîne de repli, escalade d'un cran (bouton « Modèle supérieur »), validation d'alias (modèle inconnu du catalogue accepté, préfixe inconnu refusé), `supports_tools` depuis le catalogue, classifieur coupé = défaut | §10.3, décision 0009 | `llm/router.rs` (`Router`, `RouteInput`, `Decision`, `RouteReason`, `Complexity`) | `trivial_messages_skip_the_classifier`, `image_attachment_routes_to_vision`, `image_request_routes_to_image_model`, `only_an_explicit_image_request_goes_to_the_image_model`, `step_model_wins_over_everything`, `step_role_selects_its_alias`, `a_pinned_model_beats_sticky_and_classifier_but_not_images`, `ca_10_1_sticky_model_survives_until_a_boundary`, `ca_10_2_high_complexity_routes_to_reasoning`, `classifier_output_is_schema_validated`, `ca_10_3_fallback_chain_is_used_on_transient_failure`, `escalation_goes_one_rung_up_only`, `alias_validation_rejects_unknown_models`, `tool_support_follows_the_catalog`, `classifier_disabled_falls_back_to_default` (router.rs) |
| C-7.10 | Catalogue de modèles : chargé au démarrage puis toutes les 6 h, `upsert` sans effacer, prix du cache au dixième par défaut, coût avec remise sur le cache, effort de raisonnement le plus léger selon les capacités (facultatif = coupé, obligatoire = le plus faible, modèle inconnu supposé réfléchir), recherche sans préfixe, liste filtrée et triée, catalogue malformé sans panique, `model list` et `/models` montrent classifieur, étages et replis | 0.1.0, 0.2.2, #152 | `llm/catalog.rs` (`Catalog`, `lightest_reasoning_effort`) | `parses_openrouter_catalog`, `missing_cache_price_falls_back_to_a_tenth`, `cost_discounts_cached_tokens`, `lightest_reasoning_effort_respects_the_model_capabilities`, `catalog_lookup_strips_provider_prefix`, `provider_prefix_parsing`, `list_filters_and_sorts`, `upsert_keeps_existing_entries`, `malformed_catalog_yields_nothing_instead_of_panicking` (catalog.rs), `model_list_shows_the_routing_in_force` (cli commands.rs), `routing_and_costs_are_readable_from_telegram` (telegram.rs), `the_catalog_lists_the_configured_models` (live, ignoré) |
| C-7.11 | Machine d'état des requêtes LLM (`planned`, `dispatching`, `response_started`, `completed`, `failed`, `send_unknown`), transitions CAS ordonnées, crash avant les en-têtes = `send_unknown`, après = échec, politique de rejeu par fournisseur (`RetryMarkDuplicate` ou `AskHuman`), corps haché indépendamment de l'ordre, double planification dans un processus refusée | §17.4 | `llm/state.rs` (`LlmStateMachine`, `UnknownSendPolicy`) | `nominal_sequence`, `body_hash_is_order_independent`, `cas_refuses_out_of_order_transitions`, `crash_before_headers_gives_send_unknown`, `crash_after_headers_is_a_failure_not_an_unknown`, `retry_policy_follows_the_provider` (state.rs), `ca_17_4_llm_calls_are_classified_by_where_the_crash_happened`, `double_planning_inside_one_process_is_rejected` (resilience.rs) |
| C-7.12 | Un modèle qui n'appelle pas d'outils (catalogue) est refusé pour un alias servant un rôle à outils (`model set` explique, `doctor` signale) ; rôles de service et rôles d'image acceptés ; pas d'émulation d'outils ; parseur JSON tolérant conservé | décision 0009, #125 | `llm/router.rs` (`supports_tools`), `daemon/rpc.rs`, `daemon/doctor.rs` (`alias_needs_tools`), `llm/json_scan.rs` | `a_model_without_tool_calling_is_refused_for_a_conversation_alias` (rpc.rs:1766), `image_roles_do_not_need_tool_calling` (doctor.rs), `reads_a_fenced_block`, `tolerates_an_unclosed_fence`, `tolerates_trailing_commas`, `braces_inside_strings_do_not_break_balance`, `reads_an_object_surrounded_by_prose`, `plain_text_has_no_object` (json_scan.rs) |
| C-7.13 | `model set` prévient quand un alias de consolidation ou de relecture reçoit un modèle qui impose de réfléchir ; `model set` signale un identifiant absent du catalogue (`known: false`) | #152 | `daemon/rpc.rs`, `daemon/doctor.rs` (`alias_serves_extraction`) | partiel : `doctor_reports_the_reasoning_share_of_the_consolidation` (doctor.rs) ; avertissement de `model set` : aucun test |
| C-7.14 | Types : rôles, contenu multi-blocs, détection d'image, alias de `finish_reason`, sérialisation stable, total d'usage, `Transcription`, `StreamChunk`, `LlmErrorKind` | §10 | `llm/types.rs` | `message_text_concatenates_blocks`, `image_detection`, `finish_reason_aliases`, `serialisation_is_stable`, `usage_total` (types.rs) |
| C-7.15 | Fournisseur simulé scripté (fragments, appels d'outils, dépassement de contexte, sortie mesurée et coupée `Scripted::Written`, annulation) pour toute la suite | 0.1.0, #140 | `llm/mock.rs` | `mock_streams_text_in_fragments`, `mock_emits_tool_calls`, `mock_reports_context_overflow`, `cancellation_stops_the_stream` (mock.rs) |
| C-7.16 | Suite réseau OpenRouter : flux avec usage et coût, outil à arguments structurés, raisonnement exposé, image générée, catalogue | 0.4.0 | `evals/tests/live_openrouter.rs` | `streaming_answers_with_usage_and_cost`, `a_tool_is_called_with_structured_arguments`, `reasoning_is_exposed_when_requested`, `an_image_is_generated`, `the_catalog_lists_the_configured_models` (ignorés) |
| C-7.17 | `model.route_test` : simulation du routage pour un message | 0.1.0 | `daemon/rpc.rs` | aucun test dédié (méthode servie, couverte par `every_declared_method_is_either_served_or_explicitly_absent`) |
| C-7.18 | Transcription vocale : rôle `stt` (OpenRouter ou serveur local), coût compté au rôle, `TRANSCRIPTION_MAX_BYTES` 25 Mio, sans fournisseur local actif la réponse dit quoi configurer | 0.2.2 | `llm/provider.rs` (`transcribe`), `daemon/telegram.rs` | `local_whisper_transcription_uses_the_openai_multipart_form`, `openrouter_transcription_reports_its_cost` (provider.rs), `a_voice_note_is_transcribed_quoted_then_answered`, `a_voice_note_without_local_stt_explains_what_to_configure`, `audio_filenames_carry_a_format_servers_understand` (telegram.rs) |

### 2.8 Telegram

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-8.1 | Long polling `getUpdates` (`poll_timeout_s`, `allowed_updates` déclarés), offset et updates persistés (`tg_updates`), chaque `update_id` traité une seule fois même après un crash, payload vidé après traitement | §14, #46 | `daemon/telegram.rs` (boucle `telegram.poll`), `tg/api.rs` (`get_updates`, `allowed_updates`) | `the_same_update_is_processed_only_once` (telegram.rs:9056), `get_updates_declares_allowed_updates` (api.rs), `replayed_updates_do_not_duplicate_turns` (resilience.rs), `a_replayed_update_is_still_deduplicated_without_its_payload` (purge.rs) |
| C-8.2 | Propriétaire unique (`owner.telegram_user_id`) : tout autre expéditeur, message ou clic est `Unauthorized` sans réponse ; groupes ouverts par identifiant (`allowed_chats`, relu à chaud), administrateur anonyme du groupe listé accepté, hors liste ignoré en silence mais journalisé et gardé pour `doctor` (vingt au plus) | §14, #113 | `tg/lib.rs` (`Access`, `classify`, `ANONYMOUS_ADMIN_ID`), `daemon/telegram.rs` | `ca_14_5_unauthorized_users_are_rejected`, `allowed_chats_open_a_group_and_accept_the_anonymous_admin`, `groups_are_refused_unless_listed` (tg/lib.rs), `a_topic_group_listed_by_id_accepts_the_anonymous_admin`, `strangers_get_no_answer_at_all` (telegram.rs), `telegram_refuses_anyone_but_the_owner` (security.rs), `ca_14_5_non_owner_clicks_are_refused` (actions.rs) |
| C-8.3 | Classification des updates : texte, commande (`/cmd args`, `/cmd@bot`), photo, album (`media_group`), document, vocal ou audio, message transféré, message édité, `/stop`, retour OAuth collé, sujet (`message_thread_id`), clic de bouton ; type inconnu ignoré sans erreur | §14 | `tg/lib.rs` (`Incoming`, `parse_command`, `looks_like_oauth_callback`) | `text_messages_are_classified`, `commands_are_parsed`, `command_updates_are_routed`, `photos_albums_documents_and_voice`, `forwarded_messages_are_flagged`, `pasted_oauth_url_is_detected`, `edited_messages_and_stop_are_recognised`, `topic_is_carried`, `unknown_update_kinds_are_ignored_not_fatal` (tg/lib.rs) |
| C-8.4 | Rendu : Markdown en arbre de blocs (pulldown-cmark) projeté en blocs riches (Bot API 10.x) et en HTML de repli par la même traversée ; HTML du modèle échappé, liens gardés seulement s'ils sont sûrs, tableaux en blocs monospace, citations longues dépliables, balises équilibrées ; repli HTML sur refus de capacité (jamais sur erreur de transport) puis texte brut ; découpe à 4 096 sans casser un bloc de code (clôture réécrite, langue reprise), très longue ligne tronçonnée, document au-delà de `max_fragments`, pied de page coût et durée | §14.2, #145 | `tg/render.rs` (`to_blocks`, `to_html`, `blocks_to_json`, `split_message`, `should_send_as_document`, `footer`), `tg/html.rs`, `tg/api.rs` (`should_fall_back_to_html`, `RenderMode`) | `markdown_becomes_typed_blocks`, `tables_are_parsed_and_compacted`, `html_fallback_escapes_and_preserves_structure`, `long_quotes_are_expandable`, `blocks_json_shape`, `ca_14_6_splitting_never_breaks_a_code_block`, `short_text_is_not_split`, `a_single_very_long_line_is_chunked`, `document_threshold`, `buttons_serialise_with_callback_or_url`, `footer_reports_cost_and_duration`, `inline_code_survives_in_paragraphs` (render.rs), `inline_formatting_survives`, `links_are_kept_only_when_safe`, `code_blocks_are_escaped_and_tagged`, `raw_html_from_the_model_is_displayed_not_interpreted`, `lists_headings_and_quotes_render`, `tables_become_monospaced_blocks`, `every_tag_is_balanced`, `plain_fallback_strips_tags_and_unescapes` (html.rs), `html_fallback_is_triggered_by_capability_errors` (api.rs), `html_rejected_by_telegram_falls_back_to_plain_text`, `long_answers_are_split_in_order`, `a_text_message_gets_an_html_answer_as_a_reply`, `values_render_compactly` (telegram.rs), `long_messages_are_split_under_the_api_limit` (live, ignoré) |
| C-8.5 | Gabarits (voir 1.3) : catalogue complet, rendu dans les deux formes, variables déclarées obligatoires, substitution en un seul passage, boutons connus, désactivés gardés, URL substituées, surcharge et rechargement à chaud, gabarit cassé signalé sans perdre les autres | §14.5, #129 | `tg/templates.rs` | `the_catalog_is_complete`, `ca_14_1_every_template_renders_in_both_forms`, `values_are_never_read_as_variables`, `missing_variable_is_an_error_not_a_hole`, `undeclared_variable_is_rejected_at_validation`, `unknown_action_or_style_is_rejected`, `disabled_buttons_are_kept_in_place`, `url_buttons_substitute_variables`, `hot_reload_overrides_builtins`, `a_broken_template_is_reported_and_the_others_survive`, `placeholders_are_extracted_once`, `every_button_action_is_known` (templates.rs), `templates_reload_from_disk` (hot_reload.rs), `workflow_known_lists_native_tools_and_templates` (runtime.rs) |
| C-8.6 | Boutons à jetons (voir 1.3) : idempotence, autorisation, expiration et purge, multi-usage, arguments transportés, pied « décidé par » | §14.5 | `tg/actions.rs` (`ActionStore`, `ClickOutcome`, `decided_footer`) | `tokens_fit_in_callback_data`, `ca_14_4_double_click_is_idempotent`, `multi_use_tokens_can_be_clicked_repeatedly`, `ca_14_5_non_owner_clicks_are_refused`, `expired_tokens_are_refused_and_purged`, `unknown_token_is_reported`, `args_survive_the_roundtrip`, `decided_footer_names_the_channel`, `action_kinds_are_unique` (actions.rs) |
| C-8.7 | Formulaires depuis un JSON Schema : champs typés (texte, nombre, booléen, enum titré ou non, multi-sélection), requis dans l'ordre puis optionnels alphabétiques, défauts préremplis, un champ par écran avec progression, coercition et messages d'erreur, requis non sautables, navigation précédent et `goto`, récapitulatif, soumission validée contre le schéma d'origine, refus, dernière réponse clôt ; un formulaire vit dans son sujet (clé `tg.form.<chat>.<sujet>`), ne lit que son sujet, deux formulaires en parallèle, invites et erreurs dans le sujet, formulaire ouvert depuis plus d'une heure signalé par `doctor` | 0.4.0, #149 | `tg/forms.rs` (`fields_from_schema`, `FormState`), `daemon/telegram.rs` | `schema_compiles_to_typed_fields`, `enum_without_titles_falls_back_to_values`, `titled_enums_use_const_and_title`, `multi_select_arrays`, `defaults_are_prefilled`, `one_field_per_screen_with_progress`, `coercion_and_error_messages`, `required_fields_cannot_be_skipped`, `navigation_back_and_goto`, `submit_validates_against_the_original_schema`, `summary_lists_every_field`, `decline_marks_the_form_done`, `buttons_are_offered_for_enums_and_booleans`, `schema_without_properties_is_refused`, `the_last_answer_marks_the_form_done` (forms.rs), `a_form_lives_in_its_topic_and_swallows_nothing_from_another`, `a_workflow_form_is_filled_field_by_field` (telegram.rs), `open_forms_check` : partiel |
| C-8.8 | Limites de débit : `429` avec `retry_after` attendu puis rejoué, seau par chat (`rate_per_chat_per_s`), seau séparé pour les aperçus (brouillon, réaction, « écrit… »), backoff croissant plafonné sur 5xx, 4xx non rejoués, erreurs de transport sans le jeton du bot, un brouillon en vol à la fois avec le dernier texte, réponse finale jamais retardée par les brouillons | §14.3, #26, #70 | `tg/api.rs` (`RateLimiter`, `Bot::call`, `backoff_ms`), `daemon/telegram.rs` (boucle `telegram.drafts`) | `ca_14_3_rate_limit_is_respected_without_loss`, `rate_limiter_spaces_messages_per_chat`, `previews_do_not_queue_in_front_of_messages`, `server_errors_are_retried_with_backoff`, `client_errors_are_not_retried`, `transport_errors_never_carry_the_token`, `backoff_grows_and_caps`, `send_text_builds_the_expected_body`, `inline_keyboard_shape` (api.rs), `drafts_are_coalesced_and_never_delay_the_answer` (telegram.rs:8390), `draft_ids_are_stable_and_non_zero` (bus.rs), `retry_classification`, `fatal_chat_errors` (tg/error.rs) |
| C-8.9 | File d'envoi durable `tg_outbox` (boucle `telegram.outbox`) : ordre par chat (un envoi en attente retient les suivants du même chat, les autres passent), erreur de transport isolée reprise tout de suite, refus définitif dit une fois en texte brut, `outbox_failed` dans `/status`, cartes et retours de commandes par la file, textes rédigés à l'écriture, re-rédaction une fois au démarrage hors transaction par paquets de cent | §14, #101, #134, #148, #153 | `daemon/telegram.rs` (`outbox_loop`), `daemon/purge.rs` (`reredact_outbox`) | `a_failed_fragment_holds_the_rest_of_its_chat`, `a_single_transport_error_is_retried_at_once`, `a_definitive_refusal_is_told_once`, `stored_and_sent_secrets_are_masked_and_checked` (telegram.rs), `messages_already_queued_are_redacted_again` (purge.rs:560) |
| C-8.10 | Brouillons `sendMessageDraft` (Bot API 9.3+, `draft_id` entier, `rich_message.markdown`), réactions « reçu », signes de vie `sendChatAction` toutes les 4 s dans le sujet pendant tout le tour (dès la mise en file, action selon l'outil, appels jetables), ligne d'état de l'outil dans le brouillon privé | 0.1.0, #121 | `daemon/telegram.rs` (`start_activity`), `tg/api.rs` (`send_draft`, `set_reaction`, `reaction`) | `the_activity_indicator_lives_as_long_as_the_turn` (telegram.rs:13124), `reactions_use_the_prd_emojis` (api.rs) |
| C-8.11 | Rafales : morceau à la limite (≥ 4 000 caractères) ou message transféré ouvre une fenêtre `text_group_window_ms`, message court seul part tout de suite, message court pendant une rafale la ferme après 300 ms, morceaux recollés dans l'ordre en un tour, messages éloignés restent deux tours ; au-delà de `burst_messages` ou `burst_chars` (même pendant un tour) carte de choix (un document, ingérer sans répondre, un par un, tout annuler) sans appel au modèle | #49, #96, #161 | `daemon/telegram.rs`, `daemon/runner.rs` | `pieces_of_one_paste_become_a_single_turn`, `a_short_message_goes_at_once_a_split_piece_waits`, `two_messages_far_apart_stay_two_turns`, `a_burst_asks_before_answering_and_can_be_ingested` (telegram.rs), `six_pending_telegram_messages_show_a_card_without_calling_the_model`, `messages_arriving_during_tools_hit_the_burst_limit_before_another_model_call` (runner.rs) |
| C-8.12 | `/stop` : arrête le tour, vide la file de la session (nombre annoncé), annule les lignes absorbées, nomme les runs ouverts et compte les ingestions ; `/stop tout` vide aussi les autres sessions du chat et des sous-agents, met les runs `running` en pause, nomme les `blocked` et `paused` sans les annuler, annule les ingestions, ouvre l'écran des runs (⏸ ▶️ ⏹) ; « Rien à arrêter » impossible avec un run ouvert | #49, #57, #155 | `daemon/telegram.rs` (`StopReport::render`) | `stop_empties_the_queue_and_says_how_many`, `an_open_run_is_never_nothing_to_stop`, `stop_promises_only_what_it_did` (telegram.rs) |
| C-8.13 | Sessions dans un chat : une seule au focus écrit ; `/new`, `/fork`, `/switch`, menu `/sessions` lient explicitement (migration 0005) ; une session quittée finit son tour et ses tours en file en fond, sorties retenues derrière une notification silencieuse « 📬 N réponses et M approbations en attente » avec « Basculer » qui livre tout dans l'ordre (approbation déjà tranchée non renvoyée) ; `/sessions` : un bouton par session (▶️ focus, ⏳N en file, 📁 sujet, heure), sous-menu Basculer, Forker, Renommer, Fermer, douze par page, fermées masquées sauf `all` ; `/switch` par identifiant, préfixe unique ou titre ; `/new` sur une session qui travaille demande « Garder en fond » ou « Fermer (N tours perdus) » ; une session en fond s'arrête seule à son plafond ; après un fork seul le fork répond | #10, #14, #112 | `daemon/telegram.rs`, `daemon/engine.rs` (`bind_chat`) | `sessions_menu_switches_with_a_click`, `background_sessions_hold_their_replies_until_switched_back`, `a_left_session_keeps_working_and_answers_on_return`, `new_says_what_it_would_lose_before_closing`, `after_a_fork_only_the_fork_answers`, `new_starts_a_fresh_session_for_the_chat` (telegram.rs), `telegram_chats_get_their_own_bound_session` (engine.rs) |
| C-8.14 | Sujets de forum : une session par sujet (plusieurs chantiers en parallèle), sujets fixes et sujets de run (`TopicStore`, nommage), sujets fermés non réutilisés, en-têtes de remplacement sans sujets, foyer `/home` et `/home off` (`telegram.home`) pour les avis sans session (budget, rappels, digest, veille, OAuth MCP, élicitations), `doctor` réclame un foyer dès qu'un groupe est autorisé | #113, #143 | `tg/topics.rs`, `daemon/telegram.rs` (`home_chat`) | `fixed_topics_are_resolved`, `run_topics_are_scoped_by_reference`, `closed_topics_are_not_reused`, `without_topics_everything_goes_to_the_single_chat`, `topic_names_follow_the_prd`, `headers_replace_topics_when_disabled` (topics.rs), `notices_without_a_session_go_to_the_home_topic` (telegram.rs:12531) ; création réelle d'un sujet (`create_topic`) : aucun test |
| C-8.15 | Écrans de commandes : toute commande sans argument ouvre un écran, pagination, confirmation des gestes risqués, clic acquitté avant l'opération lente, résultat livré en tâche de fond, aucun `null`, `/help` par familles puis un bouton par commande, `/wf` prépare un plan, `/runs` et `/resume` avec ⏸ ▶️ ⏹ 🔎, `/schedules` (⚡, ⏸/▶️, 📍, 🗑), `/mcp` (détail, 🔄, 🧪, 📜, ⏻, 🔐), `/models` (modèle puis alias, 🔎), `/projet`, `/mode`, `/skills` et `/skill` (📖, ⏪), `/oublie` et `/forget`, `/appris` et `/pratique` (voir, ✅, 🚫), `/intentions` et `/policies` (❌, 🗑, ⚠️ règle inutile), `/status` et `/doctor` (boutons vers l'écran de chaque alerte), `/config` et `/logs` (générations, sous-systèmes, journal filtré, « Plus »), `/restart`, `/close`, `/rewind` (confirmation), `/purge`, `/fork` (↪️), `/upgrade` (⬆️, ⏪, carte de bascule), `/quiet`, `/secret` (🗑), `/p` (serveurs puis prompts, arguments par formulaire), `/retiens`, `/recall`, `/note`, `/title` (✏️ copie) | #30, #73, #115 | `daemon/telegram/screens.rs` | `every_catalog_command_without_arguments_opens_a_screen`, `a_slow_button_is_acknowledged_immediately`, `no_bubble_ever_shows_null`, `an_absent_value_is_shown_as_a_question_mark`, `a_schedule_is_deleted_after_confirmation`, `schedules_are_listed_paused_and_run_from_telegram`, `a_workflow_menu_starts_a_plan_conversation`, `mcp_servers_are_visible_and_restartable_from_telegram`, `an_mcp_server_is_restarted_from_its_menu`, `sessions_menu_switches_with_a_click`, `the_digest_links_to_screens_once_the_bot_is_known` (telegram.rs) ; écrans `/p`, `/secret`, `/logs`, `/config`, `/quiet`, `/intentions`, `/policies`, `/skill` : partiel (ouverture testée, action non) |
| C-8.16 | Une commande répond sans appeler le modèle ; un update dont le traitement échoue est signalé au propriétaire en réponse au message fautif ; un long message d'échec est tronqué à 500 caractères avec renvoi au journal ; `/export` d'une session inconnue répond « aucune session ne correspond » | #71, #72, #148 | `daemon/telegram.rs` | `commands_answer_without_calling_the_model`, `a_failing_command_says_so_to_the_owner`, `a_long_failure_is_cut_and_points_at_the_log`, `exporting_an_unknown_session_says_so` (telegram.rs) |
| C-8.17 | Médias reçus : vocal téléchargé, transcrit (rôle `stt`, jusqu'à trois minutes, détaché de la boucle), cité puis tour normal, format de fichier audio compris des serveurs ; photo montrée au modèle s'il lit les images sinon décrite par `vision` et jointe, album regroupé sur 1,5 s en un message avec les chemins ; document (20 Mo, détaché) ingéré, proposé et répondu, légende `/mien` ou question ; autres fichiers en artefact ou dans `<workspace>/telegram/` ; `/export` et `/audit` détachés | 0.2.2, 0.2.7, #69, #98 | `daemon/telegram.rs`, `daemon/media.rs` | `a_voice_note_is_transcribed_quoted_then_answered`, `a_voice_note_without_local_stt_explains_what_to_configure`, `audio_filenames_carry_a_format_servers_understand`, `a_slow_voice_note_does_not_block_the_next_update`, `a_slow_document_does_not_block_the_next_update`, `a_photo_is_described_for_a_model_that_cannot_see`, `a_multimodal_model_sees_the_album_in_one_turn`, `a_document_is_ingested_proposed_and_answered`, `mien_marks_a_document_as_written_by_the_owner`, `other_files_become_attachments_the_agent_can_reach` (telegram.rs) |
| C-8.18 | Retour OAuth collé (`code=` et `state=`, avec ou sans schéma, `127.0.0.1:7777/…`) termine l'autorisation et ne part jamais vers le modèle | #6 | `tg/lib.rs` (`looks_like_oauth_callback`), `daemon/telegram.rs` | `pasted_oauth_url_is_detected` (tg/lib.rs), `ca_8_3_paste_back_flow_over_telegram` (mcp_conformance.rs) |
| C-8.19 | Boutons de fin de tour : « 🔁 Réessayer » sur échec (même conversation, message non renvoyé), « ▶️ Continuer (24 appels de plus) » au plafond d'appels (préfixe `CALLS_EXHAUSTED`) | #5, #139 | `daemon/telegram.rs` | `a_failed_turn_offers_a_retry_button`, `a_turn_out_of_calls_offers_to_continue` (telegram.rs) |
| C-8.20 | Budget sur Telegram : `/budget` (jour, session, contexte, requêtes les plus chères, quota codex), `/budget session <montant\|off>`, carte « continuer ? » (+5 $, +20 $, Arrêter) qui reprend le tour suspendu, `/usage` (tokens, cache, coût, `turn`, `model`, `miss`, `day`) | #20, #32, #142 | `daemon/telegram.rs` | `a_session_budget_is_raised_and_the_suspended_turn_resumes`, `routing_and_costs_are_readable_from_telegram` (telegram.rs) |
| C-8.21 | Cartes d'approbation cliquables reprennent le tour ; refus avec raison (`deny_reason`) transmis au modèle | 0.1.0 | `daemon/telegram.rs` | `an_approval_card_click_resumes_the_turn`, `a_refusal_with_a_reason_reaches_the_model` (telegram.rs) |
| C-8.22 | `/model` : boutons qui épinglent un alias sur la session, « Automatique », `/model auto on\|off`, dit quand le dernier message a été reclassé à une frontière ; `/models` filtrable ; `/model auth codex` | 0.2.2, #82, #142 | `daemon/telegram.rs` | `model_buttons_pin_the_session_then_give_it_back_to_the_router`, `routing_and_costs_are_readable_from_telegram` (telegram.rs) ; `/model auth codex` depuis Telegram : aucun test |
| C-8.23 | `/mode` règle le mode d'approbation de la session ; `/projet` le sujet de travail | #111, #119 | `daemon/telegram.rs` | `the_approval_mode_is_set_from_telegram` (telegram.rs), `the_injected_memory_follows_the_session_subject` (conversation.rs) ; `/projet` depuis Telegram : partiel |
| C-8.24 | `/compact` résume la session du chat et rend compte | 0.2.6 | `daemon/telegram.rs` | `compact_summarises_the_chat_session_and_reports_back` (telegram.rs:11388) |
| C-8.25 | Prompt planifié : réponse dans le chat ou le sujet d'origine même après `/new`, alerte avec « Relancer maintenant » et « Voir la planification » sur échec | #39 | `daemon/telegram.rs`, `daemon/scheduler.rs` | `a_scheduled_prompt_answers_after_new_and_warns_on_failure` (telegram.rs:11720) |
| C-8.26 | Accueil `/accueil` : questions à boutons, écriture du profil après validation, proposé une fois sur un profil vide | #21 | `daemon/telegram.rs`, `daemon/onboarding.rs` | `onboarding_writes_the_profile_from_the_answers`, `an_empty_profile_proposes_onboarding_once` (telegram.rs) |
| C-8.27 | Élicitation MCP depuis Telegram (confirmation, formulaire, lien, MRTR) | #12 | `daemon/telegram.rs`, `daemon/elicitation.rs` | `mcp_elicitation_is_answered_from_telegram`, `mcp_links_and_mrtr_elicitations_from_telegram` (telegram.rs) |
| C-8.28 | Workflows depuis Telegram : `/wf` et `/run` ouvrent une conversation de plan (même avec des paramètres), carte de plan avec « Vas-y » qui persiste l'approbation sans lancer de run, bouton d'une ancienne version rejeté, carte de lancement qui ne contourne pas le gate, `/run` sans paramètres devient une conversation avec un bouton de formulaire, questions d'étape `user` à boutons et saisie typée, formulaires d'étape, carte de progression éditée en place, `/runs`, `/resume` | 0.2.8, #35, #186 | `daemon/telegram.rs`, `daemon/workflow.rs` | `a_workflow_menu_starts_a_plan_conversation`, `run_without_parameters_turns_into_a_conversation`, `a_workflow_launch_card_cannot_bypass_the_plan_gate`, `plan_go_button_persists_approval_without_starting_a_run`, `a_workflow_question_uses_telegram_buttons_and_typed_input`, `a_workflow_form_is_filled_field_by_field` (telegram.rs), `a_brief_reaches_the_first_agent_step_and_the_progress_card` (workflow.rs) |
| C-8.29 | Digest planifié livré une fois ; liens profonds vers les écrans | #30, #145 | `daemon/telegram.rs` | `a_scheduled_digest_is_delivered_once`, `the_digest_links_to_screens_once_the_bot_is_known` (telegram.rs) |
| C-8.30 | Vocaux sortants (`send_voice`) et repli texte | #41 | `daemon/telegram.rs` | `a_spoken_answer_arrives_as_a_voice_note`, `a_failed_synthesis_falls_back_to_text` (telegram.rs) |
| C-8.31 | Effet incertain poussé au propriétaire dès que la passerelle est prête, puis décidé par carte | #83 | `daemon/telegram.rs` | `an_uncertain_effect_is_pushed_then_decided_from_telegram` (telegram.rs:8204) |
| C-8.32 | Mock Bot API : formes réalistes, updates drainés une fois, réponses scriptées, mêmes contrats que le transport réel ; suite réseau Telegram (envoi, édition, réaction, document, découpe) | §14, 0.4.0 | `tg/mock.rs`, `evals/tests/live_telegram.rs` | `mock_answers_with_realistic_shapes`, `updates_are_drained_once`, `update_factories_produce_valid_shapes`, `scripted_replies_take_precedence` (mock.rs), `the_bot_sends_edits_reacts_and_cleans_up`, `long_messages_are_split_under_the_api_limit` (ignorés) |
| C-8.33 | `/schedules ici <id>` et bouton 📍 déplacent une planification vers le sujet courant ; `/schedules pause\|resume\|rm\|run` ; `/mcp restart` depuis le menu | #124 | `daemon/telegram.rs` | `a_schedule_is_moved_to_the_topic_it_is_asked_from`, `schedules_are_listed_paused_and_run_from_telegram`, `a_schedule_is_deleted_after_confirmation` (telegram.rs) |
| C-8.34 | `telegram.rich_messages` : envoi des blocs riches natifs (`send_rich`) au lieu de HTML | 0.1.0 | `tg/api.rs` (`send_rich`) | partiel : `ca_14_1_every_template_renders_in_both_forms` (rendu), envoi réel non testé hors suite réseau |
| C-8.35 | Une commande Telegram longue (`/export`, `/audit`, vocal, photo, document) est traitée dans une tâche à part : `/stop`, les boutons et les autres conversations ne font pas la queue derrière | #69, #98 | `daemon/telegram.rs` | `a_slow_voice_note_does_not_block_the_next_update`, `a_slow_document_does_not_block_the_next_update` (telegram.rs) ; `/export` et `/audit` détachés : partiel |

### 2.9 Workflows

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-9.1 | Validation (sans daemon) : identifiant égal au nom du fichier, identifiants uniques, `goto` connu, `entryStep` présent, `$done` atteignable, `always` en dernier (sauf choix exhaustifs d'une étape `user`), enfants de `parallel` restreints, outil, modèle et gabarit connus (outil MCP indisponible = avertissement), variable de gabarit inconnue = erreur (dynamiques acceptées), commande manquante pour une plateforme déclarée, budget obligatoire, sous-workflow auto-référent = cycle, clé de métadonnées jamais écrite = avertissement, chemins d'erreur (JSON Pointer), `wait` sans déclencheur refusé, sous-groupe quitté seulement par une transition taguée | §12.3, 0.4.0 | `wf/validate.rs`, `wf/model.rs` | `a_valid_workflow_passes`, `id_must_equal_the_file_stem`, `duplicate_ids_are_rejected`, `unknown_goto_is_rejected`, `missing_entry_step`, `done_must_be_reachable`, `always_must_be_last`, `user_steps_may_omit_always_if_choices_are_exhaustive`, `parallel_children_are_restricted`, `unknown_tool_model_or_template`, `unavailable_mcp_tool_is_only_a_warning`, `unknown_template_variable_is_an_error`, `dynamic_variables_are_accepted`, `missing_command_for_declared_platform`, `budget_is_mandatory`, `self_referencing_subworkflow_is_a_cycle`, `metadata_key_never_written_is_a_warning`, `issue_paths_point_to_the_offender`, `wait_steps_need_a_trigger`, `a_subgroup_is_left_only_through_a_tagged_transition` (validate.rs), `json_roundtrip`, `unknown_fields_are_rejected`, `os_specific_commands`, `platform_gating`, `step_results_roundtrip`, `defaults_follow_the_prd`, `graph_rendering_lists_steps_and_transitions`, `transitions_helpers` (model.rs), `workflow_validate_works_without_a_daemon` (cli), `workflow_validation_reports_paths` (rpc.rs) |
| C-9.2 | Registre : livrés puis utilisateur (surcharge par identifiant), fichier invalide rejeté sans remplacer la version précédente, JSON cassé signalé, rechargement à chaud, filtrage par plateforme, `write` refuse un brouillon invalide, génération | §12.3, décision 0005 | `wf/registry.rs` | `bundled_workflows_are_valid`, `user_files_override_bundled`, `ca_12_3_invalid_file_is_rejected_and_previous_stays`, `broken_json_is_reported_not_fatal`, `id_must_match_the_file_name`, `platform_filtering`, `write_refuses_invalid_drafts` (registry.rs), `ca_12_5_workflows_reload_and_reject_without_losing_the_previous_version` (hot_reload.rs), `bundled_workflows_are_listed` (rpc.rs) |
| C-9.3 | Runs : états (`running`, `paused`, `blocked`, `done`, `cancelled`, …), `advance` transactionnel (résultat, étape, sorties, journal d'étapes), reprise à l'étape courante sans rejouer le compteur d'itérations, `resume` d'un run bloqué, contrôle (`pause`, `resume`, `cancel`, `retry-step`, `skip-step` et `goto` sous approbation, `answer`, `budget`), trace de chaque étape, admission `parallel` / `hold` / `coalesce` / `drop`, limites (itérations, dollars, tokens facturés, durée), workspaces expirés listés, workspaces des runs en pause inventoriés | §12.7, §12.8, #136 | `wf/runs.rs` (`RunStore`, `RunState`, `Control`, `Admission`, `check_limits`) | `create_advance_and_finish`, `blocked_runs_can_be_resumed`, `ca_12_2_runs_are_recovered_at_their_current_step`, `control_operations`, `control_parsing_and_approval`, `trace_records_every_step`, `admission_policies`, `expired_workspaces_are_listed`, `paused_workspaces_are_in_capacity_inventory`, `limits_are_checked` (runs.rs), `ca_17_5_a_run_resumes_at_its_current_step` (resilience.rs), `loops_stop_at_the_iteration_limit_and_control_works` (workflow.rs) |
| C-9.4 | Neuf types d'étapes exécutés par le pilote : `agent` (session du run, outils de workflow, relances bornées jusqu'à `step_done`, `nudgePrompt`), `sub_agent` (contexte neuf, outils en lecture par défaut, `outputSchema` validé en deux tentatives), `shell` (ledger, bac à sable, `network`, commande par OS, valeurs citées), `tool` (natif ou MCP, arguments validés, approbation puis exécution unique), `user` (choix à boutons, `text`, `form:<id>`), `parallel` (concurrence bornée, agrégat `success` / `partial` / `failure`), `workflow` (profondeur `max_depth`, espace du parent), `wait` (délai, événement, cron, `mcp_task`), `verify` (contrôles puis vérificateur, statuts des critères) ; une étape qui attend rend la main ; marqueurs `wf.<quoi>.<run>.<étape>.<itération>` en `kv` | 0.2.8, 0.4.0 | `daemon/workflow.rs` | `waits_parallel_children_and_sub_workflows_compose`, `a_question_waits_for_the_owner_then_follows_the_choice`, `an_agent_step_ends_with_step_done_after_a_nudge`, `a_tool_step_waits_for_approval_and_runs_once`, `shell_steps_are_replayed_from_the_ledger_after_a_crash`, `a_long_mcp_task_is_awaited_until_it_completes`, `the_orchestrator_starts_runs_for_tools_and_schedules` (workflow.rs) |
| C-9.5 | Conditions et transitions : neuf types, ordre déclaré, `$blocked` par défaut, vacuité vraie, type inconnu faux, chemins JSON avec tableaux et préfixe `$.`, `unmet_items` qui nomme ce qui retient une boucle, choix d'une étape `user` = résultat d'étape ; substitution de gabarits (variables inconnues vides avec avertissement, placeholder non terminé laissé, détection des variables dynamiques), valeurs citées dans `command` (apostrophe échappée, jamais deux fois, `quote: false`) | §12.4, #137, #154 | `wf/conditions.rs` | `unmet_items_name_what_holds_a_loop`, `always_and_step_result`, `user_choices_are_step_results`, `metadata_all_in_with_criteria`, `empty_lists_satisfy_all_conditions`, `output_match_equals_in_regex`, `combinators`, `unknown_condition_kinds_are_false_not_fatal`, `transitions_are_evaluated_in_order_and_default_to_blocked`, `json_path_handles_arrays_and_prefixes`, `a_path_with_a_space_stays_one_argument`, `quoting_is_never_doubled_and_survives_an_apostrophe`, `template_substitution`, `unknown_variables_become_empty_with_a_warning`, `unterminated_placeholder_is_left_as_is`, `dynamic_variable_detection` (conditions.rs) |
| C-9.6 | `timeoutMs` : jeton enfant par étape, par enfant de `parallel` et par vérification ; l'étape rend `timeout`, suit sa transition, le run continue, les frères vont au bout, pas de `retry` sur `timeout` ; l'attente entre deux tentatives écoute la pause | #56 | `daemon/workflow.rs` | `a_step_that_times_out_records_its_result_and_moves_on`, `a_timed_out_child_does_not_cancel_its_siblings` (workflow.rs) |
| C-9.7 | Paramètres vérifiés avant tout démarrage (obligatoires, défauts, inconnus refusés) ; workspace éphémère `{state}/runs/<run>` nettoyé après la rétention ou persistant ; sous-workflow dans l'espace du parent, nettoyage jamais avant lui | 0.2.8, 0.4.0 | `daemon/workflow.rs`, `wf/runs.rs` | `parameters_are_checked_before_anything_starts` (workflow.rs:3457), `expired_workspaces_are_listed` (runs.rs) |
| C-9.8 | Brief de conversation transmis au run (`{{brief}}`, avant la consigne de la première étape `agent` ou `sub_agent`, sur la carte de progression) ; carte de lancement avec paramètres complétés | #35 | `daemon/workflow.rs`, `daemon/executor.rs` | `a_brief_reaches_the_first_agent_step_and_the_progress_card` (workflow.rs:3611), `a_conversation_launches_ticket_to_deploy_with_its_brief` (ticket_to_deploy_e2e.rs:491) |
| C-9.9 | Plan révisable (`workflow_plan`) : versions, corrections, retour arrière, gate « Vas-y », approuvé immuable, survit à la réouverture du store, corrections concurrentes refusées après approbation, un plan en revue ne peut être écarté par un autre, nouveau plan archive l'ancien approuvé ; l'exécution du plan approuvé n'est pas branchée (T3 de #185) | #186 | `wf/plan.rs`, `daemon/executor.rs` | `execution_waits_for_the_explicit_go_gate`, `revisions_are_unbounded_and_preserve_their_history`, `restoring_an_old_revision_creates_a_new_version`, `an_approved_plan_is_immutable_and_serializable`, `a_stale_button_or_revision_cannot_approve_another_version`, `a_plan_survives_reopening_the_store`, `concurrent_corrections_cannot_overwrite_an_approved_version`, `another_plan_cannot_discard_one_still_in_review` (plan.rs), `workflow_plan_is_durable_revisable_and_gates_telegram_start` (executor.rs) |
| C-9.10 | Budget de run : `maxTokens` compte l'entrée hors cache plus la sortie, `maxUsd` d'après le ledger, relèvement par `wf control <run> budget` (`workflow.budget_raised`), `resume` sur un run encore au-dessus répond « toujours bloqué », carte « continuer ? » sur un run et reprise après relèvement | #136, #32 | `daemon/workflow.rs`, `wf/runs.rs` | `a_run_budget_counts_billed_tokens_and_can_be_raised` (workflow.rs:3382), `limits_are_checked` (runs.rs) |
| C-9.11 | Workflows livrés : `build-verify` (plan qui déclare `project`, boucle jusqu'aux critères cochés, `verify` avec `project_tests` ou commande déduite, contrat `verification` avec preuves liées au SHA, distinction `prerequisite_missing`, `evidence_missing`, `stale_evidence`, `test_failed`, `criterion_failed`, règles du dépôt au vérificateur), `review` (lint, tests, relecture en parallèle, quatre enfants), `ticket-to-deploy` (tracker et forge par `tool_search`/`tool_call`, paramètre `tracker`, `create_pr`, tests et lint par `make` puis écosystème, réseau déclaré où il faut, commandes pour les deux familles d'OS, e2e avec redémarrage du daemon entre chaque passage : un seul push, une PR, un commentaire, un déploiement), `deploy-generic` (`.penelope/deploy.toml` exigé, `make deploy`, `make smoke`, `make rollback`, décision 0007) | 0.4.0, 0.13.0, #137, #167 | `wf/bundled.rs` (`TEST_COMMAND`, `LINT_COMMAND`), `daemon/workflow.rs`, `daemon/ticket_to_deploy_e2e.rs` | `all_bundled_workflows_validate`, `ticket_to_deploy_follows_the_reference_table`, `the_verify_step_runs_four_children_in_parallel`, `build_verify_loops_until_criteria_are_met`, `deploy_generic_has_a_rollback_path`, `network_is_declared_only_where_the_command_lives_on_it`, `shell_steps_declare_commands_for_both_families`, `every_bundled_workflow_can_reach_done` (bundled.rs), `build_verify_reaches_verify_once_its_criteria_are_ticked`, `verification_contract_overrides_the_planned_test_command`, `verification_distinguishes_missing_tool_from_red_tests`, `verification_rejects_evidence_from_another_commit`, `verifier_receives_repository_release_rules_and_conditional_criteria`, `build_verify_reports_an_unavailable_test_tool_without_a_verdict` (workflow.rs), `ca_12_1_ticket_to_deploy_runs_end_to_end_and_survives_restarts` (ticket_to_deploy_e2e.rs:267) |
| C-9.12 | Workspaces des runs vivants (`running`, `paused`, `blocked`) mesurés une fois par jour sous `state/runs`, avertissement et `workflow.workspace_large` dès 1 Gio, liens non suivis, rien d'effacé | #177 | `daemon/supervisor.rs`, `wf/runs.rs` | `workspace_measurement_stays_inside_its_root`, `paused_large_workspace_is_reported_without_deletion` (supervisor.rs), `paused_workspaces_are_in_capacity_inventory` (runs.rs) |
| C-9.13 | Sous-groupes : sortie tracée `workflow.subgroup_exited` (groupe, tag, étapes) ; `workflow.step` nomme les critères qui retiennent une boucle | 0.4.0, #137 | `daemon/workflow.rs` | partiel : `a_subgroup_is_left_only_through_a_tagged_transition` (validation), `unmet_items_name_what_holds_a_loop` (conditions) ; événement `subgroup_exited` à l'exécution : aucun test |
| C-9.14 | Une approbation d'outil d'un run envoie sa carte comme en conversation et réveille le run ; une décision de budget aussi ; approbations et sous-agents d'un run restent dans le chat et le sujet d'origine | 0.2.8, #165 | `daemon/workflow.rs`, `daemon/telegram.rs` | `a_tool_step_waits_for_approval_and_runs_once` (workflow.rs), `workflow_approval_confirmation_stays_in_the_cards_topic` (telegram.rs) |
| C-9.15 | Schéma JSON publié conforme au validateur et aux workflows livrés | 0.4.0 | `schemas/workflow.schema.json` | `every_bundled_workflow_satisfies_the_published_schema`, `the_schema_rejects_what_the_validator_rejects`, `the_schema_accepts_the_shapes_the_prd_describes`, `the_schema_itself_is_well_formed` (workflow_schema.rs) |

### 2.10 Planification

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-10.1 | Déclencheurs `cron` (cinq champs, fuseau du propriétaire, tir unique `once`), `interval`, `mcp_poll` (outil en lecture, `item_path`, `id_path`, filtre, amorçage sans tir, une notification par élément nouveau, coalescence des `notify`), `watch_file`, `event` (événement du journal, fenêtre bornée, historique ignoré) ; cibles `notify` (sans modèle), `prompt`, `workflow` ; validation des spécifications ; `next_after` ; `pause`, `resume`, `mark_run` ; un déclencheur `event` ou `watch_file` n'a pas d'horaire | 0.2.5, §12.9 | `wf/schedules.rs` (`TriggerKind`, `TargetKind`, `Schedule`, `ScheduleStore`, `extract_items`, `passes_filter`, `coalesce`), `kernel/cron.rs` | `cron_schedules_compute_their_next_run`, `due_returns_only_ripe_schedules`, `validation_rejects_bad_specs`, `items_are_extracted_with_stable_fingerprints`, `nested_item_paths_and_filters`, `ca_12_4_poll_fires_once_per_item`, `seeding_marks_existing_items_without_firing`, `coalescing_groups_notifications_only`, `pause_and_resume`, `mark_run_schedules_the_next_occurrence`, `event_and_watch_triggers_have_no_schedule` (schedules.rs), `parses_prd_defaults`, `rejects_bad_expressions`, `dreaming_cron_fires_at_local_0330`, `steps_and_lists`, `dom_and_dow_are_or_when_both_restricted`, `impossible_date_returns_none`, `next_is_strictly_after`, `matches_ms_agrees_with_next` (cron.rs) |
| C-10.2 | Ordonnanceur toutes les 10 s : intentions échues expirées, schedules dus, `mcp_poll`, `watch_file`, `event` ; rappel daté tiré une fois puis terminé ; fichier surveillé ; événements internes tirent une fois ; `schedule.fired` après un déclenchement réussi | 0.2.5, #162 | `daemon/scheduler.rs` | `a_dated_reminder_fires_once_then_is_done`, `mcp_poll_seeds_then_notifies_new_items_only`, `a_watched_file_fires_when_it_changes`, `internal_events_fire_their_schedules_once` (scheduler.rs) ; `schedule.fired` sur le flux : aucun test dédié |
| C-10.3 | Un prompt planifié ouvre sa propre session (`scheduled`, titrée d'après la planification et le jour, `origin_session` en référence, migration 0009), répond dans le chat ou le sujet d'origine, survit à la fermeture de sa conversation ; tour de genre `trigger` | #39 | `daemon/scheduler.rs`, `kernel/turn.rs` (`TurnKind::Trigger`) | `a_recurring_prompt_survives_the_closing_of_its_conversation` (scheduler.rs:1369), `schedule_session_ids_become_informative_references` (migrations.rs), `a_scheduled_prompt_answers_after_new_and_warns_on_failure` (telegram.rs) |
| C-10.4 | Jamais de silence : exécution annulée, échouée ou au budget alertée avec « Relancer maintenant » et « Voir la planification » ; `runs` et `last_run` ne comptent qu'une exécution menée à terme, sinon `last_error` (visible `/schedules`, `doctor`) ; déclenchement manuel non dédoublonné avec le passage prévu ; planification identique à une active signalée à la création | #39 | `daemon/scheduler.rs` | `a_cancelled_or_failed_scheduled_prompt_warns_the_owner`, `a_duplicate_schedule_is_reported_at_creation` (scheduler.rs) |
| C-10.5 | Livrable déclaré (`livrable` : `message`, `fichier:<chemin>`, `run`) et état consommé (`etat`, gardé avant le tour, 2 Mio, remis si rien n'est livré) : une exécution sans livrable est un échec (« exécutée sans livrable ») ; le digest liste les échecs | #120 | `daemon/scheduler.rs` (`trigger_outcome_of`) | `a_silent_scheduled_run_is_a_failure_and_its_state_is_restored` (scheduler.rs:1264) |
| C-10.6 | La réponse finale est le livrable et part une fois : non répétée si `send_message` a déjà envoyé le même contenu (`schedule.final_not_repeated`), message intermédiaire différent ajouté, `send_message` en échec ne retient rien, `send_message` vise l'origine du tour | #133 | `daemon/scheduler.rs` | `a_repeated_final_answer_is_recognised` (scheduler.rs:1539) |
| C-10.7 | Destination en mots (`destination` : conversation privée, sujet et groupe, « (par défaut) ») dans `schedule list`, `/schedules`, `schedule_list` ; déplacement sans recréer (`schedule.move`, `schedule move --chat --topic \| --private`, `/schedules ici`, 📍, `schedule_move`) vers le propriétaire ou une conversation autorisée seulement ; digest avec l'agenda du jour | #124 | `daemon/scheduler.rs`, `daemon/rpc.rs` | `a_schedule_says_where_it_delivers_and_can_be_moved` (scheduler.rs:1448), `a_schedule_is_moved_and_listed_with_its_destination` (cli commands.rs), `a_schedule_is_moved_to_the_topic_it_is_asked_from` (telegram.rs), `schedule_move_sends_a_schedule_here_or_home` (executor.rs) |
| C-10.8 | Tir manqué pendant un arrêt rattrapé une fois au redémarrage | 0.2.5 | `daemon/scheduler.rs` | aucun test |
| C-10.9 | Crons système : `memory.dreaming_cron` (consolidation), `memory.digest_cron` (digest), `backup.cron` (sauvegarde nocturne, échec sur Telegram) ; contradiction signalée avec `quiet_hours` | 0.2.9, #42 | `daemon/dream.rs`, `daemon/backup.rs`, `daemon/supervisor.rs` | `dreaming_cron_fires_at_local_0330` (cron.rs), `a_scheduled_digest_is_delivered_once` (telegram.rs) ; déclenchement de la sauvegarde par `backup.cron` : aucun test |
| C-10.10 | `schedule_create` sous approbation (carte `schedule_preview`), `schedule.add` en RPC/CLI avec `--dedup`, `schedule.run_now` hors calendrier, `schedule.rm`, `schedule.pause`, `schedule.resume` | 0.2.5 | `daemon/rpc.rs`, `daemon/executor.rs` | `schedules_are_listed_paused_and_run_from_telegram`, `a_schedule_is_deleted_after_confirmation` (telegram.rs), `a_duplicate_schedule_is_reported_at_creation` (scheduler.rs) ; `schedule_create` par l'outil avec carte : partiel |
| C-10.11 | Intentions échues expirées par l'ordonnanceur ; intention datée redirigée vers un déclencheur | 0.2.5 | `daemon/scheduler.rs`, `memory/intents.rs` | `expiry_marks_intents_expired` (intents.rs), `a_dated_intent_is_redirected_to_a_schedule` (executor.rs) |

### 2.11 Observabilité : journal, purge, rétention, journaux, rédaction, métriques, flux

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-11.1 | Journal d'événements chaîné par hachage (JSON canonique, SHA-256, `GENESIS`), `seq` par session unique en base, hachage recalculé sur le texte canonique stocké (flottants tolérés), une erreur de lecture pendant `append` fait échouer l'écriture au lieu de forger un maillon, payload illisible dit, session inconnue refusée, appends concurrents sur une seule chaîne, événements en direct qui suivent le commit, purge qui garde la chaîne vérifiable | §4.1, #47, 0.3.0 | `kernel/event.rs` (`EventLog`, `compute_hash_from_text`, `VerifyReport`), `kernel/canonical.rs` | `chain_is_linked_and_verifies`, `a_read_error_fails_the_append_instead_of_forging_a_genesis_link`, `a_duplicate_seq_is_refused_by_the_database`, `ca_4_1_detects_tampering`, `seq_is_per_session`, `live_events_follow_the_committed_log`, `purge_erases_content_but_keeps_chain_verifiable`, `concurrent_appends_keep_a_single_chain`, `events_with_any_float_verify_after_storage` (event.rs), `key_order_does_not_matter`, `nested_objects_are_sorted`, `strings_are_escaped_strictly`, `form_feed_and_backspace_use_short_escapes`, `hash_is_stable` (canonical.rs), `ca_17_6_the_event_chain_survives_and_detects_tampering` (resilience.rs), `metadata_on_an_unknown_session_fails` (session.rs) |
| C-11.2 | `audit.verify` (`penelope audit-verify`) recalcule la chaîne et nomme le premier maillon rompu | 0.1.0 | `daemon/rpc.rs` | `audit_verify_is_reachable` (rpc.rs:2287), `database_and_audit_are_healthy_on_a_fresh_install` (doctor.rs) |
| C-11.3 | Purge RGPD d'une session (`session.purge`, `penelope session purge --yes --reason`, `/purge` avec confirmation) : messages et FTS, contexte figé, résumés, artefacts et fichiers, requêtes au modèle, payloads des tours et des updates du chat, candidats, arguments et résultats d'outils (ligne et clé d'idempotence gardées), messages envoyés, demandes d'approbation (en attente annulées), tâches MCP, runs et étapes de la session ; `events` garde lignes et hachages, `audit.purge` note la purge, `audit-verify` reste vert ; la mémoire durable n'est pas touchée | #46, #78 | `daemon/purge.rs` | `purging_a_session_leaves_the_audit_chain_and_nothing_else` (purge.rs:810), `media_paths_are_read_out_of_a_message` (purge.rs), `purge_erases_content_but_keeps_chain_verifiable` (event.rs) |
| C-11.4 | Rétention quotidienne : au-delà de `retention.days` les tours terminés, requêtes abouties, payloads d'updates, clés de travail, contenu des effets tranchés (un `unknown` garde tout), envois partis, demandes décidées, tâches MCP et sorties de workflows finies ; au-delà de `retention.memory_history_days` les pré-images ; `0` désactive ; `kv` daté (migration 0010) ; `store.retention` ; `doctor` donne la dernière passe et ce que gardent les tables | #46, #78 | `daemon/purge.rs` | `retention_removes_what_is_past_its_age_only` (purge.rs:1040), `retention_is_reported_with_the_kept_content` (doctor.rs) |
| C-11.5 | Journaux JSON par jour (`~/Library/Logs/Penelope`, `0700`/`0600`), rotation et rétention `log_retention_days`, rédaction avant disque et sur stderr, `PENELOPE_LOG` puis `observability.log_level`, pas de copie stderr sous launchd, spans `turn` (tour, session, genre), `run`, `maintenance` avec `with_current_span`, `penelope logs --turn\|--session` sans daemon | #26, #103 | `observe/lib.rs` (`init`, `restrict_log_dir`, `RedactingStream`, `LogControl`), `daemon/runner.rs`, `cli commands.rs` | `daily_file_writes_and_rotates_name`, `retention_deletes_old_files`, `stderr_is_redacted_too`, `log_files_are_private`, `writer_redacts_before_disk` (observe/lib.rs), `every_log_line_of_a_turn_carries_the_turn` (daemon/tests/log_spans.rs), `logs_are_filtered_by_turn_and_session` (cli commands.rs) |
| C-11.6 | Rédaction commune (journaux, événements, demandes stockées, file Telegram, flux runtime) : secrets enregistrés (`register_secret`, `learn_secrets` depuis les résultats d'outils, valeurs courtes ignorées), bearer, JWT, clés de fournisseurs, jeton de bot, clé privée, affectations entre guillemets, jetons longs à forte entropie, hexadécimal de plus de 64 caractères (SHA-256 exact lisible), numéros de carte (Luhn), clés JSON sensibles, idempotente, linéaire (référence orpheline `${SECRET:` jamais bouclée), références complètes intactes, `secret_spans`, `contains_secret`, `leaked_secret_kind`, `stored_secret_kind`, `redact_json` | #26, #37, #132, #134, #148, #153 | `observe/redact.rs` (`MASK`, `redact`, `register_secret`, `learn_secrets`, `luhn`, `secret_spans`) | `an_orphan_secret_reference_never_loops`, `masks_bearer_and_jwt`, `masks_provider_keys`, `masks_telegram_bot_token`, `card_number_uses_luhn`, `a_long_hex_run_is_masked_but_a_sha256_digest_is_not`, `registered_secret_is_masked_even_without_pattern`, `short_values_are_not_registered`, `redaction_is_idempotent`, `json_sensitive_keys_are_masked`, `a_key_copied_from_a_file_is_masked_where_it_is_stored`, `a_number_inside_an_identifier_is_not_a_card`, `contains_secret_detects_card_and_key`, `ca_13_2_planted_secret_never_leaks`, `secret_spans_point_at_values_only` (redact.rs), `ca_13_2_secrets_never_leak` (security.rs), `secrets_are_redacted_in_export` (trajectory.rs) |
| C-11.7 | Détecteur d'injection local (dix règles : `override_instructions`, `new_persona`, `pipe_to_shell`, `remote_exec`, `exfiltration`, `destructive_command`, `memory_write_attempt`, `tool_injection`, `hidden_unicode`, `fake_system_block`), sévérités, formule d'erreur MCP non signalée (#13), alerte qui dit venir de Pénélope et cite le motif et l'extrait, encadrement du contenu non fiable (`wrap_untrusted`), signalements bornés à dix | §13, #13 | `observe/injection.rs` (`scan`, `worst`, `is_suspicious`, `wrap_untrusted`) | `detects_instruction_override`, `detects_curl_pipe_shell`, `detects_exfiltration`, `detects_memory_write_attempt`, `detects_hidden_unicode`, `detects_fake_system_block`, `ordinary_text_is_clean`, `mcp_error_wording_is_not_an_injection`, `the_alert_says_it_comes_from_penelope_and_quotes_the_trigger`, `wrapper_marks_boundary_and_alerts`, `wrapper_on_clean_content_has_no_alert` (injection.rs), `ca_13_1_injected_instructions_never_act_on_their_own`, `skills_and_workflows_refuse_suspicious_content` (security.rs) |
| C-11.8 | Registre de métriques (compteurs, jauges, histogrammes avec quantiles, étiquettes échappées) rendu en texte Prometheus par `metrics` et `penelope metrics` | #103 | `observe/metrics.rs`, `daemon/rpc.rs` | `counters_and_gauges_render`, `histogram_quantiles_are_monotonic`, `labels_are_escaped` (metrics.rs), `metrics_are_readable_over_rpc` (rpc.rs:2335) |
| C-11.9 | Flux runtime (voir 1.8) : événements commités émis dans l'ordre, filtrés par consommateur, replay depuis `after_id`, refus sans jeton, bind sur bouclage seulement, secret absent ou trop court empêche le démarrage, payload rédigé et borné (anciens payloads compris) ; `runtime.llm` pour chaque usage enregistré, `runtime.session.*` pour les sessions, `runtime.approval.*` pour les approbations, `runtime.tool` pour les outils | #162 | `daemon/runtime_events.rs`, `kernel/budget.rs`, `kernel/session.rs`, `hitl/lib.rs`, `daemon/executor.rs` | `public_frame_keeps_order_and_redacts_payload`, `public_frame_bounds_legacy_payloads_too`, `websocket_replays_then_streams_filtered_committed_events`, `websocket_rejects_unauthenticated_clients`, `stream_does_not_bind_without_a_valid_stored_secret` (runtime_events.rs), `runtime_stream_requires_loopback_and_per_consumer_token` (config.rs), `every_recorded_model_usage_has_a_runtime_event` (budget.rs), `session_creation_is_in_the_runtime_log` (session.rs), `approval_lifecycle_is_in_the_runtime_log` (hitl/lib.rs), `every_native_tool_execution_is_in_the_runtime_log` (executor.rs) |
| C-11.10 | `tail` : notification RPC qui reçoit les `StreamEvent` du bus (fragments et journaux) ; abonné lent peut perdre des fragments, jamais l'issue d'un tour | 0.1.0 | `daemon/rpc.rs`, `daemon/bus.rs` | `a_late_waiter_still_gets_the_outcome`, `an_early_waiter_is_woken` (bus.rs) ; méthode `tail` : aucun test dédié |
| C-11.11 | Trajectoires JSONL (export d'évaluation) : ordre préservé, secrets rédigés, fichier tronqué lisible | §20 | `observe/trajectory.rs` | `roundtrip_preserves_order`, `secrets_are_redacted_in_export`, `truncated_file_is_partially_readable` (trajectory.rs) |
| C-11.12 | Usage et coûts : chaque appel enregistré avec coût facturé ou estimé (`estimated`), tokens (entrée, cache, sortie, raisonnement), rôle, requête d'origine, génération, fournisseur amont, run ; sommes par axe, part de cache, journée locale (`owner.timezone` relu à chaque calcul), `UsageWatcher` ; `penelope usage`, `/usage`, `/budget` | 0.2.2, #20, #79 | `kernel/budget.rs` (`BudgetLedger`, `UsageRecord`, `UsageRow`, `day_at`) | `records_and_sums`, `the_budget_day_is_the_owners_day`, `costs_are_attributed_to_sessions_and_requests`, `status_thresholds`, `no_limit_never_alerts` (budget.rs) |
| C-11.13 | Boucles de fond surveillées (`spawn_supervised` : runners, ordonnanceur, pilote de workflows, maintenance, catalogue, rappel OAuth, `mcp.maintenance`, `telegram.poll`, `telegram.drafts`, `telegram.outbox`) : panique journalisée avec le nom, comptée, `daemon.task_panicked`, relance 1 s, 2 s… 5 min, interrompue par l'arrêt ; `status` donne les runners vivants ; `doctor` les boucles relancées dans l'heure | #84 | `daemon/tasks.rs`, `daemon/supervisor.rs` | `a_panicking_loop_is_restarted_and_reported`, `shutdown_interrupts_the_restart_backoff` (tasks.rs), `status_reports_a_coherent_snapshot` (runtime.rs) |
| C-11.14 | Écrivain à l'épreuve des paniques : transaction annulée, journalisée, comptée (`penelope_store_writer_panics_total`, contrôle `doctor`), `WriterPanic` renvoyé au demandeur, thread écrivain vivant, pile de 8 Mio | #44, #153 | `store/lib.rs` (`writer_panics`) | `a_panicking_write_does_not_kill_the_writer`, `write_rolls_back_on_error`, `concurrent_writes_are_serialised` (store/lib.rs) |
| C-11.15 | Instantané de base cohérent pendant l'écriture (`VACUUM INTO` sur connexion de lecture, écritures qui continuent), copie relisible | #77 | `store/lib.rs` (`backup_to`) | `writes_go_on_while_a_snapshot_is_taken`, `backup_produces_readable_copy` (store/lib.rs) |
| C-11.16 | Non commité (#205) : instantanés du prompt système dédupliqués par `system_hash` (une ligne par prompt distinct, jamais sur le chemin de la réponse, sans T4), `audit.show` / `penelope audit show --turn\|--session` (prompt reconstitué, messages, outils, ce qui manque dit, prompt purgé annoncé, secrets jamais en clair), cause de raté qui nomme la tuile, `doctor prompt.stability`, purge et rétention des instantanés, migration 0018 | #205 | `daemon/prompt_snapshot.rs`, `daemon/audit.rs`, `daemon/cache_audit.rs`, `context/tiers.rs` (`TileMap`), `store/migrations.rs` | `the_same_prompt_is_written_once`, `the_volatile_tier_never_enters_the_snapshot`, `a_reloaded_skill_names_the_index_tile`, `the_prefix_miss_names_the_tile_that_moved`, `two_turns_without_a_reload_share_one_snapshot`, `a_prefix_that_does_not_match_its_fingerprint_is_not_written`, `a_prompt_without_tiles_is_still_kept` (prompt_snapshot.rs), `a_turn_is_replayed_from_its_fingerprint`, `an_unknown_turn_is_an_error_not_an_empty_answer`, `a_purged_prompt_is_announced_not_invented`, `a_secret_inside_the_prompt_never_comes_back_in_the_clear` (audit.rs), `tiles_slice_the_rendered_prefix`, `only_the_changed_tile_is_named`, `several_tiles_are_named_in_order`, `the_map_carries_no_text` (tiers.rs), `purging_a_session_takes_the_prompts_only_it_used`, `retention_only_drops_prompts_nothing_points_to` (purge.rs), `prompt_snapshots_replace_the_dead_request_column` (migrations.rs), `an_unstable_system_prompt_is_reported_with_its_tile`, `a_quiet_instance_keeps_a_green_prompt_check` (doctor.rs), `the_three_keys_make_a_request_findable` (state.rs) |

### 2.12 Exploitation : CLI, service, doctor, mise à jour, sauvegarde, configuration, store

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-12.1 | CLI : définition cohérente, options globales partout, routage de chaque commande vers une méthode du contrat, scalaires typés depuis la ligne, rendu (objets en clé-valeur, tableaux, listes verticales larges, tableaux vides explicites, booléens en français, chaînes jointes), `--json` | §15 | `cli commands.rs`, `cli output.rs`, `cli client.rs` | `the_cli_definition_is_coherent`, `global_flags_work_anywhere`, `commands_route_to_rpc_methods`, `every_routed_method_exists_in_the_contract`, `scalars_are_typed_from_the_command_line` (commands.rs), `objects_render_as_key_value_lines`, `arrays_of_objects_render_as_a_table`, `wide_objects_fall_back_to_vertical_lists`, `empty_arrays_are_explicit`, `booleans_are_readable_in_french`, `string_arrays_are_joined_inline` (output.rs), `socket_path_follows_home` (client.rs) |
| C-12.2 | Daemon muet : 15 s puis code 7 et marche à suivre (délai par méthode, `--timeout`) ; daemon absent : code 3 et comment le démarrer ; `doctor` rend ses contrôles locaux sans daemon et nomme le daemon en tête | #99 | `cli client.rs`, `cli commands.rs` | `a_mute_daemon_is_reported_not_waited_for`, `unreachable_daemon_suggests_how_to_start_it`, `calling_an_absent_daemon_is_a_clean_error`, `each_method_has_its_limit`, `exit_codes_follow_the_documented_table` (client.rs), `doctor_reports_local_checks_without_a_daemon` (commands.rs) |
| C-12.3 | `secret set` sans daemon, valeur à l'invite ou sur stdin, jamais en argument (refus avec conseil), nom slug, backend et longueur affichés jamais la valeur ; `config validate`, `wf validate`, `paths` sans daemon | §4, #95 | `cli commands.rs` | `setting_a_secret_never_goes_through_the_rpc`, `a_secret_value_on_the_command_line_is_refused_with_guidance`, `a_secret_name_must_be_a_slug`, `config_validate_works_without_a_daemon`, `workflow_validate_works_without_a_daemon`, `paths_works_without_a_daemon` (commands.rs) |
| C-12.4 | Socket RPC : NDJSON, socket `0600`, fichier résiduel retiré avant `bind`, second `bind` refusé si vivant, jeton de session tiré à chaque démarrage, écrit avant la socket, comparé strictement, requête sans jeton refusée sans rien exécuter, jeton absent des journaux et de `doctor`, ancien jeton refusé après redémarrage, méthode inconnue `-32601`, paramètre manquant `-32602`, `status`, `paths`, `shutdown`, `restart` | décision 0004, #91 | `platform/ipc.rs` (`IpcListener`, `issue_token`, `read_token`, `tokens_match`), `daemon/rpc.rs` | `a_token_is_issued_privately_and_compared_strictly`, `bind_accept_and_roundtrip`, `stale_socket_is_replaced`, `second_bind_while_alive_is_refused`, `socket_permissions_are_0600`, `pipe_name_format` (ipc.rs), `a_request_without_the_session_token_is_refused` (chat_socket.rs), `unknown_method_returns_32601`, `missing_parameter_returns_32602`, `status_and_paths`, `shutdown_and_restart_flags` (rpc.rs), `handle_controls_shutdown_and_restart` (runtime.rs), `request_serialises_as_jsonrpc`, `notification_has_no_id` (api.rs) |
| C-12.5 | Service : `install` écrit `~/Library/LaunchAgents/com.penelope.daemon.plist` (clés du PRD, `PENELOPE_HOME` transmis, XML échappé, PATH complété par les emplacements usuels et nvm) et charge ; `uninstall`, `start`, `stop`, `restart` ; programme lancé lu et réécrit ; unité systemd générée (`Restart=always`) pour Linux | §2 | `platform/service.rs`, `platform/process.rs` (`extra_bin_dirs`, `merge_paths`) | `the_launched_program_is_read_and_rewritten`, `plist_has_the_prd_keys`, `plist_passes_home_when_set`, `plist_escapes_xml`, `systemd_unit_restarts_always` (service.rs), `paths_keep_the_user_order_and_add_existing_extras`, `a_service_path_still_finds_homebrew_tools`, `the_newest_nvm_node_is_found`, `which_finds_a_standard_binary`, `which_accepts_absolute_paths`, `default_shell_is_reasonable` (process.rs) ; `launchctl` réel : `a_reload_asked_from_inside_the_service_job_brings_the_service_back` (launchd_relay.rs, opt-in) |
| C-12.6 | Répertoires : table macOS, arbre créé au démarrage, tout sous une racine avec `PENELOPE_HOME` ou `--home`, placeholders, slugs (français, collisions insensibles à la casse), fichiers texte en LF, `penelope paths` | §2.2 | `platform/dirs.rs`, `platform/lib.rs` (`Platform::bootstrap`) | `rooted_dirs_place_everything_under_one_root`, `ensure_all_creates_the_prd_tree`, `expand_placeholders`, `slug_rules`, `slugify_handles_french`, `case_insensitive_collision`, `text_files_are_written_in_lf`, `explicit_home_wins` (dirs.rs), `bootstrap_for_tests_creates_the_tree`, `macos_directories_follow_the_prd_table`, `ca_2_2_penelope_home_reroots_everything` (platform/lib.rs), `bootstrap_wires_everything` (runtime.rs) |
| C-12.7 | `doctor` (voir 1.13) : contrôles, corrections proposées jamais exécutées, sévérité critique, rendu lisible, boutons Telegram vers les écrans des alertes | §2.11 | `daemon/doctor.rs`, `platform/lib.rs` | tests listés en 1.13 |
| C-12.8 | Mise à jour : dernière release (pre-release comprises), `SHA256SUMS` vérifié, `SHA256SUMS.minisig` vérifié dès qu'une clé est connue (`upgrade.minisign_pubkey` ou clé du binaire) et release non signée alors refusée, `upgrade.base_url`, archive extraite, le nouveau binaire doit annoncer sa version, ancien gardé en `.previous`, bascule par renommage, `upgrade.json`, redémarrage, essais comptés avant l'ouverture de la base, sans santé 60 s après le premier essai ou au-delà de 5 essais retour arrière et propriétaire prévenu (carte qui cite la dernière ligne parlante de `daemon.err.log`), chien de garde 90 s, confirmation annoncée une fois, `--check`, `--tag`, `--force`, `--rollback` (bascule entre les deux binaires), hors ligne sans daemon, binaire de `target/` refusé, re-signature `upgrade.codesign_identity`, `/upgrade [install\|rollback]` | 0.3.1, 0.4.0, #28, #153 | `daemon/upgrade.rs`, `platform/codesign.rs` | `sums_and_versions_are_parsed_strictly`, `the_rollback_card_repeats_the_last_error_of_the_failed_binary`, `ca_2_8_a_broken_upgrade_is_rolled_back_automatically`, `repeated_fast_crashes_also_roll_back_and_confirmation_is_announced_once`, `manual_rollback_toggles_between_the_two_binaries`, `install_verifies_the_archive_then_swaps_the_binary`, `release_sums_are_checked_against_the_minisign_key` (upgrade.rs), `codesign_details_are_classified`, `the_running_binary_has_a_signature` (codesign.rs) |
| C-12.9 | Installation source vers releases (`--switch`, carte `/upgrade install`) : préconditions (identité de signature utilisable depuis le daemon, `install_dir` inscriptible, LaunchAgent qui lance ce binaire et fichier modifiable), binaire installé et re-signé, compilation gardée en précédent, `ProgramArguments` réécrit (original en `.plist.sources`), sans santé retour au binaire de compilation sur le chemin stable ; `make deploy` migre un service qui lance `target/release` | #33, #36 | `daemon/upgrade.rs`, `platform/service.rs` (`launchd_with_program`) | `a_source_install_switches_to_releases`, `an_unconfirmed_switch_brings_the_source_build_back_to_the_stable_path`, `a_switch_without_a_usable_identity_changes_nothing`, `the_state_file_speaks_the_relay_contract` (upgrade.rs), `a_service_launching_a_build_output_is_flagged` (doctor.rs) |
| C-12.10 | Relais launchd (`com.penelope.daemon.reloader`) hors du job du daemon : attend l'arrêt réel, vérifie et retente `bootstrap`, remet le fichier de service d'origine si refusé, garde-fou de deux minutes qui remet le binaire précédent, chemins cités, aide non gardée vivante, armé aussi après une mise à jour ordinaire, journal `reloader.log` ; `doctor` signale une mise à jour installée depuis plus de cinq minutes jamais démarrée | #36 | `platform/handoff.rs` (`HandOff`, `GUARD_S`), `daemon/upgrade.rs` | `a_started_binary_ends_the_relay_without_touching_anything`, `a_binary_that_never_starts_is_replaced_by_the_previous_one`, `a_bootstrap_refused_while_the_job_winds_down_is_retried`, `a_refused_service_file_is_replaced_by_the_original`, `paths_are_quoted_and_the_helper_is_not_kept_alive` (handoff.rs), `an_ordinary_upgrade_arms_a_guard_outside_the_daemon` (upgrade.rs), `an_upgrade_that_never_booted_is_reported` (doctor.rs), `a_reload_asked_from_inside_the_service_job_brings_the_service_back`, `a_new_binary_that_never_starts_is_replaced_by_the_previous_one` (launchd_relay.rs, CI macOS) |
| C-12.11 | Sauvegarde : `penelope backup` (instantané cohérent dans `backups/`), `--push` (archive de la base, du vault, des skills, des workflows, des gabarits, de `mcp.d` et de `config.toml`, médias exclus sauf `--media`, chiffrée Argon2id puis XChaCha20-Poly1305 par `backup_passphrase` avant de quitter la machine, `MANIFEST.json` avec date, version, tailles, SHA-256 et noms des secrets), dépôt public refusé (`gh`), taille au-delà de `max_push_bytes` refusée, phrase de passe absente dite avant tout travail, rotation 7/4/12 sans réécrire l'historique, une par nuit à `backup.cron`, échec sur Telegram, sur thread bloquant sans geler l'écrivain, `store.backup` et `backup.done`, `doctor` (âge > 48 h, taille, durée) et `self_status` | #42, #77 | `daemon/backup.rs`, `platform/archive.rs` (`seal`, `open`, `MAX_ARCHIVE_BYTES`), `platform/process.rs` (`create_tar_gz`, `git_sync_repo`, `git_commit_push`) | `a_backup_restores_the_database_and_the_vault`, `a_backup_neither_blocks_the_runtime_nor_the_writer`, `without_a_passphrase_nothing_is_written`, `an_oversized_archive_is_refused_before_pushing`, `doctor_says_when_there_is_no_backup_yet`, `a_github_slug_is_read_from_any_remote_form`, `rotation_keeps_seven_four_and_twelve` (backup.rs), `sealed_archive_reopens_with_its_passphrase_only`, `an_empty_passphrase_is_refused`, `a_foreign_file_is_not_mistaken_for_a_backup` (archive.rs) ; refus d'un dépôt public : aucun test |
| C-12.12 | `restore-all [source] [--dry-run]` (clone ou archive locale, phrase de passe à l'invite, base et fichiers remis, existant mis de côté, daemon arrêté, marche à suivre : `install`, `start`, secrets du manifeste, `doctor`) ; `restore <fichier>` (base seule, actuelle mise de côté) | #42, 0.3.1 | `cli commands.rs`, `daemon/backup.rs` | `a_backup_restores_the_database_and_the_vault` (backup.rs) ; `restore <fichier>` et `--dry-run` : aucun test |
| C-12.13 | Import Hermes (`import hermes [--path] [--dry-run] [--no-test]`) : sous-ensemble YAML lu, skills (slug, description repliée, annexes, existantes gardées), `SOUL.md` et `AGENTS.md` (mis de côté si différents), `MEMORY.md` et `USER.md` (uid, origine `owner`, doublons ignorés, secrets et injections refusés), `mcp_servers` convertis en `mcp.d` puis essayés (`ok`, `auth_required`, `failed`), secrets (littéraux, `${VAR}`, `${env:VAR}` du `.env`) vers le magasin, second import sans doublon, rapport sur Telegram, `import.hermes` | 0.3.1 | `daemon/hermes.rs` | `the_yaml_subset_reads_hermes_config`, `servers_are_converted_with_their_secrets_moved_out`, `hermes_skills_and_memories_are_normalised`, `an_instance_is_simulated_then_imported_once` (hermes.rs) |
| C-12.14 | Configuration à chaud (voir 1.6) : générations publiées à chaque sous-système, changement invalide refusé sans rien publier, `config.status` par sous-système, `config.reload` depuis le disque, `config.get`, `config.set` (valeur seule pour une liste, entrée nouvelle d'une table à clés libres), survit au redémarrage, contradictions refusées ou signalées, audit au démarrage et dans `doctor` | §4.3, #16, #45, #76, #138 | `kernel/config.rs`, `kernel/coherence.rs`, `daemon/runtime.rs`, `daemon/rpc.rs` | `config_generations_are_published_to_every_subsystem`, `invalid_config_change_publishes_nothing` (runtime.rs), `ca_4_4_config_changes_are_published_live`, `an_invalid_config_change_is_rejected_and_nothing_moves` (hot_reload.rs), `config_get_set_and_status`, `self_cancelling_settings_are_refused_or_warned`, `unknown_config_path_is_refused`, `a_new_role_can_be_set_on_an_older_configuration`, `config_set_takes_a_single_value_for_a_list` (rpc.rs), `the_configuration_survives_a_restart` (resilience.rs), `reload_from_disk_reports_changed_paths`, `durations_parse`, `toml_roundtrip`, `map_paths_are_maps`, `default_config_is_valid`, `owner_is_required`, `role_pointing_to_unknown_alias_is_rejected`, `an_explicit_shell_network_survives_the_new_default` (config.rs), `the_sample_configuration_is_coherent`, `self_cancelling_settings_are_named` (coherence.rs) |
| C-12.15 | Store : ouverture et migrations, écrivain unique sérialisé, pool de lecture qui sert plus de lecteurs que sa capacité, intégrité en trois verdicts (fichier sain, lecteur qui ment remplacé, index dérivé confirmé nommant la table), toutes les lignes d'un `PRAGMA` lues, index FTS reconstruit au démarrage (`store.fts_rebuilt`), `store.rebuild` (FTS, index mémoire, audit), SQLite ≥ 3.50 | décision 0001, #158, 0.3.1 | `store/lib.rs` (`Store::open`, `integrity_report`, `IntegrityReport`), `store/pool.rs`, `daemon/session_ops.rs` | `open_migrate_read_write`, `a_broken_search_index_does_not_stop_the_daemon`, `the_bundled_sqlite_is_recent_enough`, `only_a_verdict_about_derived_indexes_is_repairable`, `every_line_of_a_verdict_is_read`, `a_sound_database_needs_no_second_opinion`, `a_lying_reader_is_recognised_and_replaced`, `a_confirmed_index_fault_names_the_table` (store/lib.rs), `pool_serves_more_readers_than_capacity` (pool.rs), `fts5_is_available`, `all_prd_tables_exist` (migrations.rs), `export_writes_jsonl_and_rebuild_restores_search` (session_ops.rs) |
| C-12.16 | Reprise au démarrage : tours interrompus remis en file une fois, effets en vol devenus questions, requêtes LLM classées, runs repris à leur étape, tâches MCP reprises, orphelins MCP tués, dossier de profils Seatbelt d'anciennes versions effacé, `daemon.recovered` | §17 | `daemon/runtime.rs`, `daemon/supervisor.rs` | `recovery_requeues_and_asks_instead_of_retrying` (runtime.rs:715), `ca_17_1_a_turn_interrupted_mid_flight_is_requeued_once`, `ca_17_5_a_run_resumes_at_its_current_step` (resilience.rs), `ca_8_6_tasks_survive_a_restart` (tasks.rs) |
| C-12.17 | Maintenance périodique : rappels d'approbation, jetons de boutons expirés purgés, demandes échues reprises, skills relues au changement, autocommit du vault, rétention quotidienne, inventaire machine horaire, mesure des workspaces, rappel OAuth, re-rédaction de la file une fois | 0.1.0 à 0.17.51 | `daemon/supervisor.rs` (`maintenance`) | `pending_approvals_are_reminded_at_one_and_six_hours`, `an_expired_approval_resumes_its_turn`, `a_skill_dropped_by_scp_is_picked_up_by_the_maintenance_pass`, `skills_reload_only_when_their_content_changes`, `workspace_measurement_stays_inside_its_root` (supervisor.rs), `expired_tokens_are_refused_and_purged` (actions.rs) ; autocommit périodique, inventaire horaire, rappel OAuth : aucun test |
| C-12.18 | Livraison : CI (`tests` Linux suite complète, `verification` macOS format + clippy + plateforme + suite complète, `dependances` cargo deny, `livraison` qui pose le tag et appelle `release.yml`, un seul à la fois), release (vérification, build par cible, binaire universel `lipo`, `SHA256SUMS`, minisign si secret présent, rapport mémoire joint), `make bump V=x.y.z` (section de `progress.md` exigée, seize lignes de `Cargo.toml`, `Cargo.lock`, commit), test `docs` (index, liens et ancres, catalogue Telegram, clés, outils, page contexte, section de version égale au workspace, rien de livré présenté comme manquant, README sans compteurs figés), matrice CA, gabarit de PR, `cargo deny` | #38, #147, #194 | `.github/workflows/ci.yml`, `release.yml`, `Makefile`, `scripts/bump.sh`, `evals/tests/docs.rs`, `evals/tests/ca_matrix.rs` | `the_index_cites_every_guide_and_decision`, `the_readme_does_not_freeze_changing_coverage_counts`, `no_relative_link_is_dead_anchors_included`, `telegram_commands_and_their_documentation_match`, `every_configuration_key_is_documented`, `the_context_page_follows_the_code`, `every_native_tool_is_documented`, `the_workspace_version_has_its_progress_section`, `the_highest_progress_section_is_the_workspace_version`, `no_doc_presents_something_shipped_as_missing`, `sections_are_cut_at_the_next_heading_of_the_same_level`, `anchors_follow_github` (docs.rs), `the_matrix_is_up_to_date`, `every_section_with_acceptance_criteria_is_covered`, `acceptance_tests_are_spread_across_the_workspace` (ca_matrix.rs), `source_scanning_only_keeps_well_formed_names`, `labels_are_readable`, `rendering_groups_by_section_in_order` (evals/src/ca_matrix.rs) ; jobs CI et `bump.sh` : aucun test |
| C-12.19 | Signature macOS stable : `make build` / `make sign` avec `SIGN_IDENTITY` et `SIGN_IDENTIFIER`, exigence désignée constante, `scripts/check-codesign.sh`, workflow CI « Signature macOS », `doctor binary_signature` | #28 | `platform/codesign.rs` (`inspect`, `designated_requirement`, `sign`), `Makefile` | `codesign_details_are_classified`, `the_running_binary_has_a_signature` (codesign.rs) |
| C-12.20 | Suites d'évaluation : catalogue conforme, drapeaux réseau, commande `cargo` par suite, résultats rendus, suite inconnue signalée ; `penelope eval <suite>` | §20.1 | `evals/src/suites.rs` | `the_catalog_matches_the_prd_table`, `network_flags_follow_the_table`, `every_suite_has_a_runnable_command`, `results_render_readably`, `unknown_suites_are_reported` (suites.rs) |
| C-12.21 | État de la machine (`self_status`, `doctor`) : batterie et alimentation (`pmset`), démarrage, charge, disque, jamais en échec sans outils ; garde de sommeil `caffeinate` comptée pendant un run | 0.2.2 | `platform/host.rs`, `platform/power.rs` | `battery_on_battery_power`, `battery_on_ac_power_while_estimating`, `a_desktop_mac_has_no_battery`, `sysctl_and_df_outputs_are_read`, `status_never_fails_even_without_tools` (host.rs), `assertions_are_counted_and_released`, `mechanism_is_reported` (power.rs), `doctor_reports_dependencies_and_disk` (platform/lib.rs) |
| C-12.22 | Sondes de version des binaires avec délai et entrée fermée ; `binary_version` du nouveau binaire | #156, 0.3.1 | `platform/process.rs` (`probe_command`, `probe_version`, `binary_version`) | `versions_are_probed_with_a_timeout` (process.rs) |
| C-12.23 | Un tour de conversation depuis la CLI et un retour par le socket (`chat.send`, `chat.stream`, `chat.stop`) | 0.1.0 | `daemon/rpc.rs`, `cli commands.rs` | `chat_stream_sends_deltas_then_the_final_answer` (chat_socket.rs) ; `chat.send` synchrone : partiel |
| C-12.24 | `penelope model list --filter`, `model set` (validation, catalogue), `/models` | 0.1.0 | `daemon/rpc.rs` | `model_list_shows_the_routing_in_force` (cli commands.rs), `alias_validation_rejects_unknown_models` (router.rs) |

### 2.13 Sécurité

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-13.1 | Secrets dans le trousseau macOS par `/usr/bin/security` exécutable (jamais un shell), valeur sur stdin en hexadécimal, découpe au-delà de 4 000 octets par ligne en items `#n` avec tête imprimable (ancienne marque lue, jamais écrite), `get` réassemble, `delete` efface tout, `list` montre le nom logique, tête illisible = erreur explicite, hexadécimal décodé seulement s'il redonne une tête, échec sans jamais recopier la sortie de `security`, aller-retour réel ; fichier chiffré et mémoire en repli ; noms validés ; placeholders `${SECRET:…}` et `${env:…}` développés ou listés | §13.1, #95, #148, #157 | `platform/secrets.rs` (`SecretStore`, `chunks`, `EncryptedFileStore`, `MemorySecretStore`, `validate_secret_name`, `placeholders`), `platform/backend/macos.rs` (`KeychainStore`) | `secret_names_accept_what_the_project_actually_uses`, `a_long_value_is_cut_into_readable_chunks`, `the_chunk_mark_is_printable_and_the_old_one_is_still_read`, `a_store_holds_a_sixteen_kilobyte_secret`, `encrypted_store_roundtrip`, `file_is_not_readable_as_plaintext`, `wrong_passphrase_fails_cleanly`, `permissions_are_0600`, `key_file_backend`, `expand_secret_and_env_placeholders`, `missing_secret_is_an_explicit_error`, `placeholders_are_listed_without_resolution`, `expand_handles_unterminated_placeholder` (secrets.rs), `a_secret_goes_through_stdin_in_hex`, `a_failing_security_never_leaks_the_value_into_the_error`, `a_chunked_secret_survives_a_security_that_prints_hex`, `a_secret_written_with_the_old_mark_is_still_read`, `an_unreadable_head_is_an_error_not_a_value` (macos.rs), `a_secret_round_trips_through_the_keychain`, `keychain_holds_a_long_secret_and_forgets_it` (macos.rs, ignorés), `secret_roundtrip_of` : `an_altered_read_is_not_blamed_on_a_locked_keychain`, `a_locked_keychain_is_still_told_to_unlock` (doctor.rs) |
| C-13.2 | Bac à sable Seatbelt (voir C-5.17) toujours appliqué au shell, aux étapes `shell` et aux serveurs stdio ; `full` seulement par choix explicite ; échec fermé hors macOS ; aucun fichier de profil écrit | #68, #90 | `platform/sandbox.rs`, `platform/backend/macos.rs` | tests de C-5.17, `sandbox_failure_is_closed_not_open`, `ca_2_5_workspace_write_blocks_outside_writes` (security.rs) |
| C-13.3 | Environnement des processus filtré (`default_inherited_env` sans secret, `SHELL_EXTRA_ENV`), commandes interdites, préfixes de privilège, jamais de jeton dans l'environnement d'un shell ; secrets injectés seulement dans l'environnement du serveur MCP concerné | §13 | `platform/process.rs`, `tools/shell.rs`, `mcp/config.rs` | `default_inherited_env_has_no_secrets`, `environment_is_filtered`, `explicit_env_overrides_inherited` (process.rs), `the_shell_env_never_carries_tokens`, `forbidden_commands_are_refused`, `sudo_in_a_pipeline_is_caught` (shell.rs), `secrets_are_resolved_in_headers_and_env` (mcp/config.rs) |
| C-13.4 | Contenu observé = donnée, jamais instruction (`HARNESS_RULES`, encadrement `frame_untrusted` / `wrap_untrusted`, détecteur d'injection) ; une consigne injectée n'agit jamais seule ; skills et workflows suspects refusés | §13.3 | `context/tiers.rs`, `memory/provenance.rs`, `observe/injection.rs`, `skills/lib.rs`, `wf/registry.rs` | `ca_13_1_injected_instructions_never_act_on_their_own`, `skills_and_workflows_refuse_suspicious_content` (security.rs), `untrusted_content_is_always_framed` (provenance.rs), `harness_rules_are_always_present` (tiers.rs) |
| C-13.5 | SSRF : adresses privées, locales, IPv6 entre crochets, métadonnées, redirections revérifiées, épinglage DNS (voir C-5.8) | #64, #93, suite `security` | `tools/http.rs` | `ssrf_is_blocked_including_after_redirects` (security.rs) et tests de C-5.8 |
| C-13.6 | Validateur JSON Schema local : `$ref` externe refusé sans requête réseau, cycle borné, sous-ensemble 2020-12 (`type`, `enum`, `const`, `properties`, `required`, `additionalProperties`, `patternProperties`, `items`, `prefixItems`, `minItems`, `maxItems`, `uniqueItems`, bornes numériques, `multipleOf`, longueurs, `pattern`, `allOf`, `anyOf`, `oneOf`, `not`, `$defs`), erreurs en français avec JSON Pointer, `property_default`, `schema_bytes`, schémas booléens | décision 0003 | `kernel/schema.rs` | `object_required_and_types`, `additional_properties_false`, `local_ref_is_resolved`, `external_ref_is_refused_not_fetched`, `cyclic_ref_is_bounded`, `arrays_prefix_and_items`, `one_of_requires_exactly_one`, `enum_and_const`, `pattern_and_lengths`, `boolean_schemas`, `validate_ok_summarises`, `nested_error_paths_point_to_the_offender` (schema.rs), `schema_refs_never_hit_the_network` (security.rs) |
| C-13.7 | Socket RPC authentifiée par jeton ; sockets Unix fermées aux processus confinés ; un serveur MCP ne peut ni configurer, ni approuver, ni poser un secret | #91 | `platform/ipc.rs`, `platform/sandbox.rs` | `a_request_without_the_session_token_is_refused` (chat_socket.rs), `open_network_still_closes_unix_sockets` (sandbox.rs), `seatbelt_closes_unix_sockets_on_this_mac` (macos.rs, ignoré) |
| C-13.8 | Jeton du bot jamais dans les erreurs de transport ; secrets jamais dans les journaux ni stderr ; `doctor logs_secrets` et `stored_secrets` nomment ce qui a fui | #26, #134 | `tg/api.rs`, `observe/lib.rs`, `daemon/doctor.rs` | `transport_errors_never_carry_the_token` (api.rs), `stderr_is_redacted_too`, `writer_redacts_before_disk` (observe/lib.rs), `a_token_left_in_a_log_is_reported` (doctor.rs) |
| C-13.9 | Chemin d'écriture de la mémoire = frontière de sécurité : secrets et injections refusés, contenu non fiable jamais promouvable, `vault check` signale un secret écrit à la main et une pratique cassée, contenu interdit bloqué en consolidation | §6.10, 0.2.9 | `daemon/vault_ops.rs`, `daemon/dream.rs`, `memory/consolidation.rs` | `secrets_and_injections_never_enter_the_vault` (vault_ops.rs), `memory_write_path_refuses_forbidden_content`, `untrusted_content_is_never_promotable` (security.rs), `the_vault_check_flags_secrets_and_broken_practices` (dream.rs), `ca_6_13_forbidden_content_is_blocked_in_consolidation` (consolidation.rs) |
| C-13.10 | Skills : proposition avec secret refusée, archive tierce qui ne sort jamais de son dossier (liens symboliques, remontées, tailles bornées `MAX_FILE_BYTES`, `MAX_SKILL_BYTES`, `MAX_ARCHIVE_BYTES`), nom de paquet à échappement refusé | §7, #146 | `skills/lib.rs`, `skills/install.rs`, `daemon/skill_deps.rs` | `proposal_with_a_secret_is_refused` (skills/lib.rs), `an_archive_never_writes_outside_the_skill_directory` (install.rs), `a_requirement_is_read_or_refused` (skill_deps.rs) |
| C-13.11 | Références git validées contre l'injection d'options ; `git_clone` refuse les chemins locaux | 0.1.0, #160 | `tools/git.rs` | `ref_validation_blocks_option_injection`, `clone_rejects_local_paths_before_starting_git` (git.rs), `git_refs_cannot_smuggle_options` (security.rs) |
| C-13.12 | Sauvegarde chiffrée avant de quitter la machine ; jamais de valeur de secret dans l'archive ni le manifeste ; dépôt public refusé | #42 | `daemon/backup.rs`, `platform/archive.rs` | `without_a_passphrase_nothing_is_written` (backup.rs), `sealed_archive_reopens_with_its_passphrase_only`, `a_foreign_file_is_not_mistaken_for_a_backup` (archive.rs) ; dépôt public : aucun test |
| C-13.13 | Descriptions et schémas d'outils MCP encadrés, empreintes épinglées, règles révoquées sur changement (voir C-6.15, C-4.14) | #92 | `mcp/registry.rs`, `daemon/mcp.rs` | `fingerprints_and_exposed_descriptions`, `poisoned_or_changed_tools_are_flagged_and_lose_their_rules` |
| C-13.14 | Trousseau fermé à tout profil imposé, ouvert par serveur déclaré seulement ; `deny_read` livré ; clés jamais relues par `security find-generic-password` sous bac à sable | #68, #89, #122 | `platform/sandbox.rs` | `the_keychain_is_closed_to_every_enforced_profile`, `the_keychain_opens_only_for_a_profile_that_declares_it`, `denied_reads_come_after_the_allow_and_close_the_keychain` (sandbox.rs), `a_denied_path_is_unreadable_under_the_sandbox` (shell.rs) |
| C-13.15 | Clés recopiées par l'agent masquées là où elles sont stockées (carte, demande, file) sans modifier ce qui s'exécute ; rédacteur qui apprend les valeurs lues dans les résultats d'outils | #134 | `observe/redact.rs` (`learn_secrets`), `daemon/engine.rs` | `a_copied_key_is_stored_masked_and_executed_whole` (engine.rs:2661), `a_key_copied_from_a_file_is_masked_where_it_is_stored` (redact.rs) |
| C-13.16 | Jeton OAuth codex et secret client MCP jamais persistés en clair, masqués à l'obtention et à chaque rotation, révoqués si le rangement échoue | #142, #148, #174, #188 | `daemon/codex_auth.rs`, `daemon/mcp_auth.rs` | `confidential_client_secret_is_registered_for_redaction`, `basic_authorization_value_is_redacted_even_in_a_generic_structured_field` (mcp_auth.rs), `a_reused_refresh_token_disconnects_for_good` (codex_auth.rs) ; révocation sur échec de rangement : aucun test |
| C-13.17 | `config_set` refuse les secrets et l'identité du propriétaire ; `owner.telegram_user_id = 0` rend la configuration invalide (bot fermé) | §13, 0.2.2 | `daemon/selfknow.rs`, `kernel/config.rs` | `config_set_guards` (selfknow.rs), `owner_is_required` (config.rs), `a_missing_owner_is_critical_with_a_fix` (doctor.rs) |

### 2.14 Budgets et coûts

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-14.1 | Plafonds `daily_usd`, `session_usd`, `run_usd` ; `BudgetStatus` avec seuils ; sans plafond jamais d'alerte ; dépassement du jour détecté | §16, §10.4 | `kernel/budget.rs` (`BudgetScope`, `BudgetStatus::compute`) | `status_thresholds`, `no_limit_never_alerts`, `ca_10_4_daily_budget_exceeded_is_detected` (budget.rs) |
| C-14.2 | Alerte à `alert_ratio` (80 %) : une seule notification par périmètre au premier passage, avec les trois plus gros postes, leur coût et leur part de cache ; relever le plafond réarme | #20 | `daemon/budget_alert.rs` | `crossing_the_alert_ratio_notifies_once` (budget_alert.rs:182) |
| C-14.3 | À 100 % : carte « continuer ? » (+5 $, +20 $, Arrêter) ; session du propriétaire reprend son tour suspendu après relèvement ; jour = arrêt ferme relevable pour la journée (`budget.daily.<jour>`) ; run reprend après relèvement ; demande en attente jamais dupliquée ; budget par session stocké, affiché, recopié au fork | #32, #4 | `daemon/agent.rs`, `daemon/telegram.rs`, `daemon/rpc.rs` | `a_session_budget_is_raised_and_the_suspended_turn_resumes` (telegram.rs:10994), `exceeded_budget_stops_before_calling_the_model`, `the_budget_message_names_the_key_of_the_scope_reached` (agent.rs) ; relèvement du jour et run : partiel |
| C-14.4 | Point de contrôle par tour (`turn_checkpoint_usd`), coût affiché (`show_turn_cost_usd`), délégation suggérée (`delegate_after_calls`), plafond de 24 appels | #19 | `daemon/agent.rs` | `a_costly_turn_asks_before_going_on`, `the_call_cap_spans_resumptions_and_suggests_delegating` (agent.rs) |
| C-14.5 | Journée budgétaire dans `owner.timezone` (relu à chaud) pour `usage.day`, `spent_today`, relèvement, `/budget`, `/usage`, `--by day` ; `doctor` signale les consommations récentes comptées dans un autre fuseau | #79 | `kernel/budget.rs` (`day_at`, `with_timezone`) | `the_budget_day_is_the_owners_day` (budget.rs:770) |
| C-14.6 | Réserve de compaction `compaction_reserve_usd` ; plafonds de session et de run sans effet sur les résumés | #40 | `daemon/compaction.rs` | `budgets_do_not_block_compaction_until_the_summary_reserve_is_spent` (daemon/compaction.rs) |
| C-14.7 | Codex : 0 $ connu, plafonds en dollars sans effet, quota du plan (voir C-1.49) | #142 | `llm/codex.rs`, `daemon/codex_quota.rs` | `a_subscription_call_costs_nothing_and_says_so` (codex.rs), `the_plan_gauge_alerts_once_per_window` (engine.rs) |
| C-14.8 | Budget de run en dollars, tokens facturés, durée et itérations, relevable pour le run seul (voir C-9.10) | #136 | `wf/runs.rs`, `daemon/workflow.rs` | `a_run_budget_counts_billed_tokens_and_can_be_raised` (workflow.rs), `limits_are_checked` (runs.rs) |
| C-14.9 | Coût facturé plutôt qu'estimé ; attribution à la requête d'origine, au rôle, au fournisseur amont ; `usage` par neuf axes, filtres `--session` et `--since` | 0.2.2, #17 | `llm/provider.rs`, `kernel/budget.rs` | `billed_cost_wins_over_the_catalog` (provider.rs), `costs_are_attributed_to_sessions_and_requests`, `records_and_sums` (budget.rs), `routing_and_costs_are_readable_from_telegram` (telegram.rs) |
| C-14.10 | Coût de la transcription compté au rôle `stt`, de la compaction au rôle `compaction` sur le tour déclencheur, de la synthèse au rôle `tts` | 0.2.2, 0.2.6, #41 | `daemon/telegram.rs`, `daemon/compaction.rs`, `daemon/voice.rs` | `openrouter_transcription_reports_its_cost` (provider.rs) ; attribution du rôle `compaction` et `tts` : partiel |

### 2.15 Architecture et plateforme

| # | Capacité | Origine | Code | Test |
|---|---|---|---|---|
| C-15.1 | 17 crates aux dépendances orientées : `store` sans dépendance interne, `kernel` dépend de `store` seul, aucun cycle, aucun code ni dépendance propre à un OS hors `penelope-platform`, `unsafe` interdit partout, chaque crate a des sources ; règle vérifiée par un test, pas par la discipline | décision 0001, §3.1, §2.3 | `crates/penelope-archtest/src/lib.rs` | `the_workspace_is_discovered`, `ca_3_1_dependency_rules_hold`, `store_depends_on_no_business_crate`, `kernel_depends_only_on_store`, `there_is_no_dependency_cycle`, `ca_2_3_no_os_specific_code_outside_the_platform_crate`, `no_os_specific_dependencies_outside_the_platform_crate`, `unsafe_is_forbidden_everywhere`, `the_pattern_detector_actually_detects`, `every_crate_has_sources` (archtest) |
| C-15.2 | Surveillance des fichiers par scrutation (`TreeWatcher` : créations, modifications, suppressions, filtre d'extension, exclusions `.git`, `.dreams`, `archive`, arborescence), debounce 300 ms, resynchronisation complète toutes les 10 min ; vault, skills, workflows, gabarits, `mcp.d` rechargés à chaud | décision 0005, §2.9 | `platform/watcher.rs` (`TreeWatcher`, `Debouncer`, `DEBOUNCE`, `POLL_INTERVAL`, `FULL_RESYNC`) | `detects_create_modify_remove`, `extension_filter_applies`, `excluded_directories_are_skipped`, `nested_directories_are_walked`, `debouncer_groups_repeated_changes` (watcher.rs), `the_watcher_sees_creations_modifications_and_removals`, `bursts_are_debounced` (hot_reload.rs) |
| C-15.3 | Identifiants ULID (monotones sur l'horodatage, Crockford tolérant), jetons courts, horloge injectable (`TestClock`) et RFC 3339 | §4 | `kernel/ids.rs`, `kernel/clock.rs` | `ulid_roundtrip`, `ulid_is_monotonic_on_timestamp`, `ulid_timestamp_extraction`, `crockford_accepts_ambiguous_chars`, `short_token_length` (ids.rs), `test_clock_advances`, `rfc3339_formats` (clock.rs) |
| C-15.4 | Bus du daemon : fragments par `broadcast` (perte tolérée), issue d'un tour par attentes nominatives et cache (jamais perdue), origines transportées dans le payload, annulation par session, identifiants de brouillon stables | §3.3 | `daemon/bus.rs` | `origins_roundtrip_through_the_payload`, `a_late_waiter_still_gets_the_outcome`, `an_early_waiter_is_woken`, `cancelling_a_session_reaches_its_turn`, `draft_ids_are_stable_and_non_zero` (bus.rs) |
| C-15.5 | Backends Linux et Windows compilent et renvoient `Unsupported` ; nom de pipe Windows ; la suite entière tourne sur Linux sans bac à sable (`Services::for_tests` sans profil imposé) | #102, décision 0004 | `platform/backend/stub.rs`, `platform/ipc.rs` (`windows_pipe_name`) | `pipe_name_format` (ipc.rs) ; CI `ubuntu-latest` rejoue la suite ; `Unsupported` : aucun test dédié |
| C-15.6 | Composition unique du daemon (`runtime.rs`), sessions de test (`Services::for_tests`), `Platform::for_tests`, mocks LLM et Telegram : toute la suite tourne hors réseau et sans secret | §3.3 | `daemon/runtime.rs`, `platform/lib.rs`, `llm/mock.rs`, `tg/mock.rs` | `bootstrap_wires_everything` (runtime.rs), `bootstrap_for_tests_creates_the_tree` (platform/lib.rs) |

## 3. Limites assumées et documentées

La V1 n'a pas à les combler, mais ne doit pas les aggraver. Sources : `README.md`
« Ce qu'elle ne fait pas », `docs/telegram.md`, `docs/mcp.md`, `docs/workflows.md`
« Limites actuelles », `docs/install-headless.md` « Ce qui n'est pas encore branché »,
`docs/progress.md` « Autres manques » et « Encore à brancher », `selfdocs::known_limits`
(qui rassemble ces sections pour `self_docs limits`). Le test
`no_doc_presents_something_shipped_as_missing` refuse qu'une de ces sections décrive comme
manquante une méthode, une commande ou un outil livrés : la V1 doit garder ce test et ces
sections à jour.

- Plateforme : macOS seulement ; les backends Linux et Windows compilent et renvoyent
  `Unsupported` (`platform/backend/stub.rs`). Le bac à sable n'existe que sur macOS ; la
  suite entière tourne aussi sur Linux (CI `ubuntu-latest`), les tests Seatbelt, launchd et
  trousseau ne tournent que sur `macos-14`.
- Telegram seulement, en long polling ; `telegram.mode = "webhook"` et `webhook_url` ne
  sont pas servis. Un seul propriétaire par instance ; tout autre expéditeur est refusé.
- MCP : `sampling/createMessage` refusé et non annoncé ; en-têtes `Mcp-Param-*`
  (`x-mcp-header`) non couverts ; les filtres d'outils `include`/`exclude` d'Hermes n'ont pas
  d'équivalent (tous les outils exposés, `tool_policy` restreint) ; une tâche MCP n'est
  suivie qu'une fois connue d'une étape `wait` (un appel d'outil qui rend une tâche
  n'enregistre rien seul) ; `client_secret` optionnel documenté pour Slack sans usage tant
  que PKCE est actif.
- Documents : l'OCR ne tourne que sur macOS (Vision) et seulement si le PDF n'a aucune
  couche texte (un PDF mixte garde ses pages scannées illisibles) ; `lopdf` ignore la mise
  en page (colonnes, tableaux) et les polices sans table Unicode.
- Mémoire : recherche vectorielle exhaustive, à revoir au-delà de 200 000 entrées
  (décision 0002) ; le scoring de mémoire à cinq critères est moins riche que six signaux ;
  l'usage mesuré ordonne le rappel et sert de preuve à la grille mais ne promeut rien seul ;
  la confrontation à l'arbre d'accessibilité (vision `locate`) reste à l'agent.
- Workflows : `.penelope/deploy.toml` n'est pas interprété, c'est un marqueur ;
  `deploy-generic` passe par `make deploy`, `make smoke`, `make rollback` (décision 0007) ;
  le plan approuvé par « Vas-y » (#186) n'est pas encore exécuté (T3 de #185) ;
  `workflow_start` reste refusé depuis Telegram ; `review` reste écrit pour un dépôt Rust.
- Releases : la signature minisign est implémentée, la clé n'est pas créée : seule la
  somme SHA-256 est vérifiée aujourd'hui ; un lot fusionné n'est installable qu'une douzaine
  de minutes après (tag par la CI puis release) ; `--check` dit « à jour » du dernier
  binaire publié, pas du dernier commit.
- Suites réseau (`live-openrouter`, `live-telegram`, `ctx-recall`, `mem-longitudinal`,
  `mem-bench-live`, `ab-hermes`) écrites et jamais lancées par la CI ; `penelope import
  hermes` jamais exécuté sur l'instance réelle ; `ab-hermes` jamais joué contre Hermes.
- Contrôle `doctor` « tag sans release depuis 30 min » : non fait, assumé (#147).
- Clés de configuration acceptées mais sans effet (voir 1.6) : `telegram.topics`,
  `text_limit`, `caption_limit`, `webhook_url`, `allow_groups`, `memory.dedup_cosine`,
  `episode_idle`, `episode_topic_shift` (valeurs codées : 2 h et 0,35), `promotion.fact_*`,
  `preference_min_sessions`, `contested_*`, `prune_episodic_days`, `mcp.registry_mode`,
  `default_timeout`, `preferred_protocol`, `idle_timeout`, `max_concurrency_per_server`,
  `restart_backoff_max`, `observability.otlp_endpoint`, `prometheus`,
  `workflows.default_max_iterations`, `upgrade.channel`, `health_timeout`,
  `heartbeat_daily`. Le gabarit `heartbeat` et le genre de session `heartbeat` existent
  sans qu'un battement quotidien soit envoyé.
- Documentation : les captures d'écran de `docs/telegram.md` sont des maquettes ASCII ;
  `docs/progress.md` liste huit décisions dans sa table alors que `docs/README.md` en
  liste dix (0009 et 0010 manquent dans la table de `progress.md`).
- Maturité : une seule instance réelle en service ; les défauts connus viennent d'une
  revue adversariale du code, pas de milliers d'usagers ; tout est en pre-release 0.x.

## 4. Décisions d'architecture : à garder, à remettre en cause

| Décision | Contenu | Verdict pour la V1 |
|---|---|---|
| 0001 `penelope-kernel` dépend de `penelope-store` | Le noyau est la couche durable ; `store` est une infrastructure sans concept métier qui réexporte `rusqlite` ; règle encodée par `ca_3_1_dependency_rules_hold` | Reste valable. Si la V1 change la couche de persistance ou le découpage des crates, la garantie à préserver est la testabilité isolée des primitives durables (journal, ledger, file, sessions, budgets, générations) avec une base en mémoire et une horloge fictive, et l'archtest doit encoder la nouvelle règle de dépendance le jour où elle change. Aucun crate métier ne déclare `rusqlite`. |
| 0002 Pas de `sqlite-vec` | Vecteurs en BLOB `f32`, cosinus en Rust, 0.0 sur dimensions différentes, filtrage SQL avant la coupe aux 200 | Reste valable jusqu'à 200 000 lignes ; point de bascule à conserver dans la documentation ; le test `ingested_passages_do_not_evict_memories_from_recall` fixe le filtrage avant coupe. |
| 0003 Validateur JSON Schema local | `$ref` externe refusé, cycle borné, annotations lues par le moteur de formulaires, messages en français avec JSON Pointer, sous-ensemble 2020-12 | Reste valable : c'est une décision de sécurité (schémas MCP non fiables) et de rendu (formulaires Telegram). `schemas/workflow.schema.json` doit rester dans ce sous-ensemble. |
| 0004 IPC `tokio::net::UnixListener` | Socket `0600`, fichier résiduel retiré, jeton de session par démarrage, NDJSON identique quel que soit le transport, une façade `IpcListener`/`IpcStream` à porter | Reste valable. Si la V1 remplace l'IPC, elle garde : `0600`, jeton tiré à chaque démarrage et exigé sur chaque requête, comparaison à temps constant, sockets Unix fermées aux processus confinés, protocole JSON-RPC 2.0 inchangé (section 1.1). |
| 0005 Surveillance par scrutation | Horodatages et tailles, debounce 300 ms, resynchronisation complète toutes les 10 min comme mécanisme de vérité, exclusions | Reste valable. Si la V1 adopte un flux d'événements de fichiers, la resynchronisation périodique reste la vérité et les tests `hot_reload` doivent passer sans `sleep`. |
| 0006 Changement de sujet lexical | Racines de quatre lettres, cosinus, trois messages sous 0,35, messages courts neutres | Reste valable ; peut passer par l'index vectoriel sans changer le reste. Les bornes sûres (2 h, `/new`) restent. |
| 0007 `deploy-generic` par cibles `make` | `.penelope/deploy.toml` exigé mais non interprété ; `make deploy`, `make smoke`, `make rollback`, `ENV=` | Reste valable tant que `deploy.toml` n'est pas interprété ; un workflow utilisateur de même identifiant remplace le livré. |
| 0008 Cache de prompt : rien ne bouge avant le dernier message | Volatil figé avec son message (`message_context`), raisonnement avec les appels d'outil seulement, préfixe T0 à T2 modifié différé à un cache froid ou une compaction, fournisseur amont épinglé 10 min, empreinte et cause de chaque raté | **Doit être respectée par la nouvelle source de vérité de la V1.** Quelle que soit la façon dont la V1 assemble le prompt ou persiste l'historique, les invariants testables à reprendre tels quels sont `ca_5_3_prefix_is_byte_identical_across_turns`, `ca_5_4_each_request_extends_the_previous_one`, `assemble_puts_volatile_into_the_last_user_message`, `volatile_stays_with_the_user_message_during_tool_iterations`, `reasoning_goes_back_only_with_tool_calls`, `promotions_wait_for_a_compaction_boundary`, `a_running_turn_keeps_its_frozen_snapshot`, `the_previous_upstream_is_pinned_unless_routing_is_configured`, `a_miss_is_explained_by_what_changed`. Le lot non commité #205 (instantanés du prompt, tuile nommée dans la cause du raté) prolonge cette décision sans la contredire. Conséquence assumée à conserver : un souvenir, une skill ou un serveur MCP ajoutés en pleine conversation n'entrent dans le prompt qu'au prochain cache froid ou à la compaction suivante, leurs outils restant appelables tout de suite. |
| 0009 Pas d'émulation d'outils | Un modèle sans tool calling est refusé pour un alias à outils ; rôles de service acceptés ; parseur JSON tolérant conservé | Reste valable ; à rouvrir seulement avec un identifiant d'appel unique (ULID) et un test de bout en bout. |
| 0010 Fournisseur Codex | Identité empruntée et dite (`doctor` permanent), périmètre borné aux tours du propriétaire (garde unique avant le choix du fournisseur), quota du plan au lieu du dollar, un seul compte, `refresh_token` à usage unique, sortie écrite (`openai_compat` vers `api.openai.com`) | Reste valable ; la V1 garde les trois décisions, l'événement `llm.codex_scope_fallback`, le refus de `model set` sur un alias de fond et l'avertissement `provider.codex.identity`. |

Aucune des dix décisions n'est invalidée par le contrat fonctionnel. Celles que
l'architecture de la V1 peut toucher sont 0001 (persistance), 0004 (IPC), 0005 (surveillance)
et 0008 (assemblage du prompt) : pour chacune, la garantie à conserver est nommée ci-dessus,
et le test qui la fixe existe déjà.

## 5. Capacités sans test : les filets à écrire avant la refonte

### 5.1 Sans aucun test (3)

| # | Capacité | Filet à écrire |
|---|---|---|
| C-6.17 | Rappel quotidien des serveurs MCP en attente d'autorisation OAuth (`mcp.oauth.notified.<nom>`) | Une passe d'entretien avec horloge fictive : un serveur `auth_required` est rappelé une fois par jour au foyer, pas deux fois le même jour, plus jamais après autorisation. |
| C-7.17 | `model.route_test` (simulation du routage d'un message) | Appel RPC avec un message simple et un complexe : alias rendu, raison, sans appel réel au modèle. |
| C-10.8 | Tir manqué d'une planification pendant un arrêt rattrapé une fois au redémarrage | Planification `cron` due pendant l'arrêt, redémarrage : un seul tir, `runs` incrémenté une fois, `next_run` recalculé. |

### 5.2 Testées partiellement (43) : le comportement précis qui manque

| # | Ce qui manque |
|---|---|
| C-1.9 | Rappel de délégation tous les `budget.delegate_after_calls` appels (résultat d'outil annoté) ; coût du tour ajouté à la réponse au-delà de `show_turn_cost_usd`. |
| C-1.30 | Un titre posé à la main (`/title`, `/new <titre>`, `session title`) n'est jamais remplacé par le titre automatique. |
| C-1.33 | `outputSchema` d'un sous-agent validé en deux tentatives (seconde chance puis échec) ; outils en lecture par défaut d'un `sub_agent`. |
| C-1.35 | Le budget de schémas du premier tour reste sous 3 000 tokens avec `pourquoi` sans description (assertion dédiée). |
| C-1.40 | `penelope chat` interactif : Ctrl-C appelle `chat.stop` et sort avec 130 ; second Ctrl-C quitte sans attendre. |
| C-1.46 | Passe d'inventaire machine relancée toutes les heures et à chaque `doctor`. |
| C-2.19 | `history_expand` et `history_describe` appelés par l'outil (manifeste d'un nœud, pagination d'un intervalle brut). |
| C-2.23 | Événements `context.compaction_skipped` (avec chaque raison) et `context.compaction_failed` ; `penelope_compactions_total` par déclencheur et issue. |
| C-3.20 | Score d'audit dans le digest du lundi ; envoi du digest en document au-delà de `max_fragments`. |
| C-3.24 | Annulation d'une ingestion en cours par `/stop tout` (jeton traversant `ingest`, `summarise`, `chat_stream`). |
| C-3.25 | Outil `mem_neighbors` ; `concepts/_a-definir.md` repris par le digest ; `index.md` généré. |
| C-3.26 | Rattrapage de fond des vecteurs manquants après tour, réindexation et inscription d'outils ; bascule de l'ancien défaut d'embeddings au chargement avec avertissement. |
| C-3.31 | Commit périodique du vault (`vault_git_autocommit`), push vers `vault_git_remote`, `mem diff` et `mem diff --since dream`, `vault sync`. |
| C-3.33 | `mem restore <id>`, `mem learned`, `mem retry-rejected`, `/appris`, `/pratique`, `/oublie`, `/retiens`, `/note`, `/recall` (action, pas seulement l'écran). |
| C-4.13 | `/approvals` renvoie une demande restée bloquée avec des boutons neufs. |
| C-4.17 | Méthode `quiet` (lecture et réglage de `telegram.quiet_hours` à chaud). |
| C-5.14 | `workflow_start` attend le premier verdict du run (cinq secondes au plus) et joint une remarque quand le run est `blocked` ou `failed`. |
| C-5.16 | `time_now` (fuseau du propriétaire), `send_file` (envoi d'un fichier du workspace), `ask_user` en conversation (question, attente, réponse dans le résultat). |
| C-6.14 | Un `-32020` en conversation passe le serveur en `degraded` et dit au modèle de ne pas réessayer ; `mcp logs` rend stderr puis la fin du processus (sortie vide dite). |
| C-6.19 | Parcours Slack complet (client pré-enregistré, `callback_host = localhost`, portées du manifeste) contre un serveur d'autorisation simulé sans DCR ni CIMD. |
| C-7.13 | `model set` prévient quand un alias de consolidation ou de relecture reçoit un modèle qui impose de réfléchir ; `known: false` quand le catalogue ne connaît pas l'identifiant. |
| C-8.7 | Contrôle `doctor telegram_forms` (formulaire ouvert depuis plus d'une heure, avec son sujet). |
| C-8.14 | Création réelle d'un sujet de forum (`create_topic`) pour un run ou une session longue. |
| C-8.15 | Actions des écrans `/p` (exécution d'un prompt MCP avec formulaire d'arguments), `/secret rm`, `/logs <composant>`, `/config`, `/quiet`, `/intentions` (annuler), `/policies` (retirer une règle), `/skill rollback`, `/close`, `/rewind`, `/purge`, `/fork` (↪️), `/upgrade` (⬆️ et ⏪ confirmés). |
| C-8.22 | `/model auth codex`, `/model auth codex status`, `/model auth codex logout` depuis Telegram. |
| C-8.23 | `/projet <nom>` et `/projet aucun` depuis Telegram (écran et effet sur l'instantané). |
| C-8.34 | Envoi réel en blocs riches (`send_rich`) avec `telegram.rich_messages = true` et repli HTML sur refus de capacité (hors suite réseau). |
| C-8.35 | `/export` et `/audit` traités dans une tâche à part sans bloquer les updates suivants. |
| C-9.13 | Événement `workflow.subgroup_exited` (groupe, tag, étapes) émis à l'exécution ; `workflow.step` qui nomme les critères retenant une boucle. |
| C-10.2 | Événement `schedule.fired` sur le journal et le flux runtime après un déclenchement réussi. |
| C-10.9 | Déclenchement de la sauvegarde par `backup.cron` et alerte Telegram sur échec de la sauvegarde nocturne. |
| C-10.10 | `schedule_create` par l'outil : carte `schedule_preview`, création après approbation, refus motivé. |
| C-11.10 | Notification `tail` : réception des `StreamEvent` (fragments, journaux) par un client RPC abonné. |
| C-12.11 | Refus d'un dépôt de sauvegarde public (vérification `gh`). |
| C-12.12 | `penelope restore <fichier>` (base actuelle mise de côté) et `restore-all --dry-run` (liste sans écrire). |
| C-12.17 | Autocommit périodique du vault par la maintenance, inventaire machine horaire, rappel OAuth quotidien (voir C-6.17). |
| C-12.18 | Jobs CI `livraison` (tag posé une fois, jamais déplacé, `workflow_dispatch` de `release.yml`) et `scripts/bump.sh` (refus sur arbre sale, version déjà posée, section manquante). |
| C-12.23 | `chat.send` synchrone (réponse finale dans le résultat RPC). |
| C-13.12 | Refus d'un dépôt public à `backup --push` (même filet que C-12.11). |
| C-13.16 | Révocation des jetons Codex côté OpenAI si le rangement dans le magasin échoue après une connexion réussie. |
| C-14.3 | Relèvement du plafond du jour valable pour la journée (`budget.daily.<jour>`) avec renvoi de la demande ; reprise d'un run après relèvement de son budget par la carte. |
| C-14.10 | Attribution du coût de la compaction au rôle `compaction` sur le tour déclencheur et de la synthèse vocale au rôle `tts` dans `usage`. |
| C-15.5 | Backends Linux et Windows : `Unsupported` explicite pour le bac à sable, le service et le trousseau (test compilé sur Linux). |

### 5.3 Contrats publics sans test dédié

- Méthodes RPC servies mais sans test de comportement propre (couvertes seulement par
  `every_declared_method_is_either_served_or_explicitly_absent`) : `model.route_test`,
  `tail`, `quiet`, `config.reload`, `secret.list`, `secret.rm`, `secret.backend`,
  `policy.revoke`, `mem.restore`, `mem.learned`, `mem.retry_rejected`, `mem.diff`,
  `mem.show`, `intent.list`, `vault.sync`, `vault.lint` (via RPC), `eval.run`, `restore`,
  `skill.show`, `skill.reload` (via RPC), `wf.show`, `wf.trace`, `wf.runs`,
  `session.switch`, `session.close`, `session.title`, `session.mode`, `session.project`,
  `session.budget` (testés par Telegram, pas par la CLI), `chat.send`, `export` (run, all),
  `import.hermes` (module testé, méthode non), `upgrade` (module testé, méthode non).
- Commandes Telegram dont seule l'ouverture d'écran est testée (voir C-8.15).
- Contrôles `doctor` sans test unitaire dédié : `shell_lines`, `redactor`, `dream_power`,
  `telegram_forms`, `telegram.home`, `telegram.allowed_chats`, `clock`, hôtes joignables,
  `budget.day`, `memory.size`, `vault_index`, embeddings, `sandbox.deny_read`,
  `sandbox.shell_network`, `schedules`, `config_coherence`, `install_mode` (partiel),
  `secret_roundtrip` (testé sur un faux magasin), `macos.*`.
- Événements du journal sans test qui vérifie leur émission : `context.compaction_skipped`,
  `context.compaction_failed`, `schedule.fired`, `schedule.notified`, `workflow.subgroup_exited`,
  `workflow.control`, `workflow.question`, `memory.index_gap`, `memory.clash_unanswered`,
  `memory.episode_ingested`, `store.retention`, `store.rebuilt`, `voice.sent`,
  `daemon.recovered`, `import.hermes`, `skill.installed`, `session.rewound`,
  `session.forked`, `llm.provider_tokens_revoked`.
- Clés « sans effet » (section 3) : aucun test ne fixe qu'elles sont acceptées ; la V1 doit
  au moins les accepter sans erreur (`unknown_sections_and_keys_are_tolerated_and_named`
  couvre les clés inconnues, pas les clés connues inertes).

## 6. Comptes

- Capacités recensées (section 2) : **320**, réparties en conversation 52, contexte 23,
  mémoire 37, HITL 17, outils natifs 20, MCP 19, LLM 18, Telegram 35, workflows 15,
  planification 11, observabilité 16, exploitation 24, sécurité 17, budgets 10,
  architecture 6.
- Couvertes par au moins un test automatisé nommé : **274** (85,6 %).
- Partiellement couvertes (mécanisme testé, comportement nommé non testé) : **43**
  (13,4 %), détaillées en 5.2.
- Sans aucun test : **3** (0,9 %), détaillées en 5.1.
- Contrats publics exposés à l'identique (section 1) : 103 méthodes RPC (plus `audit.show`
  non commité), 25 commandes CLI de premier niveau et 12 groupes de sous-commandes, 50
  commandes Telegram, 26 gabarits, 59 types d'actions de boutons, 57 outils natifs (16 de
  noyau, 39 à la demande, 2 de workflow), 3 méta-outils MCP, 5 versions de protocole MCP,
  25 champs de déclaration `mcp.d`, 222 clés de configuration (26 sans effet), 75 types
  d'événements du journal, 6 types d'événements runtime, 9 types d'étapes et 9 types de
  conditions de workflow, 4 workflows livrés, 1 skill livrée, 7 niveaux de mémoire, 55
  tables et 17 migrations SQLite, 14 séries de métriques, 18 suites d'évaluation.
- Index des tests : 1 783 fonctions de test dans l'arbre de travail, dont 11 non commitées
  (#205) et 19 ignorées par défaut (réseau ou machine réelle) ; 71 tests `ca_*` couvrant 14
  sections de critères d'acceptation.
- Lot non commité à ne pas perdre : #205 (C-11.16), 22 tests, une méthode RPC, une
  commande CLI, une migration, un contrôle `doctor`.
