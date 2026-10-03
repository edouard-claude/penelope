# 0018 : L'agenda entre par CalDAV, en lecture, dans un serveur MCP à part

Statut : acceptée (3 octobre 2026). Portée : digest du matin (`penelope-dream`,
`penelope-orchestrator`), configuration (`digest.agenda`), nouvelle crate
`penelope-agenda-mcp`, sous-commande `penelope agenda-mcp`. Issue #295.

## Contexte

Le PRD ne met aucun agenda dans le périmètre de Pénélope : l'« Aujourd'hui » du digest ne
listait que ses propres planifications, et rien ne lisait les rendez-vous, anniversaires
ou vacances du propriétaire, alors qu'elle sert déjà de rappel pour l'école, les factures
et les démarches (#295). L'instance tourne sur un Mac sans écran, piloté en SSH : aucune
autorisation TCC ne peut être cliquée, ce qui ferme la voie du calendrier Apple local
(EventKit, `shortcuts run`).

Un agenda est aussi une source de données de tiers : un calendrier partagé (famille,
travail) décrit des rendez-vous qui ne sont pas ceux du propriétaire.

## Décision

1. **CalDAV, en lecture seule.** Le connecteur parle le protocole ouvert (RFC 4791) avec
   un mot de passe d'application : iCloud, Fastmail, Nextcloud, tout serveur CalDAV en
   authentification Basic. Aucune écriture dans ce lot ; elle viendra, soumise à
   approbation, dans un lot à part.
2. **Un serveur MCP à part, pas du code dans le cœur.** La crate `penelope-agenda-mcp`
   est un serveur stdio comme le pont de messagerie : Pénélope le déclare dans
   `mcp.d/agenda.toml`, le modèle l'atteint par `tool_call`, le digest par le port
   `McpGateway` (jamais par le client MCP en direct). Le cœur ne connaît pas CalDAV.
3. **Le binaire est le même.** Le serveur est aussi la sous-commande `penelope
   agenda-mcp` : la release ne change pas (un seul binaire emballé, `penelope upgrade`
   inchangé), et `mcp.d/agenda.toml` lance `penelope` lui-même. Le binaire
   `penelope-agenda-mcp` existe pour qui le veut seul, hors release.
4. **Les identifiants hors du dépôt et hors de `mcp.d`.** Adresse et compte dans `env` de
   la déclaration, mot de passe par `${SECRET:agenda_password}` ; le serveur ne lit jamais
   le trousseau lui-même et ne répète jamais le mot de passe dans une erreur.
5. **Rien n'entre en mémoire durable depuis l'agenda.** Les événements sont cités dans le
   digest et dans les réponses, jamais promus : un calendrier partagé contient des
   données de tiers. La relecture mémoire et la cohérence des planifications (points 2 et
   3 de #295) restent ouvertes.
6. **Le fuseau est celui du propriétaire.** `events_today` reçoit `owner.timezone` ; le
   serveur convertit chaque événement (TZID, UTC, journée entière) dans ce fuseau.

## Raisons

- **Sans écran, CalDAV est la seule voie** qui marche en SSH et sur toute machine ; elle
  couvre iCloud, le cas du propriétaire, sans dépendre d'Apple.
- **La frontière de la V1** (décision [0013](0013-decoupage-du-daemon.md)) veut qu'une
  capacité externe soit un serveur MCP derrière un port : un connecteur dans le cœur
  aurait fait entrer un protocole réseau et un parseur iCalendar dans `penelope-dream`.
- **Un second binaire dans la release** aurait demandé de reprendre `release.yml`
  (matrice, `lipo`, archives, sommes), le `Makefile` et `penelope upgrade`, qui remplace
  un fichier : la sous-commande donne la même capacité sans toucher à la chaîne de
  livraison.
- **Le parseur iCalendar est écrit à la main** parce qu'aucune crate raisonnable ne
  couvrait les récurrences sans tirer son propre modèle de dates ; il tient en deux
  fichiers et ses cas sont des fixtures `.ics`.

## Conséquences

- Nouvelle crate `penelope-agenda-mcp` (hors des règles de dépendance d'archtest, qui ne
  la nomment pas ; elle ne dépend que de `penelope-mcp` pour les types du protocole) ;
  `penelope-cli` en dépend. Nouvelle dépendance `roxmltree` (MIT ou Apache-2.0, sans
  dépendance) pour les réponses `multistatus`.
- Clé `digest.agenda` ; champ `agenda_error` dans `DigestInputs` ; `DigestFeed` reçoit le
  branchement MCP.
- Google Agenda n'entre pas dans ce lot : son point CalDAV exige OAuth 2, pas un mot de
  passe d'application.
- Récurrences servies : quotidienne, hebdomadaire (`BYDAY`), mensuelle (`BYMONTHDAY`,
  `BYDAY` avec rang), annuelle, `INTERVAL`, `COUNT`, `UNTIL`, `EXDATE`, `RECURRENCE-ID` ;
  les fréquences infra-journalières et `BYSETPOS` ne le sont pas.
- Un lot futur pourra ajouter l'écriture (sous approbation), la relecture mémoire qui
  propose une entrée d'agenda, et l'avertissement de cohérence quand une planification
  double un événement.
