# 0004 — IPC : `tokio::net::UnixListener` plutôt qu'un crate d'abstraction

Statut : acceptée. Portée : §2.7 (IPC locale CLI ↔ daemon).

## Contexte

La CLI parle au daemon par une IPC locale. Le PRD décrit un socket Unix sur macOS et un
named pipe sur Windows, avec le même protocole des deux côtés (JSON-RPC 2.0, une
enveloppe par ligne). Des crates comme `interprocess` unifient les deux.

## Décision

`tokio::net::UnixListener` directement, derrière une façade `penelope_platform::ipc`
(`IpcListener`, `IpcStream`) qui est le seul point à réimplémenter pour Windows.

## Raisons

- Seul macOS est livré. Payer aujourd'hui une dépendance dont l'API bouge, pour un
  portage qui n'est pas au programme, c'est acheter une dette avant d'en avoir l'usage.
- Les permissions comptent : le socket est créé en `0600` et le fichier résiduel est
  retiré avant le `bind`. Ce sont deux gestes explicites que l'on veut voir dans le code,
  pas déduire du comportement d'une abstraction.
- `tokio` est déjà là. Aucun runtime ni aucune dépendance supplémentaire.

## Conséquences

- La surface à porter tient dans un fichier : `IpcListener::bind`, `accept`, `connect`.
  Le reste du projet ne connaît que `IpcStream`.
- Le protocole NDJSON est identique quel que soit le transport, ce qui garde la CLI et
  ses tests indépendants de ce choix.
