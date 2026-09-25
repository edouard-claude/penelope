# Lot K : mesure avant le juge d'approbation (épopée #208, T21)

Agent `k-juge`, branche `v1-k-juge`, base 1.0.0-alpha.13 (`12b187e`).
Spécification : `design/v1/boucle-et-outils.md` §5 T21, arbitrage 6 (`design/v1/README.md`
§10), issue #203 section « Mesure préalable ».

## Ce qui est livré

- `penelope approvals stats [--days 30] [--json]` (`crates/penelope-cli/src/commands/approval_stats.rs`) :
  cartes `shell_exec` de la fenêtre, celles sans motif possible, celles que le juge verrait
  (ni `double`, ni risque `destructive`), commandes distinctes, dix plus fréquentes
  (40 caractères après `penelope_observe::redact::redact`, douze caractères du SHA-256 de
  la ligne normalisée entière), états finaux, part de « oui » parmi les cartes tranchées,
  verdict go/no-go avec la raison de chaque seuil manqué.
- Tests (`approval_stats/tests.rs`) sur une base construite par `Store::open` (le vrai
  schéma) : comptes exacts sur une fixture de dix cartes (doublon à la mise en page près,
  secret, destructif, double confirmation, ligne simple, hors fenêtre, autre outil) ;
  empreinte de la base et de son WAL identique avant et après, daemon « lancé » (store
  ouvert) et « arrêté » (copie `VACUUM INTO` seule dans son répertoire : aucun fichier
  créé) ; écritures refusées par la connexion ; secret absent des sorties texte et JSON.
- `docs/install-headless.md`, « Mesurer avant d'activer le juge » : commande, ce qui est
  compté, comment la lancer sur une instance 0.17, seuils motivés.
- Déplacement pur de `penelope logs` dans `commands/logs.rs`, pour que `commands.rs`
  (au plafond du gel) descende malgré la nouvelle sous-commande.

## Choix

- **Sous-commande Rust plutôt que script shell.** Le caractère « sans motif » n'est pas
  en base : `rule_created` vaut `always` ou `NULL`, et `NULL` couvre aussi un simple
  « Autoriser » sur une ligne qui avait un motif. Il faut donc rejouer le prédicat de la
  carte, `always_creates_no_rule`, donc le lexer `cmdline` : un script `sqlite3` ne
  pourrait qu'en approcher la forme par des `LIKE '%;%'`. La sous-commande est testable
  avec le vrai schéma et le vrai prédicat.
- **Sans RPC.** Le daemon est gelé (R4, R5) ; la base s'ouvre directement avec
  `SQLITE_OPEN_READ_ONLY` et `PRAGMA query_only=ON`, sans `Store::open` (qui migre).
  Dépendance `rusqlite` ajoutée au CLI (déjà dans le workspace), raison écrite dans son
  `Cargo.toml`.
- `penelope approvals` sans sous-commande reste la liste des demandes en attente (RPC).
- La fenêtre est `[maintenant - N jours, maintenant]` sur `created_at` ; une date
  illisible est ignorée plutôt que comptée.
- Le prédicat est celui du binaire qui mesure : une ligne que le lexer de `v1` sait lire
  mieux que celui de la 0.17 n'est plus comptée. C'est voulu (le juge serait branché sur
  `v1`), et dit dans la doc.
- Seuils go : au moins 10 cartes éligibles par semaine, 5 commandes distinctes, 80 % de
  « oui » parmi les cartes tranchées. Motifs dans la doc.

## Commentaire prêt à coller dans #203 (non posté)

> Mesure préalable (T21, épopée #208) : l'outil est sur la branche `v1`.
>
> Sur l'instance, depuis un checkout de `v1` :
>
> ```bash
> cargo build --release -p penelope-cli
> ./target/release/penelope --home <racine de l'instance> approvals stats --days 30
> ./target/release/penelope --home <racine de l'instance> approvals stats --days 30 --json
> ```
>
> Lecture seule (`SQLITE_OPEN_READ_ONLY`, `query_only`), daemon lancé ou non, aucune
> migration. La sortie donne : cartes `shell_exec` sur 30 jours, cartes sans motif
> possible, dont éligibles au juge (ni double confirmation, ni destructif), par semaine,
> commandes distinctes, les dix plus fréquentes (tronquées, secrets masqués, empreinte),
> états, part de « oui ».
>
> Seuil proposé pour T22 : **go** si au moins 10 cartes éligibles par semaine, au moins 5
> commandes distinctes et au moins 80 % de « oui » parmi les cartes tranchées ; sinon
> no-go, et chaque seuil manqué dit quoi faire à la place (répondre aux cartes, correctif
> ciblé du lexer ou consigne, ou laisser les cartes faire leur travail).
>
> Résultat sur l'instance : _à coller ici (sortie `--json`)_. Décision : _go / no-go_.

## Notes de version (pour docs/progress.md)

#### Mesure avant le juge d'approbation (épopée #208, lot K, T21)

- `penelope approvals stats [--days 30] [--json]` compte, en lecture seule et sans daemon,
  les cartes `shell_exec` pour lesquelles « Toujours » n'écrit aucune règle, celles que le
  juge de #203 verrait, les commandes distinctes, les dix plus fréquentes (tronquées,
  secrets masqués, hachées) et la part de « oui », avec un verdict go/no-go pour T22.
- La base est ouverte en `SQLITE_OPEN_READ_ONLY` et `query_only` : les tests prouvent
  qu'elle ne change pas, daemon lancé ou arrêté.
- `docs/install-headless.md` : « Mesurer avant d'activer le juge », avec les seuils motivés.

## Blocages

Aucun. La mesure elle-même reste à faire par le propriétaire sur l'instance.
