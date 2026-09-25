# 0014 : La boucle d'agent est un pipeline d'étapes typées

Statut : acceptée (25 septembre 2026), écrite après coup sur le code de la
1.0.0-alpha.13, branche `v1`. Portée : `penelope-agent`, les ports de la boucle dans
`penelope-app`, leurs implémentations dans le daemon et `penelope-conversation`. Épopée
#208, lots F, J et K. Spécification : `design/v1/boucle-et-outils.md` ; charte :
`design/v1/README.md` §5. Voir aussi [0013](0013-decoupage-du-daemon.md) (la crate) et
[0008](0008-cache-de-prompt.md) (le préfixe ne bouge pas).

## Contexte

À la 0.17.58, la boucle d'agent tenait dans `agent.rs` : 4 192 lignes dont 2 146 de code,
avec une fonction `call_model` de plus de 200 lignes qui portait six variables mutables pour décider des
nouvelles tentatives. La politique d'un appel d'outil était une suite de `if` : liste
blanche, décision antérieure, garde de boucle, précheck, règles, déclaration du serveur,
mode de session, sans nom pour la couche qui tranchait. Le steering était caché dans une
lecture : `request_messages` absorbait les messages arrivés pendant le tour, de sorte que
lire la conversation la modifiait.

Tout ce qui s'y ajoutait ou devait s'y ajouter (jobs d'outils #204, juge d'approbation
#203, tentatives #206) touchait le même fichier, et un ordre de contrôles ne se lisait
qu'en suivant le code.

## Décision

1. **Un tour est une suite d'étapes nommées, dans un ordre fixe écrit dans le code** :
   résolution des appels en attente, gardes de tour, réclamation de la boîte, appel au
   modèle avec son plan de tentatives, règlement. Pas d'enregistrement dynamique : une
   chaîne est un tableau constant, et un test fixe son ordre.
2. **Gardes de tour** (`guards.rs`) : `default_chain()` = annulation, budget, plafond
   d'appels, palier de coût ; `run_guards` s'arrête à la première qui ne rend pas
   `Proceed`.
3. **Gardes d'appel** (`pipeline/decide.rs`) : `call_chain()` = liste blanche, décision
   antérieure, garde de boucle, précheck, sur un `DescribedCall` ; un refus est un
   `Refusal` typé, une décision déjà prise par le propriétaire (`GuardStop::Approved`) est
   définitive.
4. **Politique en couches nommées** (`pipeline/policy.rs`) : `PolicyStage` rend un
   `Verdict` qui dit quelle couche a tranché (`VerdictLayer` : règle, défaut de la classe
   de risque, déclaration du serveur, autorisation déclarée, mode de session, réglage
   sensible). Les huit raisons d'aujourd'hui sont fixées par un test doré.
5. **Plan de tentatives pur** (`model/retry.rs`) : `RetryPlan::on_error(erreur, phase,
   arrêt)` rend `RetrySame`, `Fallback` ou `GiveUp` ; `call_model` garde les effets
   (attente, journal, messages d'échec). La phase `AfterText` rend toujours `GiveUp` :
   jamais de repli silencieux après du texte.
6. **Steering explicite** : la boucle réclame la boîte (`Inbox::claim`) à deux points,
   `Checkpoint::BeforeModelCall` et `Checkpoint::BetweenCalls`. Un message arrivé pendant
   un lot laisse finir l'appel en cours ; les suivants reçoivent « Non exécuté : nouveau
   message du propriétaire. ». Lire la conversation n'absorbe plus rien.
7. **Ce que le harnais ajoute est nommé** : `Injection::Note`, placée après le dernier
   message utilisateur, jamais écrite dans l'historique (la note d'interruption après
   `/stop`).
8. **La boucle ne connaît que des ports.** Elle reçoit `AgentServices` : les registres
   qu'elle touche (base, horloge, journal, ledger, configuration, budget, catalogue,
   approbations, politiques) et des ports, `SessionModes`, `SessionInfo`,
   `PromptSnapshots`, `CacheAudit`, `JobRunner`, `AttemptSink`, plus `Conversation`,
   `Compactor`, `ToolExecutor`, `TurnSink` et `Inbox` de `penelope-app`. Elle écrit ses
   tentatives par `AttemptSink`, lance un job par `JobRunner`, nomme ses événements par
   `TurnEventKind`.
9. **La boucle est une crate** (`penelope-agent`) qui ne dépend ni de
   `penelope-context`, ni de `penelope-memory`, ni de `penelope-telegram`, ni du daemon ;
   le vocabulaire du journal qu'elle écrit vit dans `penelope_kernel::journal`.

## Raisons

