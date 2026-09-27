# Notes de livraison : lot B, filets, fixture de base 0.17 et configuration (épopée #208, T10)

Branche `v1-b-fixture`, dérivée de `v1` à `a9c4214`, poussée sur `origin`. Périmètre tenu :
`crates/penelope-store/tests/migration_from_0_17.rs`,
`crates/penelope-store/tests/fixtures/{penelope-0.17.59.db,README.md}`,
`crates/penelope-store/Cargo.toml` (une dev-dependency), `Cargo.lock` (la même),
`crates/penelope-kernel/tests/config_0_17.rs`,
`crates/penelope-kernel/tests/fixtures/config-0.17.toml`, `.gitignore` (une négation), et
ce fichier. Aucun fichier de `src/`, aucun fichier de `docs/`, aucune ligne `version =`.

## Ce qui est livré

| Commit | Contenu |
|---|---|
| `9c8b2be` | La fixture `penelope-0.17.59.db` et son générateur `generate_fixture` (`#[ignore]`, `UPDATE_FIXTURE=1`). `Store::open` sur une base neuve (les 18 migrations), semis par SQL direct et par `EventLog` / `EffectLedger`, `VACUUM INTO` (la sauvegarde de la 0.17) puis `VACUUM` en pages de 1 Kio, journal en mode `delete`. Sans la variable, le générateur produit et vérifie la base dans un répertoire temporaire. `penelope-kernel` en dev-dependency de `penelope-store`, raison en commentaire. `.gitignore` dé-ignore `crates/penelope-store/tests/fixtures/*.db`. `tests/fixtures/README.md`. |
| `b1da908` | `a_real_0_17_database_migrates_and_reads_back` : copie de chaque `penelope-*.db` du répertoire, `Store::open`, puis `schema_migrations` complète et ordonnée, schéma migré identique à celui d'une base neuve (colonnes de chaque table par `PRAGMA table_info`, définition de chaque index, différence nommée), les 45 tables du PRD, chaque donnée semée relue (compte et valeur), chaîne vérifiée par `EventLog::verify` puis prolongée d'un événement et revérifiée, `integrity_check`. Une fixture plus récente que le code est refusée. |
| `39aa326` | `config-0.17.toml` (222 clés, chacune avec une valeur valide) et `config_0_17.rs` : chargement par `ConfigStore::load_or_create` sans clé inconnue, `validate` sans erreur, aucune contradiction (ni refus ni avertissement), valeurs relues, fichier non réécrit ; et l'égalité des clés feuilles du fichier avec le bloc généré `reference:config` de `docs/install-headless.md`, segment libre ramené à `<nom>`, clés manquantes et clés en trop nommées. |

## La fixture

