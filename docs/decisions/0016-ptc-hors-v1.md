# 0016 : Le PTC (`run_code`) est hors V1, avec une couture

Statut : acceptée (25 septembre 2026). Portée : boucle d'agent (`crates/penelope-agent`,
pipeline d'un appel d'outil), approbations (HITL), ledger d'effets. Épopée #208, lot K,
tâche T24. Spécification : `design/v1/boucle-et-outils.md` §3.8.

## Contexte

Le PTC (*programmatic tool calling*) laisse le modèle écrire un petit programme qui appelle
lui-même plusieurs outils, au lieu d'un aller-retour avec le modèle par appel. DeepSeek
Harness le fait : le programme et ses sous-appels passent par le même pipeline, les
sous-appels portent le jeton du parent, et **une demande d'approbation à l'intérieur d'un
programme est un refus** (« sub-calls carry the parent token […] return denials as binding
rejections »).

Chez Pénélope, une approbation suspend **le tour** : l'appel reste dans le transcript sans
résultat, la carte part au propriétaire, et la reprise relit les appels en attente à la fin
du transcript. Un programme en vol ne se suspend pas ainsi : il faudrait soit refuser tout
appel non `Auto` dans un programme, soit inventer une reprise de programme.

Il faut aussi un interpréteur confiné (TypeScript ou Python) : l'instance tourne en profil
`full` assumé, sans bac à sable, `penelope-platform` n'embarque aucun interpréteur, et
l'archtest interdit tout appel shell hors plateforme. Enfin, le ledger devrait planifier
chaque sous-appel (une clé par sous-appel, `step = <call_id>:ptc:<n>`) : compatible, mais
c'est un lot entier.

## Décision

1. **Pas de `run_code` en V1.** Ni outil, ni interpréteur embarqué, ni reprise de
   programme.
2. **La couture est posée** : `CallContext { call_id, parent, root }`
   (`penelope_agent::CallContext`) accompagne chaque appel dans le pipeline de décision.
   Aujourd'hui tout appel est une racine (`CallContext::root`) ; un sous-appel se crée par
   `CallContext::child`, même racine, parent = l'appel qui l'émet.
3. **La règle est écrite et testée dès maintenant** : un appel imbriqué (avec `parent`)
   dont la politique rend `Ask` ou `AskTwice` est refusé **sans carte** ; le refus revient
   au modèle comme résultat (« Non exécuté : un appel imbriqué ne peut pas demander
   d'approbation (…) »). Seul un appel racine peut suspendre le tour. Test :
   `a_nested_call_that_asks_is_refused_without_a_card`.
4. **Si `run_code` vient**, ce sera un `ToolExecutor` de plus qui dispatche ses sous-appels
   par le même pipeline, chacun avec son `CallContext` enfant et son effet planifié au
   ledger ; pas un chemin parallèle.

## Raisons

- **Ce que le PTC apporterait est déjà couvert** : sans approbation dans un programme, il
  ne servirait qu'aux lectures et aux règles « Toujours » ; les lectures parallèles (#85)
  et les jobs d'outils (#204, décision [0012](0012-jobs-outils-durables.md)) couvrent déjà
  ces cas en Rust, sans runtime embarqué.
- **Un interpréteur sans bac à sable** élargirait la surface d'exécution d'une instance qui
  tourne en profil `full` : le coût de sécurité dépasse le gain de latence.
- **Poser la couture maintenant coûte peu** (un type, une règle, un test) et fixe le
  contrat le plus délicat, celui de l'approbation, avant qu'un lot ne soit tenté de
  suspendre un programme.

## Conséquences

- Le pipeline de décision (`pipeline/decide.rs`) porte le contexte d'appel ; le contexte des
  gardes s'appelle désormais `GuardContext`.
- Un refus de plus dans `Refusal` (`NestedApproval`), jamais atteint tant que rien ne crée
  d'appel imbriqué.
- Un futur lot PTC devra : un exécuteur `run_code` confiné, la planification de chaque
  sous-appel au ledger (`<call_id>:ptc:<n>`), la journalisation des sous-appels, et une
  nouvelle décision qui remplace celle-ci.
