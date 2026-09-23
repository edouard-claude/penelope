# Fixture de base 0.17

`penelope-0.17.<version>.db` est une base SQLite **produite par la version du workspace
que son nom porte** : `Store::open` (toutes les migrations de `MIGRATIONS` appliquées),
puis un semis de données représentatives, puis `VACUUM`. Le test
`tests/migration_from_0_17.rs` en ouvre une copie et vérifie que le code courant la migre
entière, que son schéma est celui d'une base neuve, que chaque donnée semée se relit et
que la chaîne d'événements se vérifie encore (lot B, épopée #208 ; tâche T10 de
`design/v1/gel-et-outillage.md`).

C'est le filet qui manquait : `upgrade_from_each_previous_version` (`src/migrations.rs`)
ne rejoue que `0001` puis migre, sans base réelle. Ici, la base a été écrite par la 0.17,
avec ses index FTS5, ses tables d'ombre, ses lignes.

## Ce qu'elle contient

Une session `chat` liée à Telegram (`tg_chat_id`), douze messages (utilisateur,
assistant, deux résultats d'outil avec `tool_call_id`), leur index plein texte, un tour
terminé dans `turn_queue`, un nœud LCM condensé actif et la feuille qu'il remplace, un
contexte figé (`message_context`), sept événements chaînés écrits par `EventLog` (cinq
de session, un de run, un global), un effet `shell_exec` planifié puis `completed` par
`EffectLedger`, une ligne `usage` avec sa requête `llm_requests` et son prompt figé
`prompt_snapshots`, un souvenir `mem_entries` avec provenance, index, signaux, drapeau,
historique et candidat promu, une planification `cron` (forme d'après la migration
`0009`), une ligne `tg_outbox` envoyée, une mise à jour Telegram traitée, une approbation
décidée, une règle `policies`, trois clés `kv`, une génération de configuration.

Les identifiants sont des constantes en tête de `migration_from_0_17.rs`, partagées par
le générateur et par le test.

## Forme du fichier

Un seul fichier, journal en mode `delete` (aucun `-wal` ni `-shm` à côté), pages de
1 Kio. Une base neuve compte près de 170 objets (tables, index, tables d'ombre FTS5) et
chacun occupe au moins une page : en pages de 4 Kio elle pèserait plus de 600 Ko presque
vides ; en 1 Kio, `penelope-0.17.59.db` fait 216 064 octets (211 pages). `Store::open`
lit toute taille de page et remet la base en WAL.

Le fichier est binaire et suivi par git malgré `*.db` dans `.gitignore` (ligne de
négation `!crates/penelope-store/tests/fixtures/*.db`).

## Régénérer

```bash
UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored
```

Le générateur écrit `tests/fixtures/penelope-<CARGO_PKG_VERSION>.db`. Sans
`UPDATE_FIXTURE=1`, il produit et vérifie la base dans un répertoire temporaire, sans
rien écrire dans le dépôt.

**Quand.** Une seule fois, quand la 0.17 finale est connue (le dernier tag `0.17.x`
avant la bascule, décision 0015) : la fixture doit être la base qu'une instance en
service laissera derrière elle au moment de passer à la 1.0. Jusque-là, la 0.17.59
tient ce rôle.

**Quand ne pas.** Jamais à chaque migration nouvelle. Le test vaut par l'écart entre la
fixture et le code : régénérer après chaque migration reviendrait à tester une base neuve
et à retomber dans le défaut de `upgrade_from_each_previous_version`. Une migration
nouvelle doit s'appliquer sur cette fixture telle quelle ; si elle ne le peut pas, c'est
la migration qu'il faut corriger, pas la fixture. Le nom du fichier dit d'où vient la
base, pour que personne n'ait à deviner.

**Plusieurs fixtures.** Le test rejoue chaque `penelope-*.db` du répertoire. Régénérer
sur une version plus récente n'oblige pas à retirer l'ancienne : la garder, c'est garder
une base d'une version antérieure dans le filet (une fixture pèse environ 210 Ko).

**Si les constantes changent.** Modifier les identifiants ou les lignes semées dans
`migration_from_0_17.rs` impose de régénérer, sinon le test relit d'autres valeurs que
celles de la base.
