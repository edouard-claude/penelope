# Notes de livraison : lot I, avertissement des forks à la purge (épopée #208)

Branche `v1-i-purge-cli`, dérivée de `v1` à `95ec5ea` (1.0.0-alpha.8). Aucune migration.

## Ce qui est livré

- **Arbitrage 3** (design/v1/README.md §10) : `penelope session purge` prévient avant
  d'agir quand la session a des forks. Message du propriétaire, nombre en toutes lettres
  de un à dix, en chiffres au-delà : « Cette session a deux forks, ils perdront leur
  début : s_… « Titre (24/09) », s_… ». Au singulier : « un fork, il perdra son début ».
- **Lecture préalable** : méthode RPC nouvelle `session.purge_preview` (`session`,
  `title`, `forks: [{id, title}]`, `avertissement` si des forks). Elle n'efface rien et
  ne touche pas à `session.purge`. Choisie plutôt qu'un paramètre `preview` de
  `session.purge` : un daemon plus ancien qui ignorerait ce paramètre purgerait ; une
  méthode inconnue, elle, est refusée.
- **CLI** (`crates/penelope-cli/src/commands/purge.rs`, nouveau) : sans `--yes`, lecture
  préalable, avertissement, question. Sans terminal sur l'entrée standard et sans
  `--yes`, refus explicite (erreur d'usage, rien d'effacé) : avant, une entrée vide
  valait « non » en silence. `--yes` existait déjà et saute tout, lecture comprise.
  `commands.rs` perd onze lignes.
- **Telegram** : `/purge` existait avec un écran de confirmation ; l'avertissement y est
  cité avant la question.
- **Rapport de purge et journal du daemon** : même message (`forks_warning`), au lieu de
  « N session(s) née(s) d'un fork… ».
- **Docs** : `docs/install-headless.md` (Effacer une conversation) ; golden
  `session.purge_preview.json`.

## Tests

- CLI : avec forks (avertissement avant la question), sans forks, non interactif (refus,
  avertissement quand même affiché), `--yes` (route directe vers `session.purge`).
- Daemon : `purging_a_forked_session_warns_about_its_forks` passe par le contrat RPC
  (`session.purge_preview` nomme le fork, n'efface rien) ;
  `the_fork_warning_counts_in_words_up_to_ten` (1, 2, 10, 11).
- Telegram : `purging_a_forked_session_warns_on_the_confirmation_screen`.

## Notes de version

#### Purge : les forks sont prévenus avant

`penelope session purge` et `/purge` disent, avant de demander confirmation, quelles
sessions nées d'un fork perdront leur début (« Cette session a deux forks, ils perdront
leur début : … »). Sans terminal pour répondre, la commande refuse sans `--yes` au lieu
de ne rien faire en silence. Nouvelle méthode RPC `session.purge_preview`, lecture seule.

## Reste et blocages

- Plafond `[crates]` du daemon : une trentaine de lignes de plus (`preview`), à poser
  par l'intégrateur si rouge.
