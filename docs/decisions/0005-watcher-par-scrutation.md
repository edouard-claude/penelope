# 0005 — Surveillance des fichiers par scrutation, pas par `notify`

Statut : acceptée. Portée : §2.9 (rechargement à chaud).

## Contexte

Le vault, les skills, les workflows, les gabarits et `mcp.d/` sont rechargés à chaud
quand leurs fichiers changent. Le réflexe est d'utiliser `notify` (FSEvents sur macOS,
inotify sur Linux).

## Décision

Scrutation périodique des horodatages et des tailles, avec un debounce de 300 ms.

## Raisons

1. **Le PRD impose déjà une resynchronisation complète périodique** (10 minutes par
   défaut) « pour couvrir les pertes d'événements propres à chaque backend ». Cette
   resynchronisation est donc le mécanisme de vérité. Garder en plus un flux
   d'événements, c'est maintenir deux chemins dont un seul fait foi.
2. **Les volumes sont petits.** Quelques centaines de fichiers Markdown, JSON et TOML.
   Un parcours complet coûte quelques millisecondes.
3. **C'est testable.** Un `TreeWatcher` se pilote avec une horloge fictive et des
   écritures de fichiers ; les créations, modifications et suppressions sont vérifiées
   sans course ni `sleep`. Un backend d'événements se teste beaucoup moins bien, et
   diffère selon l'OS, ce que le test d'architecture interdit hors de `penelope-platform`.
4. **FSEvents perd des événements** lors d'un `git checkout` massif ou d'une écriture
   atomique par `rename`. Le debounce plus la scrutation traitent ces cas de la même
   façon qu'un changement ordinaire.

## Conséquences

- La latence de détection vaut l'intervalle de scrutation, pas quelques millisecondes.
  Acceptable : un fichier déposé est pris en compte « au tour suivant », ce qui est la
  garantie que le PRD demande (§7.4).
- `TreeWatcher` filtre par extension et exclut `.git`, `.dreams` et `archive`, sinon le
  coût du parcours dériverait avec l'historique du vault.
- Si un jour un répertoire surveillé devient gros (vault de plusieurs dizaines de
  milliers de notes), il faudra revoir ce choix.
