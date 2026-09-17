# 0008 — Cache de prompt : rien ne bouge avant le dernier message

Statut : acceptée. Portée : §5 (moteur de contexte), §10 (providers), §16 (coûts).

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

Un nouveau souvenir, une skill ou un serveur MCP ajoutés en pleine conversation
n'apparaissent dans le prompt qu'après 5 min de pause ou à la prochaine compaction ; leurs
outils restent appelables tout de suite. Les anciens messages gardent l'heure et le rappel
de leur tour.
