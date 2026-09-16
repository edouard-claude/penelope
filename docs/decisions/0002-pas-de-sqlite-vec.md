# 0002 — Pas de `sqlite-vec` : recherche vectorielle exhaustive en Rust

Statut : acceptée. Portée : §6.11 (index de mémoire).

## Contexte

Le PRD demande une recherche sémantique sur la mémoire, et précise qu'elle DEVRAIT rester
**exhaustive** tant que la table `mem_vec` reste sous 200 000 lignes. L'extension
`sqlite-vec` est le choix évident sur le papier.

## Décision

Les embeddings sont stockés en BLOB (`f32` little-endian) et la recherche est une boucle
Rust qui calcule la similarité cosinus sur tous les vecteurs candidats.

## Raisons

- **Même sémantique.** À l'échelle visée, exhaustif contre exhaustif : le résultat est
  identique, seul le lieu du calcul change.
- **Une dépendance C de moins.** `rusqlite` est déjà compilé en `bundled` ; ajouter une
  extension chargée dynamiquement, avec sa propre compatibilité de version et son propre
  chemin de chargement, complique l'installation headless pour rien.
- **Robustesse au changement de modèle.** Le calcul rend 0.0 quand les dimensions
  diffèrent, ce qui arrive pendant une ré-indexation avec un nouveau modèle d'embedding.
  C'est une décision métier, pas un détail que l'on veut caché dans une extension.
- **Filtrage d'abord.** Le rappel filtre par portée, niveau et fraîcheur avant de comparer
  les vecteurs : le nombre réel de comparaisons est très inférieur au total.

## Conséquences

- `penelope-store::vector` expose `encode_embedding`, `decode_embedding` et
  `cosine_similarity`, testés unitairement.
- Au-delà de 200 000 lignes, la décision doit être réexaminée : un index approximatif
  deviendrait nécessaire. Le point de bascule est écrit ici pour ne pas être oublié.
