# 0023 : Des hooks de cycle de vie qui ne peuvent que resserrer

Statut : acceptée (16 octobre 2026). Portée : boucle d'agent, politique d'approbation
(décision 0014), exécuteur. Épopée #386, issues #382 à #385.

## Contexte

Le 16/10, la lecture de Claude Desktop (claude-agent-sdk 0.3.299) a montré des hooks de
cycle de vie déclarés par l'utilisateur, que Pénélope n'avait pas : le seul hook était le
`--hook` de mission (#366), codé en dur, un événement, une ligne de shell. Le propriétaire
veut brancher ses contrôles (format, lint, test ciblé, garde-fou maison) sans Rust ni
release.

## Décision

1. Un hook est un fichier de `hooks.d/`, rechargé à chaud, déclaré par le propriétaire.
   Jamais un fichier de dépôt : l'agent peut écrire dans un dépôt, et un hook qu'il aurait
   écrit passerait outre la politique du shell (revue de sécurité du 14/10). Les outils de
   fichiers refusent d'écrire dans `hooks.d/`.
2. Forme exec **sans shell** : `command` + `args[]`, chaque élément substitué à part ; une
   valeur qui porte un caractère que le shell interprète ne lance pas le hook. Seul le
   `--hook` de mission garde une ligne de shell, tapée par le propriétaire.
3. **Pas de verdict `allow`.** Un hook lit les arguments et les sorties d'outils, donc des
   données non fiables ; il peut signaler, bloquer, forcer une carte ou resserrer des
   arguments (#384), jamais desserrer. Le propriétaire qui veut auto-approuver a déjà les
   règles bornées et le mode de session.
4. Un hook s'exécute sous un bac à sable jamais plus large que celui de la configuration ;
   ses exécutions sont des événements du journal, jamais des effets rejoués.

## Conséquences

- Les contrôles du propriétaire se branchent sans code ; leur effet est visible (résultat
  de l'outil, journal, `penelope hooks test`).
- Un hook ne peut pas rendre Pénélope plus permissive ; un hook lent ou cassé est borné
  (délai, 8 par événement, 2 minutes par appel) et tranché par `on_failure`.
- Les événements autres que `post_tool` arrivent aux lots #384 et #385 ; ils sont refusés
  au chargement d'ici là, avec le motif.
