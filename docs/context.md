# Contexte et compaction

Ce qui se passe quand une conversation s'allonge : ce qui part au modèle à chaque appel, ce
qui déclenche un résumé, ce qui reste mot pour mot, ce qui arrive quand le résumé échoue,
et comment le cache du fournisseur survit à tout cela. Les chiffres de cette page sont
vérifiés contre le code par `cargo test -p penelope-evals --test docs` : une clé qui
n'existe plus ou un défaut qui a changé fait échouer le test.

## Ce qu'un appel envoie

```
 [T0 identité + règles][T1 index des capacités][T2 instantanés mémoire]   préfixe stable
 [résumé de la conversation antérieure, s'il y en a un]                   message système
 [messages non résumés, chacun avec le contexte volatil de son tour]      historique
 [T4 heure, rappel mémoire, intentions, notes de travail]                 fin de prompt
 + définitions d'outils (noyau et méta-outils)
```

Le préfixe ne change pas en cours de conversation (décision
[0008](decisions/0008-cache-de-prompt.md)) : un souvenir, une skill ou un serveur MCP
ajoutés n'y entrent qu'après une pause plus longue que le cache (5 min) ou à la
compaction suivante. Le contexte volatil est figé avec le message qu'il accompagne : un
ancien message garde l'heure et le rappel de son tour, et rien ne bouge avant le dernier
message. Les outils rares ne sont que nommés (voir « Outils natifs » dans
[install-headless.md](install-headless.md#outils-natifs)).

## Les chiffres par fenêtre

Tout dépend de la fenêtre du modèle de conversation (lue dans le catalogue, 128 000
tokens pour un modèle inconnu) et de six clés :

- `context.compaction_threshold` = `0.7` : part de la fenêtre où le résumé devient dû ;
- `context.background_compaction_margin` = `0.1` : le résumé se prépare dix points plus
  tôt, en tâche de fond ;
- `context.max_prompt_tokens` = `120000` : plafond de coût, quelle que soit la fenêtre ;
  sur une fenêtre immense, la compaction part vers 103 k au lieu de 600 k ;
- `context.tail_ratio` = `0.025`, `context.tail_min_tokens` = `10000` et
  `context.tail_max_tokens` = `25000` : la queue gardée mot pour mot ;
- `context.max_tool_result_share` = `0.25` et `context.large_payload_tokens` = `25000` :
  la taille d'un groupe de résultats d'outils gardé entier.

Deux garde-fous s'y ajoutent sur les fenêtres courtes. Le seuil descend pour laisser la
place, au-dessus de lui, d'un groupe de résultats d'outils entier et de la réserve de
réponse (10 % de la fenêtre, entre 1 000 et 32 000 tokens) ; un seuil propre au modèle
(`context.model_thresholds.<nom>`) l'emporte toujours. La queue verbatim ne dépasse
jamais le quart du seuil de fond, sinon elle couvrirait tout ce qu'il faudrait résumer.

<!-- reference:fenetres:debut (générée : UPDATE_DOCS=1 cargo test -p penelope-evals --test docs) -->
| Fenêtre | Seuil | Compaction de fond dès | Réserve de réponse | Queue verbatim | Groupe d'outils gardé entier |
|---|---|---|---|---|---|
| 8 192 | 63 % | 4 324 | 1 000 | 1 081 | 2 048 |
| 32 768 | 65 % | 18 023 | 3 276 | 4 505 | 8 192 |
| 131 072 | 70 % | 78 643 | 13 107 | 10 000 | 25 000 |
| 200 000 | 70 % | 102 856 | 20 000 | 10 000 | 25 000 |
| 1 000 000 | 70 % | 102 856 | 32 000 | 25 000 | 25 000 |
<!-- reference:fenetres:fin -->

## Une session, du premier tour à la cinquième compaction

```
 tour 1 ──► historique court : tout part tel quel
   │
   ▼ un résultat d'outil dépasse le budget d'un groupe
 niveau 1 : il part en artefact, le transcript garde tête, queue et identifiant
   │
   ▼ la requête approche la fenêtre
 niveaux 0 et 2 : vieux résultats élidés dans la requête seulement
   │
   ▼ la requête (estimée ou facturée) atteint le seuil de fond
 niveau 3 : résumé en tâche de fond, publié à la fin du tour
   │
   ▼ compactions suivantes : le même résumé est mis à jour et prolongé
   ▼ la requête ne tient toujours pas dans la fenêtre
 niveau 4 : réduction sans modèle, taille prouvée avant l'envoi
```

**Premiers tours.** L'historique part entier. Chaque message est estimé en tokens à son
enregistrement ; l'estimation est recalée sur ce que le fournisseur facture.

**Un gros résultat d'outil (niveau 1).** Un résultat plus gros que
`context.large_payload_tokens` est rangé en artefact dès son arrivée : le transcript garde
60 % de début et 40 % de fin du budget, avec `artifact_read("…")` pour relire le reste.
C'est la seule modification de l'historique faite sans modèle. Plusieurs résultats d'un
même appel se partagent le budget d'un groupe, ce que les petits n'utilisent pas revient
aux gros.

**La requête approche la fenêtre (niveaux 0 et 2).** Sans toucher à l'historique, la
requête remplace d'abord les vieux résultats volatils (listings, lectures) par un
pointeur `history_expand(…)`, puis réduit les autres vieux résultats à 200 caractères de
début et 100 de fin, puis à un pointeur, du plus ancien au plus récent, jusqu'à tenir. La
queue verbatim n'est jamais touchée. Les paires appel et résultat restent valides.

**Le seuil de fond est atteint (niveau 3).** À la fin du tour, la requête estimée **ou**
le prompt réellement facturé au dernier appel est comparé au seuil de fond : les
instantanés, l'index et les outils, que l'estimation voit mal, comptent. Le résumé part en
tâche de fond sur l'alias du rôle `compaction` (`summarizer`) ; la conversation ne
s'arrête pas, et le résumé est publié à la fin du tour en cours, jamais au milieu. Ce qui
est résumé : les messages que la queue ne garde pas. La queue garde, en partant de la fin,
des groupes entiers (un appel d'outils et ses résultats ne sont jamais séparés) dans son
budget, et toujours au moins `context.min_tail_user_messages` = `2` messages du
propriétaire. Un lot de moins de 2 000 tokens n'appelle pas le résumeur, sauf `/compact`.

Le résumé suit un gabarit de neuf sections : objectif, contraintes et préférences, fait,
en cours, bloqué, décisions clés, fichiers et ressources, prochaines étapes, contexte
critique ; 4 000 caractères au plus par section, sortie structurée stricte quand le modèle
la sait. S'y ajoutent mécaniquement les ancres (identifiants, chemins, tickets, SHA, URLs,
120 au plus, recopiés du texte complet) et les derniers messages du propriétaire cités tels
quels, dans un huitième du budget de la queue. Le résumeur voit les longs messages
échantillonnés (12 000 caractères, 4 000 pour un résultat d'outil, début et fin), pas les
ancres, qui viennent du texte entier.

**Deuxième à cinquième compaction.** Le résumé existant n'est pas refait : le résumeur
reçoit le résumé précédent et les nouveaux échanges, garde ce qui reste vrai, corrige ce
qui a changé, retire ce qui est clos. Le même nœud prolonge sa couverture (messages #1 à
#N), ses ancres fusionnent sous le même plafond, ses citations du propriétaire sont
reprises sur toute la couverture. Une conversation longue ne porte donc qu'un résumé, dont
la taille reste bornée par le gabarit. Si le retard est grand, le travail est découpé en
lots qui tiennent dans la fenêtre du résumeur, douze au plus par passe ; la passe suivante
reprend le reste. Rien n'est effacé : `history_grep`, `history_expand` et
`history_describe` relisent les échanges résumés.

**Reprise d'une session froide et fork.** Une session reprise après une pause plus
longue que le cache, ou le premier tour d'un fork, dont le dernier prompt dépasse le seuil
de fond, est résumée **avant** l'appel au modèle : le cache est perdu de toute façon.

## Quand le résumé échoue

La sortie du résumeur est validée : un objet JSON, au moins une des sections objectif,
fait, en cours ou prochaines étapes remplie, sinon elle est rejetée et rien n'est
publié. Le résumeur a 120 s, plus une seconde par millier de tokens du lot (420 s au
plus) ; au-delà, il est en échec. Après un échec, la compaction de fond attend
`context.cooldown_ms` = `[60000,300000,900000]` : 60 s, puis 5 min, puis 15 min ; les
niveaux 0 à 2 continuent de protéger la requête, et rien n'est publié, donc le préfixe ne
bouge pas. `/compact` (ou `penelope session compact`) lève l'attente et force un lot, même
petit.

Avant de compter un échec, un résumeur qui n'a pas répondu à temps (ou erreur passagère :
flux muet, 5xx, 429) est relancé une fois sur le début du même lot, trois fois plus court.
Si un alias de repli est déclaré pour le résumeur (`models.routing.fallback`, par exemple
`summarizer = ["main"]`), il est essayé ensuite, une fois, seulement si son coût estimé
tient dans la réserve des résumés ; un résumé invalide n'est pas une affaire de taille et
n'est pas relancé. Au troisième échec de suite, la compaction se fait **sans modèle** :
un seul nœud, qui garde les derniers messages du propriétaire du passage et ses ancres
tels quels et dit le reste relisible par `history_expand` ; le propriétaire reçoit un
message avec le coût moyen des derniers tours, et `context.compaction_mechanical` est
écrit. `/status` dit une session dont le résumé échoue (échecs de suite, coût par tour),
et le digest du matin liste celles des dernières 24 h.

Une fois le plafond du jour atteint, les résumés de fond continuent dans la limite de
`budget.compaction_reserve_usd` = `0.5` : un résumé coûte peu et allège chaque appel
suivant. Les plafonds de session et de run ne les arrêtent jamais.

**Requête trop grosse (niveau 4).** Si, malgré les niveaux 0 à 2, la requête estimée
ne tient pas dans la fenêtre moins la réserve de réponse, elle est réduite sans modèle
avant l'envoi : corps des anciens résultats d'outils vidés, puis groupes les plus anciens
retirés, en gardant le système et le dernier message du propriétaire ; la taille est
prouvée localement, jamais devinée. Si le fournisseur refuse quand même pour la taille, le
tour compacte aussitôt (en attendant jusqu'à 200 s un résumé déjà en route) et réessaie
une fois ; sans résumé publié, le tour échoue avec l'erreur du fournisseur.

## Le cache

Un résumé publié change le début de l'historique : c'est une frontière de cache, comme
une pause de plus de 5 min. C'est pourquoi il n'est publié qu'à la fin d'un tour, et
pourquoi le préfixe en attente (souvenir, skill, serveur ajoutés) profite de la même
frontière. Entre deux compactions, chaque appel relit le préfixe au tarif du cache ; chez
Anthropic, un marqueur `cache_control` le signale explicitement. `penelope usage --by miss`
donne, pour chaque raté, sa cause probable : premier appel, pause, préfixe, outils,
modèle, historique réécrit, fournisseur amont différent.

## Observer

Chaque décision laisse un événement : `context.compaction_requested` (avec la taille qui
l'a déclenché), `context.compaction_skipped` (et sa raison : attente après échec, rien à
compacter, réserve épuisée), `context.compaction_failed`, `context.compacted`. `/status`,
`/budget` et `self_status` donnent la taille réelle du contexte, les deux seuils et la
dernière compaction ; le compteur `penelope_compactions_total` de `penelope metrics` les
compte par déclencheur et par issue.

Le champ `evidence` de `context.compacted` observe sans bloquer les actions marquées
`TODO:`, `À faire:` ou `- [ ]` et les identifiants des messages utilisateur du
transcript échantillonné du lot.
Il les recherche dans le contexte final, **ancres et citations verbatim comprises**.
L'événement donne les nombres trouvés et absents, avec au plus trois exemples de
80 caractères par catégorie ; la métrique
`penelope_compaction_missing_evidence_total` ne contient que les comptes. Le contrôle
est une comparaison textuelle : une reformulation peut apparaître comme absente, et
une obligation non marquée n'est pas détectée. Il n'ajoute ni appel au modèle ni
modification du résumé accepté, y compris lors d'une compaction sans modèle.
