# 0026 : « Toujours » vaut toujours, y compris sous une double confirmation

Statut : acceptée (12 novembre 2026). Portée : §14.5 (approbations), C-1.21, C-4.8 et
C-4.9 du contrat fonctionnel. Revient sur une partie de #445 (1.0.99) et sur un effet de
bord de #150 (0.17.31).

## Contexte

Deux lots, chacun juste sur son défaut, ont retiré « Toujours » de là où le propriétaire
s'en servait.

- **#150 (20/09).** Pour ne plus laisser un bouton prendre la place de « Toujours » sur une
  commande composée, la carte ne garde ce bouton que si elle sait nommer sa portée
  (famille, répertoire, hôte). Un outil MCP n'a pas de motif : sa règle porte sur l'outil,
  et la carte n'avait rien à nommer. Son « Toujours » a disparu avec les autres. Dans la
  base du propriétaire, la dernière règle « Toujours » sur un outil MCP date du 20/09 au
  matin ; depuis, aucune, et 272 règles « Pour cette session » sur 90 outils MCP, à
  redonner à chaque session.
- **#445 (11/11).** Sur une carte à double confirmation, « Pour cette session » et
  « Toujours » tranchaient en un clic, sans la seconde carte. Le lot a retiré les deux
  boutons, refusé toute fenêtre dans le cœur, et fait ignorer au moteur **toute** règle
  `Auto` pour un appel de classe destructive. La règle « Toujours » que le propriétaire
  avait posée sur un outil MCP que son serveur annonce destructif, alors qu'il ne fait que
  marquer un élément, servie 523 fois, aurait cessé de valoir à l'installation : deux
  clics par appel, sur un geste qu'il avait déjà autorisé pour de bon.

Le défaut de #445 était réel, mais il tenait au **clic unique** et à la règle de session
posée sans y penser, pas au fait qu'un propriétaire puisse autoriser durablement un outil
destructif. Le propriétaire a tranché : « Toujours = toujours ».

## Décision

1. **Une règle « Toujours » s'applique quelle que soit la classe de l'appel.**
   `PolicyEngine::evaluate_in` ne l'ignore plus sous `ask_twice` ; elle compte ses usages,
   se lit et se retire dans `/policies`. Les règles posées avant la 1.0.99 valent de
   nouveau, sans migration.
2. **Une règle de run ou de session n'abaisse toujours pas une double confirmation**
   (#445, inchangé) : le verdict reste `ask_twice` et nomme la règle qu'il ignore.
3. **La carte double propose « Toujours », derrière la seconde confirmation.** Le premier
   clic ne tranche rien ; la seconde carte dit ce que la règle couvrira et que les appels
   suivants partiront sans carte ; « Confirmer « Toujours » » pose la règle. « Pour cette
   session » n'y revient pas. `penelope approve <id> --always`, commande tapée en toutes
   lettres, est accepté.
4. **Un outil sans motif retrouve son bouton**, qui dit sa portée : « Toujours pour cet
   outil ». Une commande composée reste sans bouton (#150) : aucune règle ne la couvrirait.
5. **Le plancher des réglages sensibles ne bouge pas** (C-4.8) : `config_set` sur le bac à
   sable, les fournisseurs, le canal, les outils, les politiques ou le juge redemande deux
   fois quoi que dise une règle. Sa demande porte `every_time`, sa carte n'a pas de
   « Toujours », et le cœur le refuse : une règle qui ne s'appliquerait jamais ne s'écrit
   pas.
6. **Le contrôle `policy_dead_rules` de `doctor` est retiré** : plus aucune règle
   « Toujours » n'est morte.

## Conséquences

- Un « Toujours » sur un outil destructif est un vrai pouvoir : l'agent le rappellera sans
  demander. La garde est la seconde confirmation, qui le dit en toutes lettres, et la
  révocation en un geste. Qui veut l'inverse garde ses sorties : retirer la règle,
  refuser un outil dans la déclaration de son serveur (`[tool_policy]`, un refus y
  l'emporte sur toute règle), ou resserrer par un hook `pre_tool`.
- Les règles « Toujours » d'un outil MCP restent révoquées quand sa description, son
  schéma ou ses annotations changent (#92) : l'autorisation vaut pour l'outil que le
  propriétaire a vu.
- Les règles de session posées avant la 1.0.99 sur des outils destructifs restent sans
  effet, et s'éteignent avec leur session.
- Une demande créée par une version antérieure ne porte pas `every_time` : sur un réglage
  sensible resté en attente pendant la mise à jour, « Toujours » écrirait une règle sans
  effet. Le plancher, lui, tient.
