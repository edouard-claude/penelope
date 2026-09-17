---
name: wiki-markdown
description: Règles d'écriture du vault, un wiki Markdown (propriétés YAML, wikilinks, identifiants de bloc, log.md), à suivre avant toute note écrite ou modifiée à la main.
version: 1.0.0
declencheurs:
  - vault
  - wiki
  - note
  - wikilink
  - frontmatter
---
# Écrire dans le wiki Markdown du vault

Le vault est un wiki Markdown : des fichiers texte, des propriétés YAML, des wikilinks. Il
reste valide à tout moment, y compris quand le propriétaire l'édite en même temps. Les
outils `mem_*` appliquent ces règles d'eux-mêmes ; elles valent pour toute écriture directe.

## Propriétés YAML

- Frontmatter entre deux lignes `---`, en tête de fichier.
- `type` sur toute note : `journal`, `source`, `concept`, `profil`, `memoire`, `projets`,
  `notes`, `accueil`, `audit`, `revue`, `index`, `log`, `entite`, `pratique`.
- `created` et `updated` au format `AAAA-MM-JJ`, sans guillemets.
- `aliases` et `tags` toujours en listes à tirets, jamais en chaîne séparée par des virgules :

  ```yaml
  aliases:
    - Factur-X
    - ZUGFeRD
  tags:
    - concepts
  ```

- Tags au pluriel, sans espace, jamais composés uniquement de chiffres.
- Un wikilink dans une propriété s'écrit entre guillemets : `source: "[[contrat.pdf]]"`.
- Une valeur qui contient `: `, ` #`, ou qui commence par `[`, `-`, `>`, `|`, `*`, `&`, `!`,
  `%`, `@` ou un guillemet s'écrit entre guillemets.

## Entrées et identifiants de bloc

- Une entrée de mémoire est un élément de liste d'une ligne, terminé par son identifiant de
  bloc : `- Le serveur de prod est à Lyon <!-- importance: 7 --> ^01J9ABC`.
- Identifiant : lettres latines, chiffres et tirets seulement, unique dans la note.
- Les annotations (`importance`, `declencheurs`, `quand`, `expire`, `sensible`…) restent des
  commentaires HTML placés avant l'identifiant.
- Pour une citation ou un encadré (`> [!abstract] …`), l'identifiant va seul sur sa ligne,
  entouré de lignes vides.
- Ne jamais changer ni recopier l'identifiant d'une entrée existante : la provenance le suit.
- Viser une entrée : `[[memoire#^01J9ABC]]`.

## Wikilinks et noms de fichiers

- Un wikilink vise un nom de fichier, sans chemin ni `.md` : `[[factur-x]]`.
- Chaque nom de fichier est unique dans tout le vault. S'il ne l'est pas, écrire le chemin :
  `[[sources/yobbu]]`. Un nom neuf déjà pris reçoit un suffixe (`yobbu-concept`).
- Noms de fichiers sans `# | ^ : %% [[ ]]`, en minuscules et tirets.
- Texte affiché différent : `[[factur-x|la norme]]`. Pièce jointe embarquée : `![[contrat.pdf]]`.
- Ne jamais renommer ni déplacer une note sans réécrire tous les wikilinks qui la visent.

## Emplacements

- `journal/AAAA-MM-JJ.md` : note du jour, `type: journal` et `date`.
- `sources/<slug>.md` : fiche d'un document ; l'original, immuable, est dans `attachments/`.
- `concepts/<slug>.md` : une page par concept, `aliases` en liste, section `## Sources`.
- `accueil/accueil-AAAA-MM-JJ.md`, `audits/audit-AAAA-MM-JJ.md` : comptes rendus.
- `log.md` : journal des opérations en ajout seul, une ligne `## [AAAA-MM-JJ] <op> | <titre>`
  par opération (`ingest`, `dream`, `accueil`, `lint`). Ne jamais réécrire une ligne passée.
- Dossiers cachés (configuration d'éditeur) : ne jamais y écrire.

## Vérifier

`penelope vault lint` signale liens non résolus, notes orphelines et impasses, alias et
noms en double, identifiants de bloc invalides ou dupliqués, propriétés mal typées, et
propose de trancher les entrées expirées et les contradictions.
