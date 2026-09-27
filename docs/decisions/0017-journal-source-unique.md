# 0017 : Le journal d'événements est la source unique de la conversation

Statut : acceptée (26 septembre 2026). Portée : §4.1 (event log et chaîne d'audit), §5
(moteur de contexte), §19 (RGPD). Épopée #208, lots E et I (T1 à T22) ; issues #205 et
#206. Spécification : `design/v1/source-de-verite.md`.

## Contexte

Jusqu'à la 0.17, deux mondes cohabitaient sans se connaître. Le journal `events` (chaîne
de hachage) prouvait le contrôle : tours, effets, approbations. Le contenu de la
conversation vivait ailleurs, dans `messages`, `message_context`, `lcm_nodes` et
`prompt_snapshots`, réécrits en place : la compaction marquait `compacted`, le niveau 1
remplaçait un corps, `/rewind` tronquait, un fork copiait. Conséquences mesurées :

- aucune requête passée n'était reconstituable après une compaction : `audit show`
  relisait les lignes d'aujourd'hui (#205 a journalisé le prompt système, pas
  l'historique) ;
- une réponse vide ou un flux coupé ne laissait aucune trace de ce que le modèle avait
  rendu (#206) ;
- la chaîne d'audit prouvait tout sauf ce que le modèle avait lu, et une ligne modifiée à
  la main ne se voyait nulle part.

La migration s'est faite en quatre phases (spécification §4) : double écriture et
scellement (1.0.0-alpha.4 et .5), comparaison par `penelope history verify` et projecteur
(1.0.0-alpha.6), lecture depuis le journal par défaut (1.0.0-alpha.7), retrait du chemin
direct (T16). Le propriétaire a validé la
troisième phase le 26 septembre 2026 sur une copie de sa base réelle : 63 sessions
scellées (8 630 messages), `history verify` à zéro divergence sur 69 sessions et 8 793
nœuds, deux tours de conversation justes.

## Décision

1. **La conversation est une suite d'événements `conv.*`** (`system`, `user`, `context`,
   `assistant`, `tool_result`, `attempt`, `summary`, `rewind`, `fork`, `import`),
   versionnés (`"v": 1`), chacun avec son opération de surface : `append`, `replace` d'une
   plage, `cut`, `inherit`, `seal`. Un contenu ne change jamais en place : le niveau 1, la
   compaction, le retour arrière sont des événements qui remplacent ou coupent.
2. **Ce que le modèle lit est un pliage pur du journal** (`derive`, dans
   `penelope-context`, sans base ni horloge), repris sur les seuls événements nouveaux. Les
   seules transformations non journalisées sont les niveaux 0, 2 et 4 et la réparation
   des paires, fonctions pures dont la réponse enregistre l'effet (`projection`).
3. **Les tables sont des caches** : `messages`, `messages_fts`, `message_context`,
   `lcm_nodes`, `lcm_edges`, `prompt_snapshots` et le filigrane `projections_session` sont
   écrits dans la seconde transaction de chaque événement (`HistoryStore::journaled`), un
   fork et l'archive d'un retour arrière projetés depuis leur héritage, rattrapés depuis le
   filigrane, refondus par `penelope history reindex` et comparés au journal par
   `penelope history verify`. Aucune écriture SQL sur ces tables hors de
   `penelope-context` : `penelope-archtest` le vérifie (`caches`,
   `only_the_context_crate_writes_the_conversation_caches`) ; la seule exemption nommée est
   le harnais des scénarios, qui efface les caches d'une base temporaire pour prouver que
   la refonte les redonne.
4. **La conversation se relit toujours depuis le journal.** La clé `history.source` est
   retirée : un fichier qui la porte encore se charge, avec un avertissement. Les caches ne
   servent plus qu'au repli, pour une session que le journal ne sait pas redonner
   (l'archive d'un retour arrière, qui n'a pas de journal à elle) ou dont le journal ne se
   plie pas (erreur au journal du daemon, `doctor` et `verify` nomment la session).
5. **L'historique d'avant le journal est scellé**, pas recopié : un `conv.import` par
   session porte l'empreinte du préfixe 0.17, que `verify` recalcule ; les lignes scellées
   ne sont jamais réécrites par la refonte.
6. **Tentatives hors historique** (#206) : une réponse vide, un flux coupé, un repli
   laissent un `conv.attempt`, épinglé à sa requête (`llm_request_id`), sans effet sur la
   surface ; seule la consigne de relance entre dans la requête suivante.
7. **Plus d'état de conversation en `kv`** : le message d'un tour déjà écrit se lit au
   journal par la clé du tour (`HistoryStore::recorded`, au lieu de `turn.recorded.*`), le
   préfixe retenu par le dernier `conv.system` (`HistoryStore::retained_prefix`, au lieu
   de `prompt.prefix.*`), libéré par `context.compacted` ou `session.project`.

## Raisons

C'est le principe que Pénélope applique déjà à sa mémoire (le vault est la vérité, l'index
se reconstruit) et celui de DeepSeek Harness : un journal append-only, une dérivation pure,
des projections jetables. Il donne trois preuves que la 0.17 n'avait pas : chaque requête
se recalcule depuis le journal (critère CA 4.5, comparé octet pour octet à chaque appel des
scénarios enregistrés), les caches se refont sans perte (CA 4.6), et le cache de prompt ne
peut être cassé au milieu d'un tour que par les deux remplacements qui ne coûtent rien
(CA 5.5 ; décision [0008](0008-cache-de-prompt.md)). CA 4.5 remplace le test
`both_history_sources_send_the_same_requests`, qui comparait la lecture des tables à celle
du journal tant que les deux existaient.

Retirer le chemin direct plutôt que le garder en secours : une écriture sans événement est
une ligne que `verify` signale et que `reindex` efface ; deux chemins d'écriture, c'est
deux vérités possibles. Le cliquet d'architecture empêche qu'il revienne par un correctif.

## Conséquences assumées

- **Le contenu entre dans la chaîne hachée** : `audit verify` le relit, la base grossit
  d'à peu près la taille de `messages`. Les gros résultats restent en artefact (niveau 1),
  l'événement ne porte que la tête, la queue et le pointeur.
- **Numérotation à trous** : l'adresse d'un nœud est le `seq` de son événement ; les
  événements d'observation consomment des numéros sans produire de nœud. Rien ne doit
  plus supposer `seq + 1`.
- **Un fork est une référence** : la fille plie sa mère jusqu'au point de fork. La purge
  de la mère emporte le préfixe hérité de ses filles ; `penelope session purge` et
  `/purge` les nomment avant de demander confirmation (`session.purge_preview`).
- **Rétention des tentatives par payload** : le texte partiel d'une `conv.attempt` part
  après `retention.days` par le mécanisme de purge (`event_purges`, hash conservé) ;
  `audit verify` compte alors des événements `purged` dans des sessions vivantes, ce qui
  est attendu.
- **Un journal incohérent est une erreur, jamais une devinette** : `verify` le dit ; en
  production, la lecture retombe sur les caches et `doctor` nomme la session.
- **Les sous-agents** (`MemoryConversation`) n'écrivent rien : leur conclusion entre dans
  le journal du parent comme résultat d'outil.
- **`penelope-ops` dépend de `penelope-context`** : la purge d'une session efface ses
  caches par `HistoryStore::purge_session_in`, dans sa propre transaction.
- **Écarts restants** : l'archive d'un `/rewind` n'est pas un fork par référence (elle n'a
  pas de journal à elle ; ses lignes sont projetées depuis le `conv.rewind` de sa mère) ;
  les niveaux 0 et 2 ne sont pas journalisés (à rouvrir si `audit show` doit rendre une
  requête dégradée sans la recalculer) ; un retour arrière qui coupe dans le préfixe
  scellé retire des lignes que le `conv.import` compte, et la mère ne se replie plus (la
  lecture retombe sur ses caches, `verify` le signale).

## Alternatives écartées

- **Un événement d'import par message plutôt qu'un scellement** : plus de double source,
  mais une migration de démarrage longue, une chaîne alourdie d'autant et des maillons
  datés d'aujourd'hui pour des contenus d'hier.
- **Garder la lecture des tables en secours derrière une clé** : c'était l'état de la
  phase 3 ; la comparaison à chaque requête a tenu sur toute la suite et sur la base du
  propriétaire, et une clé de plus est un chemin de plus à tester.
- **Le cache seul, sans journal du contenu** (la 0.17) : ce que le modèle a lu n'était ni
  prouvé ni reconstituable.
