# 0012 — Un job d'outil mort au redémarrage n'est jamais relancé d'office

Statut : acceptée. Portée : §4.2 (ledger d'effets), §8.4 (jobs durables), §11 (outils
natifs), §19 (RGPD). Issue : #204. Croise #83, #57, #65, #19, #104.

## Contexte

Un appel d'outil occupait le tour du début à la fin : `run_effect` planifiait l'effet puis
attendait `execute_cancellable`. Un `shell_exec` à `timeout_ms` de 3 600 000 ms
immobilisait donc le tour jusqu'à une heure, et le message du propriétaire attendait
derrière lui — même pour dire « laisse tomber ». Les jobs d'outils (`tool_jobs`) sortent
ces appels du tour, sur le modèle des tâches MCP (`mcp_tasks`).

Sortir du tour ne change rien au ledger : un effet non-lecture est planifié **avant**
exécution, et un effet resté `dispatching` au redémarrage devient `unknown`, ce qui pose
au propriétaire la carte « C'est fait / Relancer / Ignorer » (#83). Un job ouvre
simplement une fenêtre plus large pour ce cas : le daemon peut redémarrer pendant qu'un
`cargo test` de quarante minutes tourne.

Le ticket pose la question franchement : **faut-il relancer d'office un job mort quand
l'outil est déclaré idempotent ?**

## Décision

**Non. Un job dont le processus est mort n'est jamais relancé automatiquement, quelle que
soit l'idempotence déclarée de son outil.**

Au démarrage, `JobStore::recover_on_boot` passe tout job `working` ou `input_required` à
`failed`, avec pour résultat « le daemon a redémarré pendant ce job : son processus est
mort, rien n'a été relancé ». L'effet, lui, suit le chemin existant sans exception :
`dispatching` → `unknown` → une carte, une seule, même après plusieurs redémarrages. Le
job reste **à livrer** : sa relance part dans la session d'origine pour que le modèle
cesse de l'attendre.

### Pourquoi

1. **L'idempotence déclarée ne dit pas ce qu'on veut savoir ici.** `ToolSpec.idempotent`
   répond à « rejouer cet appel donne-t-il le même résultat ? ». La question d'un job mort
   est autre : « jusqu'où est-il allé ? ». Une commande shell tuée à mi-course a pu écrire
   des fichiers, pousser une branche, lancer un conteneur. Aucun `idempotent: true` ne
   couvre ça.
2. **La branche n'aurait aucun membre.** Les deux seuls outils détachables aujourd'hui —
   `shell_exec` et `sub_agent_spawn` — sont tous deux `idempotent: false`, et le resteront :
   l'un exécute une commande libre, l'autre dépense un budget de modèle. Écrire un chemin
   de relance automatique, ce serait écrire un chemin que rien n'emprunte et que le
   prochain outil détachable emprunterait par accident.
3. **Le propriétaire a déjà le bouton.** La carte de #83 porte « Relancer » : la relance
   existe, avec un humain dans la boucle, et elle coûte un clic. Ce n'est pas un manque,
   c'est le même mécanisme sans le silence.
4. **La promesse « aucun retry silencieux » est le contrat du ledger.** L'ouvrir pour les
   jobs créerait exactement le chemin que le §4.2 interdit, pour le cas où il est le plus
   difficile à raisonner : un travail long, interrompu à un point inconnu.

## Ce que ça coûte

Le propriétaire reçoit une carte pour un `cargo test` interrompu, alors qu'un simple
« relance » aurait suffi. C'est assumé : la carte dit quel job, quelle commande et quel
âge, et le tour de relance qui suit donne au modèle de quoi proposer la reprise lui-même.

## Un job n'existe que pour une conversation

Corollaire de la livraison : un job rend son résultat par un tour `Nudge` dans sa session
d'origine. Une session de sous-agent meurt avec sa conclusion, et une session de run de
workflow est pilotée par le moteur, pas par des tours de conversation — `enqueue_resume`
la détourne déjà vers le pilote. Une relance n'y trouverait personne.

Dans ces deux contextes, `background: true` est donc **ignoré** et l'appel s'exécute dans
le tour, comme avant #204 : il rend son vrai résultat, et un run de workflow garde son
mécanisme d'attente d'étape. Le flag n'est pas refusé, pour ne pas faire perdre une étape
ou une conclusion à cause d'un argument de trop.

## Table distincte plutôt qu'extension de `mcp_tasks`

Le ticket laissait le choix. `tool_jobs` est une table à part, pour trois raisons :

- `mcp_tasks.server` et `mcp_tasks.task_ref` sont `NOT NULL` et décrivent une tâche
  **chez un serveur MCP** ; un job natif n'a ni l'un ni l'autre ;
- `mcp_tasks` est un registre de **sondage** : `poll_at` pilote un `tasks/get` vers
  l'extérieur, et `TaskStore::due()` alimente cette boucle. Un job natif tourne dans le
  processus ; le faire apparaître dans `due()` enverrait la boucle MCP interroger un
  serveur qui n'existe pas ;
- un job porte ce qu'une tâche MCP n'a pas : l'outil, ses arguments, l'`effect_id` du
  ledger, le tour d'origine et la date de livraison.

Ce qui est repris tel quel : les états (`penelope_mcp::tasks::TaskState`, importé et non
redéfini), les colonnes `request`/`result`, et les mêmes règles de purge et de rétention,
dans le même lot.

Un écart assumé au ticket, qui demandait « même `poll_at` » : la colonne n'existe pas.
Elle pilote un sondage, et rien ne sonde un job natif — ni `job_wait`, qui relit la ligne,
ni la boucle de livraison, qui suit `delivered_at`. Une colonne morte qui prétend porter
une échéance est pire qu'une colonne absente, comme pour `llm_requests.request` en
[0011](0011-prompt-systeme-journalise.md). `updated_at` donne l'âge, `delivered_at` la
livraison.
