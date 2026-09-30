# 0008 — Cache de prompt : rien ne bouge avant le dernier message

Statut : acceptée, complétée par #236 (1.0.12, 27 septembre 2026) : le point 3 tient
toujours pour le préfixe lui-même, mais la différence n'attend plus, elle part en fin de
prompt (voir « Conséquences »). Portée : §5 (moteur de contexte), §10 (providers), §16
(coûts).

## Contexte

Sur une journée réelle, des appels de plusieurs centaines de milliers de tokens ont raté le
cache du fournisseur alors qu'ils suivaient de peu un appel presque identique. Trois
mécanismes réécrivaient le début de la requête sans compaction :

- le contexte volatil (heure, rappel mémoire, intentions) était injecté dans le **dernier**
  message utilisateur : au tour suivant, il quittait ce message, qui changeait ;
- le raisonnement n'était renvoyé que pour le tour en cours : au tour suivant, les appels
  d'outil du tour précédent le perdaient ;
- l'instantané mémoire (T2), la liste des skills et des serveurs MCP (T1) pouvaient changer
  en pleine conversation (frontière d'épisode, serveur qui démarre).

OpenRouter garde déjà un fournisseur amont par `session_id` (dix minutes), mais un ordre
imposé (`provider.order`) désactive ce mécanisme.

## Décision

1. Le contexte volatil est **figé avec le message** qu'il accompagne (table
   `message_context`) et projeté avec lui dans toutes les requêtes suivantes.
2. Le raisonnement accompagne les messages d'**appel d'outil**, quel que soit le tour, et
   jamais les réponses finales.
3. Un préfixe T0 à T2 modifié **attend un cache froid** : pause de plus de 5 min ou
   compaction.
4. Le fournisseur amont de l'appel précédent passe en tête de `provider.order` pendant
   10 min, sauf ordre ou liste imposés par la configuration ; les replis restent permis.
   Son identifiant vient de `GET /models/{id}/endpoints` (`provider_name` → `tag`).
5. Chaque appel enregistre son empreinte (hachage chaîné des messages, du système, des
   outils) et, sur un raté, sa cause probable (`penelope usage --by miss`).

## Raisons

Un préfixe relu coûte une fraction du prix d'entrée : garder quelques centaines de tokens
de contexte ou de raisonnement déjà vus est moins cher que recalculer tout ce qui les suit
à chaque tour. La mesure dit ensuite, cause par cause, ce qui reste à corriger.

## Conséquences

Telles qu'écrites le jour de la décision : un nouveau souvenir, une skill ou un serveur
MCP ajoutés en pleine conversation n'apparaissaient dans le prompt qu'après 5 min de
pause ou à la prochaine compaction ; leurs outils restaient appelables tout de suite. Les
anciens messages gardent l'heure et le rappel de leur tour.

Depuis #236 (1.0.12), un banc de dix harnais ayant montré que le meilleur ajoute la
différence en fin quand le fichier d'instructions change :

- le préfixe retenu part toujours inchangé, et le message qui suit le changement porte
  dans son contexte volatil un bloc `<mise-a-jour>` : lignes retirées et ajoutées de
  chaque tuile, la tuile dite réécrite au-delà de 1 500 caractères, les skills chargées
  dont le corps a changé. Une seule fois : la différence est journalisée
  (`prompt.updated`) quand elle part avec son message, la suivante ne porte que ce qui
  est nouveau depuis (`penelope-conversation/src/prefix.rs`) ;
- la liste d'outils suit la même frontière que le préfixe (premier tour, pause plus
  longue que le cache, compaction) et est resservie telle quelle entre deux
  (`penelope_app::frozen_tools`) ; d'ici là, `tool_call` atteint l'outil découvert ;
- l'appel de résumé peut relire le préfixe de la conversation au prix du cache
  (`context.compaction_on_prefix` : `auto`, `always`, `never`) plutôt que repartir d'un
  système à lui.

Le préfixe modifié attend donc toujours un cache froid (point 3) ; ce qui a changé, c'est
que le modèle le sait dès le message suivant.
