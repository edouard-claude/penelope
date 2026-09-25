# Point d'étape V1 : 25 septembre 2026, fin de la clôture du code

Suite du point du 24 (`point-etape-2026-09-24.md`). Ce fichier dit où en est `v1`, ce qui
reste et ce qui attend le propriétaire.

## Où on en est

- **Version** : `v1` en `1.0.0-alpha.15`, poussée ; jamais taguée ni publiée (décision
  0015). `main` est toujours en `0.17.62`, sans commit nouveau depuis le 24.
- **Tests** : 2 044 tests verts sur macOS (suite complète) ; CI de l'alpha.14 verte, celle
  de l'alpha.15 lancée à la pose.
- **Crates** : 27. Toutes celles du découpage sont sorties du daemon : `penelope-app`,
  `penelope-vault`, `penelope-mcp-host`, `penelope-ops` (purge et sessions comprises),
  `penelope-gateway-telegram` (au-dessus du daemon), `penelope-agent`,
  `penelope-conversation`, `penelope-executor`, `penelope-dream`, `penelope-orchestrator`.
- **Daemon** : 14 752 lignes (86 462 au plus haut). Il ne réexporte plus rien d'une autre
  crate (T30) ; la CLI et les évaluations n'en citent que `Daemon`, `VERSION`,
  `runner::{process, run_pool}`, `rpc::Rpc` et deux adaptateurs.
- **Fichiers** : tous sous 800 lignes sauf trois, tous dans le daemon :
  `engine.rs` (1 091), `supervisor.rs` (938), `engine/tests/turns.rs` (904, fichier de
  tests). Une seule entrée reste dans `[files.oversized]`.
- **Frontière canal** : le cœur ne dépend plus de `penelope-telegram` ; mentions du canal
  dans le cœur 166 → 74 ; `[channel.allowed]` au total 249.
- **Journal** : source de lecture par défaut, lecture incrémentale, `history verify` et
  `reindex`, CA 4.5, 4.6, 5.5 prouvés à chaque appel des scénarios.
- **Boucle** : pipeline typé, steering (message pendant un lot, `/stop`), tentatives par le
  port `AttemptSink`, événements typés, couture PTC (0016), jobs d'outils conformes.
- **Documentation** : `docs/architecture.md`, décisions 0013, 0014, 0016.

## Versions posées le 25

| Version | Contenu |
|---|---|
| 1.0.0-alpha.11 | conversation, exécuteur, rêve entier en crates ; CA 4.5, 4.6, 5.5 |
| 1.0.0-alpha.12 | steering, orchestrateur, fin de `penelope-ops`, ordre de `doctor` rétabli |
| 1.0.0-alpha.13 | le cœur ne nomme plus le canal ; tentatives par un port ; jobs d'outils |
| 1.0.0-alpha.14 | `penelope approvals stats` ; API de la boucle ; architecture documentée |
| 1.0.0-alpha.15 | réexports de transition retirés ; fichiers sous 800 lignes |

## Ce qui attend le propriétaire

1. **Tester l'alpha sur une copie des données** (jamais la base réelle : la migration 0021
   scelle la base, la 0.17 ne la relit plus). Commandes : `penelope history verify`,
   `penelope doctor`, un tour ordinaire, un `/fork` puis `penelope purge` sur la mère.
   C'est la condition de T16 (retrait du chemin d'écriture direct et de
   `history.source = tables`) et de la décision 0017 (brouillon dans `notes/i-ca.md`).
2. **Mesurer le besoin du juge d'approbation** (#203, arbitrage 6) :
   `penelope approvals stats --days 30` (lecture seule ; sur une instance 0.17, CLI de
   `v1` compilé et `--home` ; procédure dans `docs/install-headless.md`, « Mesurer avant
   d'activer le juge »). Verdict go/no-go affiché ; commentaire prêt pour #203 dans
   `notes/k-juge.md`.
3. **Donner le feu vert à la bascule** `v1` → `main` (`scripts/switch-check.sh` dit ce qui
   manque).

## Ce qui reste sans le propriétaire, après V1 ou en petite dette

- T33 : sortir le moteur de tours de `impl Daemon` (ports `TurnIntake`, `SessionModels`,
  `Transcriber`) ; c'est ce qui fera passer `engine.rs` et `supervisor.rs` sous 800.
- T37 : `Origin::Channel` générique, liaison de session ; les 74 mentions restantes.
- `penelope-agent` atteint encore `penelope-context` dans le graphe cargo, par
  `penelope-app` (`Services.context`) ; il ne nomme plus aucun de ses types (test d'archtest
  au grain des modules). Sortir les ports de `penelope-app` vers une crate plus basse.
- `titles::label` pourrait déléguer à `helpers::session_label` ; `CACHE_TTL_MS` existe en
  deux copies tenues égales par un test.
- Archtest : `UPDATE_BUDGET` décale des commentaires et ne sait pas renommer une clé de
  `[channel.allowed]` quand un fichier change de crate (fait à la main à chaque fois).
- Sur `main` : le correctif de T19 (un redémarrage pendant un job d'outil ne pose plus de
  carte `effect_unknown`) n'existe que sur `v1` ; #211 et #215 à reporter ; #212 attend le
  test manuel de `release.yml`.

## Méthode

Inchangée (`notes/methode-agents.md`), avec une règle ajoutée le 25 : un agent fait
`cargo clean` une seule fois, avant son rapport, puis ne lance plus aucune commande dans
son worktree (deux suites de l'intégrateur ont été interrompues ainsi).
