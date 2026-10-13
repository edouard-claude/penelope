# Atelier

Petit inventaire en ligne de commande, en Rust, sans dépendance.

- `cargo test` doit rester vert ; chaque changement de comportement vient avec son test
  dans `tests/`.
- `cargo fmt` avant de commiter ; un commit par story, message en français qui dit quoi et
  pourquoi.
- Les prix sont manipulés en centimes (`u32`) ; l'affichage passe par `format_cents`.
- Pas de `unsafe`, pas de `unwrap` hors des tests.
