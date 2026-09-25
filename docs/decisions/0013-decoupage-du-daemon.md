# 0013 : Le daemon découpé en crates, la passerelle au-dessus

Statut : acceptée (25 septembre 2026), écrite après coup sur le code de la
1.0.0-alpha.13, branche `v1`. Portée : la structure du workspace, `penelope-daemon`,
`penelope-cli`, `penelope-archtest`. Épopée #208, lots D, G, H, J et L. Spécification :
`design/v1/decoupage-daemon.md` ; charte : `design/v1/README.md` §3. Description de
l'état obtenu : [docs/architecture.md](../architecture.md).

## Contexte

À la 0.17.58, `penelope-daemon` faisait 77 548 lignes en 63 modules : sept fichiers
dépassaient 3 000 lignes (`telegram.rs` 13 716), 42 fichiers nommaient le type `Daemon`,
et la plupart des modules le prenaient en paramètre pour n'en lire que quatre choses (la
table kv, `provider_for`, le canal du propriétaire, le signal d'arrêt). La passerelle
Telegram vivait dans le daemon et l'appelait par une centaine de symboles ; le cœur
nommait Telegram 279 fois hors d'elle. Le daemon est monté jusqu'à 86 462 lignes sur la
branche `v1`, le temps que les tests sortent des fichiers géants.

Rien ne se testait sans construire un daemon complet, et un correctif dans un module
touchait le fichier que tous les autres lots touchaient aussi.

L'architecture hexagonale existait déjà dans les faits : la boucle ne connaissait que
quatre traits, la livraison passait par `ChannelDelivery`, les serveurs MCP par
`McpGateway`, les workflows par `Orchestrator`. Ce qui manquait était ce que les modules
prenaient sur `Daemon` sans trait.

## Décision

1. **Les ports d'abord, les crates ensuite.** Avant tout déplacement, chaque module cesse
   de prendre `&Daemon` : la table kv sur `Services`, `ProviderSource`, `Handle`,
   `Supervision`, `McpAdmin`, `Messenger` reçu explicitement, un `Slot` pour ce qui se
   branche après le démarrage. Les crates sortent ensuite par des déplacements purs.
2. **Une crate `penelope-app` en bas du cœur** porte `Services`, le bus des tours, les
   boucles supervisées, l'élicitation et tous les ports partagés. Elle ne dépend que des
   crates métier, jamais du daemon, d'une crate extraite ni du canal.
3. **Dix crates sortent du daemon** : `penelope-app`, `penelope-vault`,
   `penelope-mcp-host`, `penelope-ops`, `penelope-agent`, `penelope-conversation`,
   `penelope-executor`, `penelope-dream`, `penelope-orchestrator`,
   `penelope-gateway-telegram`. La boucle et la conversation sont deux crates (charte
   §3.2) : la boucle ne dépend ni du moteur de contexte, ni de la mémoire, ni du canal.
4. **La passerelle Telegram est au-dessus du daemon**, pas en dessous : c'est un
   adaptateur pilotant, qui appelle l'application. `penelope-gateway-telegram` dépend de
   `penelope-daemon` ; le daemon ne la connaît que par les ports `Gateway`,
   `ChannelDelivery`, `Messenger`, `OwnerChannel` et `Cards`. `penelope-cli` compose les
   deux (`Daemon::new`, `compose`, `Daemon::run`) et reste la seule crate qui dépend de la
   passerelle.
5. **Une crate sortie reçoit un contexte, pas le daemon** : `compaction::Context`,
   `penelope_dream::Context`, `penelope_orchestrator::Context`, `AgentServices` portent ce
   qu'elle lisait de `Daemon`. Le daemon les compose ; leurs tests les montent sans lui.
6. **Chaque frontière est une règle d'archtest** : une liste de dépendances permises par
   crate du cœur, un test nommé par interdit qui compte, aucune autre crate que la CLI
   sur la passerelle, la frontière canal/cœur à cliquet.
7. **Les anciens chemins restent valides jusqu'à T30** : le daemon réexporte chaque module
   parti (`pub use penelope_app::bus`, façades `agent`, `workflow`…), pour que la
   passerelle, les évaluations et les tests ne changent pas pendant les déplacements.

## Raisons

