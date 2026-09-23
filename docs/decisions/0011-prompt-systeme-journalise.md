# 0011 — Le prompt système est journalisé en clair, adressé par son empreinte

Statut : acceptée. Portée : §4.1 (chaîne d'audit), §5 (moteur de contexte), §16 (coûts),
§19 (RGPD). Issue : #205.

## Contexte

Aucune requête envoyée à un modèle n'était reconstituable après coup. Les messages le
sont (table `messages`, store immuable) ; le prompt système, non : il est réassemblé à
chaque tour depuis le vault, la configuration, les skills, les serveurs MCP et la mémoire
rappelée, puis jeté. Le seul vestige était une empreinte, `usage.system_hash`.

Quatre conséquences, toutes mesurées :

- `penelope usage --by miss` savait dire « le préfixe a changé », jamais **quoi** ;
- une réponse surprenante n'était pas auditable : impossible de relire ce que le modèle
  avait sous les yeux ;
- la chaîne de hachage de l'event log prouvait tout sauf ce que le modèle avait lu ;
- la colonne `llm_requests.request`, prévue pour le corps de la requête, n'a **jamais**
  été écrite depuis l'origine : `plan()` ne renseignait que `body_hash`.

## Décision

1. **Le prompt rendu devient une ligne**, dans `prompt_snapshots`, adressée par le
   `system_hash` déjà calculé par `Fingerprint` — aucun nouveau calcul, aucune nouvelle
   clé. Le même préfixe sur cinq cents tours reste une ligne ; `uses` compte les appels.
2. **L'instantané porte le préfixe stable (T0 à T2) seulement.** Le contexte volatil (T4)
   reste dans `message_context`, où #17 l'a mis : l'y inclure ferait varier le rendu à
   chaque tour et effondrerait la déduplication.
3. **La découpe en tuiles accompagne le rendu, sans texte** : décalage, longueur et
   empreinte courte par tuile. Le diff se lit tuile par tuile, et `usage --by miss` gagne
   la cause `prefixe:T1` plutôt que `prefixe`.
4. **`llm_requests.request` est supprimée** (migration `0018`) et remplacée par les trois
   clés déjà calculées : `system_hash`, `tools_hash`, `request_hash`. Une colonne morte
   qui prétend porter le corps d'une requête est pire qu'une colonne absente.
5. **Le texte est gardé tel quel, et rédigé à la sortie.** Redacter avant l'écriture
   casserait l'égalité octet pour octet avec l'empreinte, c'est-à-dire la seule preuve que
   la reconstitution est exacte. `penelope audit show` passe donc le rendu par
   `redact` (#134), comme tout ce qui sort de la base.
6. **Purge et rétention dans le même lot.** Un prompt contient le profil, la mémoire
   rappelée et les notes de session : c'est de la donnée personnelle. La purge d'une
   session emporte les instantanés qu'elle seule référençait et coupe le renvoi depuis
   `usage` ; la rétention n'efface que ce que plus aucune ligne ne cite.

## Raisons

L'invariant visé est celui de DeepSeek Harness : *ce que le modèle voit est journalisé*.
Un journal qui prouve les effets mais pas leur cause explique la moitié d'un tour.

Le coût suit le nombre de prompts **distincts**, pas le nombre de tours : sur une instance
dont le préfixe est stable par construction (décision
[0008](0008-cache-de-prompt.md)), c'est quelques dizaines de kilo-octets par mois.
`doctor` le mesure et le dit, avec le nombre de changements de préfixe en 24 h.

## Conséquences assumées

- **`usage` n'est plus totalement intouchable par la purge.** La ligne comptable reste
  avec ses jetons et son coût ; seule la clé qui menait au texte est mise à `NULL`. Sans
  cela, le prompt d'une session purgée survivrait à sa purge.
- **Un tour très ancien n'est pas relisible** si son instantané est parti en rétention :
  `penelope audit show` le dit en réserve plutôt que de rendre un texte approchant.
- **La reconstitution des messages reste approximative** quand l'historique a été résumé
  depuis : l'outil le déclare, il ne comble pas.
- L'événement `turn.started` porte désormais `system_hash` et `tools_hash` : la chaîne
  hachée référence ce que le modèle a lu, sans le recopier.
