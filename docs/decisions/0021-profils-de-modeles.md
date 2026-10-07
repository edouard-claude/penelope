# 0021 : Un modèle principal par profil, une résolution unique, la voix à part ; garde Codex réglable, écarts annoncés

Statut : acceptée (7 octobre 2026). Portée : §10.2 et §10.3 (alias, rôles, routage,
repli), décision [0010](0010-fournisseur-codex-oauth.md) (périmètre de l'abonnement
ChatGPT). Issues #332, #333, #334, #335.

## Contexte

Le propriétaire a mis `main` sur `codex:gpt-5.6-sol` et pensait être « sur Codex ». En
réalité, tous les autres alias (`fast`, `reasoning`, `summarizer`…) visaient DeepSeek
Flash, le classifieur envoyait les messages difficiles vers `reasoning`, et la garde
`codex_scope` remplaçait Codex sans un mot pour tout travail de fond, runs de workflow
lancés par le propriétaire compris. Il n'existait pas de notion de modèle principal : tout
était rôle → alias → `fournisseur:modèle`, avec un repli propre à chaque site (titre et
juge sur `fast`, rêve sur `compaction`, `memoire` lu comme un rôle qui n'existait pas,
pointage sur la vision). Rien ne disait ce qui tournait vraiment.

L'état des lieux du 07/10 a aussi relevé des défauts : étapes de workflow sans repli ni
compaction, échec d'un tour avant la chaîne de repli quand le fournisseur du principal
manque, voix qui ignorait `providers.extra`, modèle local envoyé en silence à OpenRouter,
escalade et règle « image jointe » mortes.

Références : OpenClaw (`model.primary`, `utilityModel`, `mediaModels`, annonce d'un repli
une fois par changement d'état), Hermes (`model.default`, `auxiliary.<tâche>`).

## Décision

1. **Un profil** (`[models.profiles.<nom>]`) porte un `primary`, des `overrides.<rôle>`,
   des `capabilities.<capacité>` (`image_generate`, `vision`, `embedding`), des étages
   `routing.low|medium|high` et des chaînes `fallback`. `models.profile` nomme l'actif ;
   une bascule est une génération de configuration, sans redémarrage.
2. **Une seule résolution**, `Config::resolve_role_with` (noyau), appelée par tous les
   sites qui choisissent un modèle : surcharge du profil, sinon rôle de voix, sinon pour un
   rôle de capacité la capacité posée, puis le principal si le catalogue dit qu'il sait
   faire (`ModelCaps`, que `Catalog` implémente), puis le modèle livré ; sinon le
   principal. Un rôle inconnu suit le principal : plus aucun repli codé en dur.
3. **La voix vit à part** : `voice.stt`, `voice.tts`, `voice.narrator`. Elle ne passe
   jamais par le principal, ni par l'abonnement.
4. **Migration sans écriture** : sans `[models.profiles.defaut]`, le profil `defaut` est
   déduit à chaque lecture des clés d'avant, rôle par rôle comme l'ancien code, garde
   `deny`. La première modification d'un profil déduit l'écrit en entier
   (`Models::materialize`), pour qu'une écriture partielle ne le perde pas.
5. **Le classifieur** ne choisit plus un autre modèle que le principal, sauf étage posé
   dans le profil ; sans étage, il n'est pas appelé.
6. **Garde Codex réglable** : `codex_background = "deny" | "allow"` par profil. Un run de
   workflow lancé par le propriétaire est son tour : il suit le principal ; seuls les runs
   planifiés (marqués au démarrage par la planification, sous-workflows compris) restent
   sous la garde. Le choix est explicite à la création d'un profil Codex, et le risque
   (suspension d'un abonnement utilisé en automatique) est écrit une fois, dans la doc et
   dans `/model`.
7. **Tout écart est annoncé** une fois par changement d'état (`penelope_app::model_watch`) :
   repli après panne, garde appliquée, principal injoignable, et le retour. Le cœur verse
   un événement `model.notice` ; le canal l'écrit dans la conversation concernée. Un modèle
   épinglé n'a pas de repli : il échoue en le disant.
8. **Les méthodes `model.*` sortent du daemon** vers `penelope_ops::models`, avec
   `model.unset` et `model.profile` ; `/model` et `penelope model` en sont deux vues.

## Écarts assumés

- **La résolution vit dans le noyau**, pas dans `penelope-llm` ni `penelope-app` comme
  l'issue le proposait : toutes les crates qui choisissent un modèle tiennent déjà une
  `Config`, et le catalogue entre par un trait (`ModelCaps`) au lieu d'une dépendance.
- **Une capacité posée prime sur le principal** : l'issue place le principal avant la
  capacité. Un modèle d'image choisi est un choix ; le principal ne prend la capacité que
  si le profil n'en nomme pas. Le profil déduit nomme toujours ses capacités, ce qui
  garde la migration exacte quel que soit le catalogue.
- **Les alias restent** : des noms courts que les profils peuvent citer. `juge`,
  `memoire`, `pointage` ne sont pas convertis en surcharges (cela changerait ce qui
  tourne) : `doctor` les nomme comme lus par rien.
- **`models.routing.classifier` et `sticky` restent globaux** ; seuls les étages et les
  replis sont par profil.
- **L'escalade et la règle « image jointe » du routeur sont retirées** : jamais appelées,
  et une photo est déjà montrée au modèle de la session s'il lit les images, décrite sinon.

## Conséquences

- La table rôle → modèle effectif → raison est la même partout : `penelope model list`,
  `/model` (« Tout voir »), `self_status`.
- Un fichier écrit par 1.0.47 et relu par une version plus ancienne garde ses clés
  d'avant : les profils y sont des clés inconnues, ignorées et signalées (#76).