- **Un port de 90 méthodes serait une fausse frontière.** La passerelle appelait douze
  membres de `Daemon` et environ 80 fonctions libres ; les vraies frontières dans l'autre
  sens existaient déjà (`ChannelDelivery`, `Messenger`, `OwnerChannel`). Au-dessus, elle
  est sortie tôt (T29, 18 000 lignes d'un coup), avant les crates métier.
- **`Services` en bas, sinon rien ne sort.** Les fonctions extractibles prenaient déjà
  `&Services` ; sans lui dans une crate commune, chaque crate extraite aurait dû dépendre
  du daemon.
- **Les frontières passent là où la mesure montre un col étroit** : une poignée de
  fonctions `&Services` plus un ou deux ports, jamais un module qui lit vingt membres du
  daemon. Ce qui a besoin du daemon pour autre chose (le moteur des tours, les coureurs,
  la supervision, la RPC) y reste.
- **Des déplacements purs, puis les signatures.** Un commit qui déplace ne change aucun
  corps : `git blame` survit, et la fusion quotidienne de `main` (qui touche les mêmes
  fichiers) reste possible.
- **Des réexports plutôt qu'une réécriture de tous les appelants** : chaque lot touchait
  ses fichiers et les `use`, pas la passerelle ni les évaluations que d'autres lots
  modifiaient en parallèle.

## Conséquences

- `penelope-daemon` passe de 86 462 lignes (plafond le plus haut sur `v1`) à 14 986, en
  21 modules ; le plus gros fichier du dépôt fait 2 179 lignes (`cli/commands.rs`), contre
  13 716 au départ. Le type `Daemon` est nommé 65 fois dans 16 fichiers.
- Les crates du cœur se testent sans daemon : la boucle sur une base en mémoire
  (`AgentServices::for_tests`), l'orchestrateur, le rêve et la conversation sur
  `Services::for_tests` et `MockProviders`.
- `penelope-app`, `penelope-executor` et `penelope-daemon` ne dépendent plus de
  `penelope-telegram` ; les mentions du canal dans le cœur passent de 166 à 74 pour les
  six crates touchées par T36.
- Le démarrage change de forme sans changer d'ordre : la CLI construit la passerelle
  avant `Daemon::run`, qui annonce le propriétaire avant les serveurs MCP et la démarre
  après eux (issue #12, désormais testé).
- `ticket_to_deploy_e2e` et le filet `telegram_e2e` vivent dans la passerelle : un test
  du daemon ne peut pas voir une crate au-dessus de lui.
- Le workspace compte dix lignes `version =` de plus ; `scripts/bump.sh` les réécrit toutes.

## Ce qui est fait et ce qui reste

Fait (1.0.0-alpha.13) : les ports du daemon (T05 à T10), les fichiers géants découpés
(T11 à T20), les dix crates (T21 à T29, T10, T23), le cœur qui ne nomme plus le canal
(T36), les règles d'archtest correspondantes.

Reste :

- **T30** : retirer les réexports de transition et réécrire les appelants (CLI,
  évaluations, passerelle, tests du daemon) sur les nouvelles crates ; les façades du
  daemon (`agent`, `compaction`, `dream`, `executor`, `ingest`, `scheduler`,
  `selfknow`, `workflow`) disparaissent avec.
- **T32** : resserrer les listes du gel ; la liste de référence compte encore 23
  fichiers, dont `engine.rs` et `tool_jobs.rs` au daemon.
- **Après la V1** : T33 (ports `TurnIntake`, `SessionModels`, `Transcriber` : la
  passerelle se testerait sans daemon), T34 (registre d'outils natifs enfichable), T37
  (`Origin::Channel`, `EffectKind::Message`, liaison de session générique ; la plupart des
  mentions restantes du canal y sont).

Écarts à la spécification, assumés :

- le daemon garde 14 986 lignes au lieu d'environ 9 200 : façades de transition,
  `tool_jobs`, `audit` et les implémentations des ports de la boucle sur la base
  (`KvModes`, `StoredSnapshots`) y sont encore ;
- `penelope-app` ne dépend pas de `penelope-telegram` (la spécification l'admettait) :
  les gabarits et les actions sont dans la passerelle, derrière `Cards` ;
- `decide_approval` est resté dans la boucle, pas dans `penelope-app` ;
- `DigestSource` et `DigestInputs` vivent dans `penelope-dream`, seul lecteur du digest ;
- les branchements tardifs sont des `Slot` et non des `Option` : les boucles de fond
  partent avant la passerelle et le superviseur MCP ;
- trois règles de la spécification ne sont pas posées : la liste des crates qui peuvent
  dépendre du daemon, l'interdiction d'`impl` dans le module des ports de
  `penelope-app`, et le plafond de 800 lignes (le gel reste à 1 000).

## Alternatives écartées

- **La passerelle sous le daemon, derrière un port unique** : un trait de 90 méthodes,
  modifié à chaque commande Telegram ajoutée, et une passerelle qui ne sortirait qu'après
  toutes les crates métier.
- **Réécrire tous les appelants à chaque sortie de crate** : chaque lot aurait touché la
  passerelle et les évaluations, que d'autres lots modifiaient en même temps.
- **Des enveloppes homonymes en `&Arc<Daemon>` dans chaque crate sortie** : une API de
  plus à retirer en T30, et un commit de signature impossible à séparer du déplacement
  (même nom, même module). Les enveloppes restent dans les façades du daemon, pas dans
  les crates.
- **`Option<Arc<dyn Messenger>>` lu au lancement de chaque boucle** : les boucles de fond
  démarrent avant la passerelle et le superviseur MCP, la valeur serait restée vide pour
  toujours. Seules les fonctions appelées après le démarrage reçoivent un `Option`.

Point ouvert, sans décision : `penelope-agent` atteint encore `penelope-context` dans le
graphe cargo, par `penelope-app` qui porte `Services` et son moteur de contexte. La règle
`reach.rs` garantit qu'aucun type du moteur de contexte ne passe par les ports ; couper
l'arête demanderait de sortir les ports dans une crate plus basse.
