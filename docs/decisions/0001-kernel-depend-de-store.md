# 0001 — `penelope-kernel` dépend de `penelope-store`

Statut : acceptée. Portée : §3.1 (règles de dépendance).

## Contexte

Le PRD dessine un noyau qui ne dépend d'« aucun crate métier ». Lu au pied de la lettre,
cela interdirait aussi `penelope-store`, puisque c'est un autre crate du workspace.

Or le noyau **est** la couche durable : journal d'événements chaîné, ledger d'effets,
file de tours, sessions, budgets, générations de configuration. Chacune de ces primitives
est définie par ce qu'elle garantit après un `kill -9`, pas par une structure en mémoire.
Sans accès à la base, le noyau se réduirait à des types sans comportement, et la
durabilité migrerait dans le daemon, c'est-à-dire exactement là où le PRD ne veut pas
qu'elle soit (§0 : « tout est durable », §4 : le ledger précède l'exécution).

## Décision

`penelope-kernel` dépend de `penelope-store`, et de rien d'autre du workspace.

`penelope-store` est traité comme une **infrastructure**, au même titre que `serde` ou
`tokio` : il n'a aucune dépendance sur un crate du projet, ne connaît aucun concept
métier, et se contente d'offrir un écrivain unique, un pool de lecture et des migrations.

Pour que cette frontière reste vraie, `penelope-store` réexporte `rusqlite`
(`pub use rusqlite;`). Aucun crate métier ne déclare le pilote SQL dans son `Cargo.toml` :
changer de pilote reste un changement local à un seul crate.

## Conséquences

- Le test d'architecture (`penelope-archtest`) encode la règle telle quelle :
  `penelope-store` n'a aucune dépendance interne, `penelope-kernel` en a exactement une.
  Une dépendance supplémentaire du noyau fait échouer `ca_3_1_dependency_rules_hold`.
- Un crate métier qui importerait `rusqlite` directement est refusé par le même test.
- En échange, les primitives durables se testent seules, avec une base en mémoire et une
  horloge fictive, sans monter le daemon.

## Alternatives écartées

- **Trait de persistance dans le noyau, implémentation dans `store`.** Une couche
  d'indirection de plus pour une seule implémentation, et des transactions qui traversent
  la frontière du trait : le coût dépasse le bénéfice.
- **Déplacer le ledger dans le daemon.** Rendrait le noyau non testable isolément et
  placerait la garantie d'idempotence dans la couche qui change le plus souvent.