- **Taille** : 216 064 octets (211 pages de 1 Kio, 169 objets dans `sqlite_master`), sous
  les 300 Ko visés. En pages de 4 Kio elle aurait dépassé 600 Ko : près de 170 objets
  (tables, index, tables d'ombre FTS5) qui occupent chacun au moins une page. `Store::open`
  lit toute taille de page et remet la base en WAL ; `PRAGMA integrity_check` rend `ok`
  avant et après migration.
- **Tables semées** (compte) : `sessions` (1, `chat`, `tg_chat_id`), `episodes` (1),
  `messages` (12 : 4 `user`, 6 `assistant`, 2 `tool` avec `tool_call_id`), `messages_fts`
  (10), `turn_queue` (1, `done`), `lcm_nodes` (2 : un condensé actif, la feuille qu'il
  remplace), `lcm_edges` (1), `message_context` (1), `events` (7 par `EventLog` : 5 de
  session, 1 de run, 1 global), `effects` (1, `shell_exec`, `completed` par
  `EffectLedger`), `usage` (1), `llm_requests` (1, forme d'après `0018`), `prompt_snapshots`
  (1), `mem_entries` (1) avec `mem_fts`, `mem_provenance` (`owner`), `mem_signals`,
  `mem_flags`, `mem_history`, `mem_candidates` (`promoted`), `schedules` (1, `cron`, cible
  en `origin_session` comme après `0009`), `tg_outbox` (1, `sent`, `message_id`),
  `tg_updates` (1), `approval_requests` (1, `approved` via `telegram`), `policies` (1,
  `shell_exec`, `auto`), `kv` (3), `config_generations` (1), `subsystem_apply_results` (1).
- **Ce qui vient du noyau et non du SQL** : les sept événements (hachage réel, `seq` par
  session) et l'effet (`plan`, `dispatching`, `complete`, clé d'idempotence réelle). C'est
  la raison de la dev-dependency `penelope-kernel` dans `penelope-store` : sans elle, le
  test aurait réinventé SHA-256 ou figé un hachage à la main. Cargo accepte ce cycle pour
  les tests d'intégration (comme `tokio` et `tokio-test`) ; `archtest` ignore les
  dev-dependencies dans `dependency_cycles` (`lib.rs:62-70`), le graphe livré ne change pas.
- **Régénération** : `UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored`,
  une seule fois, quand la 0.17 finale est connue ; jamais à chaque migration, sinon le test
  retombe dans le défaut de `upgrade_from_each_previous_version`. Le README des fixtures le
  dit, avec le cas de plusieurs fixtures (le test les rejoue toutes).
- **Preuve que le test mord** : une copie altérée (index `usage_turn` supprimé, `0018`
  retirée de `schema_migrations` avec sa table et ses trois colonnes) a été migrée par
  `Store::open`, la `0018` rejouée, et le test a échoué sur le seul index manquant :
  « `index:usage_turn` : seulement dans la base neuve ». La copie a été supprimée.

## Clés de configuration sans valeur valide évidente

- **Tables à clés libres** : `providers.extra.<nom>` (7 clés) et
  `context.model_thresholds.<nom>` (1 clé) n'ont pas de valeur par défaut ; le fichier
  porte une entrée inventée par table (`[providers.extra.ollama]`, endpoint désactivé ;
  `"openrouter:deepseek/deepseek-v4-pro" = 0.6`). Le test ramène le segment libre à `<nom>`
  pour comparer avec la référence.
- **`observability.runtime_consumers`** : une seule ligne dans la référence (`[]`), mais
  une liste de tables (`name`, `token_secret`, `kinds`). Un consommateur déclaré exerce la
  validation de `runtime_stream_bind` (127.0.0.1, port non nul). Le jeton est une référence
  au magasin de secrets, jamais une valeur.
- **Chaînes vides qui veulent dire « aucun »** : `telegram.webhook_url`,
  `mcp.public_callback_url`, `mcp.cimd_url`, `upgrade.base_url`, `upgrade.minisign_pubkey`,
  `upgrade.codesign_identity`, `memory.vault_git_remote`, `backup.git_remote`,
  `observability.otlp_endpoint`, `tools.shell`, `providers.local.api_key`,
  `providers.openrouter.routing.sort`. Portées vides, comme le défaut : une valeur non
  vide changerait le mode (`webhook`, `public_callback`) ou pointerait vers un dépôt.
- **Pour éviter l'avertissement « alias vers un fournisseur désactivé »** : les alias
  `stt` et `tts` visent `openai_compat:` ; le fichier met `providers.local.enabled = true`
  (avec ses deux modèles). Sinon le test aurait dû tolérer deux avertissements.
- **`providers.codex.client_id`, `client_version`, `originator`** : les défauts recopiés
  (l'identifiant OAuth de Codex CLI) ; toute autre valeur serait fausse.
- **Clés « sans effet » ou « ignorées depuis 0.14.0 »** (`telegram.topics`, `text_limit`,
  `caption_limit`, `memory.dedup_cosine`, `episode_idle`, `episode_topic_shift`,
  `promotion.fact_min_*`, `preference_min_sessions`, `contested_*`, `prune_episodic_days`,
  `mcp.registry_mode`, `default_timeout`, `preferred_protocol`, `idle_timeout`,
  `max_concurrency_per_server`, `restart_backoff_max`, `observability.otlp_endpoint`,
  `prometheus`, `workflows.default_max_iterations`, `upgrade.channel`, `health_timeout`,
  `heartbeat_daily`) : portées avec leur défaut. La V1 peut les garder inertes, pas les
  refuser (contrat fonctionnel §1.6).
- **Incohérence relevée, hors périmètre** : la référence dit `sandbox.default_profile` :
  `read-only`, `workspace-write` ou `full` ; `Config::validate` (`config.rs`) accepte
  `readonly`, `workspace-write`, `mcp-stdio`, `full`. Le fichier garde `workspace-write`.
  À corriger côté commentaire de doc ou côté `validate`.

## Vérifications

- `cargo fmt --all --check` : propre après chaque commit.
- `cargo clippy -p penelope-store -p penelope-kernel --all-targets -- -D warnings` : propre.
- `cargo test -p penelope-store` : 27 tests unitaires et 1 test d'intégration verts, le
  générateur ignoré ; lancé aussi avec `-- --ignored` (sans et avec `UPDATE_FIXTURE=1`).
- `cargo test -p penelope-kernel --test config_0_17` : 2 tests verts ; retirer
  `telegram.max_fragments` du fichier fait nommer la clé.
- `git check-ignore -v` : la fixture est suivie par la négation de `.gitignore` ; git la
  voit binaire (`Bin 0 -> 216064 bytes`).
- `cargo test --workspace` : lancé une fois à la fin, vert (51 suites, 1 760 tests passés, 0 échec, 20 ignorés, sortie 0).

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Filets de migration et de configuration (#208, T10)

- **Une vraie base 0.17 dans le dépôt** : `crates/penelope-store/tests/fixtures/penelope-0.17.59.db`
  (216 064 octets, pages de 1 Kio), produite par la 0.17.59 (`Store::open`, semis
  représentatif, `VACUUM`) : session `chat` liée à Telegram, douze messages dont deux
  résultats d'outil, index plein texte, tour terminé, nœud LCM actif, contexte figé, sept
  événements chaînés par `EventLog`, un effet `completed` par `EffectLedger`, usage et
  requête, souvenir avec provenance, planification, outbox envoyée, approbation décidée,
  règle, clés `kv`, génération de configuration. Le test
  `a_real_0_17_database_migrates_and_reads_back` ouvre une copie par `Store::open` et
  vérifie les migrations, le schéma (identique à une base neuve, table par table et index
  par index), les 45 tables du PRD, chaque donnée semée, la chaîne d'événements avant et
  après un événement de plus, l'intégrité. Jusque-là `upgrade_from_each_previous_version`
  ne rejouait que `0001` puis migrait, sans base réelle.
- **Régénération** : `UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored`,
  une fois, quand la 0.17 finale est connue ; jamais à chaque migration
  (`tests/fixtures/README.md`). `penelope-kernel` en dev-dependency de `penelope-store`
  pour semer et vérifier la chaîne par les vraies primitives ; le graphe livré ne change pas.
- **Configuration 0.17 complète relue** : `crates/penelope-kernel/tests/fixtures/config-0.17.toml`
  porte les 222 clés de la référence générée d'`install-headless.md` ; `config_0_17.rs`
  la charge par `ConfigStore::load_or_create` sans clé inconnue, valide, sans
  contradiction, et refuse que le fichier prenne du retard sur la référence (la clé
  manquante est nommée).
```

## Blocages

Aucun. À relire par l'intégrateur :

- le numéro de version et la section `### 0.17.x` (ou `1.0.0-alpha.N`) qui recevra les
  notes ci-dessus : hors de mon périmètre (`docs/`, `version =`) ;
- la dev-dependency cyclique `penelope-store` → `penelope-kernel` : acceptée par Cargo et
  par `archtest`, mais elle fait compiler `penelope-kernel` et ses dépendances pour
  `cargo test -p penelope-store` ; si elle gêne, l'alternative est de déplacer les deux
  tests dans `penelope-kernel/tests/` (qui dépend déjà du store) ;
- l'incohérence `read-only` / `readonly` de `sandbox.default_profile` (doc contre
  `validate`), hors périmètre ;
- quand la 0.17 finale sera taguée : régénérer la fixture une fois, garder ou retirer
  `penelope-0.17.59.db` (le test rejoue chaque fixture présente).
