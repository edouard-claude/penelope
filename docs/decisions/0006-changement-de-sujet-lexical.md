# 0006 — Changement de sujet mesuré sans modèle

Statut : acceptée. Portée : §6.6 (frontières d'épisode).

## Contexte

Le PRD clôt un épisode quand « le classifieur » détecte un changement de sujet
(similarité thématique inférieure à 0,35 sur 3 messages). Le classifieur existant est
un appel au modèle rapide qui range chaque message par complexité ; il ne voit ni
l'épisode ni son sujet.

## Décision

La similarité est calculée localement : racines de 4 lettres des mots significatifs
(mots vides retirés), cosinus entre le message entrant et les messages du propriétaire
de l'épisode. Trois messages consécutifs sous 0,35 ouvrent un nouvel épisode ; un
message de moins de trois mots significatifs ne compte ni pour ni contre.

## Raisons

1. **Aucun appel de plus par message.** Élargir le classifieur à l'épisode entier
   multiplierait ses tokens à chaque tour, pour une décision qui ne sert qu'à déclencher
   une relecture en arrière-plan.
2. **Une fausse frontière coûte peu.** Elle ouvre un épisode (relecture d'un transcript,
   instantané T2 recalculé), elle ne perd rien : l'historique et la session restent.
3. **C'est déterministe et testable** sans modèle réel.

## Conséquences

Un sujet qui glisse de vocabulaire (même projet, termes tous nouveaux) peut produire une
frontière de trop ; deux sujets qui partagent beaucoup de mots peuvent n'en produire
aucune, l'inactivité de 2 h et `/new` restant des bornes sûres. Si la précision déçoit à
l'usage, le calcul peut passer par l'index vectoriel de la mémoire sans changer le reste.
