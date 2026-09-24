# Point d'étape V1 : 24 septembre 2026, 23h00

Arrêt demandé par le propriétaire à 23h00. Ce fichier dit où en est la branche `v1`, ce
qui reste, et comment reprendre. Il complète la charte (`design/v1/README.md`) et les
notes de chaque lot (`design/v1/notes/*.md`).

## Où on en est

- **Version** : `v1` en `1.0.0-alpha.10`, poussée, CI verte (Linux, macOS, `cargo deny`).
  Aucune version `v1` n'est taguée ni publiée (décision 0015).
- **`main`** : `0.17.62` publiée (gel de la dette et correctif SQLite `IMMEDIATE`). Rien
  n'est arrivé sur `main` depuis : pas de fusion `main` → `v1` en attente.
- **Tests** : 2 003 (dernier compte complet, alpha.9 ; la suite de l'alpha.10 a été passée verte par l'agent du lot) tests verts sur le poste (macOS), suite complète.
- **Taille** : `penelope-daemon` 38 177 lignes (86 462 au plus haut, 82 637 à
  l'alpha.6). Le plus gros fichier source du dépôt fait 2 179 lignes (`cli/commands.rs`),
  contre 13 716 (`telegram.rs`) au départ ; 25 fichiers restent dans la liste de référence
  du gel, qui ne peut que rétrécir.
- **Crates sorties du daemon** : `penelope-app` (T21), `penelope-vault` (T22),
  `penelope-mcp-host` (T25), `penelope-ops` (T28), `penelope-gateway-telegram` (T29,
  au-dessus du daemon, composée par la CLI), `penelope-agent` (T10), `penelope-dream` (T26, partiel : accueil et ingestion).
- **Journal d'événements** : source de lecture par défaut (`history.source = journal`,
  T14), lecture incrémentale, `penelope history verify` et `reindex`, scellement de
  l'historique 0.17 au premier démarrage (migration 0021, sans retour vers la 0.17),
  tentatives (#206), reprise après crash, compaction, niveau 1, fork, rewind, purge et
  rétention journalisés.

## Versions posées aujourd'hui

| Version | Contenu |
|---|---|
| 1.0.0-alpha.4 | vague 3 : fichiers géants découpés (Telegram, dream, workflow, executor, rpc, mcp, doctor), double écriture du journal |
| 1.0.0-alpha.5 | vague 4 : compaction, niveau 1, fork, rewind, tentatives, reprise après crash, scellement, ports du daemon |
| 1.0.0-alpha.6 | vague 5 : `penelope-app`, `history verify` et `reindex` |
| 1.0.0-alpha.7 | vague 6 : lecture depuis le journal, vault, hôte MCP, passerelle Telegram |
| 1.0.0-alpha.8 | vague 7 : lecture incrémentale, `audit show` exact, purge du journal, `penelope-ops`, ports de la boucle |
| 1.0.0-alpha.9 | vague 8 : avertissement de purge des forks (arbitrage 3), `penelope-agent` |
| 1.0.0-alpha.10 | vague 9 : `penelope-dream` (accueil, ingestion, `DigestInputs`) ; `dream/` reste au daemon |

## Ce qui reste (par lot de la charte §8)

- **H, crates feuilles** : fait, sauf `purge.rs` et `session_ops.rs`, restés au daemon
  (à rejoindre `penelope-ops`).
- **I, journal phases 3 et 4** : restent T16 (retrait du chemin d'écriture direct et de la
  clé `history.source`), T19 (décision 0017 et documentation), T22 (critères
  d'acceptation `ca_4_5`, `ca_4_6`). **T16 attend que le propriétaire ait fait tourner la
  lecture depuis le journal sur une copie de sa base** : c'est la dernière porte de sortie
  (`history.source = tables`).
- **J, crates du cœur** : restent T23 côté conversation (`penelope-conversation` :
  transcript, compaction, prompt), T24 (`penelope-executor`), la fin de T26 (déplacer `dream/` après avoir remplacé `&Arc<Daemon>` par `&penelope_dream::Context`, puis `scheduler` cite `penelope_dream::{digest_text, system_crons}`), T27
  (`penelope-orchestrator`), boucle T11 (sous-agents et étapes de workflow appellent la
  crate directement).
- **K, boucle** : tout reste : steering (T12 à T14), tentatives côté boucle (T15, port
  `AttemptSink`), jobs (T17 à T20), juge d'approbation (T21 à T23, seulement si la mesure
  préalable le justifie, arbitrage 6), T24 à T27.
- **L, clôture** : T30 à T32 (consommateurs, matrice CA, resserrer les plafonds), T36 (le
  cœur ne nomme plus le canal ; `penelope-app` dépend encore de `penelope-telegram`),
  décisions 0013, 0014, 0016, 0017, `docs/architecture.md`, bascule `v1` → `main`.

Estimation : environ 70 % des tâches, un peu plus de la moitié de l'effort ; trois à
quatre vagues avant la clôture.

## Dettes et points ouverts relevés par les lots

- `penelope-agent` voit les charges du journal par `penelope_app::journal`, qui réexporte
  `penelope-context` : la boucle ne dépend plus de context directement, mais encore par
  transitivité. À couper avec le port `AttemptSink` (K, T15) ou en faisant descendre ces
  types.
- Réexports de transition (`pub use`) dans le daemon pour chaque crate sortie : à retirer
  en T30.
- `check_endpoint` est dupliqué dans `penelope-ops` (upgrade) : à descendre dans
  `penelope_app::helpers`.
- Quatre contrôles de `doctor` restent dans `rpc/methods/doctor.rs` (MCP, stabilité du
  prompt, jobs d'outils, historique) : ils apparaissent désormais en fin de sortie.
- Outils `history_*` et relecture d'épisode lisent encore les caches (vérifiés par
  `history verify`).
- Archtest : `UPDATE_BUDGET` décale les commentaires de `[daemon].modules` quand une
  entrée sort (corrigé à la main à chaque fois) ; il ne sait pas renommer une clé de
  `[channel.allowed]` quand un fichier change de crate.
- Issues : #211 et #215 faites sur `v1`, à reporter sur `main` ; #212 attend le test
  manuel `workflow_dispatch` de `release.yml` avec `tag=v1.0.0-alpha.0` (doit échouer
  sans rien publier).

## Pour tester

Compiler depuis `v1` et lancer sur une **copie** du dossier de données, jamais sur la
base réelle : la migration 0021 scelle la base et la 0.17 ne la relit plus. Commandes
utiles : `penelope history verify`, `penelope doctor`, `penelope purge` sur une session
qui a un fork.

## Comment reprendre

1. `cd /Users/edouard/Code/agent/penelope-wt/v1 && git pull` (worktree d'intégration ;
   ne jamais lancer cargo dans l'arbre principal, utilisé par une autre session).
2. Vérifier qu'aucun commit n'est arrivé sur `main` (`git log v1..origin/main`) ; sinon
   `scripts/sync-main.sh`.
3. Prochaine vague proposée : `penelope-conversation` (T23), `penelope-executor` (T24),
   T36 (le cœur ne nomme plus le canal), T19 + T22 du journal. Puis `penelope-orchestrator`
   (T27) et le lot K.
4. Méthode : un worktree et une branche `v1-<nom>` par agent, consigne commune
   (`commun.md` des sessions précédentes), intégration par rebase sur `v1`, plafond du
   daemon posé à l'intégration, une version `1.0.0-alpha.N` par vague (`make bump`).