- **Un ordre de contrôles est une garantie de sécurité** : une décision du propriétaire
  qui passe avant la garde de boucle, une liste blanche avant la politique. Une chaîne
  constante et son test d'ordre le rendent lisible et impossible à déplacer par
  accident ; un registre dynamique le rendrait dépendant de l'ordre d'enregistrement.
- **Une politique qui dit sa couche** s'explique au propriétaire et se teste couche par
  couche ; le test doré garantit que les textes vus sur les cartes n'ont pas bougé.
- **Un plan de tentatives pur se teste en table** (quinze cas : repli restant, dernier
  candidat, `Retry-After` court ou long, arrêt demandé, flux coupé avant ou après texte)
  sans réseau ni horloge.
- **Une lecture sans effet de bord** : les deux sources d'historique (journal et tables)
  doivent rendre la même requête ; une lecture qui absorbe des messages ne le permettait
  pas.
- **Des ports plutôt que `Services`** : les tests de la boucle tournent sur une base en
  mémoire avec des doubles (`MemoryModes`, `NoAudit`, `NoJobs`, `MemoryAttempts`), sans
  daemon ni moteur de contexte.
- **Rien ne change pour le propriétaire** : textes, cartes, événements et ordre des
  contrôles sont restés ceux d'avant le découpage ; les scénarios rejouables le vérifient.

## Conséquences

- `penelope-agent` compte 7 043 lignes en 32 fichiers (tests compris), le plus gros
  (`pipeline.rs`) à 622 lignes ; `call_model` repasse sous 200 lignes et perd son
  `allow`.
- Le daemon garde les implémentations des ports qui lisent la base (`KvModes`,
  `StoredSnapshots`, `UsageAudit`, `DaemonJobs`) et compose `AgentServices` par
  `agent::services_of` ; l'orchestrateur appelle `AgentLoop` directement pour les étapes
  `agent` et les sous-agents.
- `/stop` pendant un lot donne « Non exécuté : arrêté par le propriétaire. » aux appels
  non démarrés, et le tour suivant le sait sans que le préfixe du prompt change.
- Une réponse vide est épinglée à sa requête : sa tentative porte le `llm_request_id`
  de l'appel qui l'a rendue.
- La table `turn_attempts` prévue n'existe pas : le journal (`conv.attempt`) la remplace.

## Ce qui est fait et ce qui reste

Fait (1.0.0-alpha.13) : T02 à T06 (modules, `RetryPlan`, gardes de tour et d'appel,
couches de politique), T09 à T15 (ports, crate, sous-agents sur la crate, steering,
tentatives), T17 à T20 (jobs d'outils, par le port `JobRunner`), T26 (événements
typés).

Reste :

- T07 : carte d'approbation typée partagée et port `Approver` ; aucun des deux n'existe,
  la carte est construite par la passerelle.
- T08 : mode d'exécution dérivé de la spécification de l'outil ; la liste des lectures
  parallélisables est encore une constante de `pipeline.rs`, et un job n'est pas une
  variante `Execution::Job` (le port `JobRunner` en rend l'équivalent).
- T21 à T23 : le juge d'approbation (#203), seulement si la mesure préalable sur
  l'instance le justifie ; aucune couche `Judge` ni plancher `Destructive` n'existe
  encore dans `VerdictLayer`.
- T24 : la couture du PTC (`run_code`) pour les appels imbriqués (décision 0016).
- T25 : nettoyage d'API (`resume_after_approval` existe encore) ; T27 : les parties pures
  de l'audit du cache vers `penelope-llm`.
- La note de fusion d'un message arrivé pendant le tour reste un message système après
  les messages système ; la passer en `Injection::Note` en queue change la surface d'un
  scénario et se fera à part.

## Alternatives écartées

- **Un registre d'étapes dynamique**, comme les cascades de DeepSeek Harness : l'ordre y
  dépend de l'enregistrement, et rien ne l'y fixe. Ici, un trait à implémentation unique
  par étape et des chaînes constantes.
- **Écrire la boîte à la réclamation** : le message serait posé avant les résultats
  « Non exécuté » des appels qu'il remplace ; la boucle écrit, la boîte réclame.
- **Une couche de politique par condition** (réseau, destructif) : le réseau reste une
  annotation de la raison, et le destructif n'est qu'une condition du mode `auto`
  aujourd'hui ; une couche naîtra avec ce qui la produit.
- **Un port `TurnJournal` pour toutes les écritures de la boucle** dès la crate : il
  aurait changé la forme des écritures avant que `Attempt` existe ; les tentatives passent
  par `AttemptSink`, les bornes de tour sont des types purs du noyau.
