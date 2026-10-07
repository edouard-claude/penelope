# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 7 octobre 2026.

## Version 1

Une section `### x.y.z` par lot, la plus récente en tête (décision
[0015](decisions/0015-gel-0.17-et-branche-v1.md), épopée #208). Les versions `1.0.0-alpha.N`
ont été écrites sur la branche `v1`, sans tag ni release, avant la bascule vers `main`.
La charte et les spécifications sont dans `design/v1/`.

### 1.0.47

**Modèles : ce que tu choisis est ce qui tourne. Un modèle principal par profil, une
résolution unique pour tous les rôles, la voix à part ; la garde Codex devient un réglage,
un workflow lancé par le propriétaire suit le principal, tout écart est annoncé ; `/model`
refondu et son équivalent CLI ; défauts relevés par l'état des lieux (#332, #333, #334,
#335).** Décision [0021](decisions/0021-profils-de-modeles.md).

Constat (07/10) : `main` sur `codex:gpt-5.6-sol`, le propriétaire se croyait « sur Codex ».
Tous les autres alias visaient DeepSeek Flash, le classifieur envoyait les messages
difficiles sur `reasoning`, et `codex_scope` remplaçait Codex sans un mot pour tout travail
de fond, runs de workflow lancés par le propriétaire compris. Chaque site avait son repli
(titre et juge sur `fast`, rêve sur `compaction`, `memoire` lu comme un rôle qui
n'existait pas, pointage sur la vision), et rien ne disait ce qui tournait.

- **Profils et résolution unique** (#332). `[models.profiles.<nom>]` : `primary`,
  `overrides.<rôle>`, `capabilities.<capacité>` (`image_generate`, `vision`,
  `embedding`), étages `routing.*`, chaînes `fallback`, `codex_background` ;
  `models.profile` nomme l'actif, bascule à chaud. `Config::resolve_role_with` (noyau) est
  appelée par tous les sites : moteur, classifieur, titre, juge, compaction (et son
  repli), rêve (rôle `dream`), mémoire, épisodes, ingestion, embeddings, vision, images,
  voix, narration, étapes et phases de workflow, sous-agents, catalogue. Surcharge, sinon
  voix, sinon capacité (posée, puis le principal si le catalogue dit qu'il sait faire, puis
  le modèle livré), sinon le principal ; un rôle inconnu suit le principal. La voix vit
  dans `voice.stt`, `voice.tts`, `voice.narrator`. Le classifieur n'est appelé que si un
  étage du profil nomme un autre modèle que le principal. Un profil cite un alias ou un
  identifiant ; `alias_model` rend un identifiant tel quel.
- **Migration transparente** (#332). Sans `[models.profiles.defaut]`, le profil `defaut`
  est déduit à chaque lecture de `models.roles` et `models.routing`, rôle par rôle comme
  l'ancien code, garde `deny` : rien n'est écrit au démarrage. La première modification
  d'un profil l'écrit en entier. Un test fige, sur la configuration de l'instance (`main`
  sur Codex, le reste sur DeepSeek Flash, images sur Gemini, embeddings sur OpenRouter,
  transcription sur `extra.mlx`, narrateur Qwen3 local, `fallback.main = []`), la table
  rôle → modèle effectif d'avant. `doctor` (`models.profiles`) dit d'où vient le profil et
  nomme les alias que rien ne lit (`juge`, `memoire`, `pointage` : signalés, pas
  convertis, ce qui changerait ce qui tourne).
- **Garde Codex et annonces** (#333). `codex_background = "allow" | "deny"` par profil ;
  `deny` à la migration, choix exigé à la création d'un profil Codex, risque écrit une fois
  (doc, `/model`, `doctor`). Un run lancé par le propriétaire (gate, `/run`, CLI, outil de
  son tour) suit le principal ; seuls les runs planifiés, marqués au démarrage par leur
  planification et transmis aux sous-workflows, restent sous la garde. Tout écart est dit
  une fois par changement d'état dans la conversation concernée (`model_watch`, événement
  `model.notice`, écrit par le canal) : repli après panne avec le motif et le nombre de
  tentatives, garde appliquée, principal injoignable, puis « ✅ retour sur … ». Un modèle
  épinglé n'a plus de repli : il échoue en le disant. Les relances Codex de la 1.0.40 sont
  inchangées (le repli n'arrive qu'après le budget).
- **`/model` refondu et CLI** (#334). Écran : profil actif et principal, ● l'actif
  (dupliquer, renommer, garde confirmée, supprimer confirmé), ○ les autres (bascule
  confirmée), « + Nouveau profil » ; familles Conversation, Travail de fond, Médias,
  Local / voix, chaque rôle avec son modèle effectif et sa raison (✓ ✎ ⚡ 🏠 ⛔), « suivre
  le principal » ou « choisir un modèle » (fournisseur, puis modèle, prix et fenêtre) ;
  🔁 principal, 📋 tout voir, ⚠️ écarts des 24 h, 📌 cette session. Plus de « Usage : »
  sous `/model auto`. CLI : `penelope model list`, `set <cible> <modèle>`, `unset
  <cible>`, `profile use|new|copy|rename|rm|guard`, sortie `--json`. Les méthodes
  `model.*` sortent du daemon vers `penelope_ops::models` (le daemon perd 170 lignes),
  avec `model.unset` et `model.profile`.
- **Défauts** (#335). Les étapes de workflow et les sous-agents ont la chaîne de repli du
  profil et la compaction sur dépassement ; un principal sans fournisseur cède au premier
  repli joignable, annoncé, au lieu de faire échouer le tour avant la chaîne ; transcription
  et synthèse suivent `providers.extra` comme le texte ; `ProviderSet::get` ne route plus
  un modèle `local:` ou `openai_compat:` sans endpoint vers OpenRouter, il échoue en le
  disant ; `Router::escalation`, jamais appelé, et la règle « image jointe », jamais
  nourrie, sont retirés.

Clés nouvelles : `models.profile`, `models.profiles.<nom>.*`, `voice.stt`, `voice.tts`,
`voice.narrator`. Méthodes nouvelles : `model.unset`, `model.profile`. Commandes nouvelles :
`penelope model unset`, `penelope model profile …`, `/model profil …`, `/model session`.

Closes #332.
Closes #333.
Closes #334.
Closes #335.

### 1.0.46

**Sauvegarde clé en main : une archive complète vers un fournisseur unique (S3, dossier ou
iCloud Drive), GitHub retiré, kit de secours, restauration qui remet tout en place, alerte
dès 24 h sans sauvegarde (#327, #328, #329, #330).** Promesse du propriétaire : une seule
sauvegarde, un seul fournisseur ; sur un ordinateur neuf, on la tire et ça repart comme
hier. Décision [0020](decisions/0020-sauvegarde-fournisseur-unique.md).

Constat : le vault partait en clair toutes les 15 min vers un dépôt GitHub, l'archive
chiffrée vers un autre et, ou, S3 ; un `backup.git_remote` vide retombait sur le dépôt du
vault. Depuis le 04/10, l'archive dépassait la limite de 100 Mo d'un fichier GitHub : la
sauvegarde nocturne échouait chaque nuit, sans que `doctor` alerte avant 48 h. L'archive
ne portait ni les valeurs des secrets, ni `data/workspace`, ni `data/mcp-data` ; la phrase
de passe ne vivait que dans le trousseau de la machine ; `restore-all` finissait par une
liste d'étapes à la main, restaurait le vault dans `data/vault` codé en dur et laissait
l'archive déchiffrée sur le disque ; `--full` ignorait `backup.include_media`.

- **Fournisseur unique** (#327), `backup.provider` : `s3`, `dir` (`backup.dir`) ou
  `icloud` ; `[backup.s3]` seule vaut `s3`. `backup.git_remote` et
  `backup.max_push_bytes` sont des clés retirées, ignorées et dites (démarrage,
  `doctor`) : plus aucun push git. Le vault garde son git local ; son push en clair
  passe derrière `memory.vault_git_push` (faux par défaut). L'archive ajoute
  `workspace`, `mcp-data` et les valeurs des secrets, chiffrées une seconde fois ; le
  manifeste dit ce qui est exclu et pourquoi. La méthode `backup` sort du daemon
  (`penelope_ops::backup::rpc`) ; `penelope backup` envoie par défaut, `--local` garde
  l'archive, `--db` ne prend que la base ; sans `--media`, `backup.include_media` décide.
- **Restauration** (#329) : `penelope restore [source]` (`restore-all` en alias, un
  `.db` garde l'ancienne restauration de la base), sans argument le fournisseur
  configuré ou demandé ; base, vault à son chemin, configuration, skills, workflows,
  `mcp.d`, workspace, `mcp-data`, secrets dans le magasin ; service et serveurs
  d'inférence réinstallés, daemon démarré, `doctor` ; il ne reste que les modèles à
  retélécharger et les commandes MCP absentes. Rien de déchiffré ne reste. Test de bout
  en bout en CI (`penelope-evals/tests/backup_restore.rs`).
- **Kit de secours** (#328) : `penelope backup setup` essaie le fournisseur, génère la
  phrase de passe (six mots, plus de 110 bits) ou la reçoit, affiche le kit une fois et
  le fait confirmer par quatre mots ; `penelope backup kit` le réaffiche. Chaque
  sauvegarde se déchiffre et se liste sans rien extraire ; `doctor` date le contrôle. La
  phrase de passe est demandée masquée.
- **Surveillance** (#330) : alerte au foyer dès 24 h sans sauvegarde réussie, une fois
  par jour, avec la cause et la commande ; `doctor` en rouge au même seuil ; ligne au
  digest du matin. Le dossier local ne garde que la dernière archive
  (`backup.keep_local`, 1).
- **Sécurité de l'archive** (revue du lot) : la collecte lit chaque entrée par
  `symlink_metadata` et ne suit aucun lien symbolique, racines `workspace` et
  `mcp-data` comprises (seuls le vault et `config.toml`, désignés par la configuration,
  peuvent être des liens) ; liens, sockets et tubes sont nommés au manifeste
  (`links_skipped`), aucune boucle ni sortie des racines n'est possible. La taille est
  comptée pendant la collecte, refus dès 2 Gio franchis, avant toute archive ; le plafond
  de 512 Mio de l'archive chiffrée vaut aussi à la lecture. La restauration lit les
  entrées avant d'extraire et refuse l'archive entière pour un chemin absolu, un `..`, une
  entrée hors de `penelope/`, un lien ou un fichier spécial.

Clés nouvelles : `backup.provider`, `backup.dir`, `backup.keep_local`,
`memory.vault_git_push`. Commandes nouvelles : `penelope backup setup`, `penelope backup
kit`, `penelope restore [source] [--list --archive --dry-run --no-start]`.

Closes #327.
Closes #328.
Closes #329.
Closes #330.

### 1.0.45

**Catalogue chargé pour chaque fournisseur visé par un alias : la fenêtre des modèles
OpenRouter est connue même quand `main` est sur Codex, et le seuil de compaction la suit
(#324).** Constat : `penelope model list` ne connaissait que `codex:gpt-5.6-sol` ; les
modèles OpenRouter des alias affichaient `context null`, et leur seuil de compaction
retombait sur le repli de 128 000 quelle que soit leur fenêtre réelle (jusqu'à 1 M).
Cause : la boucle du superviseur ne rafraîchissait que le fournisseur de `chat_default`,
plus Codex si un alias le visait ; `main` passé sur Codex, OpenRouter n'était plus jamais
chargé. Et le `replace` d'OpenRouter, passé après Codex, aurait effacé les entrées de
Codex.

La boucle sort du daemon vers `penelope_app::catalog_refresh` (le daemon garde un appel) :
chaque endpoint visé par un alias (OpenRouter, Codex, `providers.local`, chaque
`providers.extra.*`) a son échéance, la cadence de `providers.openrouter.catalog_refresh`
après un succès, une minute après un échec (clé absente, serveur local arrêté), sans
relire les autres. `Catalog::replace` garde les modèles de Codex comme ceux de l'endpoint
local. Chaque fenêtre porte sa source (`WindowSource`) : déclarée par le fournisseur, prise
de `providers.*.context_window` quand un endpoint local ne la déclare pas, ou repli
(modèle inconnu, Codex muet, fenêtre nulle d'OpenRouter, qui donnait un seuil à zéro).
`penelope model list` la donne (`context_source`) et `/context` l'écrit après la fenêtre.
Le seuil de compaction lisait déjà la fenêtre du modèle routé du tour (alias collant et
`codex_scope` compris) ; la demande de fin de tour lit maintenant celle du modèle qui a
réellement répondu, pour qu'un repli vers une petite fenêtre déclenche la compaction.

Closes #324.

### 1.0.44

**Commande `/context` et `penelope context [session] [--json]` : remplissage de la fenêtre,
part de chaque tuile, distance à la compaction, cache et totaux de la session (#322).**
Besoin (inspiré d'Hermes) : voir d'un coup d'œil où en est la fenêtre d'une session.
Les morceaux existaient séparément : le prompt et le cache du dernier appel (`usage`), la
fenêtre (`catalog`), les seuils (`context.*`), les compactions (`context.compacted`) et
les tuiles reconstituées par `penelope audit show` ; `/status` et `/budget` n'en donnaient
qu'une ligne. Ce qui reste : `/usage` pour les coûts, `/status` pour l'état général,
`/context` ne les double pas.

Le rapport vit dans `penelope_conversation::context_report` : le prompt et le cache tels
que le fournisseur les a comptés, la barre sur vingt cases, le seuil de la compaction de
fond (et sa distance) puis le seuil forcé, la part de chaque tuile en **estimation
locale** (T0 à T2 depuis l'instantané du prompt, T3 et T4 depuis la requête repliée du
journal, l'écart au prompt réel, surtout les définitions d'outils, par différence), les
totaux de la session. Un modèle absent du catalogue est signalé avec la fenêtre de repli ;
ce qui ne se relit plus (instantané purgé, appel antérieur au journal) est dit en
réserve, jamais estimé. Des nombres seulement : aucun texte de la conversation n'en sort.
Méthode RPC `context` (sans session : celle de la CLI) routée en une ligne vers le crate
de la conversation, le daemon ne grossit pas ; écran Telegram avec 🔄 Rafraîchir.
Nombres à espaces fines. Scénario de rejeu `commande-contexte`.

Closes #322.

### 1.0.43

**Telegram : une URL collée à `code=` et `state=` n'est un retour OAuth de Pénélope que si
une autorisation attend ce `state` ; sinon elle va à l'agent, dans son sujet (#320).**
Constat (06/10, et deux fois le 30/09) : le propriétaire colle dans un sujet l'URL de
retour OAuth d'un outil tiers que l'agent pilote (`http://localhost:8080/callback?code=…&state=…`) ;
l'agent ne la reçoit jamais, et « 🔐 Autorisation impossible : aucune autorisation en
attente » part dans le sujet Général. Cause : la classification (`penelope-telegram`)
prenait pour un retour `paste_back` (§8.5) **tout** texte à `code=` et `state=`, et la
passerelle répondait sans sujet. Ce qui marchait déjà et reste : une adresse de Pénélope
ne sert qu'une fois, une demande expirée est refusée avec sa raison, le serveur local de
retour (`loopback`) n'est pas touché et prévient toujours dans la conversation du
propriétaire.

Correctif. La classification reste syntaxique, mais `Incoming::OAuthCallback` garde le
sujet, le message d'origine, sa réponse et son transfert. La passerelle demande à
`penelope_mcp_host::auth::awaits` si une demande attend ce `state` (`mcp.oauth.pending.*`,
expirée comprise) : sinon le message redevient un `Incoming::Text` (`into_text`) et suit le
chemin ordinaire (réponse attendue, session du sujet, regroupement), tel quel, sans
masquage au-delà de la redaction existante. Une adresse attendue passe par `complete` ; le
compte rendu, succès (`auth::reconnect`, séparé de l'envoi) ou refus, part en réponse au
message, dans son sujet. Limite : une adresse déjà consommée n'est plus en attente (la
demande est effacée à l'usage) : collée une seconde fois, elle va à l'agent.

Closes #320.

### 1.0.42

**Heures calmes : une veille retenue part en un seul tour à la sortie de la plage, et les
tours relâchés partent en série (#318).** Constat (06/10, 7 h) : une veille `mcp_poll` à
la minute, cible `prompt`, vers un sujet Telegram, a vu plusieurs messages pendant les
heures calmes ; à la fin de la plage, 23 messages sont arrivés dans le sujet en une
minute (un résumé par message, plusieurs cartes « Approbation demandée », des traces
d'outils). Cause : le sondage de sortie de plage réunissait bien la nuit, mais le
regroupement d'un `mcp_poll` (`coalesce`) tire une fois **par élément** pour un prompt ou
un workflow ; chaque élément donnait son tour, tous lancés en parallèle (une session par
exécution, #39), chacun avec la mention « 🌙 Pendant les heures calmes ». Ce qui marchait
déjà et reste : les notifications retenues partent dans un seul message par conversation
(#296), un `cron` ou un `interval` ne part qu'une fois avec ses créneaux comptés, et une
planification retenue ne marque `schedule.held` qu'une fois par créneau.

Correctif. Au rattrapage des heures calmes, un prompt ou un workflow tire **une** fois par
planification (`scheduler/quiet.rs`, `groups`) : `mcp_poll` et `mcp_subscribe` réunissent
leurs éléments nouveaux en un seul tir, `event` réunit les événements de la nuit en
éléments (chacun avec son heure). Un `mcp_poll` retenu continue de sonder à sa cadence
sans rien tirer : ses éléments nouveaux complètent sa ligne de `quiet_queue`, une par
planification (migration `0027_quiet_queue_schedule` : colonne `schedule`, index unique
partiel), avec l'heure où chacun a été vu (`observed_at`) ; une source qui ne rend que ses
derniers éléments ne perd plus rien de la nuit. Le tour de la sortie les reçoit tous, dans
l'ordre, et la ligne s'efface une fois le tir parti. Un `mcp_subscribe` relit sa ressource
dès la sortie, sans attendre la fenêtre de ses notifications périmées. Le texte d'un
créneau retenu dit « Les N occurrences manquées partent en une seule exécution ». Les
tours relâchés portent le couloir `heures_calmes` : la file des tours (`penelope-kernel`,
champ `lane` du payload) n'en réclame qu'un à la fois par couloir, les autres tours ne sont
pas touchés. Risques : un sondage de nuit en échec se tait (le passage de sortie dira
l'erreur) ; un workflow fusionné reçoit les paramètres de son premier élément, la liste
complète restant dans `{{items}}`.

Tests : une veille à la minute avec dix messages pendant la plage ne laisse qu'une entrée
dans la file, puis donne à 7 h un seul tour qui contient les dix, dans l'ordre et avec leur
heure ; un `cron` et un `interval` retenus donnent deux tours, un « 12 occurrences
manquées », lancés l'un après l'autre ; trois événements de la nuit donnent un tour ; la
file des tours fait attendre un couloir occupé sans retenir les autres ; une planification
complète sa ligne de la file au lieu d'en ajouter une.

Closes #318.

### 1.0.41

**`send_voice` garde le vocal envoyé et rend son chemin (#316).** Constat (05/10) : un
vocal produit par `send_voice` (Voxtral, Resemble Enhance et preset studio, #299) est
validé en brouillon sur Telegram, puis le propriétaire demande de l'envoyer sur WhatsApp.
Le résultat de l'outil ne donnait que `sent`, `seconds`, `chars`, `voice` et
`postprocess`, aucun chemin : le modèle a régénéré l'audio avec `say -v Flo`, et le
destinataire a reçu une autre voix, nettement dégradée.

Correctif. L'OGG/Opus réellement envoyé (après post-traitement) est rangé sous
`{data}/media/voice/out/<session>/<ULID>.ogg` et son chemin rendu dans `path`. Les
intermédiaires (WAV assemblé, répertoire de travail du post-traitement) sont toujours
effacés ; l'OGG l'est aussi quand la conversion ou l'envoi échoue, car rien ne le citerait.
La purge d'une session le trouve dans le résultat de l'outil, comme l'original d'un vocal
reçu (#308) ; la rétention (`retention.days`) retire les vocaux envoyés plus vieux, par
dossier de session comme les médias MCP (#304), et le rapport porte `voice_out`. La
description de `send_voice` dit de réutiliser `path` tel quel pour transférer un vocal déjà
envoyé ou validé, jamais de le régénérer : le rejeu `outils-soi` (surface et attendus) est
régénéré en conséquence.

Tests : le chemin rendu est celui du fichier envoyé, même empreinte SHA-256, et seul l'OGG
reste dans `media/voice` ; la purge de la session efface le vocal cité, la rétention efface
un dossier de session périmé.

Closes #316.

### 1.0.40

**Codex : l'identité annoncée suit la machine, version d'OS et Codex CLI installé
compris (#313).** Constat (05/10), pendant les surcharges de #311 : le `User-Agent` valait
`codex_cli_rs/0.104.0 (macos 0.0.0; aarch64)`. La version client par défaut était
`0.104.0` alors que la machine porte un Codex CLI `0.155.0` (et `0.160.0` dans
l'application ChatGPT), la version d'OS venait de `PENELOPE_OS_VERSION`, que le
LaunchAgent ne définit pas, et le format n'était pas celui du Codex CLI.

Correctif, en quatre points. **Format** : `user_agent` reproduit `get_codex_user_agent`
d'openai/codex (`codex-rs/login/src/auth/default_client.rs`, commit `7f892275`) :
`codex_cli_rs/0.160.0 (Mac OS 27.0.0; arm64) unknown`, nom et version à la façon de la
crate `os_info`, architecture de `uname -m` (`arm64` sur un Mac Apple), puis le jeton du
terminal, `unknown` pour un daemon sans terminal ; même assainissement des caractères. Un
test le fige contre l'expression du test macOS amont. **OS** : `penelope_platform::host::
os_identity` lit `sw_vers -productVersion` une fois par processus (`/etc/os-release` sous
Linux), complète en trois nombres (`27.0` devient `27.0.0`) ; `PENELOPE_OS_VERSION` reste
une surcharge. Le système passe au fournisseur par `CodexOptions::os` : `penelope-llm`
n'y touche pas. **Version client** : `providers.codex.client_version` vaut `auto` par
défaut, soit la plus récente des Codex CLI installés (`codex` du PATH et
`ChatGPT.app/Contents/Resources/codex-cli/bin/codex`, sondés par `codex --version` via
`Platform::discovery`, donc jamais en test), sinon `0.160.0`, dernière publiée. L'inventaire
machine ne suffisait pas : il ne voit que le `codex` du PATH, ici un Homebrew en `0.46.0`.
Une valeur posée l'épingle toujours ; `doctor` affiche l'identité résolue et un contrôle
`provider.codex.version` échoue au-delà de dix versions mineures de retard sur le CLI
installé. **Rechargement** : `config set` reconstruisait déjà les fournisseurs, pas
`config reload` ; un `[providers.codex]` édité à la main n'était lu qu'au redémarrage. Le
rechargement les invalide maintenant. L'accès Codex se construit dans
`penelope_ops::codex_auth::access` au lieu du daemon, qui perd six lignes.

**Relances en flux** (décision du propriétaire : rester sur le modèle principal). Une
coupure en flux avant tout texte, comme le « servers overloaded » de Codex, n'avait droit
qu'à un nouvel essai avant le repli ou l'abandon. `RetryPlan` relance maintenant le même
modèle jusqu'à `max_retries` fois, après 2 s, 4 s, 8 s puis 16 s au plus (ou le
`Retry-After` s'il fait 20 s ou moins), comme Codex CLI (`stream_max_retries`, 5 par
défaut, `codex-rs/model-provider-info/src/lib.rs`, openai/codex@7f892275), et ne bascule
qu'ensuite, s'il y a un repli ; un repli reçoit le même budget. Le budget est celui du
fournisseur du modèle principal : `providers.codex.request_retries`, déclaré mais lu nulle
part jusqu'ici, vaut **5** par défaut pour un modèle `codex:` ;
`providers.openrouter.request_retries` passe de 3 à **4** pour les autres, ce qui ajoute
aussi une attente de 8 s aux erreurs d'avant flux sur le dernier modèle. Les deux se
règlent par `config set` ; 0 coupe toute relance. Un abandon après des relances en flux
dit maintenant, comme avant flux, combien de tentatives et combien de secondes d'attente.

Tests : format du `User-Agent`, résolution de `auto` et retard en versions mineures,
identité du système à la façon d'`os_info`, version la plus récente entre PATH et
application ChatGPT, contrôle `doctor`, et `config reload` qui reconstruit les
fournisseurs (échoue sans le correctif) ; table de `RetryPlan` (relances jusqu'au budget,
attente plafonnée), et dans la boucle : trois surcharges puis la réponse du même modèle,
surcharges au-delà du budget sans repli (abandon qui dit `5 tentatives` et l'attente),
avec repli (bascule après le budget).

Closes #313.

### 1.0.39

**Codex : une surcharge se réessaie puis passe au repli, au lieu d'abandonner le tour (#311).**
Constat (05/10, 06:05 à 06:23 UTC) : le backend Codex rendait dans le flux « Our servers
are currently overloaded. Please try again later. » ; le code n'était pas l'un des quatre
reconnus (`rate_limit_exceeded`, `slow_down`, `server_error`, `overloaded`), l'erreur
tombait en `Other`, que `RetryPlan` abandonne : sept tours perdus, aucun nouvel essai ni
repli malgré `models.routing.fallback.main = ["fast"]`.

Correctif. `types::signals_overload` reconnaît la surcharge au code (`overload`,
`capacity`) comme au seul message (`overloaded`, `try again later`, `at capacity`).
Chez Codex, `response.failed` et l'événement `error` nu (jusqu'ici ignoré, il ne laissait
qu'une fermeture sans cause) passent par `failure_chunk` : une surcharge devient
`provider_overloaded`, réessayable, donc `Transient`. `LlmError::mid_stream` applique la
même règle quel que soit le fournisseur (flux OpenRouter, serveur local), le flux SSE
d'OpenRouter marque une surcharge sans `error_type` réessayable, et `from_status` classe
en `Transient` un statut sinon `Other` dont le corps dit la surcharge (un 400 ou un 429
gardent leur catégorie). Le code et le type bruts d'un flux Codex en échec sont
journalisés (`flux Codex en échec`), sans rien de la requête. La boucle n'a pas changé :
une panne passagère avant tout texte est rejouée une fois, puis part sur le repli ; après
des fragments affichés, le message de coupure de #206 reste. Rien dans le daemon. Tests :
surcharge sans code, avec code inconnu, par le seul code, par l'événement `error`,
erreur ordinaire inchangée ; un tour dont le modèle principal rend deux fois la surcharge
Codex (événements bruts passés par l'accumulateur, nouvelle variante
`Scripted::CodexEvents` du mock) et que le repli termine.

Closes #311.

### 1.0.38

**Vocaux Telegram : l'original est conservé et son chemin suit le transcript (#308).**
Constat : à la réception d'un vocal, la passerelle téléchargeait les octets, les confiait
à la transcription et les oubliait ; l'agent ne recevait que le texte et ne pouvait pas
réutiliser l'audio, par exemple pour importer dans un serveur MCP un échantillon de voix
envoyé par le propriétaire, sans lui demander de le renvoyer comme document.

Correctif. `penelope_app::media::save_voice` range l'original sous `{data}/media/voice`, au
format reçu (OGG/Opus pour un vocal, l'extension d'origine pour un fichier audio), sous un
nom tiré du message (`tg_<chat>_<message>`) : une réception rejouée réécrit le même
fichier. `voice_note` ajoute au message transcrit `(vocal enregistré : <chemin> ; …)`,
comme la mention d'une photo ; la notion reste générique côté cœur (archtest vert), le
nom vient de la passerelle. La transcription est inchangée : un original qui ne s'écrit
pas est journalisé et le transcript part sans chemin. Une transcription en échec garde
l'original, seule trace du vocal : le propriétaire voit l'erreur comme avant, et le tour
reçoit `(message vocal, transcription échouée : …)` avec le chemin, pour que l'agent
puisse la relancer ; une transcription vide l'efface, qu'aucun message ne citerait. **Purge et rétention** : la purge d'une
session efface déjà les fichiers sous `{data}/media` cités par ses messages
(`media_paths`), elle emporte donc l'original avec son message ; comme pour les photos, il
n'existe pas de rétention par âge des médias (`retention.days` ne touche que la base), ce
lot n'en ajoute pas. Rien dans le daemon. Tests : vocal reçu, fichier écrit à l'octet,
chemin vu par le modèle, transcript intact, purge de la session qui l'efface, écriture
impossible qui laisse passer la transcription, échec de transcription qui garde
l'original et en transmet le chemin. Doc : install-headless.md (« Messages vocaux »).

Closes #308.

**Binaire local d'abord : `git_clone` emprunte les identifiants de `gh` ou de `glab`
connecté, et le message système le dit (#305).** Constat (run de plan vu dans #302) :
`git_clone` sur un dépôt GitHub privé en https échouait faute d'identifiants, puis le
modèle relançait `gh repo clone` dans le shell, qui réussissait. Un appel perdu, et un
clone qui échappait aux contrôles de `git_clone` (`normalize_clone_url`, réutilisation du
dépôt déjà présent, #160). L'inventaire savait pourtant `gh` connecté (#156) ; personne
ne s'en servait pour décider. Principe validé par le propriétaire : comme sa bibliothèque
de serveurs MCP, Pénélope a une bibliothèque de binaires locaux, et elle les privilégie,
surtout authentifiés.

Correctif. **`git_clone`** : sur une source `https://` dont l'hôte est github.com avec
`gh` connecté, ou l'instance à laquelle `glab` est connecté, l'executor lit l'inventaire
rangé (`machine.inventory`, sans sonde dans le tour) et passe à git
`-c credential.https://<hôte>.helper=` puis `-c credential.https://<hôte>.helper=!gh auth
git-credential` (resp. `glab`) : git demande le jeton à l'assistant par son entrée
standard, rien ne le porte dans l'adresse, les arguments, l'environnement, le journal,
l'audit ni la trace, ni dans `.git/config` du clone. Les contrôles restent ; le résultat
dit `credentials: "gh"`. Un clone qui échoue avec l'assistant refait **une** sonde de
connexion (`machine::recheck_login`), range l'état corrigé dans `kv` s'il a changé (la
ligne T1 cesse de dire connecté ce qui ne l'est plus) et le dit dans l'erreur, sans
nouvel essai. `ssh://` et `git@…` gardent les clés SSH. **Inventaire** : l'état
d'authentification y était déjà, sans jeton (`account` : compte `gh`, instance `glab`,
`joignable` pour Docker, sondes rafraîchies avec l'inventaire, jamais `--show-token`) ; un
test le fixe sur une sortie de `gh auth status` qui affiche une ligne `Token:`. **Message
système** : une phrase « Binaire local d'abord, serveur MCP ou service tiers ensuite : un
binaire connecté porte déjà ses identifiants, ne les redemande pas (`git_clone` passe par
ceux de `gh`). » ferme la ligne machine du bloc d'environnement dès qu'un binaire connu est
présent ; la parenthèse ne nomme que les forges connectées. Elle ne dépend que des
binaires et de leur connexion, comme le reste de la ligne : elle ne bouge pas d'un tour à
l'autre, le préfixe reste stable. Aucun rejeu existant ne sème d'inventaire : aucun
attendu ni test doré ne change. Pour les rejeux, `Platform` porte `git_config` (vide sur la machine) et
`Services::for_tests_with` la retouche avant de la partager ; le harnais gagne
`[[forge_repos]]`, un dépôt privé fictif servi par un dépôt nu local derrière une
réécriture d'adresse. Doc : install-headless.md (« Binaire local d'abord », ligne
machine).

Tests : un faux hôte git en HTTP qui exige une authentification Basic, et un faux client
de forge comme assistant : refus sans lui, clone réussi avec lui, jeton absent du
résultat, de l'erreur et de la configuration du clone ; l'assistant scopé à l'hôte et
seulement pour `https://` ; le choix du client (`gh` pour github.com, `glab` pour son
instance, rien sans connexion) ; la règle présente une fois et fixe, absente sans binaire ;
la nouvelle sonde qui corrige un inventaire périmé. Scénario de rejeu
`outils-git-prive` : `gh` marqué connecté, dépôt privé fictif, `git_clone` par le
raccourci `owner/repo` réussit en un appel (`credentials: gh`) sans repli shell, puis
`fs_read` du README. Ce qui marchait déjà et continue : la réutilisation d'un clone
présent, la validation des sources, les clones `file://`, la ligne T1 stable à la mise à
jour d'un binaire, la remarque de `http_fetch` vers une forge connectée. Reste ouvert :
les autres outils natifs ne consultent pas encore l'inventaire ; le scénario ne sollicite
pas l'assistant lui-même (la forge fictive est locale), c'est le test de `penelope-tools`
qui l'exerce.

Closes #305.

**MCP : une image ou un son rendu par un outil n'est plus perdu (#304).** Constat (run de
plan vu dans #302) : une étape de workflow téléchargeait les captures d'écran d'un ticket
par l'outil de pièces jointes du serveur de tickets ; le modèle recevait `[image
image/png, N octets en base64, non transmise]` et rien n'apparaissait dans le répertoire
du run. Cause : `result_json` (`penelope-mcp-host`) remplaçait tout bloc `image`/`audio`
par ce texte, sans le poser nulle part ni le montrer au modèle. Le `save_path` de l'outil
est une affaire du serveur, qui écrit chez lui.

Correctif. `result_json` garde la donnée du bloc à côté de sa mention (le port
`McpGateway` le dit) ; qui ne lit que le texte (relève `mcp_poll`, agenda du digest) voit
la même mention qu'avant. L'exécuteur (`penelope-executor`, module `mcp_media`) pose le
bloc sur disque avant tout transcript : `mcp-media/` du répertoire du run pour une étape
de workflow, sinon `{data}/media/mcp/<session>/` ; nom tiré du contenu (16 caractères de
SHA-256, extension d'après le type), donc la même capture rendue deux fois est le même
fichier ; plafond de 10 Mo décodés, celui d'une photo reçue, jugé avant de décoder. Le
résultat garde `[image image/png, 12 Ko, enregistrée : <chemin>]` ; au-delà du plafond ou
en base64 illisible, rien n'est écrit et la mention le dit. Aucun base64 ne reste dans la
valeur, le texte, le ledger ni l'historique. Si le modèle du tour lit les images (d'après
le catalogue, comme une photo Telegram), la boucle les lui montre à l'appel qui suit le
résultat, dans la copie envoyée seulement, en un message final marqué contenu observé :
l'historique et le préfixe ne bougent pas, et une image refusée par le fournisseur suit
le chemin de #231. Codex, modèle principal de l'instance, la reçoit comme les autres :
son catalogue annonce les images et le message part en `input_image` après le
`function_call_output` (testé sur le corps Responses et dans la boucle). L'URI passe par `model_data_url`, la réduction des photos reçues
(#242), via deux méthodes du port `ToolExecutor` (`shown_images`, `model_images`) : la
boucle ne connaît ni le disque ni MCP. Seul l'exécuteur écrit le champ `saved` ; celui
qu'un serveur glisserait est retiré, et un chemin hors des dossiers de médias n'est jamais
montré. `image_inspect` lit aussi `{data}/media/mcp`. Rétention : la purge d'une session
emporte les fichiers que cite son historique (chemin sous `{data}/media`, déjà en place) ;
la passe quotidienne retire ceux de `{data}/media/mcp` plus vieux que `retention.days`
(`mcp_media` dans son rapport) ; ceux d'un run partent avec son répertoire. Le daemon ne
change pas. Doc : mcp.md (« Risque et approbation »).

Tests : bloc image écrit, nom stable, mention avec chemin et sans base64 ; bloc audio écrit
mais jamais montré ; plafond dépassé et base64 illisible sans fichier ; `saved` forgé par un
serveur retiré et chemin hors racine refusé ; appel MCP de bout en bout hors workflow
(dossier de la session, URI pour le modèle, racine d'`image_inspect`) et dans un run
(`mcp-media/` du répertoire du run) ; boucle : un modèle qui voit reçoit la capture à
l'appel suivant et une seule fois, l'historique n'en garde que le chemin, un modèle sans
vision n'a que le chemin ; rétention des vieux médias seulement. Ce qui marchait déjà et
continue : les photos Telegram et `image_inspect`, les blocs texte et ressource, la relève
`mcp_poll` et l'agenda du digest. Assumé : un audio n'est jamais transmis au modèle, seul son
chemin l'est (un outil de transcription le relit). Reste ouvert : les blocs binaires d'une ressource
(`mcp_resource_read`) restent une mention.

Closes #304.

### 1.0.37

**Agenda : un connecteur CalDAV en lecture, serveur MCP livré dans le binaire, et les
rendez-vous du jour dans l'« Aujourd'hui » du digest (#295).** Constat : Pénélope n'avait
aucun agenda ; le digest ne listait que ses propres planifications et ignorait les
rendez-vous, anniversaires et vacances du propriétaire, alors qu'elle sert déjà de rappel.
L'instance est pilotée en SSH : aucune autorisation TCC ne peut être cliquée, le
calendrier Apple local (EventKit, `shortcuts`) est hors de portée. Voie retenue, écrite
dans la décision [0018](decisions/0018-agenda-caldav-en-lecture.md) : CalDAV avec un mot
de passe d'application, en lecture seule, dans un serveur MCP à part, conformément à la
frontière canal/cœur de la V1.

Correctif. Nouvelle crate `penelope-agenda-mcp` : un serveur MCP stdio (protocole
2025-06-18 au plus, `server/discover` inconnu, `initialize`, `tools/list`, `tools/call`,
`ping`) qui lit un compte CalDAV (iCloud, Fastmail, Nextcloud, tout serveur en
authentification Basic) : découverte RFC 6764 (`current-user-principal`,
`calendar-home-set`, dossier), calendriers d'événements seuls (rappels et boîtes de
réception écartés), redirection suivie avec la même méthode, `REPORT calendar-query`
borné ; parseur iCalendar écrit à la main (dépliage, paramètres, `DTSTART`/`DTEND` en
date, heure flottante, UTC ou `TZID`, `DURATION`, `EXDATE`, `RECURRENCE-ID`,
`STATUS:CANCELLED`) et récurrences quotidienne (`BYDAY` filtre), hebdomadaire (`BYDAY`,
`WKST`), mensuelle (`BYMONTHDAY`, `BYDAY` avec rang), annuelle (`BYMONTH`), `INTERVAL`,
`COUNT`, `UNTIL` ; un `TZID` inconnu est rabattu sur `AGENDA_TIMEZONE`. Quatre outils,
tous en lecture (`readOnlyHint`) : `calendar_list`, `events_today(timezone)`,
`events_range(start, end, timezone)` (366 jours au plus), `event_search(query, from, to,
timezone)` (50 résultats) ; texte lisible et `structuredContent` (`events[]` : `uid`,
`summary`, `calendar`, `all_day`, `start`, `end` en RFC 3339 dans le fuseau demandé ou en
dates pour une journée entière, `location`) ; arguments illisibles et identifiants refusés
en `isError`, outil inconnu en erreur de protocole. Identifiants par l'environnement
(`AGENDA_URL`, `AGENDA_USER`, `AGENDA_PASSWORD` par `${SECRET:…}`, `AGENDA_TIMEZONE`,
`AGENDA_CALENDARS`, `AGENDA_TIMEOUT`) ; le mot de passe n'apparaît dans aucune erreur,
aucun `Debug`, aucune adresse. **Release** : le serveur est aussi la sous-commande
`penelope agenda-mcp` (`penelope-cli` dépend de la crate), donc `mcp.d/agenda.toml`
déclare `command = "penelope"`, `args = ["agenda-mcp"]` et `release.yml`, le `Makefile`
et `penelope upgrade` ne changent pas ; le binaire `penelope-agenda-mcp` existe hors
release. **Digest** : clé `digest.agenda` (vide par défaut ; un nom qualifié
`mcp__<serveur>__<outil>`, validé) ; `DigestFeed` reçoit le branchement MCP de la
composition et l'« Aujourd'hui » mêle rendez-vous et planifications, rangés par heure,
journées entières en tête, dans `owner.timezone` ; l'outil doit être en lecture et connu
du registre, l'appel passe par le port `McpGateway` (archtest vert) ; un outil inconnu, en
écriture, un superviseur absent ou une réponse en erreur donnent une ligne « Agenda non
lu : … » (`DigestInputs.agenda_error`) sans retenir le digest. Rien de l'agenda n'entre en
mémoire durable. Dépendance nouvelle `roxmltree` (MIT ou Apache-2.0, sans dépendance),
justifiée dans le `Cargo.toml`. Doc : mcp.md (« L'agenda CalDAV, un serveur livré »),
install-headless.md (« Agenda CalDAV », digest), README (commande, décision), architecture
(crate), référence des clés et golden `config.get` régénérés.

Tests (34 dans la crate) : fixtures `.ics` (événement `TZID` avec lignes pliées et
échappements, journées entières, hebdomadaire avec `EXDATE`, `COUNT`, instance déplacée et
instance annulée, quotidienne à `UNTIL` en UTC, anniversaire annuel, mensuelle par jour et
par dernier vendredi, UTC avec `DURATION`, heure flottante, fréquence horaire inconnue) ;
règles sans borne, 31 du mois, jours ouvrés ; faux serveur CalDAV (Basic vérifié,
découverte en trois requêtes, `REPORT` avec sa plage, 401 sans le mot de passe, 500 avec
l'étape, redirection, adresse déjà dossier, adresses refusées) ; le protocole (négociation
descendante, `server/discover` inconnu, notifications muettes, JSON illisible, quatre
outils en lecture, `events_today` sur deux calendriers dans deux fuseaux avec cache de la
liste, `events_range` et `event_search` avec leurs bornes, arguments fautifs en `isError`,
outil inconnu en `-32602`, identifiants refusés, calendriers restreints). Côté Pénélope :
validation de `digest.agenda` ; l'ordonnanceur avec un faux serveur MCP (éteint par
défaut, mélange trié avec les planifications, fuseau du propriétaire passé à l'outil,
superviseur absent, outil en erreur, en écriture ou inconnu refusés sans appel) ; le
digest dit l'agenda non lu en une ligne tronquée, rien sans configuration ; la commande
`agenda-mcp` hors daemon. Ce qui marchait déjà et continue : l'« Aujourd'hui » des
planifications seules et son ordre (`due_today`), les `mcp_poll`, la négociation du
client MCP. Reste ouvert (#295, points 2 et 3) : l'écriture sous approbation, la
relecture mémoire qui propose une entrée d'agenda, la cohérence des planifications avec
l'agenda ; Google Agenda (OAuth 2 sur son point CalDAV). Closes #295.

**Webhooks entrants : un déclencheur `webhook` signé en HMAC, sur un listener local
dédié, et le mode webhook de Telegram enfin refusé plutôt que promis (#294).** Constat :
rien ne pouvait pousser un événement dans Pénélope depuis l'extérieur ; une forge, un
suivi de tickets, un formulaire ou un script sur une autre machine n'avaient que la
scrutation (`mcp_poll`). Et `telegram.mode = "webhook"` avec `telegram.webhook_url`,
acceptés à la validation depuis la 0.17, n'ont jamais été servis : un bot ainsi réglé ne
recevait rien, sans un mot.

Correctif. **Kind `webhook`** (`penelope schedule add webhook --spec '{}' --target …`,
ou `schedule_create` soumis à approbation) : à la création, Pénélope attribue le chemin
`/hook/<jeton>` (24 caractères tirés au sort) et un secret de 48 caractères rangé dans le
magasin de secrets sous `webhook_<jeton>` ; la spécification ne porte que le chemin et le
nom du secret (`secret_ref`), jamais sa valeur, et `spec.secret` est refusé. Le secret est
montré une fois, en ligne de commande, avec l'adresse locale du hook ; par l'outil, il
n'est pas montré au modèle (la réponse entre dans le journal) et le propriétaire pose le
sien avec `penelope secret set`. **Serveur** (`scheduler/webhook.rs`, listener dédié,
décision [0019](decisions/0019-webhook-entrant.md)) : `[webhooks] listen =
"127.0.0.1:7778"` par défaut, vide pour ne rien ouvrir, au redémarrage ; `POST` seulement
(405 sinon, un `GET` ne déclenche jamais), chemin actif (404 pour inconnu, en pause ou
supprimé), horodatage `X-Penelope-Timestamp: <secondes Unix>` à ±5 min de l'horloge et
signature `X-Penelope-Signature: sha256=<HMAC-SHA256 hex de « <horodatage>.<corps> »>`
comparée en temps constant (401), signature déjà admise dans la fenêtre refusée comme
rejeu (409), corps borné par `webhooks.max_body_bytes` (64 Ko, 413 avant
lecture), débit par hook `webhooks.rate_per_minute` (60, 429 avec `Retry-After`), plafond
`webhooks.prompt_turns_per_hour` de tours `prompt` pour l'ensemble des hooks (20, 429),
`Expect: 100-continue` honoré, transfert par morceaux refusé (411), connexion coupée à
20 s. L'en-tête HTTP est lu par `httparse` (celui de `hyper` et de `tokio-tungstenite`,
déjà dans le graphe, dépendance directe justifiée dans `Cargo.toml`), et tout ce qu'un
intermédiaire lirait autrement que nous est refusé en 400 : CR ou LF nus, ligne repliée,
`Content-Length` répété ou non décimal, `Transfer-Encoding` avec `Content-Length`. Le
chemin et le secret viennent de l'aléa du système, sans biais de modulo et sans repli
(`ids::secret_token`) : s'il manque, la création échoue. Ces trois derniers points
(rejeu, *request smuggling*, secret faible si l'aléa échoue) viennent de la revue de
sécurité du lot. Le corps JSON est l'événement, filtrable par `filter` comme un élément de
`mcp_poll` (202 `accepted: false` quand il est écarté), et passe aux mêmes cibles :
`notify` reçoit `{{payload}}` et les champs de premier niveau, `workflow` le corps en
`{{item}}`, `prompt` ne le substitue **jamais** dans son texte, il arrive après, encadré
par `wrap_untrusted` avec l'alerte du détecteur local (#92), un tour par réception
dédoublonné par l'identifiant de livraison. Chaque réception, acceptée ou refusée, est un
événement `webhook.received` de l'audit : statut, motif, taille, empreinte SHA-256 du
corps, adresse ; jamais le corps ni les en-têtes. La suppression d'un hook (RPC, outil)
efface son secret du magasin. Le HMAC-SHA256 de la signature SigV4 (#289) remonte dans
`penelope_kernel::hmac`, partagé. **Telegram** : `telegram.mode` n'admet plus que
`polling`, la validation nomme le refus et sa raison ; `webhook_url` reste lue, sans
effet, et la doc le dit. CLI et `/schedules` montrent `POST /hook/<jeton>` ; doc
« Webhooks entrants » dans install-headless.md (créer, signer avec `openssl`, exposer par
un tunnel ou un réseau privé sans rien ouvrir), référence des clés, fixture 0.17 et
golden `config.get` régénérés.

Tests : serveur sur un port éphémère servi par `serve_on`, interrogé par le client HTTP du
workspace. Un POST signé déclenche la notification (corps et champs), la réception et le
tir sont au journal, le secret n'est ni dans le store ni dans le journal et la création le
montre une fois avec l'adresse locale ; signature absente, fausse, mal formée, d'un autre
secret ou d'un autre corps : 401 et rien ne part, chaque refus journalisé, préfixe
`sha256=` facultatif ; une livraison renvoyée telle quelle : 409 sans tir, la même après
un 429 repasse, horodatage absent, signé `+`, à 301 s dans le passé ou le futur, ou
signature du corps seul : 401, la copie passée la fenêtre refusée par son horodatage ; le
cache anti-rejeu purge les expirées ; l'horodatage fait partie du message signé ;
dix-sept en-têtes ambigus refusés en 400 (dont `Content-Length` doublé à l'identique,
`+5`, `5, 5`, ligne repliée, LF nu, CR nu, `Transfer-Encoding` dans les deux ordres),
`Transfer-Encoding: identity` seul 411, et sur le fil `Transfer-Encoding` avec
`Content-Length` 400 journalisé ; sans aléa, `prepare` échoue sans rien ranger ;
`secret_token` sans biais et en erreur sur une source en échec ; `GET` 405 avec `Allow`, chemin inconnu, hook en pause ou supprimé
404, corps trop grand 413, non JSON 400 ; le filtre écarte sans déclencher ; débit par hook
et plafond horaire des prompts partagé entre deux hooks, `Retry-After`, fenêtres qui
glissent à l'horloge de test ; le prompt porte le corps encadré, le motif d'injection
signalé et son texte non substitué, trois livraisons font trois tours ; la création refuse
un chemin ou un secret choisis par l'appelant et n'oublie pas un secret rangé pour une
planification refusée ; dialogue HTTP brut (`100 Continue`, 411, 431, 413 avant lecture,
400) ; magasin : validation du kind (chemin, `secret_ref`, `secret` refusé, `filter`),
pas de prochain passage ; HMAC contre la RFC 4231 ; validation de `[webhooks]` et refus
de `telegram.mode = "webhook"`. Ce qui marchait déjà et continue : les cinq autres
déclencheurs et leurs tests, la signature SigV4 (vecteurs d'AWS inchangés), le retour
OAuth sur `mcp.callback_port`, la suppression d'une planification sans webhook. Closes
#294.

**Réponses vocales : un post-traitement local, optionnel et configurable, entre le WAV de
Voxtral et l'OGG envoyé (#299).** Constat : un essai à la main sur l'instance (Voxtral
`fr_female` → Resemble Enhance complet → finition studio douce FFmpeg → OGG/Opus) a donné
le rendu de référence, mais `send_voice` ne connaissait que `tts_voice`, `max_chars` et
`reply_in_kind` : la chaîne se rejouait à la main, et Resemble Enhance ne vivait que dans
le cache éphémère de `uvx`.

Correctif. Section `[voice.postprocess]`, éteinte par défaut (`enabled`, `engine` :
`resemble_enhance` ou `none`, `timeout` 180 s, `ffmpeg_bin`, `ffmpeg_filters`,
`ffmpeg_output_args`, `opus_bitrate`, `resemble_bin`, `resemble_run_dir`,
`resemble_device` `cpu` ou `mps`), validée au chargement même éteinte (moteur et
périphérique connus, délai non nul). Allumée, après l'assemblage du WAV : le moteur, puis
`ffmpeg -af` avec le preset « studio doux » retrouvé dans la base de l'instance (passe-haut
70 Hz, passe-bas 16 kHz, égaliseurs doux à 180 Hz et 3,2 kHz, compression douce, `loudnorm`
à −16 LUFS, sortie mono 24 kHz ; constantes `VOICE_STUDIO_SOFT_*` du noyau, reprises comme
défauts des clés), puis `wav_to_ogg_opus_with` : Opus à 48 kb/s pour le vocal fini, 32 kb/s
inchangé pour un vocal brut. Le moteur est derrière un trait
(`Enhancer`) ; `ResembleEnhance` lance `resemble-enhance <in> <out> --device … --run_dir …`
hors ligne (`HF_HUB_OFFLINE`, `TRANSFORMERS_OFFLINE`, et `PYTORCH_ENABLE_MPS_FALLBACK` en
`mps`), sur des poids préparés à l'installation (`uv tool install --python 3.11
resemble-enhance`, dépôt Hugging Face cloné avec Git LFS sous
`<données>/models/resemble-enhance`) : poids absents ou pointeur Git LFS à leur place, le
moteur est écarté avant tout lancement. Binaire absent, poids manquants, erreur ou délai
dépassé (processus tué) : avertissement journalisé, le WAV brut part converti ; `ffmpeg`
désigné par la configuration sert aussi à la conversion finale
(`audio::wav_to_ogg_opus_with`). Les temporaires (`<id>.wav`, `<id>.work/`) disparaissent
sur succès comme sur échec. `voice.sent` et le résultat de l'outil portent `postprocess`
(`status` `disabled`, `applied`, `skipped`, `failed`, `timed_out` ; `engine` ; `seconds` ;
`reason`). `doctor` : le contrôle `voice` cherche `ffmpeg` dans le PATH étendu (Homebrew,
`~/.local/bin`) ou au chemin configuré ; un contrôle `voice.postprocess` apparaît quand la
section est allumée et nomme ce qui manque avec la commande qui l'installe, sans rien
lancer. Inventaire `install` : `postprocess`, `postprocess_engine`. Doc : section « Post-
traitement du vocal » d'install-headless.md (installation durable, poids, CPU contre MPS,
exemple), référence des clés, fixture 0.17 et golden `config.get` régénérés. Pas de
commande `penelope voice install` : la préparation est documentée et `doctor` la vérifie.

Tests (faux `resemble-enhance` et `ffmpeg` dans un répertoire temporaire, jamais les vrais) :
section éteinte par défaut, relue depuis TOML, clé inconnue refusée, moteur, délai et
périphérique validés ; la chaîne complète dans l'ordre (moteur avec `--device cpu
--run_dir`, puis `-af` du preset) ; `none` seul et filtres vides ; rien lancé si le
binaire, les poids ou `ffmpeg` manquent, `~` développé dans `resemble_run_dir`, pointeur
Git LFS nommé ; moteur en erreur ou muet ; délai dépassé ; `doctor` ok et en échec ;
`send` de bout en bout avec les services de test : OGG envoyé et seul fichier restant dans
les trois cas (appliqué, échec, éteint), `voice.sent` avec les trois statuts et la durée,
aucun repli en texte. Ce qui marchait déjà et continue : section absente ou `enabled =
false`, le pipeline d'origine octet pour octet (synthèse, `wav_to_ogg_opus`, `sendVoice`,
usage `tts`) ; texte trop long renvoyé au modèle ; synthèse impossible, réponse en texte
avec la raison. Closes #299.

**Heures calmes : `telegram.quiet_hours` retient enfin les livraisons proactives, qui
partent groupées à la fin de la plage, une fois (#296).** Constat : la plage était validée,
réglable par `/quiet`, affichée, et le doctor affirmait « sa notification attendra » ;
aucun code de l'ordonnanceur ni de la livraison ne la consultait. Une planification de
23 h partait à 23 h ; `quiet_backlog` (HITL) n'était appelé nulle part.

Correctif. **Planifications** : dans `tick_after`, un créneau dû sans `spec.urgent` pendant
la plage n'est pas tiré ; il reste dû (dans la base : un redémarrage n'y change rien), un
événement `schedule.held` le note une fois par créneau, et il part au premier passage après
la plage par le chemin du rattrapage après veille (#228) : `late_of` reçoit la plage et
marque `Late.quiet` quand le créneau y tombait et que le tir n'y est plus, toujours annoncé
(même deux minutes après) ; créneaux manqués comptés, un seul run. Les tirs retenus d'un
passage forment une fournée (`scheduler/quiet.rs`) : un message par conversation sous
« 🌙 Pendant les heures calmes : », chaque `notify` suivi de « (prévue à 23h00 ; les N
créneaux manqués partent en une seule livraison) », un `prompt` ou `workflow` annoncé
(« elle part maintenant, sa réponse suivra »), le texte du prompt précédé de la mention ;
`schedule.notified` n'est écrit qu'à l'envoi, un envoi raté est un échec de la
planification. `mcp_poll` retenu rattrape tout d'un coup ; `event` et `watch_file` sans
`urgent` sont passés la nuit et relisent à la sortie le journal depuis le curseur gardé
(`scheduler.quiet.pushed_from`) ou comparent l'empreinte à celle d'avant (la première
observation est prise quand même) ; `urgent` passe partout. **File persistée** : nouvelle
table `quiet_queue` (migration `0025`) et module `penelope_app::quiet` (`is_quiet`, `hold`,
`deliver_or_hold`, `held`, `forget`) ; les avis du superviseur MCP (règles révoquées) y
attendent et partent avec la fournée, horodatés (« reçu à 3h12 »). Le lien OAuth, qui
expire, n'est pas produit pendant la plage : il part frais au premier passage après.
**Relances d'approbation** : rien n'est envoyé ni marqué pendant la plage ; à la sortie,
une seule relance par conversation nomme toutes les demandes retenues sous l'en-tête, puis
leurs cartes ; une demande du jour garde « Rappel 1/2 ». **Création** : la réponse de
`schedule_create` porte `heures_calmes` quand le premier passage tombe dans la plage
(heure, plage, fin, et comment recréer avec `"urgent": true`), `urgent` est validé
booléen, `/schedules` marque 🔔. **Doctor** : la cohérence ignore les planifications
urgentes et dit « retenu jusqu'à 07:00 … `"urgent": true` pour qu'il parte à l'heure » ;
`schedules` dit pendant la plage ce qui attend (retenues, livraisons en file).
`quiet_backlog` est supprimé avec son test (la colonne `quiet` de `approval_requests` et
son paramètre restent écrits, jamais lus). La clé reste `telegram.quiet_hours` (configuration
installée, `/quiet`) ; le cœur ne lit que `Config::quiet_range` et `quiet_at`, et
`TimeRange` gagne `contains_at`, `text`, `end_text`. Exceptions assumées : réponses au
propriétaire, digest (son cron), `urgent`. Doc : install-headless.md (« Heures calmes »),
telegram.md, runtime-events.md, référence des clés et des outils régénérée.

Tests : la plage à l'heure du propriétaire (`contains_at`, `quiet_at`, UTC par défaut) ;
la file dans la base (retenu la nuit, parti le jour, oublié une fois parti) ; l'ordonnanceur
: trois planifications de nuit (récurrente, rappel daté, prompt) retenues sans rien
envoyer, créneau inchangé, `schedule.held` une fois chacune, une urgente qui part à
l'heure, puis à 7 h un seul message groupé octet pour octet, le tour du prompt avec sa
mention, le rappel `done`, rien au passage suivant et le créneau suivant compté depuis 7 h
30 ; un avis MCP seul en file part sous l'en-tête avec son heure ; `event` et `watch_file`
retenus rattrapent deux événements et un fichier sans perte, l'`event` urgent parle la nuit,
le curseur gardé s'efface ; la création dit `heures_calmes` et propose `urgent`, se tait
avec `urgent` ou de jour, refuse un `urgent` non booléen, le doctor dit la même chose ;
`late_of` : deux minutes retenues sont annoncées, un tir encore dans la plage ne l'est pas,
textes avec veille et au lendemain. Relances (`penelope_app::quiet::remind_approvals`) :
deux demandes retenues dans une conversation et une dans un sujet donnent une relance
groupée par conversation sous l'en-tête puis les trois cartes, une demande du jour garde
« Rappel 1/2 », deux du jour sont groupées sans en-tête ; la passe de maintenance du daemon
n'envoie ni ne marque rien pendant la plage. Les harnais dont l'horloge est à 4 h du matin
éteignent la plage explicitement ; les tests du superviseur passent dans
`supervisor/tests.rs` (plafond de taille).
Ce qui marchait déjà et continue : le rattrapage après veille et au redémarrage (#228),
`run_now` qui ne retient jamais, les alertes d'échec et le retour au succès (#229), les
relances à T+1 h et T+6 h de jour (#97), la carte de rappel d'une commande `go-template`.
Closes #296.

**MCP : réagir aux notifications `resources/updated` d'un serveur, par abonnement, au lieu
de sonder (#293).** Constat : Pénélope ne savait que sonder un serveur MCP (`mcp_poll`,
60 s au mieux). Un serveur qui déclare `resources.subscribe` (un pont de messagerie
installé sur l'instance) pouvait la prévenir à l'instant, mais le superviseur jetait la
notification (`lifecycle.rs`, `_ => {}`) ; le client savait `subscribe_resource` et
`read_resource` sans qu'aucun chemin de production ne les appelle, et le modèle n'avait
aucun outil pour lire une ressource.

Correctif. Un kind `mcp_subscribe` (`server`, `uri` ; `item_path`, `id_path`, `filter`
comme `mcp_poll` ; sans `id_path`, la ressource entière est l'élément et chaque changement
déclenche) pour les trois cibles. L'ordonnanceur demande l'abonnement à chaque passage par
le port `McpGateway::subscribe_resource` : le superviseur le pose une fois par connexion
(`Live.subscriptions`), donc le **repose après chaque reconnexion**, et un serveur abonné
n'est plus arrêté pour inactivité (sa santé est sondée). La notification devient un
événement du journal (`mcp.resource_updated`) ; le passage suivant ouvre une fenêtre de
regroupement (`window_ms`, 30 s), puis relit la ressource (`read_resource`), dédoublonne
par empreinte (`seen_items`) et tire. Plafond `max_per_hour` (20) pour `prompt` et
`workflow` : le surplus est notifié sans modèle (`schedule.capped`). Repli automatique :
serveur sans `resources.subscribe` (ou « méthode inconnue ») sondé toutes les `every_ms`
(60 s au moins). Abonnement perdu : aucun échec compté ni alerte à chaque passage, mais
`schedule.subscription_lost` une fois, une ligne dans le digest du matin au-delà d'une
heure, puis `schedule.subscription_restored` et une relecture de rattrapage au retour.
Outil à la demande `mcp_resource_read` (`server`, `uri`, lecture, contenu encadré comme
observé). Cible `notify` avec un gabarit du catalogue (`ticket_detected`, jamais émis
jusqu'ici) : nouveau port `ChannelDelivery::schedule_card`, la passerelle Telegram rend la
carte avec « ⚡ Relire maintenant » et « 📅 Voir la planification » ; sans canal qui sait
la rendre, le texte part comme avant. `/schedules`, `penelope schedule list` et
`schedule_create` connaissent le kind (« abonnement `pont` mail://inbox »). Doc :
install-headless.md (déclencheurs), mcp.md (« Abonnements aux ressources »),
runtime-events.md, telegram.md ; référence des outils régénérée.

Tests : validation et non-programmation du kind (`penelope-workflow`) ; superviseur :
abonnement posé une fois, reposé après une connexion perdue, notification au journal,
lecture, serveur sans capacité (`Ok(false)`), serveur abonné jamais arrêté pour
inactivité ; ordonnanceur, avec le vrai superviseur et un faux pont qui pousse des
notifications : amorçage sans tir, fenêtre de 30 s puis tir, dédoublonnage, perte
constatée par la sonde de santé puis digest à une heure puis reprise avec rattrapage,
repli en sondage, plafond horaire et surplus notifié, carte du catalogue par le port ;
conformité MCP : la notification poussée après `resources/subscribe` arrive au client
(2024-11-05 et 2026-07-28) ; exécuteur : `mcp_resource_read` par la passerelle, en
lecture, encadré. Ce qui marchait déjà et continue : `mcp_poll` inchangé (mêmes
`seen_items`), les quatre autres kinds, les alertes de planification, les notifications
en texte libre. Closes #293.

**Historique : les appels d'outils entrent dans l'index plein texte, dans `history_expand`
et dans les ancres des résumés ; une commande lancée la veille se retrouve après
compaction (#300).** Constat du 02 au 03/10 sur une instance : Pénélope avait produit un
rendu audio par un `shell_exec` FFmpeg (`-af "highpass=…,acompressor=…,loudnorm=…"`) ; le
lendemain, dans la même session compactée, `history_grep` sur `acompressor` ou `loudnorm`
rendait 0 résultat, `history_expand` du bon nœud relisait le passage sans la commande, et
elle a conclu « la commande exacte n'est plus récupérable » (commentaire de #299). La
commande était intacte dans `messages.content`. Cause : les deux voies d'accès ne gardaient
que le **texte** d'un message. `searchable()` indexait `message.text()`, vide pour un
message assistant qui n'est qu'un appel (`messages_fts.content = ''`, vérifié sur
l'instance) ; `history_expand` rendait `{seq, role, texte}`.

Correctif, en cinq points. **Index** : `store::searchable_text` indexe le texte du message
puis chaque appel sur sa ligne (`outil clé: valeur …`, feuilles de l'objet d'arguments
aplaties, 2 000 caractères par appel, `fs_write.content` exclu, ainsi que la requête de
`history_grep` et la question de `history_expand_query`, qui se retrouveraient elles-mêmes à
chaque recherche), arguments passés par le
rédacteur de `penelope-observe` (le même détecteur que `secret_shelf`, sans ranger : un
jeton dans une commande est masqué avant l'index) ; le projecteur, la double écriture
(`Row::of`) et `rebuild_fts` passent tous par `store::searchable`, qui garde le texte
d'origine d'un corps externalisé ; `rebuild_fts` relit maintenant `tool_call_id`,
`tool_name`, `artifact_id` et `event_id`, donc redonne les mêmes entrées que l'écriture
directe. **`history_expand`** rend `appels: [{outil, arguments}]` pour un message qui en
porte, arguments passés par `redact_json`, chaînes coupées à 1 500 caractères
(`… [tronqué : N caractères]`), structure gardée. **Ancres** : type `commande`
(`AnchorKind::Command`, rendu « commande »), première ligne de chaque `shell_exec` du lot,
rédigée, 300 caractères, placée avant les ancres du texte sous le même plafond ;
`history_describe` la montre, et le résumé la porte, donc `history_grep` trouve le nœud par
un mot de la commande. **Réindexation** : migration `0026_messages_fts_tool_calls`, qui ne
réécrit pas le cache (règle des caches de `penelope-archtest`) : elle pose
`store.messages_fts_pending` dans `kv` si la base a des messages ; la passe de maintenance
du daemon (`session_ops::reindex_fts_if_pending`) refait `messages_fts` depuis `messages`,
lignes scellées comprises, lève la clé et laisse un `store.rebuilt` (`reason: migration`) ;
`penelope store rebuild` et la reconstruction après restauration lèvent la clé aussi ; le
journal n'est pas touché et `history verify` ne lit pas l'index. **Comportement** : règle du
harnais dans le prompt système (« Ce que tu as fait reste écrit, même après un résumé » :
`history_grep` sur un mot de la commande ou de l'argument avant de dire « irrécupérable »,
puis `history_expand`), descriptions de `history_grep` et `history_expand` mises à jour.

Tests : index (un appel trouvé par un mot de ses arguments, `loudnorm=I` compris, le
résultat indexé à part ; corps d'un `fs_write` hors index et chemin dedans ; jeton masqué ;
appel coupé au plafond ; `rebuild_fts` redonne les entrées de l'écriture directe, corps
externalisé compris) ; `expanded_message` (appels rendus, jeton masqué, contenu tronqué,
pas de clé sans appel) ; ancres (`shell_exec` seul, première ligne, rédigée, coupée) ;
migration (marque posée sur une base avec messages, pas sur une base vide, journal
intact, pas rejouée) ; `reindex_fts_if_pending` (une fois, journal de la conversation
intact). Scénario `outils-historique-appels` : commande lancée (un `true` qui porte les
filtres en arguments, sortie vide), deux échanges semés plus gros que la queue, `/compact`
qui résume les quatre premiers messages, `history_grep`
« acompressor » trouve le message brut et le résumé par son ancre, `history_expand` rend
l'appel, la réponse cite la commande. La sortie du `shell_exec` qu'il recopie porte une
durée mesurée : le normaliseur des scénarios masque désormais l'estimation de jetons d'un
objet qui cite une durée (`{{tokens}}`), comme il le faisait pour la racine temporaire et
la version, sans quoi deux rejeux différaient d'un jeton. Le texte du prompt système et les descriptions
d'outils changent : les surfaces de tous les scénarios ont été régénérées
(`UPDATE_SCENARIOS=1`), la référence des outils aussi (`UPDATE_DOCS=1`). Ce qui marchait
déjà et continue : `history_grep` sur le texte et les sorties d'outils, les ancres `path`,
`sha`, `ticket`, `url`, la refonte `history reindex`, les empreintes scellées (l'index n'en
fait pas partie). Closes #300.

**Workflows : après le clic « vas-y », la conversation sait que le run est parti, et
`workflow_start` depuis un canal dit l'état réel au lieu de renvoyer à `workflow_plan`
(#302).** Constat le 03/10, dans un sujet Telegram : `workflow_plan` publie la carte, le
propriétaire clique « vas-y », le run `r_plan_…` démarre et travaille six minutes. La
session de conversation n'en sait rien : elle reçoit « vasy » puis « vas-y » en texte et
appelle `workflow_start` quatre fois ; chaque appel passe par une carte d'approbation que
le propriétaire valide, puis est refusé par « propose d'abord un plan avec workflow_plan ;
seul le propriétaire peut valider « vas-y » », comme si rien n'avait été approuvé ;
`workflow_plan` répond « le plan a déjà été approuvé » sans dire qu'un run tourne ;
Pénélope conclut à tort que le lancement n'a pas reconnu « vasy ». Un seul run, aucune
information.

Correctif, en six points. **Le cœur écrit le lancement dans la session d'origine** : le
clic (`go_plan`, Telegram ou `wf.plan.go`) ajoute l'événement `workflow.plan.launched`
(run, version, empreinte, workflow, étape) à la session qui a préparé le plan, et une note
pour son prochain tour (`penelope_app::notices`, en `kv`) ; rien n'entre dans le
transcript hors tour, un message inséré tomberait entre un appel d'outil en attente et son
résultat. La note part dans le contexte volatil du message suivant, bloc `<evenements>`
figé avec lui (`prefix::settle`), et n'est retirée qu'une fois partie : « Le propriétaire a
cliqué « vas-y » sur la carte du plan v1 (« … ») : le run `r_plan_…` est lancé, étape
`e1-spec`. Ne rappelle ni `workflow_start` ni `workflow_plan` pour ce plan ;
`workflow_status` donne son état ». **Le refus dit l'état réel**
(`penelope_workflow::plan::gate`) : aucun plan (« propose d'abord un plan ») ; plan en
revue (« le propriétaire le valide par le bouton de la carte », et le run encore vivant
d'un plan précédent s'il en reste un) ; approuvé sans run (« attend son run : seul le
clic le lance ») ; lancé (« déjà lancé : run `r_plan_…`, en pause à l'étape `e1-spec` ;
`workflow_status` donne son état, `workflow_control` op `resume` le reprend »).
`workflow_plan` sur un plan approuvé le dit aussi, derrière « déjà approuvé ». **Pas de
carte pour un appel voué à l'échec** : le gate est vérifié au précontrôle de l'exécuteur
(`validate_call`), avant la politique et l'approbation, directement et par `tool_call` ;
l'exécution le redit au cas où. **« vas-y » tapé après le clic** (« vasy », « Vas-y ! »,
« ok vas-y » ; jamais « go » ni « ok », qui répondent au modèle) reçoit de la passerelle
l'état du run (« Déjà lancé : run `…` du plan v1, en cours ; `/stop` l'arrête,
`/resume` le reprend »), sans tour de modèle, tant qu'un run du plan est vivant ; sans
plan, ou le run fini, le texte part au modèle comme avant. **Garde anti-boucle** : un
refus de l'exécuteur avant toute carte (`ToolError::Denied` au précontrôle) revient au
modèle avec l'état ; le deuxième sur le même outil dans le tour arrête le tour
(`turn.halted`) : le résultat entre dans la conversation, les appels qui suivaient sont
fermés, la réponse au propriétaire est l'état connu, sans autre appel au modèle.
**Scénario** `plan-clic-puis-vas-y-tape` : plan, clic, « vasy » et « Vas-y ! » répondus
sans tour, puis le modèle rappelle `workflow_start` deux fois : refus avec le run, arrêt du
tour ; la surface montre la note `<evenements>` dans le message suivant, le monde un seul
run et aucune approbation.

Documentation : description de `workflow_start`, « Plans et gate » dans workflows.md,
`turn.halted` dans runtime-events.md, référence des outils régénérée.

Tests : le gate (textes des quatre états, « vas-y » strict, lecture plan et run) ; les
notes (ordre, retrait de ce qui est parti seulement, bloc) ; le préfixe (note envoyée une
fois) ; l'orchestrateur (événement et note dans la conversation d'origine, une fois, rien
dans la session du run) ; l'exécuteur (refus au précontrôle, direct et par `tool_call`,
dans les quatre états, à l'exécution, `workflow_plan` approuvé, CLI et run non concernés) ;
la boucle (deuxième refus : tour arrêté, deux résultats, aucune carte, deux appels au
modèle, `turn.halted`) ; la passerelle (« vas-y » tapé : état sans tour, message ordinaire
sans plan ou run fini). Ce qui marchait déjà et continue : le clic périmé, un seul run par
plan, `/stop` sur les runs de la conversation (`plan-en-phases`, `rpc-plans`,
`livraison-*`), la garde d'arguments invalides (#117), le détecteur de boucles (#31).

Vus au passage dans le même run, hors de ce lot : les captures renvoyées par un outil MCP
en bloc image sont marquées « non transmise » et rien n'est écrit sur disque ;
`git_clone` en https sans identifiants échoue là où `gh repo clone` réussit. Deux issues
séparées, #304 et #305. Closes #302.

### 1.0.36

**Un sujet Telegram = un projet : créé avec le sujet, rattache toutes ses sessions, et les
sujets existants le deviennent au premier démarrage (#301).** Constat sur une instance, le
02/10 : 18 sujets nommés, une session chacun, 8 projets enregistrés seulement, dont 4
déduits du premier message avec des faux positifs (`clients`, `projets`, `helloasso` pour
le sujet « WestRiders ») ; les 10 autres sujets sans projet. Cause : `resolve()` ne
rattachait une session qu'à un projet déjà connu du vault (annotation ou section de
`projets.md`) ; un sujet nouveau ne créait jamais rien. Règle du propriétaire : un sujet
est un projet, systématiquement et rétroactivement.

Correctif. **Le sujet prime** : la session d'un sujet a pour projet `normalize(nom du
sujet)` (`how = "sujet"`), connu du vault ou pas ; le titre et le premier message ne servent
plus qu'aux sessions hors sujet, parmi les projets connus, fiches comprises. Le cœur ne
connaît pas Telegram : la passerelle dit « cette session a un sujet nommé » par le port
`ChannelDelivery::subject_of` (`penelope-app`), en taisant le sujet « Général » (id 1) et le
foyer (`telegram.home`), et `session_project.rs` perd ses deux mentions du canal. **Fiche
de projet** `projets/<slug>.md` au gabarit du wiki (`type: projet`, `nom`, `aliases`,
`tags`, `created`, `updated`, titre, d'où il vient, lien vers `[[projets]]`), créée à la
création du sujet (`forum_topic_created`) et par la migration ; hors index (inventaire et
réindexation : ses entrées vivent dans `projets.md` et les annotations), type `projet`
connu du wiki, ligne `projet` dans `log.md`. **Renommage** (`forum_topic_edited`) : la fiche
est déplacée vers le nouveau slug (`rename_note`, wikilinks réécrits, ancien nom et ancien
slug en `aliases`), les annotations `<!-- projet: … -->` de `memoire.md` et `projets.md`
et la section `## Ancien nom` de `projets.md` prennent le nouveau nom, l'index est refait,
les sessions du sujet suivent ; jamais de seconde fiche. Le nom de naissance d'un sujet,
répété par Telegram en tête de chaque message, ne fait plus foi : il ne sert qu'à un sujet
encore inconnu, et ne défait plus un renommage (il écrasait le nom appris, avant).
**Migration au démarrage** de la passerelle, idempotente : pour chaque `tg.topic_name.*`
(hors Général et foyer), fiche manquante créée et **toutes** les sessions du sujet,
fermées comprises, rattachées ; un rattachement `titre` ou `message` est remplacé, un
`sujet` d'un autre nom aussi, un `explicite` (`/projet`) jamais ; le bon projet venu du
titre est juste requalifié `sujet`. Le préfixe retenu d'une session active rattachée est
libéré une fois (`session.project` au journal, instantané refigé), comme `set()` ; une
session fermée n'est que réécrite. Le compte rendu (sujets, fiches créées, sessions
rattachées, rattachements corrigés, choix conservés) est gardé quand quelque chose a
changé et paraît dans le digest du jour et du lendemain, par un point d'extension
générique du digest (`DigestInputs.notes`, lignes libres rendues telles quelles). **`/projet`
dans un sujet** montre « le projet du sujet « Dose » », avant même le premier message ; le
changer ne vaut que pour la session et la réponse le dit (« une nouvelle session du sujet
reviendra à son projet ») ; en privé, la réponse d'avant mot pour mot. `known()` liste
aussi les fiches `projets/`, donc les boutons de `/projet`. Doc : telegram.md (section
Sujets, ligne `/projet`), install-headless.md (sujet de travail), skill `wiki-markdown`
(type `projet`, emplacement `projets/`, opération `projet`).

Tests : cœur (`session_project/tests.rs`, port double) : le sujet est le projet sans être
connu, le titre et le message ne servent qu'aux sessions sans sujet et voient les fiches,
l'explicite prime ; migration sur un store mêlant session sans projet, rattachement
`message` faux, `sujet` d'un ancien nom, bon projet venu du titre, explicite et session
fermée (comptes, kv, un seul `session.project` par session active changée, aucun pour la
fermée ni l'explicite, fiches au gabarit, `log.md`, exclusion de l'index, ligne du
digest), puis second passage sans changement ; renommage (fiche déplacée avec alias,
wikilink et annotations réécrits, section de `projets.md`, index, sessions, explicite
gardé, second renommage à vide). Passerelle (`tests/subjects.rs`) : migration avec Général,
foyer, trois sujets dont un rattachement `message` et un explicite, port `subject_of` muet
pour le foyer et Général, second passage à vide ; `/projet` dans un sujet et le choix
limité à la session, réponse privée inchangée ; création de sujet, renommage, nom de
naissance répété sans effet, foyer sans fiche. Conversation : la tuile T2 suit le sujet
nommé par le port, connu (`LinkedIn`) ou pas (`Dose`). Ce qui marchait déjà et continue :
`/projet` et `penelope session project` (scénarios `commandes-reglages`, `rpc-sessions`),
le filtre T2 par projet, la stabilité du préfixe d'un tour à l'autre, les noms de sujets
pour `/schedules` et le digest. Closes #301.

### 1.0.35

**Mémoire : le digest compte chaque candidat écarté une fois (#297), et la nuit tient le
budget du Cœur au lieu de demander au propriétaire de l'alléger (#298).**

Constat #297, digest du 02/10 : « 189 candidat(s) examiné(s) : 20 promu(s), 185
écarté(s) », soit 205 pour 189, et deux « motifs d'écart » qui étaient des numéros de
candidat (« candidat 16 écarté par la grille : 4 »), « autres motifs : 62 ». Cause :
`sort_and_plan` poussait un candidat écarté par la grille une seconde fois dans
`report.rejected`, pour chaque opération que le modèle proposait quand même dessus ; comme
`rejection_family` lit ce qui suit « : », chaque numéro devenait une famille. Les
justifications libres du modèle n'y sont pour rien : le motif d'un écart est l'étiquette
fixe de la grille, et ce sont ces numéros qui fragmentaient les familles. Correctif : une
opération sur un candidat écarté ne va plus qu'au journal du tri de `DREAMS.md` (« ⏭
add_entry non appliquée : candidat 16 écarté par la grille »). Le tri en base était juste
(un seul `rejected` par candidat), seul le rapport mentait.

Constat #298 : le Cœur dépasse `memory.core_budget_tokens` (1 200) chaque nuit depuis le
19/09, jusqu'à ~5 576 jetons et 111 entrées hors du bloc le 22/09 ; la nuit continuait d'y
promouvoir sans regarder la place, et le bloc servi d'office triait à importance égale par
uid **croissant** : ce que la nuit venait d'apprendre tombait le premier. L'avertissement
« alléger memoire.md ou relever core_budget_tokens » était répété douze matins sans effet.
Correctif, en trois points. **Le bloc** (`Snapshots::build_block_with_uids`) sert, à
importance égale, la plus récente d'abord (uid décroissant : les uid sont des ULID) ;
ordre total, mêmes entrées, même bloc. **La nuit** (`dream/core_budget.rs`) décide la
place de chaque `add_entry` vers `memoire.md` après la validation : la place reste, il
s'écrit ; Cœur plein, la nouveauté que le bloc ne servirait pas va en fiche curée
(`notes.md`, rappelée à la demande), celle qui s'y classe s'écrit au Cœur et, une fois par
nuit au plus, la moins utile des entrées **hors du bloc** (importance la plus basse, puis
la moins souvent utile, la moins rappelée, la plus ancienne) descend en `notes.md`, section
« Descendues du Cœur », ligne et uid conservés (signaux d'usage et provenance suivent),
index mis à jour, deux pré-images `demote_entry` dans `mem_history`. Jamais une entrée de
provenance `owner` (écrite par le propriétaire), jamais une qui reprend sa phrase (#245,
`CandidateStore::owner_quoted_texts`), jamais ce qui a été écrit la nuit même. Ce qui
descend est hors du bloc servi : le préfixe stable (T2) ne change que par ce que la nuit
ajoute, et seulement la nuit. **Le digest** garde le constat (« niveau Cœur à ~N jetons
pour un budget de B : K entrée(s) hors du bloc servi d'office, trouvables par le rappel »)
et dit ce que la nuit a fait (« Cœur plein : N nouveauté(s) rangée(s) en notes »,
« Rétrogradée du Cœur en notes : « … » (importance i, r rappel(s)) ») ; plus d'injonction ni
de commande. `doctor` (`memory.size`) garde le constat et donne le réglage. Rapport :
`DreamReport.demoted`, `DreamReport.core_full` ; `DREAMS.md` trace « ↓ rangée en notes »
et « ↓ descendue en notes ». Pas de clé nouvelle. Doc : install-headless.md (forme du
digest, « Le Cœur a un budget, la nuit le tient »).

Tests : trois candidats dont deux écartés par la grille avec deux opérations proposées sur
chacun, « 3 examinés : 1 promu, 2 écartés », une seule famille « retrouvable ailleurs »,
quatre lignes « non appliquée » au tri, `promoted + rejected == candidates_seen` ; le bloc
à importance égale sert la plus récente, déterministe, l'importance prime ; une première
nuit écrit quatre faits au Cœur (dont un qui reprend la phrase du propriétaire, à côté
d'une ligne manuscrite), la seconde, budget réduit à trois entrées, range la nouveauté
d'importance 1 en notes, écrit les deux d'importance 9 au Cœur et fait descendre la seule
entrée non protégée hors du bloc (importance 4, un rappel), une fois, pas deux : même uid
dans `notes.md`, index en `cure`, rappels conservés, deux pré-images, bloc T2 changé par la
nuit et stable ensuite, digest sans « config set » ni « alléger » ; quand seules des
entrées protégées sont hors du bloc, rien ne descend. Ce qui marchait déjà et continue :
le tri de la grille, la validation et l'écriture lot par lot, le rapport complet de
`DREAMS.md`, les entrées hors du bloc trouvables par le rappel (#62), le digest en une
bulle (#145). Closes #297. Closes #298.

### 1.0.34

**Expérience persévérance : un indice de contexte en fin de requête et le rappel de
délégation sans chiffres, deux interrupteurs éteints par défaut (#291).** Constat : sur des
tours longs, Pénélope conclut parfois avant d'avoir fini, sans que le plafond d'appels ni le
budget soient atteints ; le seul texte qui lui parle de budget est la note de
`delegate_after_calls`, qui cite le nombre d'appels et le coût du tour. Hypothèse,
anecdotique et non prouvée (issue 49735 du dépôt openai/codex) : rappeler au modèle son
budget le pousse à conclure. La même source rapporte qu'un texte « unlimited tokens » ne
change rien chez elle et qu'une fenêtre chiffrée (« You have 500000 tokens context
window. ») si : le texte est laissé libre. On outille la mesure sans trancher.

Correctif. `agent.context_hint` (chaîne, défaut vide : rien ne part) : renseigné, à chaque
appel au modèle d'un tour de conversation, la requête envoyée porte en toute fin un message
de rôle `developer` avec ce texte exact, porté par un champ de `ChatRequest` hors
`messages` (`developer_note`) : jamais dans l'historique, le journal `conv.*`, le transcript
ni l'instantané du prompt, et hors de l'empreinte (`system_hash`, `tools_hash`,
`request_hash` inchangés, cache de préfixe intact). Sérialisé par le corps Responses
(dernier item de `input`) et par le corps « chat completions » (dernier message) ; décidé
par le modèle de la tentative : `codex:` seulement, parce que le schéma d'OpenRouter ne
liste pas le rôle `developer` (relu le 01/10) et qu'un serveur local ne le connaît pas ; un
repli hors Codex part sans. Présence journalisée dans `runtime.llm` (`context_hint: true`,
absent sinon). `budget.delegation_nudge_style`
(`classique` par défaut, `doux`) : en `doux`, la note ne cite ni compte ni coût ; elle dit
de regrouper ou de déléguer à `sub_agent_spawn`, de tenir `session_notes` et de continuer
jusqu'au bout de la demande ; `tool.result` porte `nudge_style` quand il porte la note.
Nouvelle section `[agent]` ; référence des clés, fixture 0.17 et golden `config.get`
régénérés ; section « Expérience : persévérance » dans install-headless.md (activer,
mesurer, revenir en arrière).

Tests : corps « chat completions » et Responses avec et sans la note (rôle, texte exact,
dernière position) ; la boucle : éteint par défaut (chaîne vide ou blanche), texte exact
présent pour `codex:` avec un historique envoyé et des empreintes identiques à la requête
sans indice, rien dans la
conversation ni dans le journal, `runtime.llm` marqué ; absent pour DeepSeek, interrupteur
activé ; la note dans les deux styles (le doux sans chiffre ni dollar) et son style dans
`tool.result`, rien sans note ; le ledger n'écrit le champ que quand l'indice est parti.
Scénario `indice-perseverance-codex` : la surface montre `developer_note`, le monde non.
Ce qui marchait déjà et continue : la note classique octet pour octet (scénarios
`session-froide`, `compaction-*`, `outils-historique-resumes`,
`outils-niveau-1-et-compaction`), l'empreinte du cache et la machine d'état des appels,
le repli vers un modèle hors `codex:`. Closes #291.

### 1.0.33

**Sauvegardes : les index recalculables restent hors de l'archive, et un stockage S3
(MinIO, Scaleway, AWS) s'ajoute au dépôt git (#289).** Constat sur une instance, le
01/10 : la sauvegarde nocturne a échoué, « archive de 111 Mo : au-delà de la limite de
100 Mo du dépôt » ; l'archive grossissait vite (70 Mo le 23/09, 99 Mo le 30/09, 116 Mo le
01/10) avec une base de 349 Mo dont plus de 110 Mo d'index recalculables (`embeddings_cache`
50 Mo, `mem_vec` 33 Mo, tables d'ombre de `messages_fts` 28 Mo et plus). Deux causes :
l'instantané `VACUUM INTO` embarquait ces index, et la seule destination hors machine était
un dépôt git borné par `backup.max_push_bytes` ; une archive qui dépasse ne partait nulle
part, chaque nuit.

Correctif, en trois lots. **Instantané allégé** : sept tables dérivées (`messages_fts`,
`mem_fts`, `mcp_tools_fts`, `mem_vec`, `intent_vec`, `mcp_tools_vec`, `embeddings_cache`)
sont vidées de la copie puis `VACUUM` ; la copie porte la clé `store.rebuild_pending`, que
la passe de maintenance du daemon honore au premier passage après une restauration (index
plein texte reconstruits par les crates qui les tiennent, `rebuild_fts` ajouté à
`MemoryIndex` et `ToolRegistry` ; les vecteurs reviennent par le rattrapage d'embeddings
déjà en place). Le manifeste dit ce qui manque (`derived_excluded`, `db_full_bytes`) et
`restore-all` le répète. Mesure sur une base synthétique de 45,9 Mo dont 32 Mo de vecteurs
incompressibles : instantané 9,5 Mo, archive 33,2 Mo -> 3,5 Mo (test ignoré
`measure_the_archive_with_and_without_derived_tables`) ; sur l'instance, les 83 Mo de
vecteurs ne se compressent pas, l'archive de 116 Mo devrait tomber autour de 30 Mo
(estimation, à relire après la première nuit). **Destination S3** : `[backup.s3]`
(`endpoint`, `bucket`, `prefix`, `region` défaut `us-east-1`, `path_style` défaut `true`
pour MinIO, clés en `${SECRET:…}`), validée au chargement (HTTPS hors boucle locale, pas
d'identifiants dans l'adresse) ; signature AWS SigV4 écrite ici (`backup/sigv4.rs`,
HMAC-SHA256 sur `sha2`, pas de SDK) ; client `backup/s3.rs` : PUT en parties au-delà de
64 Mo, taille relue par `HEAD`, LIST paginé, DELETE, GET en flux, erreurs qui nomment leur
cause. `backup --push` sert toutes les destinations configurées ; l'échec de l'une ne
retient pas l'autre (`pushed`, `pushed_s3`, `failed`, message « sauvegarde partielle » la
nuit) ; tout en échec reste une erreur ; `max_push_bytes` ne borne que le git ; la rotation
`keep_*` s'applique aux objets du préfixe ; `doctor` gagne `backup.s3` (bucket joignable,
dernière sauvegarde S3 et son âge). `penelope restore-all s3 [--list] [--archive <clé>]`,
ou `s3://bucket/prefixe --endpoint …` sur une machine neuve, clés par
`PENELOPE_S3_ACCESS_KEY_ID` et `PENELOPE_S3_SECRET_ACCESS_KEY`, sinon le magasin de
secrets, sinon l'invite. **Doc** : install-headless.md (S3, MinIO, clé limitée au bucket,
restauration), référence des clés et golden `config.get` régénérés.

Tests : une base peuplée, sauvegardée puis restaurée dans une racine neuve rend les mêmes
recherches (historique et mémoire) qu'avant, une fois les index reconstruits, et la base
vivante n'a rien perdu ; SigV4 contre les vecteurs officiels d'AWS (`get-vanilla`,
`post-x-www-form-urlencoded`, GET Object, PUT Object, GET Bucket list et lifecycle) ; un
faux serveur S3 qui recalcule la signature de chaque requête : envoi vérifié, huit parties
recomposées et abandon sur partie refusée, rétention avec manifestes et liste paginée,
restauration (liste puis téléchargement), erreurs 403, 404, mauvaise clé et serveur
injoignable, deux destinations avec échec partiel, `doctor`. Ce qui marchait déjà et
continue : le dépôt git seul (envoi, rotation, refus au-delà de la limite, dépôt public
refusé), la restauration depuis une archive locale ou un dépôt, la méthode RPC `backup` sans
`push`, la base vivante jamais touchée par l'allègement (règle des caches de
`penelope-archtest`). Closes #289.

### 1.0.32

**Mémoire : `penelope mem reclaim` rattrape les rejets « ni dit ni confirmé » que le
propriétaire avait dits, et propose au tri les faits d'une fiche qui est sa parole (#285).**
Constat de l'audit en lecture seule d'une instance, le 30/09 : 118 candidats rejetés « ni
dit ni confirmé par le propriétaire, ni constaté par un outil », dont des faits dictés à
l'oral puis reformulés par la relecture (le cas corrigé pour l'avenir par #245, mais un
rejet est définitif, et `retry-rejected` ne vise que les motifs hérités de #24) ; et
l'export de la mémoire d'un autre assistant, ingéré en fiche `sources/`, dont le rêve du
22/09 a jugé les faits « retrouvable ailleurs » : rien n'est passé au profil.

- Cause : la relecture d'un épisode ne porte pas la phrase du propriétaire, celle d'un tour
  ne la portait pas avant #245 ; le verdict `endosse ✗` s'écrit en base et n'est jamais
  relu. Les faits de l'export ont été soumis comme candidats de l'agent, la fiche elle-même
  les rendant « retrouvables ailleurs » ; personne n'a dit au tri que cette fiche est la
  parole du propriétaire.
- Correctif (`penelope_vault::reclaim`, RPC `mem.reclaim`) : sans `--source`, chaque
  candidat rejeté à ce motif sans `owner_quote` est relu avec les messages du propriétaire
  de son tour (`messages.source_turn_id`, sinon le dernier message avant `observed_at` et la
  fenêtre de son tour) ou de son épisode, déclencheurs, relances et contenus transférés
  exclus ; si `owner_quote::owner_statement` retrouve la phrase, il repasse en `new`,
  origine `owner`, phrase posée, `deferrals` à zéro (`CandidateStore::rejected_for`,
  `requeue_as_owner`). Avec `--source <fiche>` (chemin relatif au vault, sans `..`), la
  fiche est découpée de façon déterministe (puces, lignes numérotées, préfixe daté retiré ;
  sans puce, paragraphes scindés en phrases au-delà de 300 caractères), chaque fait passant
  le rangement des secrets et le filtre d'écriture avant de devenir un candidat `fait`,
  `owner`, phrase = le fait, `source_ref = source:<fiche>`. Une clé `kv` par passage
  (`mem.reclaim.rejected`, `mem.reclaim.source:<fiche>`) : relancée, la commande dit ce qui
  a déjà été fait et n'écrit rien ; `--dry-run` liste sans rien écrire (les secrets montrés
  avec la référence qu'ils auraient, `secret_shelf::masked`). Événement `memory.reclaimed`
  sur un passage réel. Rien n'entre dans le profil : le tri nocturne garde ses portes.
- Au passage : `owner_quote::sentences` retire la mention que la passerelle met en tête
  d'un vocal transcrit (« (message vocal transcrit ; réponds en vocal…) ») ; ses mots
  diluaient la première phrase dictée sous le seuil des 75 %, en direct comme au rattrapage.
- Tests : `rejected_owner_words_are_requeued_once_and_only_with_their_sentence` (tour par
  l'heure, tour par identifiant, épisode ; déduction, autre épisode, contenu transféré,
  purgé et autre motif inchangés ; à blanc rien n'est écrit ; relancée rien ne double, un
  seul événement), `a_source_that_is_the_owners_word_is_proposed_to_the_sort_once` (chemins
  refusés, secret rangé au réel et masqué à blanc, consigne injectée refusée, profil
  intact), `facts_are_cut_from_bullets_or_paragraphs`, `a_turn_window_stops_at_final_answers`,
  `a_requeued_rejection_becomes_the_owners_word`, `texts_from_a_source_are_listed_whatever_their_state`,
  `a_transcribed_voice_message_starts_after_its_preamble` (rouge avant) ; routage CLI ;
  golden `mem.reclaim.json` ; étape `mem.reclaim` du scénario `rpc-memoire`.
- Ce qui tenait tient : `retry-rejected` (#24), la phrase au tour (#245), la grille, la
  purge (`owner_quote` effacé), `mem candidates`, la relecture d'un tour et d'un épisode.
- Doc : `install-headless.md` (Rattrapage), `README.md`, `runtime-events.md`. L'instance
  réelle n'est pas touchée par ce lot : `--dry-run` d'abord, puis le passage avec l'accord
  du propriétaire.

Closes #285.

### 1.0.31

**Relecture : les plus importants survivent à la coupe, et le modèle connaît la limite
(#283).** Audit du 30/09 sur l'instance, vingt relectures relues à la main : une relecture
de tour a gardé un menu de cantine d'importance 2 alors qu'un candidat plus important, rendu
plus loin, pouvait sauter.

- Cause : `parse_candidates` s'arrêtait au `max`-ième candidat valide dans l'ordre de
  sortie du modèle ; `CandidateStore::record` trie bien, mais recevait une liste déjà
  coupée. Ni le prompt de tour ni celui d'épisode ne disaient la limite au modèle.
- Correctif (`review.rs`, `episodes.rs`) : tous les candidats valides sont gardés, la
  bonification à 8 d'une correction (tour correctif) s'applique, puis tri stable par
  importance décroissante (l'ordre du modèle départage) et coupe à
  `memory.review_max_candidates`. Les deux prompts deviennent des fonctions de `max` :
  « au plus N candidats, les plus importants d'abord ».
- Tests : sept candidats aux importances mêlées, `max` 5, les cinq plus importants dans
  l'ordre attendu ; une correction à 3 dans un tour correctif passe à 8 avant la coupe ;
  la requête envoyée au modèle porte la limite (tour et épisode).
- Ce qui continue de marcher : filtre de forme et de secret des candidats, `max` 1 rend un
  candidat, `record` reste borné, le journal du jour reçoit une ligne par candidat gardé.

Closes #283.

**Un mot de passe dicté (« le mot de passe c'est … ») part au magasin de secrets (#284).**
Le 18/09, un mot de passe de développement dicté par le propriétaire a fini en clair dans un
candidat (`mem_candidates`, rejeté « imprécis »), dans `journal/2026-09-18.md` et donc dans
l'historique git du vault, alors que le prompt de relecture demande un candidat « fait » avec
la valeur, que `secret_shelf::shelve` range ensuite.

- Cause : le seul motif générique de `penelope-observe::redact`, « affectation de secret »,
  exige un mot-clé anglais (`password`, `key`, `token`…) **et** un `:` ou `=`. Une phrase
  dictée puis reformulée par le modèle n'a ni l'un ni l'autre : `secret_spans` vide, `shelve`
  rend le texte tel quel, `write_filter` ne voit rien. Sans préfixe de fournisseur ni entropie
  élevée, aucun autre motif ne rattrapait la valeur.
- Correctif (`redact.rs`) : motif « mot de passe » : `mot de passe | mdp | password |
  passwd`, jusqu'à huit mots de contexte, puis `est | c'est | sera | is | : | =`, guillemet
  ouvrant facultatif, valeur d'un mot de six caractères au moins ayant la forme d'un mot de
  passe (deux classes parmi minuscules, majuscules, chiffres, symboles). Traité comme
  l'affectation : rangé par `shelve` sous `mot-de-passe-<contexte>-<empreinte>`, refusé par
  `write_filter`, masqué par `redact` (la valeur seule, la phrase reste lisible), `certain`
  seulement pour un jeton aléatoire. « Le mot de passe est obligatoire », « le mot de passe
  est celui du wifi » ne sont pas des secrets.
- Tests : `secret_spans` et `redact` sur sept tournures (virgule, apostrophe typographique,
  guillemets, `mdp :`, anglais, `sera`, connecteur après un « est » de prose) et huit phrases
  ordinaires ; `forbidden_secret` nomme « mot de passe », fragment `Sole…`, non certain ;
  `secret_name` porte nature et contexte ; `review()` avec un modèle scripté et une valeur
  factice : le magasin a la valeur, le candidat, sa citation et le journal n'ont que la
  référence. Scénario `purge` régénéré : sa surface (rédigée à l'enregistrement) masque
  désormais « le mot de passe du wifi est framboise-42 » ; le modèle reçoit la phrase
  entière, rien d'autre ne change.
- Ce qui continue de marcher : affectations `password: …`, clés de fournisseurs, cartes
  refusées, valeurs du magasin sans forme de secret non interdites, mots ordinaires après un
  mot-clé non appris (#258), rédaction idempotente, références `${SECRET:…}` intactes.
- Constat annexe, sans correctif : un nom de modèle a été remplacé par `${SECRET:…}` dans un
  candidat ; `secret_spans` prend toute valeur de huit caractères après `key|token|secret…
  [:=]`, sans le test de forme que `learnable` et `certain` appliquent. Limites assumées :
  une valeur en plusieurs mots ou tout en minuscules n'est pas reconnue ; la valeur du 18/09
  reste dans l'historique git du vault (à purger ou faire tourner côté instance).

Closes #284.

### 1.0.30

**Mémoire : `DREAMS.md` était indexé, le rapport des rêves remontait dans le rappel
(#282).** Relevé sur l'instance (1.0.22) : 1 375 entrées sur 4 257, un tiers de l'index,
venaient de `DREAMS.md`, le compte rendu des passes de consolidation (tri, motifs d'écart,
questions sans réponse) qui grossit à chaque nuit et à chaque ingestion. Le rappel et
`mem_search` pouvaient ramener un verdict de tri ou le texte d'un candidat **rejeté** à la
place d'une note, ce qui contredit la décision du tri. Test rouge avant le correctif : un
vault avec `DREAMS.md` et une note, une entrée du rapport déjà indexée ; après `reindex`,
l'index gardait quatre entrées au lieu d'une et `DREAMS.md` n'était pas listé comme exclu.

- Cause : `vault_inventory::excluded` écartait `log.md` (« journal des opérations, en
  ajout seul »), `inbox/`, `accueil/`, `audits/`, `archive/`, les notes de session,
  `attachments/` et les pages générées, mais pas `DREAMS.md`, du même genre ; et `reindex`
  ne retirait jamais les entrées d'un fichier qu'il ne relit plus.
- Correctif : `penelope_memory::wiki::DREAMS_FILE`, constante partagée par l'écrivain du
  rapport (rêve, nuit sans consolidation, question sans réponse), l'instantané, le niveau
  `Revue` et le type de note ; `excluded` la range avec le motif « compte rendu des rêves,
  en ajout seul », donc `vault check`, `doctor` et le rappel la traitent comme `log.md`.
  `reindex` commence par retirer (`retire_file`, même statut `retiree` que la suppression
  manuelle) les entrées de tout fichier exclu encore présent dans l'index, puis le saute :
  ses lignes ne reçoivent plus de `^uid`.
- Tests : `the_dream_report_is_excluded_and_its_entries_leave_on_reindex` (l'index ne
  garde que la note, l'entrée du rapport passe `retiree`, le fichier est dans `excluded`
  avec son motif et n'est pas réécrit) ; `no_reindex_advice_on_an_excluded_file` couvre
  `DREAMS.md`.
- Ce qui continue de marcher : le rapport s'écrit toujours dans `DREAMS.md` (tests
  `apply`, `clash`, `digest` de penelope-dream) ; `Level::from_path("DREAMS.md")` reste
  `Revue` ; `log.md` et les autres exclusions inchangées ; la provenance et les signaux
  d'une entrée retirée sont conservés comme pour `forget`.
- Sur l'instance : `penelope mem reindex` doit rendre environ 2 900 entrées, aucune de
  `DREAMS.md`.

Closes #282.

### 1.0.29

**Trace narrée : le prompt du rôle `trace` refait au banc, budget porté à 1 200 ms,
`penelope local install --no-think` (#280).** Constat du 30/09 sur l'instance réelle
(MacBook Pro M1 Pro, `mlx_lm.server`, dix tours réels de l'historique) : avec le prompt de
la 1.0.27 (#273), les trois petits modèles essayés recopiaient l'exemple de la consigne
(« Relecture des notes de septembre ») neuf fois sur dix ; la consigne annonçait « un emoji
de la liste » sans donner la liste ; la phrase précédente, passée au modèle, était une
seconde source de recopie ; la latence mesurée (0,5 à 1,1 s) dépassait souvent le budget
de 500 ms ; Qwen3 écrivait `<think>` avant la phrase, rejetée par le nettoyage.

- Cause : un exemple dans le message système est, pour un modèle de 1,7 milliard de
  paramètres, une réponse à recopier plutôt qu'une forme à suivre ; la liste d'emojis
  n'était que dans le code ; la réflexion de Qwen3 ne s'éteint qu'au serveur.
- Correctif (`trace/narrate.rs`) : consigne sans phrase d'exemple, liste des emojis
  d'activité et leur sens donnée au modèle (💬 message envoyé ajouté ; ⏸️, ✅ et 🚫
  restent admis en tête d'une phrase sans être proposés, le modèle les collait à la
  phrase), quatre tours d'exemple en few-shot, message du propriétaire réduit à la liste
  des douze derniers groupes (« n. outil argument (×N) état »), plus de phrase précédente
  ni d'état du tour. L'anti-clignotement reste dans la boucle, où il était déjà : le
  modèle n'est rappelé que si la ligne `resume` (clé de l'état) a changé, et une phrase
  identique ne retouche pas la bulle. `BUDGET` 500 → 1 200 ms (la bulle s'édite au plus
  toutes les 1,5 s, hors chemin de la réponse) ; `max_tokens` 60 → 30.
- `penelope local install --no-think` : ajoute `--chat-template-args '{"enable_thinking":
  false}'` aux arguments du LaunchAgent de `mlx_lm.server`, en un seul argument, sans
  shell.
- Mesuré avec le nouveau prompt : Qwen3-1.7B-4bit sans réflexion, médiane 0,54 s, phrases
  justes (« 💻 Historique des logs et correction des fichiers »).
- Tests : forme de la requête (système avec la liste, quatre paires, dernier message = la
  liste, chaque exemple passe `clean` tel quel), prompt sans secret ni résultat ni phrase
  précédente, douze derniers groupes numérotés de 1 à 12, 💬 admis, `<think>` rejeté ;
  passerelle : dix messages par appel, `max_tokens` 30, phrase gardée sans retouche ;
  `--no-think` ajoute l'argument, sans l'option rien ne change. Scénario
  `trace-des-outils-narre` régénéré : seuls le prompt, `max_tokens` et `budget_ms`
  changent.
- Ce qui continue de marcher : `off`, `compact`, `full` et `resume` inchangés ; la bulle
  précède toujours la réponse ; repli `resume` sur erreur, dépassement ou phrase invalide ;
  `trace.narrated` et l'usage au rôle `trace` ; `kept` dans le journal ; une bulle ouverte
  d'avant #273 se relit.
- Doc : `docs/telegram.md` (Trace des outils), `docs/install-headless.md` (rôle `trace`,
  `--no-think`, modèle conseillé, chiffres du banc sur M1 Pro).

Closes #280.

**MCP : un serveur qui répond `-32601` à `ping` reste `ready` (#276).** Constat du 30/09
sur l'instance (1.0.22) : le serveur MCP de Slack (protocole 2025-06-18) sert ses 26
outils et `penelope mcp test slack` passe, mais `mcp list` le montre `connecting` avec
« erreur JSON-RPC -32601 : Method not found: ping » et `doctor` avertit, avec une
correction (`mcp restart`) qui ne change rien.

- Cause : la sonde d'entretien (`health()`, après 60 s de silence) envoie `ping` ; Slack
  répond `-32601 Method not found`, compté comme une panne : `connection_lost` ferme la
  connexion, pose l'erreur et l'état `connecting`. Le serveur lazy ne repart qu'au
  prochain appel d'outil, puis retombe une minute plus tard.
- Correctif (`penelope-mcp`, `client.rs`) : une réponse `-32601` à `ping` prouve que le
  transport et le serveur sont vivants ; la sonde passe, le refus est retenu sur la
  session (`ping` n'est plus envoyé) et journalisé une fois en `debug`. Un serveur muet
  (délai) ou coupé reste une panne ; `-32601` sur `tools/call` reste une erreur. Le
  serveur reste `ready`, `doctor` le dit sain (« N outil(s), M appel(s) »).
- Tests : client, `a_server_that_refuses_ping_is_alive_and_is_not_pinged_again` (rouge
  avant) et `a_mute_server_still_fails_and_method_not_found_on_a_call_stays_an_error` ;
  superviseur, `a_server_that_refuses_ping_stays_ready_through_maintenance` (rouge avant :
  `connecting`, `running: false`, l'erreur de l'issue) ; `doctor`,
  `doctor_does_not_warn_about_a_server_that_refuses_ping`. Scénario `rpc-diagnostic` :
  second serveur `slack` déclaré par `mcp.add` (`ping = false`, nouveau champ de
  `[[mcp_servers]]`), `advance_clock 2m`, puis `mcp.list` montre les deux serveurs `ready`
  sans erreur et `doctor` rend `mcp.slack` sain. Harnais : avec `[[mcp_servers]]`,
  l'entretien du superviseur passe après chaque `advance_clock`, comme sa boucle l'aurait
  fait ; sans le correctif, le scénario rend `slack` en `connecting` avec l'erreur.
- Ce qui continue de marcher : `maintenance_stops_idle_servers_and_checks_health` (serveur
  perdu ⇒ panne), `health_probe_depends_on_version` (`server/discover` en 2026-07-28),
  les scénarios `rpc-mcp-et-import` et `rpc-autorisation-mcp`, sans `advance_clock`,
  inchangés.
- Limite : un serveur sans `ping` n'est plus sondé pendant son silence ; sa coupure se
  voit au prochain appel d'outil.

Closes #276.

### 1.0.28

**Serveurs MCP : `{data}` n'était pas développé dans les valeurs de `env` (#270).** Une
déclaration `mcp.d/whatsapp.toml` avec `WA_DATA_DIR = "{data}/mcp-data/whatsapp"` passait
le gabarit tel quel au processus, qui tentait de créer un dossier nommé `{data}` dans son
répertoire courant ; le bac à sable refusait (« Operation not permitted »), et Pénélope
concluait à tort que le bac à sable était en cause, en conseillant `sandbox_profile =
"full"`. `docs/mcp.md` donnait pourtant `env = { COMPTA_BASE = "{data}/compta" }` en
exemple. Deux tests rouges avant le correctif : un `env` avec `{data}` rendu littéral dans
la spécification du processus ; l'explication d'une mort sur `{data}/…` qui recommandait
le profil `full`.

- Cause : le connecteur de processus développait `command`, `args` et `cwd` avec
  `Directories::expand` (et `roots` dans le profil de bac à sable et `roots/list`), mais
  recopiait les valeurs de `env` après la seule résolution des secrets.
- Correctif : `stdio_spec` (penelope-mcp-host) construit la spécification du processus et
  développe les valeurs de `env` avec la même fonction que les autres champs ; la
  déclaration garde le gabarit. L'explication d'un échec relève un gabarit resté tel quel
  dans la sortie d'erreur et le nomme comme cause (renvoi aux champs développés, ou liste
  des gabarits connus pour un nom inconnu comme `{home}`), sans plus conseiller le profil
  `full` ; un refus d'écriture sans gabarit garde le conseil.
- Tests : la spécification développe `env` comme `args` et `cwd`, une accolade qui n'est
  pas un gabarit reste intacte ; l'explication nomme `{data}`, `{home}`, et garde le bac à
  sable pour `{0}` ou `{}` ; le serveur stdio réel (macOS) reçoit `{data}` développé et
  l'écrit sur sa sortie d'erreur.
- Scénario `rpc-mcp-et-import` : `forge` est essayé et déclaré avec `FORGE_DATA =
  "{data}/mcp-data/forge"` ; `mcp.show` rend la déclaration avec son gabarit. Le
  connecteur de test n'ouvre aucun processus : le développement à l'ouverture est prouvé
  par les tests unitaires.
- Ce qui continue de marcher : `command`, `args`, `cwd` et `roots` développés comme avant ;
  `${SECRET:nom}` résolu dans `env` et `headers` ; la phrase du trousseau (#122) ; le
  conseil du bac à sable pour un refus d'écriture sans gabarit.

Closes #270.

### 1.0.27

**Trace des outils : deux modes résumés, `resume` sans modèle et `narre` par un petit
modèle local (#273).** Constat du 30/09 : pendant un tour un peu long, la bulle `compact`
(1.0.14, #222) alignait quinze lignes `📄 fs_read ✅` qui ne disaient pas ce que Pénélope
faisait. Le propriétaire veut une phrase courte, écrite sur la machine, à partir de la liste
des outils appelés.

- `telegram.tool_trace = "resume"` : une seule ligne, déterministe, par famille et par
  verbe dans l'ordre d'apparition (`📄 6 lectures, 7 recherches · 💻 cargo test en
  cours`, puis `… · ✅`, `❌ 2 échecs`, `🚫 1 refusé`, `⏹ 1 sans réponse`) ; la commande
  shell citée en privé, comptée en groupe ; aucun appel, aucune latence. C'est aussi le
  repli de `narre`.
- `telegram.tool_trace = "narre"` : à chaque modification de la bulle, le modèle du
  nouveau rôle `trace` (`models.roles.trace`, sinon l'alias `local` puis le premier alias
  `local:` de texte) reçoit la liste des appels dans l'ordre, arguments déjà caviardés,
  état et phrase précédente, et rend une phrase de 5 à 10 mots derrière un emoji d'une
  liste fermée ; hors liste, l'emoji de l'activité ; une phrase identique ne modifie pas la
  bulle. Appel par le port de modèles (`ProviderSource`), comme le titre ; borné à 500 ms,
  une tentative ; jamais sur le chemin de la réponse : la bulle est créée avec la ligne
  `resume`, le modèle parle dans la tâche de chaque modification et à la clôture, une fois
  la réponse partie ; dépassement ou serveur absent ⇒ `resume` pour cette modification,
  sans erreur dans la bulle. Événement `trace.narrated` (jetons, durée, repli et raison),
  usage compté au rôle `trace`, 0 $ en local, mesuré sinon.
- Frontière canal/cœur (#214) : le rendu reste dans la passerelle ; `doctor` reçoit le
  contrôle `telegram.trace` par un nouveau défaut du port `ChannelDelivery`
  (`doctor_checks`), sans qu'une crate agnostique nomme le canal. Sans modèle ou serveur
  arrêté, le contrôle échoue en nommant le repli.
- Harnais des scénarios : une ligne de `model.jsonl` peut porter `"role": "trace"` ; elle
  sert les appels du modèle de ce rôle, dans sa propre file, sans voler une ligne au tour.
- Tests : rendu `resume` (comptes, commande en cours, échecs, refus, redémarrage), prompt
  du rôle sans jeton ni `${SECRET:…}` ni résultat, nettoyage de la phrase, résolution du
  rôle ; dans la passerelle, `resume` sans appel de modèle, `narre` avec phrase rendue,
  phrase précédente passée et gardée (une seule modification), repli sur erreur du modèle,
  serveur local arrêté replié sous le budget par le vrai fournisseur, `doctor` ; scénarios
  `trace-des-outils-resume` et `trace-des-outils-narre` (phrase puis repli au tour suivant,
  liste masquée dans la surface).
- Ce qui continue de marcher : `off`, `compact` et `full` sont inchangés (mêmes rendus,
  mêmes attendus de `trace-des-outils`) ; la bulle précède toujours la réponse ; une bulle
  ouverte d'avant cette version se relit (`phrase` absente admise).
- Limite : la phrase ne coiffe pas la liste en `full` ; le mode est exclusif. Un modèle
  distant sur le rôle `trace` est facturé à chaque modification, `doctor` le dit.

Closes #273.

### 1.0.26

**Détecteur d'injection : `hidden_unicode` prenait l'emoji 🏃‍♀️ pour des caractères cachés
(#271).** Chaque message WhatsApp lu par MCP dont le nom de conversation contient un emoji
composé déclenchait l'alerte du détecteur local, ce qui la rend bruyante et finira par la
faire ignorer.

- Cause : la règle était une classe de caractères `[\u{200b}-\u{200f} …]` qui compte le
  joigneur U+200D quel que soit son voisinage ; 🏃‍♀️ est U+1F3C3 U+200D U+2640 U+FE0F. Le
  sélecteur U+FE0F, lui, n'était pas regardé du tout.
- Correctif : la règle devient un parcours qui lit les voisins (`Matcher::Scan`, à côté des
  règles regex). Le joigneur ne compte pas entre deux pictogrammes (en sautant un sélecteur,
  cas de 👁️‍🗨️), un U+FE0E/U+FE0F ne compte pas derrière un caractère qui admet la
  présentation emoji (pictogramme ou base de touche `#️⃣`). Restent signalés : les balises
  de tag U+E0000..U+E007F (même dans un drapeau subdivisionnel, c'est le vecteur des
  injections invisibles), les contrôles bidi, dont les isolats U+2066..U+2069 qui
  manquaient, les espaces de largeur nulle et gluons, le BOM, un joigneur isolé, doublé ou
  entre lettres, et tout sélecteur de variante hors séquence (une lettre suivie de FE0F, un
  second sélecteur derrière un emoji, FE00..FE0D). Les pictogrammes sont reconnus par
  blocs (`is_pictograph`), sans dépendance : distinguer un symbole d'une lettre suffit ici.
- Tests : `emoji_sequences_are_not_hidden_unicode` (l'extrait de l'issue, drapeaux, tons de
  peau, famille à trois joigneurs, ⚡️, ©️, touches, ☺︎) et
  `hidden_unicode_still_catches_invisible_payloads` (seize charges invisibles), tous deux
  rouges avant le correctif ; `detects_hidden_unicode` et `ca_13_1` inchangés.
- Ce qui continue de marcher : les neuf autres règles et leur ordre, la borne de dix
  signalements, l'extrait cité dans l'alerte, `wrap_untrusted`.

Closes #271.

### 1.0.25

**Scénarios : deux rejeux de `plan-en-phases` différaient par intermittence sur la CI
Linux (#269).** Le rejeu unique contre `expected.jsonl` passait ; c'est
`two_replays_of_every_scenario_are_identical`, sous la charge de toute la suite, qui
tombait. Deux tests rouges avant le correctif : une étape `telegram` dont le clic met 300 ms
à envoyer sa carte se concluait sans elle ; et deux sessions dont les ULID s'ordonnent à
l'inverse de leur création voyaient leurs effets relevés dans l'ordre des identifiants.

- Cause 1 : un clic sur une opération d'écran répond au toast puis travaille dans une
  tâche détachée (`tokio::spawn(perform(…))`, issue #73) ; le harnais concluait l'étape
  après cinq pas de 20 ms sans nouvel envoi, sans savoir qu'une tâche courait. Pour
  « Vas-y », `go_plan` puis `launch` font une dizaine d'accès SQLite avant la première
  carte : sur un runner chargé, elle glissait dans l'étape suivante, `drive` photographiait
  les runs avant leur création, et tout se décalait jusqu'à la fin.
- Cause 2 : le relevé du monde triait les effets par `session_id`, un ULID tiré sur
  l'horloge murale : deux sessions nées dans la même milliseconde s'ordonnaient au hasard,
  et la numérotation `{{effect:N}}` avec elles.
- Correctif : `TelegramGateway` compte ses clics en vol (`clicks_idle`), compté avant le
  `spawn` et redescendu par une garde `Drop`, donc aussi quand la tâche panique ;
  `settle()` et `play` du harnais ne concluent plus tant qu'un clic court, avec un plafond
  de cinq secondes et une erreur qui le dit. Les effets sont triés par création de leur
  session (`sessions.created_at, rowid`), puis par appel comme avant.
- Attendus régénérés (`UPDATE_SCENARIOS=1`) : aucun fichier ne change. Sur l'horloge de
  test figée, `created_at` est le même pour toutes les sessions d'un rejeu et `rowid` suit
  leur création, qui est aussi l'ordre des ULID hors collision : le tri ne bouge que le cas
  qui faisait diverger deux rejeux.
- Tests : le clic lent et l'ordre des effets dans `penelope-evals` ; le compteur d'un clic
  réel et la garde sous panique dans la passerelle ;
  `two_replays_of_every_scenario_are_identical` cinq fois de suite avec la suite complète
  en parallèle.
- Ce qui continue de marcher : les clics traités en ligne (« Continuer », « Laisse
  filer », « Déjà traité ») ne passent pas par le compteur et gardent leur attente par le
  silence ; l'ordre des effets d'une même session est inchangé (`step_id, tool, rowid`) ;
  un effet sans session reste en tête, comme avant.
- Limite connue : les tâches lancées depuis `perform` lui-même (`upgrade.install`,
  `burst.ingest`) ne sont pas comptées ; aucun scénario ne les clique.

Closes #269.

### 1.0.24

**Idempotence : un appel rendu en texte par un modèle local recevait toujours `call_0`, et
le second appel identique d'une session était rejoué sans s'exécuter (#266).** Reproduit
par trois tests rouges avant le correctif : le corps capturé de Llama 3.2 relu deux fois
donnait deux fois `call_0` ; deux `fs_read` identiques d'une session sous le même
identifiant laissaient le compteur d'exécutions à 1 ; et un `shell_exec` approuvé une fois
couvrait tout `shell_exec` ultérieur du même identifiant, qui partait sans carte (la
décision antérieure est retrouvée par `(session, call_id)`).

- Cause : la relecture d'un appel rendu en texte (#259) posait l'identifiant constant
  `call_0`, et un appel natif sans `id` recevait `call_<indice>` ; or `step_id` entre dans
  la clé d'idempotence du ledger et dans la recherche d'une décision déjà prise. C'est le
  défaut que la décision 0009 avait écarté en retirant l'émulation d'outils.
- Source : l'identifiant d'un appel que le serveur ne nomme pas est dérivé de
  l'identifiant de la réponse et de l'indice de l'appel (`call_<id de réponse>_<indice>`),
  jamais d'une constante ni de l'horloge ; l'appel est ensuite persisté au transcript,
  une reprise le relit tel quel. Un appel nommé par le serveur (Qwen3) garde son
  identifiant.
- Garde-fou dans la boucle, quel que soit le fournisseur : un appel en attente dont
  l'identifiant a déjà servi plus tôt dans la queue de la session prend, pour la carte
  d'approbation, le ledger et le jeu de décisions, une identité ancrée au message qui le
  porte (`<id>@<seq>` ; l'indice du message pour un transcript en mémoire), la même d'une
  reprise à l'autre. Le transcript garde l'identifiant émis. L'écart est journalisé
  (`tool.call_id_reused`, `docs/runtime-events.md`).
- Ce qui continue de marcher : le rejeu d'un effet **terminé** du même appel (ca_17_2,
  et `completed_effects_are_replayed_not_reexecuted` réécrit sur ce cas : effet complet
  dans une vie antérieure, résultat pas encore au transcript) ; l'effet incertain qui
  attend une décision (ca_17_3) ; la reprise après approbation, qui retrouve l'appel par
  son identifiant.
- Scénario `rpc-inference-locale` : `main` repointé vers Llama 3.2, deux `time_now`
  identiques sous `call_0` à dix minutes d'écart ; le second rend l'heure avancée, le
  ledger porte deux effets.
- Limite connue : la queue relue fait 64 entrées ; un fournisseur qui réutiliserait un
  identifiant à plus de 64 entrées d'écart, avec les mêmes arguments, retomberait sur le
  rejeu. Aucun fournisseur du dépôt ne le fait après ce lot.

Closes #266.

### 1.0.23

Documentation seulement, remise à l'état de `main` à la 1.0.22. La 1.0.7 avait rattrapé la
prose à la 1.0.3 ; les quinze lots suivants (cache de prompt en fin de prompt, signaux
d'usage de la mémoire, trace des outils, plans en phases, livraison et gate de production,
carte de l'environnement, inférence locale) n'avaient pas été reportés, et le README disait
encore l'inverse de ce que le code fait sur trois points (préfixe qui « attend » un cache
froid sans rien dire, usage mesuré qui « ne promeut rien seul », suites réseau « jamais
lancées »).

- `README.md` : une section « Comment un message est traité » avec le parcours en ASCII,
  vérifié case par case contre les crates ; les sous-sections complétées avec ce que la
  1.0.x a livré, une sous-section « Un plan, pas un formulaire », l'inférence locale sur Mac
  en tête des portes vers les modèles, « Ce qu'elle ne fait pas » corrigée (Linux non
  prévu, suites réseau datées, gate qui ne déploie pas). Les tableaux comparatifs quittent
  le README pour `docs/comparaison.md`, présentés comme un instantané du 18/09/2026 non
  revérifié depuis.
- `docs/architecture.md` : refait à la 1.0.22 (lignes par crate, 221 679 en tout, 75
  scénarios, daemon 14 730/14 800, cinq fichiers au-dessus de 800 lignes, 46 traits
  publics), `penelope-cli` → `penelope-hitl`, port `FallbackProviders`, `ImageShrinker`,
  rôles complétés, `prompt.updated` et `skill.loaded` dans le journal.
- `docs/README.md` : index des commandes CLI (`penelope --help` compilé), ancres vers
  l'inférence locale, Codex, les plans, la livraison, le gate, le juge, le jeu de
  décisions, la carte de l'environnement, la trace des outils, `comparaison.md`.
- `docs/runtime-events.md` : `prompt.updated`, `skill.loaded`, `memory.outcome`,
  `memory.contested*`, `workflow.delivery` avec leurs champs lus dans le code ; le repli
  joué par la boucle ne laisse pas de `llm.fallback_used`.
- `docs/install-headless.md`, `docs/telegram.md`, `docs/workflows.md`, `docs/mcp.md`,
  `docs/context.md`, décision 0008 (complétée par #236, l'histoire gardée), `CLAUDE.md`
  (suites réseau à lancer à la main), `.github/workflows/README.md` (version de la branche,
  cliquet) : chaque constat revérifié dans le code avant d'écrire.
- Vérification : `cargo test -p penelope-evals --test docs` (liens, ancres, sections
  « limites », tables générées).

### 1.0.22

**Inférence locale sur Mac : un tour réel prouvé contre `mlx_lm.server`, et le repli sur
OpenRouter qui ne partait jamais (#259).** Aucun test ne faisait de tour contre un serveur
local. Les corps réels de `mlx_lm.server` 0.31.3 (capturés sur un MacBook Air M2) ont
montré trois défauts, et la suite locale un quatrième.

- Repli : la boucle envoyait le modèle de repli au fournisseur du modèle principal. Un
  alias `local:` qui se repliait sur `openrouter:` rappelait le serveur local arrêté avec
  le nom du modèle OpenRouter, quatre fois, et le tour finissait en erreur. Chaque repli
  part désormais chez le fournisseur de son modèle (`AgentLoop::with_providers`).
- Appels d'outils rendus en texte : Llama 3.2 sous `mlx_lm.server` rend
  `<|python_tag|>{"name": …, "parameters": {…}}` ou l'objet nu, et finit en `stop` ; la
  boucle l'affichait comme une réponse. Le fournisseur OpenAI-compatible le relit en appel
  quand le message entier est cet objet et nomme un outil proposé ; le début d'une
  réponse qui peut en être un est retenu jusqu'à la fin du flux, le reste part en direct.
  Qwen3 rend de vrais `tool_calls`, lus tels quels.
- Erreurs `{"error": "…"}` (chaîne nue) : le message devenait « erreur du provider ». Il
  est gardé, au statut HTTP comme au milieu du flux.
- `providers.extra` était déclaré sans être branché : un endpoint actif y sert les modèles
  de sa liste `models`, pour faire tourner le texte (mlx_lm) à côté de la voix
  (mlx-audio). La cohérence de configuration en tient compte et nomme maintenant un alias
  `local:` sans endpoint, comme `openai_compat:`.
- `doctor` : contrôle `local.<endpoint>` pour chaque endpoint qui sert un alias de texte
  (joignable, modèles servis, fenêtre, part de l'entrée relue du cache sur sept jours),
  avec la commande qui relance le serveur ; rien pour une instance sans endpoint local.
- `penelope local install|status|uninstall` : `mlx_lm.server` en LaunchAgent
  (`com.penelope.inference.<endpoint>`, relancé par launchd), sur l'adresse de
  l'endpoint, bouclage seulement, `--max-tokens 16384` (le défaut du serveur, 512, coupe
  les réponses). Vérifié sur la machine : processus tué, relancé par launchd.
- Documentation : section « Inférence locale sur Mac » d'`install-headless.md`
  (installation, modèle selon la mémoire, rôles, repli, supervision, chiffres mesurés :
  premier jeton de 8,9 à 13,6 s à froid pour 4 420 jetons, 0,86 s au second tour avec
  4 576 jetons relus du cache ; coût 0 $).
- Tests : corps capturés rejoués (texte et raisonnement, appels natifs simple et double,
  appels en texte avec ou sans outil proposé, erreurs, catalogue, sonde), repli de bout
  en bout contre un faux OpenRouter (sans serveur, dans la CI), `doctor` (sans endpoint,
  voix seule, serveur arrêté, sans modèle, modèle absent, cache mesuré, endpoint
  supplémentaire), cohérence, routage, plist, arguments du serveur ; scénario
  `rpc-inference-locale` ; suite `live-local` (tour, outil exécuté, deux tours avec le
  premier jeton, repli) verte contre `mlx_lm.server` et Qwen3-1.7B.

**Inférence locale : un modèle local homonyme d'un modèle d'OpenRouter en prenait
l'entrée, et le prix (#259).** Le catalogue rangeait tous les modèles sous leur
identifiant nu : le catalogue d'un serveur local qui sert `qwen/qwen3-8b` (LM Studio)
écrasait l'entrée d'OpenRouter, et le coût d'un appel se lisait sous le nom rendu par le
serveur, au prix d'OpenRouter quand il coïncidait.

- Les modèles d'un endpoint local sont rangés à part dans le catalogue. Un identifiant
  `local:` ou `openai_compat:` ne lit que ceux-là ; `openrouter:` ou `codex:` jamais ; un
  identifiant nu d'abord OpenRouter, puis le modèle local. Le rafraîchissement
  d'OpenRouter garde les modèles locaux. Le coût d'un appel se lit chez le fournisseur
  qui l'a servi.
- Les lectures du catalogue (fenêtre de compaction, prix du résumeur, outils d'un alias,
  `self_status`, rêve, vault) passent l'identifiant complet au lieu de le dénuder : un
  alias local y lit sa propre fenêtre.
- Replis : la liste `models` confiée à OpenRouter ne contient que des modèles
  d'OpenRouter, dès le plan de relance (le corps les filtrait déjà) ; un repli local
  derrière un modèle d'OpenRouter est joué par la boucle après l'échec.
- Documentation : le tableau mémoire et taille de modèle est dit estimation, pas mesure.
- Tests : collision au catalogue dans les deux sens, coût nul d'un appel local homonyme
  (corps capturé rejoué), plan de relance, repli de bout en bout d'OpenRouter en panne
  vers un faux serveur local.

Closes #259.

### 1.0.21

**Machine : Pénélope ne voyait que dix-sept binaires connus (#260).** Un outil posé par
`npm -g`, `uv tool` ou `cargo install`, Safari 27 et son serveur MCP natif, le pont MCP
de Xcode, un serveur d'inférence local, la puce et la mémoire unifiée : rien de cela
n'était dans ce qu'elle savait de sa machine. L'inventaire de #156 cherchait une liste
fermée (`KNOWN`) ; le reste se découvrait en tâtonnant par `shell_exec`.

- Carte de l'environnement (`penelope-app/src/environment.rs`, clé `machine.environment`),
  au même rythme que l'inventaire (démarrage, chaque heure, `doctor`), jamais dans un
  tour : matériel (`system_profiler`), tous les exécutables du PATH avec leur source et
  leur version (liens `Cellar`/`node_modules`/`uv/tools`, listes de `brew`, `npm`, `uv`,
  `pipx`, `cargo`), applications et version de leur `Info.plist`, MCP exposés
  (`safaridriver --mcp`, `xcrun mcpbridge`) ou déclarés dans `mcp.d`, serveurs
  d'inférence sur la boucle locale (`GET /models`) et leurs modèles. Les accès propres à
  macOS vivent dans `penelope-platform` (`discover`), stub explicite ailleurs.
- Ligne T1 : puce et mémoire, applications qui exposent un MCP, moteurs d'inférence
  installés ou déclarés, renvoi à `env_explore` ; ni version, ni modèle servi, ni état
  d'un serveur.
- Outil `env_explore` (lecture seule, à la demande) plutôt qu'une section de
  `self_status` : il cherche par besoin (« navigateur », « compilateur Swift », « MCP »,
  « inférence locale ») là où `self_status` décrit un état ; sans argument, un sommaire.
- Une capacité non branchée est proposée une fois au propriétaire (`machine.proposed`) ;
  rien n'est écrit dans `mcp.d` ni dans `[providers]`.
- `doctor` : `machine.environment` (la carte en une ligne) et `machine.changes` (outils,
  applications, capacités apparus, disparus, mis à jour depuis la passe précédente).
- Scénarios : `[kv]` sème un état de fond ; `outils-environnement` cherche un navigateur
  et une inférence locale sur une carte semée.
- Tests : faux PATH et faux `brew list`/`cargo install --list` (outil hors `KNOWN` avec
  sa source), application factice avec `Info.plist`, faux `xcrun`/`safaridriver`, faux
  serveur `/v1/models`, ligne T1 identique après une mise à jour d'outil, changements vus
  par `doctor`, recherche par besoin, proposition unique.

Closes #260.

### 1.0.20

**Workflows : gate humain avant la PR vers la production (#193, T5 de #185).** Depuis
#192, un plan livré s'arrêtait après l'E2E de dev : rien ne présentait le bilan au
propriétaire ni ne proposait la PR dev → prod, et la borne de durée d'un run (`maxWallMs`,
deux heures par défaut) comptait l'attente d'une carte : une approbation donnée le
lendemain aurait trouvé le run bloqué.

- Après `livraison-e2e`, trois étapes : `livraison-bilan` (le bilan vérifié : plan et run
  exacts, PR dev, commit, CI et E2E de ce commit, preuves, heure, empreinte), `gate-prod`
  (« Proposer la PR prod », « Re-vérifier », « Refuser »), `livraison-prod`. Aucune n'a
  de variante « laisse filer ». `branches.prod` est demandée avant toute carte.
- Le clic n'est pas cru sur parole : un plan révisé depuis arrête le run ; un bilan plus
  vieux que `prod.max_age_minutes`, une PR dev avancée d'un commit ou une CI qui n'est plus
  verte le rendent périmé, le run refait PR, CI et E2E et pose une carte neuve, l'ancienne
  ne vaut plus rien. La PR dev doit être fusionnée ; fusionnée après le bilan (ou sur un
  autre commit), elle rend le bilan périmé : CI du commit de fusion puis E2E de dev refaits
  avant toute carte ou PR prod. Un refus bloque le run pour de bon.
- Au plus une PR prod par run : effet au ledger sous une clé qui ne dépend que du run,
  marque du run dans la description, recherche sur le forgeur avant l'ouverture ; un envoi
  interrompu par un redémarrage est terminé sans rejuger le bilan. Lien dans le sujet ; ni
  fusion ni déploiement, qui suivent la politique du projet.
- L'attente d'une étape `user` ne compte plus dans la borne de durée d'un run : elle mesure
  le travail, pas le temps d'une décision.
- Tests : logique pure du gate (chemin, branche de prod, fraîcheur, bilan), orchestrateur
  contre les faux forgeurs GitHub et GitLab (tout vert sans clic et redémarrage : zéro PR ;
  approbation puis redémarrage : une seule PR, lien dans le sujet ; PR ouverte avant un
  arrêt retrouvée ; bilan vieilli, commit avancé, CI rouge, PR dev fusionnée après le
  bilan (re-vérifiée sur le commit de fusion), E2E rouge après fusion : aucune PR ;
  refus durable ; nouvelle révision ; PR dev non fusionnée), borne de durée du pilote,
  scénario `livraison-prod` (clic, redémarrage, une MR prod, second clic refusé) et
  `livraison-dev` qui finit sur la carte, sans MR prod.

Avec #186, #191 et #192, l'épopée #185 est livrée : plan révisable et gate « vas-y »,
phases à contexte neuf avec checkpoints et revue bornée, PR dev automatique, CI et E2E
externes sur GitHub et GitLab, reprise sans doublon, et gate humain avant la production.

Closes #193, closes #185.

### 1.0.19

**Mémoire : une note écrite par un chemin du vault atterrissait dans le workspace, sans
alerte (#256).** Depuis le 22/09, les fiches de la skill YouTube (`sources/<slug>.md`,
`attachments/<id>.vtt`) partaient dans `workspace/` : ni indexées ni versionnées.
`fs_write` résout un chemin relatif contre le premier workspace, et rien ne remarquait
une note de forme vault hors du vault.

- Préfixe `vault:` pour les outils `fs_*` (`vault:sources/x.md`), cité dans leur
  description : il ouvre le vault même absent de `sandbox.workspaces` (c'est le cas de
  l'instance), et n'ouvre que lui : ni `..`, ni lien symbolique qui en sort, ni pour un
  sous-agent à racines restreintes. Un chemin absolu vers le vault, sans préfixe, suit
  la règle des workspaces. Toute écriture `vault:` (`fs_write`, `fs_edit`) passe le
  filtre des secrets du vault. Sans vault ouvert, le préfixe est refusé plutôt que pris
  pour un nom de dossier. L'index suit au prochain `penelope mem reindex`.
- `fs_write` d'une note de forme vault (dossier `sources/`, `attachments/`, `concepts/`,
  `entites/`, ou `type` propre au vault) hors du vault : l'écriture est faite, le
  résultat porte un `avertissement` avec le chemin `vault:…`. Un brouillon passe sans mot.
- `doctor` : contrôle `vault.dead_zone`, qui compte ces notes dans le premier workspace
  et donne la commande `rsync` puis `penelope mem reindex`.
- La skill livrée `wiki-markdown` (1.1.0) emploie le préfixe ; le préambule de
  `skill_load` le rappelle à une skill qui cite des chemins du vault sans lui.
- Tests : résolution (vault hors workspaces, chemin absolu sans préfixe, `vault:../`,
  lien sortant, sans vault), prédicat de forme, exécuteur (écriture, secret refusé en
  écriture et en édition, avertissement, brouillon, relecture, sous-agent restreint),
  `doctor`, préambule ; scénario `outils-vault-prefixe` sans le vault dans les
  workspaces. Les surfaces des scénarios suivent les descriptions des outils `fs_*`.

Closes #256.

### 1.0.18

**Masquage : un mot ordinaire lu après `key=` n'est plus masqué partout ; deux tests
instables rendus déterministes (#258).** Le rédacteur apprenait (#134) toute valeur
repérée après `key=`, `token =`, `password:`, `Bearer` ou `Basic`, puis la masquait
partout pour tout le processus : key=`project` lu une fois, et « project » devenait
« [secret masqué] » dans une consigne (#192) ; de même `os.environ[`, `self.token`,
« authentication ». Ses valeurs vivaient dans deux `static` partagés par tous les tests :
`short_values_are_not_registered` a échoué en CI le 29/09.

- Seule une valeur à forme de jeton (alphabet de jeton, lettres et chiffres mêlés, ou
  jeton aléatoire) est apprise d'une affectation ou d'un en-tête d'autorisation ; les
  motifs de fournisseur le restent tels quels. L'accent grave délimite la valeur comme un
  guillemet. L'affectation reste masquée sur place.
- L'état du rédacteur devient un type `Redactor` ; les fonctions libres délèguent à celui
  du processus, et les tests de `penelope-observe` travaillent chacun sur un neuf.
- `poisoned_or_changed_tools_are_flagged_and_lose_their_rules` attendait la règle
  révoquée puis lisait l'avis, posé après l'écriture d'un événement : il attend l'avis,
  dernier effet. Le test du délai d'étape (#56) exigeait que le modèle soit appelé avant
  l'échéance : il vérifie un seul passage dans la trace, et retrouve ses 200 ms.
- Tests : mots ordinaires non appris (sept cas) et jetons toujours appris, deux
  rédacteurs isolés ; assertions de #134, #207 et CA 13 inchangées. Chaque test
  concerné passe 20 fois de suite dans la suite de son crate.

Closes #258.

### 1.0.17

**Workflows : un plan approuvé est livré en dev, PR, CI puis E2E externe (#192, T4 de
#185).** Depuis #191, un plan qui écrit du code s'arrêtait après sa dernière revue : la
PR, la CI et la vérification de l'environnement de dev restaient à faire à la main.

- Le `passed` du dernier juge d'un plan qui écrit du code mène à trois étapes `delivery`
  (nouveau type, validé et publié dans le schéma) : `livraison-pr`, `livraison-ci`,
  `livraison-e2e`. Chacune est suivie d'une carte « livraison bloquée » (Réessayer,
  Arrêter) ; aucune n'a de variante « laisse filer ». Un plan sans code ou sans juge
  final n'ouvre pas de PR.
- Forgeur, branche de dev, CI et URL de dev viennent de `.penelope/delivery.toml` du
  dépôt, complété par le dépôt lui-même (remote github.com ou gitlab.com, fichiers de
  CI). Rien n'est supposé, ni GitHub ni `main` : ce qui manque est demandé sur la carte,
  clé par clé, avec le fichier où l'écrire ; un jeton absent, avec la commande qui le pose.
- Une seule PR par run : push et ouverture passent par le ledger, planifiés avant
  l'appel, et la PR est cherchée sur le forgeur avant d'être ouverte ; après un arrêt
  brutal, elle est retrouvée. CI (lue sur le commit poussé, à intervalle croissant,
  bornée ; rouge, sans verdict ou indisponible) et E2E (HTTP, GraphQL, ou l'outil du
  projet ; preuves dans la sortie de l'étape et `livraison/e2e-N.json`) sont deux
  résultats distincts, chacun dit dans le sujet.
- Tests : logique pure (configuration, découverte, verdicts CI et E2E, chemin compilé),
  orchestrateur contre de faux forgeurs GitHub et GitLab (PR unique, MR retrouvée après
  un arrêt pendant son ouverture, CI en attente, verte, rouge, indisponible, sans verdict,
  E2E vert et rouge, configuration et jeton absents, travail non commité), scénario
  `livraison-dev` (Telegram, redémarrage pendant la CI, un seul POST), `plan-en-phases`
  qui finit sur la carte de livraison. Le harnais sème des dépôts git (`[[repos]]`) et
  sert des corps successifs (`bodies`).

Closes #192.

### 1.0.16

**Workflows : le plan approuvé s'exécute en phases durables (#191, T3 de #185).** Depuis
#186, « Vas-y » ne faisait que marquer le plan prêt : rien ne l'exécutait, et le bouton
ne portait que le numéro de version, qu'un plan suivant de la même conversation pouvait
reprendre.

- Le plan approuvé est compilé en workflow du moteur de runs : un pas, une étape `agent`
  en `context: fresh` (session neuve par visite, sans l'historique du propriétaire), le
  modèle choisi par phase ; un plan de deux pas sans juge reste à un seul agent. Carte
  d'OK après la spécification, les tests et le code (« Continuer », « Laisse filer »,
  « Arrêter ») ; revue et vérification renvoient au code au plus deux fois, puis arrêtent
  le run avec sa raison. Le chemin est déplié à la compilation : l'étape courante dit où
  en est le run.
- « Vas-y (vN) » porte l'empreinte de la révision : un clic périmé est refusé. Le run
  (`r_plan_…`) dérive de la session et de l'empreinte, sa ligne n'est insérée qu'une fois
  (`RunStore::create_as`) : ni double clic ni reprise ne lancent un second run.
  `wf.plan.show` et `wf.plan.go` en donnent l'équivalent par la socket. `/stop` met en
  pause les runs des plans de la conversation.
- Une étape `agent` journalise son préfixe comme un tour de chat : ce que le modèle lit
  se replie du journal (épopée #208), et la reprise d'une visite après redémarrage ne
  rejoue pas sa consigne.
- Tests : compilation pure (gates, laisse filer, bornes, mono-agent, empreinte),
  orchestrateur (clic périmé, un seul run, contexte neuf, redémarrage pendant un agent et
  entre deux phases, refus), passerelle (`Vas-y`, `/stop`), scénarios `plan-en-phases`
  (Telegram) et `rpc-plans` (RPC), avec l'étape `drive` du harnais.

Closes #191.

### 1.0.15

`penelope secret set` en SSH échouait sur l'instance réelle : « écriture dans le Trousseau
refusée (security, code 36) ». La commande écrivait toujours elle-même dans le Trousseau,
verrouillé pour une session SSH, alors que le daemon lancé par launchd y a accès et sert
déjà `secret.set`. Désormais la valeur, lue à l'invite comme avant, est confiée au daemon
par la socket locale quand il répond ; sans daemon, la commande écrit elle-même, et un
Trousseau verrouillé (erreur typée `SecretLocked` côté plateforme) donne un message qui dit
quoi faire : `penelope start`, ou `security unlock-keychain`. `secret rm` passait déjà par
le daemon. Tests : faux daemon qui reçoit `secret.set`, écriture directe sans daemon, message
d'aide en code 36, faux `security` en code 36 ; la valeur n'apparaît dans aucune sortie.
Closes #252.

### 1.0.14

**Telegram : une trace lisible des outils d'un tour (#222).** Pendant un tour, rien ne
restait dans la conversation de ce que Pénélope exécutait : la ligne d'état du brouillon
(#121) était remplacée à l'appel suivant, n'existait qu'en privé, et cinq appels
identiques passaient sans qu'on le sache.

- Une bulle par tour, posée au premier appel d'outil et modifiée en place :
  `💻 shell_exec · <code>echo test</code> (×4) ✅`. Une icône par famille (table
  exhaustive, un test parcourt le catalogue), l'argument principal (commande, chemin,
  adresse, requête ; jamais un texte libre), `tool_call` déballé, un outil MCP en
  `serveur · outil`. Les appels consécutifs identiques se groupent ; les résultats vont au
  plus ancien appel ouvert du même nom (un lot parallèle garde son ordre) ; `❌ 2/4` si
  c'est mixte, `🚫` pour un appel refusé avant exécution, `⏹` pour un appel sans résultat
  à la fin du tour.
- Le cœur n'a pas bougé : il émet déjà `ToolCall` et `ToolResult` sur le bus ; la boucle
  `telegram.trace` de la passerelle les rend (frontière #214).
- Création par `tg_outbox`, rédigée. La bulle part avant la réponse **par construction** :
  `deliver` demande à la boucle de lire tout ce que le bus porte déjà (les appels du tour
  y sont publiés avant l'issue) et n'enfile la réponse qu'après l'acquittement. Modifications par
  appel direct, sur un seau à part et en un seul essai (`Bot::edit_trace`) : un 429 ne
  retarde plus la réponse, un `not modified` n'est plus une note d'échec. Une modification
  en vol au plus, toutes les 1,5 s en privé et 3 s en groupe ; la dernière attend que la
  file du chat soit vide (#70).
- `telegram.tool_trace` : `off`, `compact` (défaut : l'argument en privé, les familles
  seules en groupe), `full` (plus un extrait du résultat en privé). Relu à chaque tour.
  Arguments et extraits caviardés (l'extrait ne l'était pas à la source). Au-delà de
  3 800 caractères, « … et N appel(s) de plus » puis l'appel courant.
- Session en arrière-plan : pas de bulle, et une bulle se fige si la session quitte le
  premier plan (#10). Bulle ouverte inscrite sous `tg.trace.open` : un redémarrage en plein
  tour la clôt au démarrage suivant (« interrompu par un redémarrage »).
- Tests : `trace::tests` (familles, argument principal, regroupement, appariement, statuts,
  modes et groupes, échappement, secrets, troncature), `tests::trace` (bulle unique avant
  la réponse et cadence, réponse prête avant que la boucle ait lu l'appel, sans outil, `off` et rechargement à chaud, sujet de groupe,
  arrière-plan, bulle orpheline), `a_trace_edit_is_tried_once_on_its_own_bucket`, lecture
  de la clé ; scénario `trace-des-outils` (le harnais lance la boucle), attendu de
  `commandes-approbations` régénéré pour sa bulle.

Closes #222.

### 1.0.13

**Juge d'approbation : un jeu de décisions local, opt-in et exportable (#233).** Les
traces du juge ne permettaient pas d'évaluer plus tard un autre juge : `approval.judged`
ne porte que l'empreinte de la ligne (voulu, #162), la rétention vide le payload des cartes
à `retention.days` (90) et une ligne passée sans carte ne vit que dans les arguments de son
effet, vidés au même terme.

- `observability.dataset.approvals` (faux par défaut) : chaque ligne `shell_exec` vue par
  la politique, carte ou pas, laisse un échantillon dans `approval_samples` (migration
  `0024_approval_samples`) : la ligne telle que le juge la reçoit (`hostile_text`, la même
  fonction), répertoire de travail, workspaces, `network` ; décision, couche, classe, règle
  et `sans_motif` ; sortie complète du juge ou son échec ; issue (`auto` et ce qui l'a
  permise, `denied` par la politique, puis `approved`, `denied`, `expired` dans la
  transaction qui tranche la carte), canal, délai ; code de sortie et durée de l'exécution.
  Une ligne est complétée, jamais réécrite ; `command_sha` est celui de `approval.judged`.
- Rétention propre, `observability.dataset.retention_days` (365, `0` : rien), que
  `retention.days` ne touche pas ; `session.purge` supprime les échantillons de la session.
  Rien n'entre dans le flux runtime, dans `approval.judged` ni dans une carte.
- `penelope dataset export --kind approvals [--since AAAA-MM-JJ] --out f.jsonl` : lecture
  seule (`SQLITE_OPEN_READ_ONLY`, `query_only`), une ligne `"v": 1` par échantillon,
  rédacteur du jour repassé, fichier en `0600`. `penelope doctor` donne l'état de la
  collecte, le volume, le plus ancien échantillon et les issues (`approval_dataset`).
- Tests : `samples::tests` (hitl : décision, expiration, jamais réécrit, export masqué),
  `tests::samples` (agent : option désactivée, chaque chemin de décision, `auto_read`,
  échec du juge, `rm -rf ~; echo ok # APPROVE`, formes de secrets de #134, empreinte),
  `purge::tests::samples` (purge d'une session, rétention propre), `dataset::tests` (JSONL,
  `0600`, base intacte, `--since`), `doctor_says_what_the_decision_dataset_holds` ;
  scénarios `purge` et `rpc-sessions` régénérés (`approval_samples` dans le rapport).

Closes #233.

### 1.0.12

**Cache de prompt : la différence part en fin (#236).** Un banc de stabilité du cache
(dix harnais, 27/09) montre que le meilleur ajoute la différence en fin quand le fichier
d'instructions change. Chez Pénélope, un AGENTS.md, un index de skills ou un serveur MCP
modifié pendant un cache chaud était caché au modèle (`stable_prefix` renvoyait l'ancien
préfixe) jusqu'à la pause suivante, puis le prompt système entier repartait ; une skill
chargée puis modifiée restait périmée dans l'historique, sans avis.

- Le préfixe retenu part toujours inchangé ; le message qui suit le changement porte, dans
  son contexte volatil, un bloc `<mise-a-jour>` : lignes retirées et ajoutées de chaque
  tuile, la tuile dite réécrite au-delà de 1 500 caractères, les skills chargées dont le
  corps a changé (`skill_load` journalise `skill.loaded`). Une seule fois : la différence
  est journalisée (`prompt.updated`) quand elle part avec son message, et la suivante ne
  porte que ce qui est nouveau depuis.
- Le préfixe et le volatil d'un tour quittent le daemon pour
  `penelope_conversation::prefix` (`held_prefix`, `settle`).
- Tests : `tiers::update` (différence, seuil, lignes vides), `prefix::tests` (envoyée une
  fois, incrémentale, attend un message, skill modifiée), scénario `prefixe-mise-a-jour`
  (trois appels, un seul `system_hash`, un seul `tools_hash`) ; `outils-skills` régénéré
  pour `skill.loaded`.

**Cache de prompt : la liste d'outils ne bouge qu'à une frontière (#236).** Un outil natif
décrit ou appelé entrait dans la liste au tour suivant et en sortait après dix tours sans
usage ; chez Anthropic les outils précèdent le prompt système, chaque entrée ou sortie
cassait tout le cache. `apply_promotions` (outils MCP marqués, « à la frontière de
compaction ») n'était appelée que par des tests.

- La liste d'un tour de conversation se calcule à une frontière (premier tour, pause plus
  longue que le cache, compaction : la même que le préfixe), puis est resservie telle
  quelle (`penelope_app::frozen_tools`, `tools_on_demand::turn_tools`). Entre deux
  frontières, `tool_call` atteint l'outil nouvellement décrit ; la tuile T1 le dit.
- La publication d'un résumé applique les promotions MCP ; les outils promus des serveurs
  actifs sans `eager_schemas` rejoignent la liste de chaque session à sa frontière suivante
  (`McpGateway::promoted_tools`, même plafond d'octets que les schémas `eager`).
- Tests : `the_tool_list_only_moves_at_a_boundary`,
  `promoted_tools_follow_the_compaction_boundary`, promotion vérifiée dans
  `manual_compaction_replaces_old_turns_with_a_summary`,
  `a_turn_offers_the_core_then_what_the_session_discovered` (gelée à cache chaud, offerte
  après la pause) ; scénario `outils-geles` (`tool_describe` puis `tool_call` sur trois tours
  chauds : un seul `tools_hash`, le second après la pause). Les scénarios dont un outil
  entrait au tour suivant (`outils-planification`, `outils-memoire`, `outils-soi`…) sont
  régénérés : l'outil n'entre plus à cache chaud ; `rpc-sessions` ne relève plus la clé
  gelée.

**Cache de prompt : le résumé relit le préfixe de la conversation (#236).** L'appel de
résumé avait son propre système, aucun outil, la conversation rendue en un message, souvent
un autre modèle : tout était relu au prix fort (rôle `compaction` : 110 appels, 5 % de
cache depuis le 13/09).

- Nouvelle clé `context.compaction_on_prefix` (`auto`, `always`, `never`). Sur le préfixe,
  la demande reprend la dernière requête de la conversation (modèle du dernier appel,
  préfixe retenu, liste gelée, historique projeté, même `tool_choice`) et ajoute la
  consigne de résumé en dernier message, portée repérée par le début du dernier message
  du lot. Pas de tentative sans préfixe chaud ni liste gelée, sur un modèle de l'abonnement
  ChatGPT, sur un dépassement prouvé, si la projection ne tient pas, ni au-delà du premier
  lot ; un échec (sortie invalide, appel d'outil) passe la main au résumeur, et le bilan le
  dit. L'usage du résumé garde `system_hash` et `tools_hash`.
- Défaut `auto` : le modèle de conversation coûte plus cher par jeton que celui de
  `compaction`, mais relu au prix du cache ; `auto` compare les deux estimations aux prix du
  catalogue (préfixe au prix du cache, lot du résumeur au prix plein, 4 000 tokens de sortie
  des deux côtés) et garde le résumeur quand un prix manque. Avec un grand modèle cher en
  sortie, le résumeur reste choisi ; avec `main` = `deepseek-v4-pro` et un cache chaud, le
  préfixe l'emporte dès que la conversation est longue. `always` sert au banc et aux
  scénarios.
- Tests : `a_summary_reads_the_conversation_prefix` (préfixe identique, empreintes égales),
  `a_failed_prefix_summary_falls_back_to_the_summarizer`,
  `auto_without_prices_keeps_the_summarizer`, `auto_picks_the_cheaper_call` ; scénario
  `compaction-sur-le-prefixe` (un seul `system_hash` et `tools_hash` pour la conversation et
  le résumé).
- Reste à mesurer sur l'instance : `cacheRatio` du rôle `compaction` avant et après
  (`penelope usage --by role`).

Closes #236.

### 1.0.11

**Mémoire : « Retiens que… » reste la parole du propriétaire sans citation mot pour mot
(#245).** La suite réseau `mem_longitudinal` perdait « réunions du mardi » 3 fois sur 3 :
le propriétaire écrit « Retiens que nos réunions d'équipe ont lieu le mardi matin à 9 h »,
Pénélope répond « C'est retenu », puis le rêve écarte le fait comme « ni dit ni confirmé
par le propriétaire ». Cause, lue dans les preuves de la passe : l'appel `mem_note` du
jour 9 n'a pas de `citation` (texte reformulé, fuseau ajouté), et la relecture du tour le
type `fait` ; les deux candidats partent d'origine `agent`, et le tri ne voit que cette
origine.

- `penelope_memory::owner_quote` : recouvrement déterministe entre un candidat et une
  phrase du propriétaire (mots pleins, accents et pluriels ramenés ; au moins 3 mots
  communs, 75 % de la phrase dans le candidat, 70 % du candidat venu de la phrase, même
  polarité).
- `mem_note` sans citation retrouvée, et la relecture d'un tour, marquent `owner` le
  candidat qui reprend une phrase du message du propriétaire **de ce tour** ; la phrase
  est gardée (`mem_candidates.owner_quote`, migration 0023, effacée par la purge). Jamais
  depuis un tour interne, un déclencheur, une relance ou un contenu transféré.
- Le tri reçoit la ligne « dit par le propriétaire : « … » », et un candidat qui la porte
  est endossé par construction : sans opération du modèle, son texte est écrit tel quel.
- `mem_longitudinal` garde aussi `appels.jsonl` (arguments et résultat de chaque appel)
  et `candidats.jsonl` (origine, phrase, sort).

Tests : `an_owner_fact_noted_without_citation_is_kept` (rouge avant le correctif),
`a_reviewed_fact_the_owner_dictated_stays_the_owners`, les cas de `owner_quote`, scénario
`outils-memoire-sans-citation`. Ce qui tenait tient : citation mot pour mot (#24),
déduction de l'agent écartée, contenu non fiable jamais promu.

Closes #245

### 1.0.10

- Constat (#242, suite du point 3 de #231) : une photo plus lourde que ce que le
  fournisseur accepte partait telle quelle, se faisait refuser (reprise de la 1.0.6) et
  n'était pas lue ; au-delà de 10 Mo, la passerelle la refusait même quand Telegram en
  proposait une taille plus légère.
- Cause : seule la dernière `PhotoSize` était gardée, avec son poids ; aucune limite par
  fournisseur n'existait, et rien ne réduisait une image avant l'envoi.
- Correctif : `Incoming::Photo` garde toutes les tailles ; la passerelle télécharge la
  plus grande sous 10 Mo (`largest_photo_within`) et ne refuse que si aucune ne tient.
  Le fournisseur n'étant connu qu'au tour (routage), c'est le tour qui applique sa
  limite (`image_limits` : 5 Mo en base64 et 8 000 px pour Anthropic, 20 Mo ailleurs)
  pour le modèle qui lira la photo, celui du tour ou celui qui la décrit. Au-delà, le
  backend plateforme réduit (`ImageShrinker` : `sips` sous macOS, erreur `Unsupported`
  ailleurs), en plusieurs essais au besoin, jamais sous 512 px de côté ; la réduction est
  journalisée (`media.image_reduced`, tailles avant et après). Échec : l'original part,
  comme en 1.0.6. La photo reçue reste intacte pour `image_inspect`.
- Tests : taille Telegram choisie sous la limite (unitaire et passerelle, photo de
  12 Mo sans refus), limites par fournisseur, réduction en deux essais et journalisée,
  image dans les limites laissée telle quelle, repli sur l'original sans outil, `sips`
  réel sous macOS, stub hors macOS. Inchangés : refus d'une photo sans taille sous
  10 Mo, album, description par le modèle de vision.

Closes #242.

### 1.0.9

**Gel : le cliquet garde les plafonds de couverture (#243).** Un plafond de
`[coverage.uncovered]` remonté à la main dans un lot passait la CI. Cause :
`ratchet.rs` attendait une table de planchers `[coverage.crates]` que `budget.toml` n'a
jamais eue, et ne comparait pas `[coverage.uncovered]` à sa base git.

- `[coverage.uncovered]` est une table de plafonds par crate, comme `[crates]` : une
  valeur qui monte ou une entrée retirée est refusée sans trailer `Dérogation-budget: #N` ;
  une valeur qui descend et une crate nouvelle (`coverage-check.sh --update`) passent. Le
  message nomme la table, les mêmes crates figurant dans `[crates]`.
- `coverage.crates` est retiré du cliquet.
- `scripts/coverage-check.sh` reste manuel (vingt minutes et plus) ; CLAUDE.md dit quand le
  lancer.
- Tests : `an_uncovered_ceiling_that_rises_is_refused`,
  `an_uncovered_ceiling_that_falls_or_a_new_crate_passes`,
  `a_removed_uncovered_ceiling_is_refused` ; les tests existants du cliquet passent
  inchangés, hormis le plancher `coverage.crates` retiré.

Closes #243

### 1.0.8

**Mémoire : une préférence du propriétaire ne se remplace plus sans lui (#224).** La
suite réseau `mem_longitudinal` perdait le tutoiement la nuit du jour 10 (5/7, 71 %).
Cause : seul un `add_entry` passait par le contrôle de contradiction ; un
`supersede_entry` ou un `replace_entry` proposé par le modèle réécrivait la ligne du profil
sans question, et l'heuristique ne voyait qu'une négation (« toujours » contre
« jamais »), jamais une valeur mise à la place d'une autre (tu contre vous).

- Le rêve pose la carte de contradiction (Remplacer, Exception, Ignorer) au lieu
  d'appliquer un `supersede_entry` ou un `replace_entry` qui vise le profil, ou une entrée
  écrite par le propriétaire quand c'est une règle qui la remplace. Passent sans question :
  la correction explicite du propriétaire (type `correction`, « non, … »), la précision qui
  garde l'ancien texte entier, la mise à jour d'un fait hors du profil.
- Le contrôle de contradiction voit la substitution : deux valeurs d'une même famille
  exclusive (tutoiement et vouvoiement, langue, devise, jour de la semaine) sur le même
  sujet. Les garde-fous de #145 tiennent : ni fait ni écart, borne et rapport de longueur,
  contexte distinct, similarité 0,80.
- « Remplacer » sur une carte écrit la nouvelle entrée au niveau de l'ancienne : une
  préférence remplacée partait dans `notes.md`, qui n'est pas injecté d'office.
- `mem_remember` refuse d'écrire au profil ou au cœur une entrée qui en contredit une
  autre, et nomme l'uid à trancher avec le propriétaire (scénario
  `outils-memoire-contradiction`) ; un niveau inconnu est refusé au lieu d'aller dans
  `notes.md` (le schéma le refusait déjà).
- `mem_longitudinal` : critères tutoiement et vouvoiement sous toutes leurs formes
  (« tutoyer » était compté absent) ; la question tu/vous se prouve par une carte ou la
  section « Questions sans réponse », plus par tout `DREAMS.md` (son tri recopiait le
  candidat du jour 1 : le critère passait à vide) ; le mardi, un fait, se cherche aussi
  dans `notes.md` et `entites/`. En échec, le dossier est gardé avec `preuves/`
  (réponses entières, digest, appels d'outils, événements de mémoire, cartes).
- Banc `mem-bench`, jeu `style-de-reponse` : « désormais vouvoie-moi » remplaçait le
  tutoiement en silence, c'était l'attendu ; le jeu dit maintenant ce que le propriétaire
  répond à la carte (`cartes`, « remplacer »), et le banc le joue par le chemin des
  boutons.

Tests : `a_substituted_value_is_a_contradiction`,
`a_substitution_needs_two_different_values_on_one_subject` (memory) ;
`a_preference_does_not_supersede_the_owner_profile_without_asking`,
`corrections_precisions_and_facts_still_rewrite_without_a_card`,
`an_entry_written_by_the_owner_is_protected_outside_the_profile`,
`an_added_substitution_of_the_owner_preference_asks` (dream) ;
`mem_remember_refuses_an_unknown_level_and_a_contradicted_profile` (executor). Rouges
avant le correctif. Sans clé de fournisseur ici, la suite réseau reste à lancer, trois
fois, par l'intégrateur :

```bash
OPENROUTER_API_KEY=… cargo test -p penelope-evals --test mem_longitudinal -- --ignored --nocapture
```

**Mémoire : les réponses du propriétaire deviennent des signaux d'usage (#230).**
`Signals::record_outcome` n'était appelé nulle part : succès et contradictions valaient 0
pour toute la mémoire, `contested_*` était « sans effet », et un écart devenait une
exception sur ses seules occurrences, sans preuve qu'il avait marché.

- Le message suivant du propriétaire juge chaque souvenir dont la réponse d'avant s'est
  servie : sans marqueur de correction, succès ; ouvert sur une correction (« non, »,
  « je t'ai dit… ») qui reprend un mot du souvenir, contradiction ; tout le reste, rien.
  Seuls les messages du propriétaire jugent (pas une relance, pas une reprise), un tour
  échoué entre les deux annule le jugement, et chaque signal est journalisé
  (`memory.outcome`).
- `contested_confidence` et `contested_min_observations` ont un effet : une entrée assez
  jugée (4 observations) de confiance `(succès + 1) / (succès + contradictions + 2)` sous
  0,5 n'est plus servie d'office (instantané, rappel automatique ; la recherche explicite
  la trouve), et la consolidation suivante la soumet une fois au propriétaire : « Tout »
  la retire, « Rien » la garde et remet ses signaux à zéro.
- Un écart ne devient une exception qu'après une réponse acceptée dans deux sessions
  distinctes (`memory.promotion.ecart_min_successes = 2`), à côté des occurrences. Sous
  ses seuils, il attend au lieu d'être rejeté : rejeté dès la première nuit, il ne
  pouvait jamais réunir ses deux jours distincts.

Tests : `the_next_owner_reply_judges_a_used_memory_conservatively`,
`a_contested_entry_is_no_longer_served_automatically`,
`an_ecart_needs_accepted_answers_in_distinct_sessions` (memory) ;
`the_owner_reply_records_a_success_or_a_contradiction`,
`no_signal_without_a_clear_owner_verdict` (vault) ;
`an_ecart_waits_for_accepted_answers_in_distinct_sessions`,
`a_contested_entry_is_submitted_once_then_retired_or_kept` (dream) ; scénario
`memoire-signaux-d-usage` (rappel, « parfait, merci » puis « non, … » : un succès, une
contradiction sur l'uid rappelé). La suite réseau ci-dessus vérifie aussi que
`mem_longitudinal` ne baisse pas.

Closes #224, closes #230.

### 1.0.7

La documentation en prose décrivait encore la 0.17 ou la branche `v1` : « 17 crates »
dans le README, une architecture photographiée à la 1.0.0-alpha.13, une commande « qui
n'existe que sur la branche `v1` », l'avancement pointé sur l'archive 0.17. Elle est
remise à l'état de `main` à la 1.0.3, chaque chiffre recalculé par la commande que le
document donne.

- `README.md` : 27 crates, « Comment c'est fait » redessiné sur les couches de la V1,
  version 1.0 publiée en release, scénarios rejouables dans « Tests ».
- `docs/architecture.md` : couches (évaluations au-dessus de la passerelle), lignes par
  crate, port `Judge`, frontière canal (35 fichiers, 248 mentions ; la passerelle admise
  aussi pour `penelope-evals`), valeurs du gel lues dans `budget.toml` (R3 vide, R7 à 0,
  R9 et R10 posées), « Ce qui reste » vérifié contre le code : six fichiers au-dessus de
  800 lignes, `[coverage.uncovered]` hors du cliquet de `check-budget.sh`.
- `docs/README.md` : l'avancement pointe vers la Version 1, l'archive 0.17 en lien
  secondaire ; la décision 0015 est close par la bascule (aussi dans son statut). 0013 et
  0014 disent ce qui est fait depuis leur rédaction.
- `docs/install-headless.md` (`approvals stats`), `CLAUDE.md` (R8, frontière canal),
  `.github/workflows/README.md`, le modèle de PR et le README des fixtures du store :
  exemples et affirmations à l'état de `main`.

### 1.0.6

Une photo refusée par le fournisseur (400 : image trop lourde, format refusé, illisible)
faisait échouer le tour, **et tous les suivants** : vérifié par un test rouge avant le
correctif, la photo reste dans l'historique projeté et chaque requête de la session la
renvoie, donc reprend le même refus. Cause : `from_status` classait ce 400 en
`BadRequest`, ni réessayable ni récupérable ; la seule reprise hors boucle était la
compaction sur `ContextLength`.

- `LlmErrorKind::AttachmentRejected`, reconnu sur des motifs précis relevés dans des corps
  d'erreur réels : types canoniques d'OpenRouter (`image_too_large`, `invalid_image`…) et
  corps amont dans `metadata.raw`, Anthropic (`image exceeds 5 MB maximum`, `Image does not
  match the provided media type`, `Could not process image`), OpenAI et le backend Codex
  (`invalid_image_format`, `image_parse_error`). Un test par forme ; un 400 sans rapport
  reste `BadRequest`, un 413 reste `ContextLength`. Mistral n'est joint que par OpenRouter :
  son refus arrive avec le type canonique.
- Reprise : la boucle remplace les images de la **copie envoyée** par « [image retirée :
  refusée par le fournisseur (motif)] » et relance une fois ; un second refus échoue en le
  disant. Toutes les images de la requête refusée sont retirées : le fournisseur ne dit
  pas toujours laquelle. L'historique, les `conv.*` et le préfixe (prompt système) sont
  intacts.
- Le refus est écrit en `llm.attachment_rejected` (empreintes SHA-256 des images, jamais
  leur contenu), que le pliage du journal lit aussi : le tour suivant part sans la photo,
  sans nouveau refus, et la requête dérivée du journal reste celle envoyée.
- La réponse du tour se termine par « Image non lue : refusée par le fournisseur (motif) »,
  hors historique.
- Tests : `penelope-llm/src/attachment.rs`, `penelope-agent/src/tests/attachments.rs`,
  pliage dans `derive/tests.rs` ; scénario rejouable `piece-jointe-refusee` (pas `photos`
  et `vision_models` ajoutés au harnais).
- Hors lot : réduire à l'entrée une photo trop lourde (point 3 de l'issue).

Closes #231.

### 1.0.5

Une planification qui échouait à chaque exécution envoyait une alerte Telegram à chaque
échec : 24 messages identiques par jour pour une planification horaire cassée, que le
propriétaire finit par ne plus lire. Cause : `alert()` partait sans condition, et la table
`schedules` ne gardait que la dernière erreur, sans série.

- Migration `0022_schedule_failure_streak` : `failures_in_a_row` (allongée par chaque
  échec enregistré, remise à zéro au premier succès) et `alerted_reason`, le motif de la
  dernière alerte de la série.
- L'alerte part au premier échec, quand le motif normalisé change (un mot de huit
  caractères ou plus avec un chiffre, identifiant ou horodatage, devient `#` ; un code
  court comme `429` reste), et aux paliers 5, 20, 100 puis toutes les 100, avec « (N échecs
  de suite) ». Sinon elle se tait ; `schedule.failed` entre toujours au journal, avec
  `alerted`. Le principe « jamais de silence » (#39) tient : la première part toujours.
- Au succès qui clôt une série alertée : « ✅ … est rétablie après N échecs »
  (`schedule.recovered`).
- `/schedules` et `doctor` montrent la série en cours (« 3 échecs de suite »).
- Tests : dix échecs au même motif donnent deux alertes puis « rétablie après 10 échecs »,
  un motif qui change alerte (`scheduler/tests.rs`) ; chaque chemin d'échec allonge la
  série, paliers et motifs (`schedules/tests.rs`) ; migration unitaire et fixture 0.17.62 ;
  `doctor` ; scénario `commande-planification-en-echec` (`/schedules` avant et après).

Closes #229.

### 1.0.4

Deux petits défauts relevés en corrigeant #219 à #221.

- **Ingestion** : un texte brut contenant un `<` isolé (`a < b`, `x <- y`) sortait avec
  son début doublé (« a a < b »). `html_to_text` repoussait le reste depuis le début
  quand aucun `>` ne suivait ; il repart maintenant du `<`, traité comme du texte. Un
  commentaire jamais fermé reste retiré.
- **Outil `schedule_delete`** : il répondait `deleted: true` pour un identifiant inconnu,
  et le modèle pouvait annoncer supprimée une planification qui tournait encore. Le port
  `Orchestrator::schedule_delete` refuse désormais « planification inconnue : <id> »,
  comme la RPC, Telegram et la CLI depuis #221 ; `schedule_move` refusait déjà.
- Tests : `a_lone_less_than_is_kept_once`, `schedule_delete_rejects_an_unknown_id`
  (suppression d'une planification existante comprise) ; scénario `outils-planification`
  enrichi d'un appel à identifiant inconnu.

Closes #223.

### 1.0.3

Pénélope ne savait pas qu'elle avait dormi (veille du 26/09, OpenClaw #158592). Un
créneau manqué pendant une veille partait au réveil comme s'il était à l'heure, les
connexions n'étaient vérifiées qu'au premier appel raté, et l'assertion anti-veille
`PowerManager::prevent_sleep` (§2.8), testée, n'était appelée par aucun code de production.

- Assertion anti-veille tenue par un garde RAII (`helpers::keep_awake`) pendant un tour
  (`run_turn`), un job d'outil (sa tâche, jusqu'à sa conclusion) et un run de workflow
  piloté (`drive`) ; relâchée à toute sortie, erreur et panique comprises. Compteur et
  processus changent sous le même verrou (un relâchement concurrent pouvait tuer
  l'inhibiteur d'une prise vivante) ; `caffeinate -i -w <pid>` s'arrête avec le daemon.
- Sortie de veille détectée à chaque passage de l'ordonnanceur par l'écart entre temps
  mural et temps monotone (au-delà de 60 s ; un passage lent n'en crée pas) : événement
  `host.woke` avec sa durée.
- Passe de santé au réveil, avant les créneaux en retard : sonde du canal
  (`ChannelDelivery::probe`, `getMe` pour Telegram, quelques essais), relance des serveurs
  MCP dégradés, en échec ou en attente de reprise ; `host.health` la journalise.
- Un créneau parti plus de cinq minutes après son heure le dit : « ⏰ Exécution en retard :
  prévue à 8h30, lancée à 10h02 après une veille de 3 h 32. » En tête d'une notification ;
  message immédiat au propriétaire pour un prompt ou un workflow, et dans le prompt. Les
  créneaux manqués d'une planification sont comptés et partent en un seul run.
- Déjà là et gardé : le compteur de nuits ratées de la consolidation, l'état
  d'alimentation de `doctor`, un seul tir après un arrêt.
- Tests : `daemon/tests/keep_awake.rs` (tour qui répond, échoue, panique ; job qui
  survit à son tour), `a_driven_run_keeps_the_machine_awake_until_it_stops`,
  `a_three_hour_sleep_is_noticed_and_each_schedule_catches_up_once_late`,
  `a_late_prompt_is_announced_before_its_answer`, `only_a_real_delay_is_announced`,
  `a_slow_step_is_not_a_sleep`, `caffeinate_stops_with_the_daemon`.

Closes #228.

### 1.0.2

`penelope vault check` rendait `ok: false` en permanence sur l'instance : 14 erreurs
« secret », aucune n'en était une. Cause : le contrôle jugeait avec `contains_secret`
(motifs, carte, **et** toute valeur du magasin ou apprise en lecture), le filtre d'écriture
avec `secret_kind` (sans les valeurs). Une adresse électronique rangée au magasin pour un
serveur MCP condamnait onze lignes, et le verdict changeait selon ce que le daemon avait lu.

- Un seul critère, `redact::forbidden_secret`, pour tout ce qui est gardé : filtre
  d'écriture, consolidation (qui détecte et nomme avec la même fonction), entités, skills,
  contrôle du vault. Une valeur du magasin n'y compte que si elle a la forme d'un secret
  (motif ou jeton aléatoire) ; jamais une valeur apprise. `redact` masque comme avant.
- Chaque erreur cite sa nature et son fragment masqué (`jeton github (« ghp_… »)`).
- Un nombre isolé qui passe Luhn n'est une carte que groupé par quatre ou nommé par la
  ligne (« carte », « visa »…) : un identifiant cité en prose passe ; les cas de #132
  restent refusés.
- Une affectation `token = …` sans valeur de forme secrète est un `warning` au contrôle.
- Plus d'`info` « entrées sans uid » sur un fichier que `reindex` exclut.
- `doctor` ne rejoue pas ce contrôle (il ne reprend que le hors-index) : inchangé.
- Tests : `redact/forbidden_tests.rs`, `dream/tests/vault_check.rs` ; documentation dans
  `install-headless.md`.

Closes #207.

### 1.0.1

Un redémarrage demandé depuis Telegram ne laissait aucune trace au journal : le 27/09 à
07h18, rien entre le dernier inventaire et « Pénélope démarre ». Cause : `shutdown` et
`restart` (RPC, donc `/restart` et `penelope restart`) et le redémarrage d'après une mise à
jour posaient le drapeau d'arrêt sans rien écrire ; seul le signal écrivait `arrêt demandé`,
sans dire lequel. Le journal, lui, s'écrit sans tampon : rien n'était perdu à la sortie.

- Tout arrêt passe par `Handle::stop` : ligne `INFO` `arrêt demandé` avec `par` (`signal`,
  `cli`, `telegram`, `mise à jour`, `rpc`), `motif` (`SIGTERM`, `/restart, confirmé`,
  `version x installée`…) et `redemarrage` ; `shutdown` et `restart` prennent `by`/`why`.
- `Pénélope s'arrête` avec la version et l'origine ; `state/daemon-run.json` la garde, et
  `Pénélope démarre` cite `arret_precedent` : l'arrêt demandé, ou un arrêt non propre si la
  version précédente tournait encore. Le retour arrière automatique (#36) y est inscrit ;
  l'arrêt du chien de garde (sortie 75) est journalisé.
- Tests : `stop_journal` (restart par la RPC, ligne relue dans le fichier, rouge avant),
  `lifecycle`, précédence dans `Handle` ; scénario `redemarrage-telegram`, `rpc-arret` relève
  l'origine.

Closes #225

### 1.0.0

La V1. Même comportement pour le propriétaire que la 0.17.62, code réorganisé : 27 crates,
architecture hexagonale tenue par `penelope-archtest`, journal d'événements comme source
unique de vérité (décision 0017), un scénario rejouable par commande Telegram, outil natif
et méthode RPC. Le détail est dans les sections `1.0.0-alpha.1` à `1.0.0-rc.1` ci-dessous.

La 1.0.0-rc.1 a été installée sur l'instance réelle le 27/09 par `penelope upgrade --tag` :
binaire re-signé « Penelope Dev », redémarrage confirmé, Telegram en écoute,
`history verify` sans divergence sur 70 sessions. La 1.0.0 est le même code.

- Scénarios : la version du workspace n'est plus remplacée que là où elle désigne
  Pénélope (`v1.0.0` d'un lien, « version 1.0.0 », champs `version` et `current` hors
  définition de skill ou de workflow). En 1.0.0, le remplacement aveugle prenait aussi le
  `version = "1.0.0"` des skills et workflows (quatre scénarios rouges). Un `tokens_est`
  voisin de la version est masqué : il changeait avec la longueur du numéro à chaque bump
  (cause du rouge de `outils-soi` après la rc.1).

### 1.0.0-rc.1

La V1 passe sur `main` (PR #226, épopée #208). Aucun changement de comportement depuis la
1.0.0-alpha.18, en service sur l'instance réelle depuis le 26/09 : 0 erreur au journal,
consolidation nocturne passée, `history verify` à 0 divergence. Cette version exerce le
chemin de publication : tag posé par `livraison`, release autorisée par `V1_RELEASES=1`,
installation par `penelope upgrade --tag v1.0.0-rc.1`.

- La CI refuse une version 0.x sur `main` ; la 0.17 vit sur la branche `0.17`.
- Les notes de la série 0.17 sont archivées dans [progress-0.17.md](progress-0.17.md).

### 1.0.0-alpha.18

La clôture de la V1 avant la bascule : les critères mesurables de `scripts/switch-check.sh`
sont tenus. Le moteur de tours sort de `Daemon` (T33) ; plus aucune fonction ni aucun
fichier au-dessus des plafonds ; la base de la dernière 0.17 (0.17.62) sert de fixture de
migration ; chaque commande Telegram, outil natif et méthode RPC a un scénario rejouable
(R10, R11) ; la couverture, comptée en lignes de produit non couvertes, est meilleure que
sur `main` dans chaque crate (10 968 contre 14 045 au total) ; les suites réseau donnent
les mêmes résultats sur `v1` et sur `main`. Dix défauts corrigés, dont #219, #220 et #221 ;
une course réelle des jobs d'outils au redémarrage corrigée ; la suite de scénarios passe
de 14 minutes à moins de 2.

#### Suites réseau : ligne de base (#208, critère 5)

Lancées le 26/09/2026 sur `v1` et sur `main` 0.17.62 avec une clé OpenRouter de test :
`live_openrouter` 5/5 des deux côtés, `ctx_recall` réussi des deux côtés,
`mem_longitudinal` 71 % (seuil 85 %) des deux côtés : un défaut de qualité antérieur à la
V1, pas une régression. Les sessions de ces suites passent en mode d'approbation `auto`
(personne ne répond aux cartes pendant une suite réseau).

#### Dix défauts relevés par les tests de la V1 (#208, #219, #220, #221)

- **Mémoire** : « Ignorer » sur une contradiction écarte bien le candidat au lieu de
  répondre « Déjà tranché » (#219) ; un second refus d'une même demande dit « Déjà
  tranché ». Un vecteur d'embeddings vide n'est plus gardé en cache ; le texte d'un
  commentaire HTML jamais fermé n'entre plus en mémoire.
- **Workflows** : la raison d'un run bloqué est dans `run.error` (`wf status`, écran du
  run), pas seulement dans l'événement (#220).
- **Planifications** : pause, reprise et suppression d'un identifiant inconnu répondent
  « planification inconnue : <id> » (#221).
- **Approbations** : `echo 'a'; b` est dit composé par `;` ; une règle sur un fichier à
  la racine se lit « à la racine de l'espace de travail ».
- **Cohérence** : `::1`, fc00::/7 et fe80::/10 sont reconnus privés dans
  `tools.http_allowlist`.
- **Outils à la demande** : deux lectures parallèles ne perdent plus de promotion.
- **`chat.stream`** : plus de notification `done` en trop avant la réponse finale.

#### Tests fiables sous charge, scénarios huit fois plus rapides (#208)

- **Un job d'outil perdu au redémarrage le reste** : si sa tâche conclut quand même
  (processus orphelin tué par la reprise), l'effet du ledger n'est réécrit que s'il est
  encore `dispatching`. Avant, un effet `failed` pouvait redevenir `completed` derrière
  un job `failed`. `tool_jobs_e2e` échouait une fois sur deux sous charge.
- **Trois tests ne parient plus sur des durées** : la rafale Telegram (fenêtre de 40 ms
  dépassée sous charge), le steering (message et arrêt déposés pendant l'appel au lieu
  de 100 ms après le départ). Chacun passe 50 fois de suite sous charge.
- **La suite `scenarios` passe de 832 s à 101 s** : la fermeture d'une vie comptait mal
  les copies des services tenues par le daemon et attendait cinq secondes à chaque vie.
- **Quatorze fonctions mortes retirées** (context, llm, mcp, memory, tools, workflow),
  et la structure `ToolContext`, sans usage.

#### Moteur : les surfaces tiennent le cœur du daemon et passent par trois ports (#208, T33)

Le moteur de tours expose trois ports dans `penelope_app::engine` : `TurnIntake`,
`SessionModels` et `Transcriber` (T33). Le daemon se scinde en `Core`, l'état partagé et
les ports, et `Daemon`, le processus (reprise, boucles, tours). Le RPC, la passerelle
Telegram, les contextes de l'orchestrateur et de la compaction et la livraison des jobs
d'outils tiennent le cœur ; plus aucun module du daemon hors de ses quatre fichiers de
composition ne nomme `Daemon`. `engine.rs` passe de 1 065 à 485 lignes
(`engine/intake.rs`, `models.rs`, `media.rs`, `admin.rs`) et sort de la liste de
référence du gel, qui est vide ; `execute_turn` perd son `allow(too_many_lines)`. Aucun
comportement ne change.

#### Fonctions trop longues : dix-neuf découpes (#208)

Les dix-neuf fonctions qui dépassaient le seuil de 200 lignes de clippy sous un
`#[allow(clippy::too_many_lines)]` du gel 0.17, hors moteur de tours, sont découpées par
étapes nommées, sans changement de comportement : purge d'une session, validation de la
configuration, statut de soi, classement des updates, catalogues de commandes et de
gabarits, workflow `ticket-to-deploy`, routage de la CLI, étape `verify`, boucle d'agent,
application et passe de consolidation du rêve, passerelle Telegram (message, clic,
opérations d'écran), et quatre tests de scénario. `[lints].allow_too_many_lines` descend
de 20 à 1 ; le dernier, `engine.rs`, part avec la refonte du moteur (T33).

#### Clôture du gel : fixture 0.17.62, R10, R11 (#208)

- **La dernière 0.17 dans le filet de migration** : `penelope-0.17.62.db` (221 184 octets,
  19 migrations), produite par le code du tag `v0.17.62` et non par v1. Les tests de
  migration et de scellement rejouent la 0.17.59 et la 0.17.62 et exigent les deux ;
  `verify` sans divergence, chaîne vérifiée.
- **R10, chaque surface visible a un scénario** : archtest lit les commandes Telegram, les
  outils natifs et les méthodes RPC dans le code, et ce que les scénarios rejouables jouent
  vraiment. Une surface nouvelle sans scénario fait échouer
  `every_visible_surface_has_a_scenario` ; ce qui manque au départ est inscrit dans
  `budget.toml` `[scenarios].missing` (213 sur 219), liste qui ne fait que rétrécir.
- **R11, dans le même lot** : `scripts/check-budget.sh` refuse une plage qui touche un
  catalogue de surfaces sans toucher `crates/penelope-evals/scenarios/`, sauf trailer
  `Sans-scénario: <raison>`.

#### Les méthodes RPC rejouées par des scénarios (#208, critère 7)

- **Étape `rpc` des scénarios** : une méthode appelée comme la socket locale la sert
  (`chat.stream` et `tail` compris), les tours qu'elle met en file joués pendant
  qu'elle attend, la réponse normalisée dans le monde attendu ; `bind` et `$nom.chemin`
  enchaînent les appels, `pick`, `mask` et `lines` écartent ce qui dépend de la
  machine, `error = true` attend un refus. `[[observe]]` relève une table après le run,
  `[[mcp_servers]]` branche le vrai superviseur MCP sur un connecteur de test.
- **Douze scénarios par famille** (sessions, configuration et modèles, approbations,
  planifications, workflows, mémoire, vault et accueil, conversation, MCP et import
  Hermes, skills, exploitation, arrêt) : 104 méthodes sur 108 exercées et assertées sur
  le monde après. Restent `doctor`, `mcp.auth`, `skill.install` et `upgrade`, qui
  demandent la machine ou le réseau réels.
- La version du workspace est normalisée dans les attendus : un bump n'en réécrit
  aucun.

#### Scénarios des outils natifs (#208, critère 7)

- **Chaque outil natif a un scénario rejouable sans clé** : dix-sept scénarios, un par
  famille (fichiers et shell, mémoire, skills, soi, garde http, workflows et runs,
  historique et résumés, question, planification, canal, git, jobs, sous-agent, images).
  Chacun asserte l'effet dans le monde : fichier corrigé, carte demandée, préférence
  retenue puis oubliée, rappel créé puis supprimé, commit refusé par un crochet, job
  annulé, run annulé, image envoyée. `[scenarios].missing` ne liste plus aucun outil.
- **Le moteur de scénarios** branche l'orchestrateur à chaque vie, offre un canal simulé
  (`messenger = true`, envois relevés en lignes `sent`), laisse le script citer un
  identifiant créé pendant le run (`{{id:<préfixe>:<n>}}`, échec clair s'il manque), et
  normalise les durées de `shell_exec`, les ULID à préfixe court, le marqueur du juge des
  commandes et le fichier de notes de session.

#### Les commandes Telegram ont leurs scénarios rejouables (#208, critère 7)

- **Étape `telegram`** des scénarios : une commande, un message ou un clic du
  propriétaire passe par la vraie passerelle sur un transport simulé ; les écrans
  envoyés (texte, boutons), les réactions et les tours joués entrent dans les attendus.
  Les jetons de boutons deviennent `{{action}}`, et un scénario peut masquer un texte
  propre à l'hôte (`masks`).
- **Sept scénarios** couvrent les 46 commandes qui n'en avaient pas : système,
  sessions, réglages (épinglage vérifié sur l'appel suivant), approbations (carte, clic,
  reprise, règle), mémoire (retenir, retrouver, oublier), workflows, extensions.
  `[scenarios].missing` ne contient plus aucune commande.

#### Les dernières méthodes RPC rejouées par des scénarios (#208, critère 7)

- **Faux serveur HTTP local des scénarios** (`[[http]]`) : ce qu'une méthode va chercher
  sur le réseau lui est servi depuis 127.0.0.1, les requêtes reçues relevées dans le
  monde ; l'étape `open` joue le navigateur du propriétaire, `without` retire d'une
  réponse ce qui décrit l'hôte.
- `doctor`, `mcp.auth` (parcours OAuth complet : découverte, enregistrement, PKCE,
  échange du code), `upgrade` (« à jour » et « mise à jour disponible » contre une
  source locale, rien de remplacé) et `skill.install` (dépôt servi en local, skill
  posée) ont leur scénario : plus aucune surface RPC sans scénario.
- **`penelope upgrade --check` marche sous Linux** : la vérification dit la dernière
  version sans exiger l'archive de l'OS, qui n'est cherchée qu'à l'installation.
- **`skills.archive_base_url`** : l'origine des archives de `penelope skill install`
  (défaut `https://codeload.github.com`), HTTPS ou boucle locale seulement ; un miroir
  devient possible.

#### Scénario du diagnostic stable en CI (#208)

Le scénario `rpc-diagnostic` compare une liste explicite des contrôles de `doctor` propres
à Pénélope (nouveau filtre `only` de l'étape `rpc`) au lieu de retirer ce qui décrit
l'hôte : le contrôle `voice`, qui cherche `ffmpeg`, faisait échouer la CI de `v1` sur les
runners qui ne l'ont pas.

#### La couverture du socle au niveau de main, et ses plafonds (#208, critère 7, R9)

La couverture des crates socle de v1 semblait avoir baissé depuis le point de fourche
(86,6 % contre 88,3 % pour le workspace). C'était surtout un effet de mesure : R1 a sorti
les tests inline vers des fichiers `tests.rs`, que `cargo-llvm-cov` ne compte pas, et ces
lignes couvertes ont quitté le dénominateur. Le reste venait de code découpé ou descendu
sans ses tests. Des tests de comportement comblent l'écart crate par crate : fournisseurs
OpenAI-compatibles contre un faux serveur, recherche hybride MCP, chaque refus de la
validation de configuration et de workflow, écarts de `history verify`, index FTS5
irréparable. Le critère compte les lignes de produit non couvertes par crate, qui ne
dépendent pas de l'endroit où vivent les tests : `budget.toml` porte le relevé de main
(`[coverage.main]`) et un plafond par crate qui ne monte jamais (`[coverage.uncovered]`).
`scripts/coverage-check.sh` mesure et compare ; `switch-check.sh` l'appelle pour le
critère 7.

#### Couverture des crates sorties du daemon (#208, critère 7)

- Les crates extraites du daemon repassent au-dessus du niveau du daemon de `main`
  (86,3 %) : passerelle Telegram 87,9 %, ops 87,0 %, orchestrateur 87,4 %, hôte MCP
  89,1 %, rêve 87,8 % ; le workspace passe de 86,6 % à 89,2 % (88,3 % sur `main`).
- La baisse venait du dénominateur : les tests en ligne de `main` comptaient comme lignes
  couvertes, rangés dans `tests/` ils ne comptent plus. Les tests ajoutés vivent tous dans
  des fichiers de tests.
- Cent vingt-six tests, sans changement de comportement : écrans cliquables de Telegram,
  démarrage de la passerelle, contrôles de `doctor`, connexion Codex, sauvegardes,
  contrôle des runs, attentes, OAuth local, contradictions de la mémoire.
- Quatre défauts relevés et consignés dans `design/v1/notes/c-extraites.md`, dont
  « Ignorer » une contradiction depuis Telegram qui répond « Déjà tranché » sans écarter
  le candidat.

### 1.0.0-alpha.17

**Le journal est la seule source de la conversation** (décision 0017) : plus aucune
écriture de l'historique sans son événement, la clé `history.source` est retirée (une
ancienne configuration qui la porte se charge avec un avertissement), et une règle
d'architecture interdit d'écrire dans les tables de cache hors de `penelope-context`. Un
`/rewind` qui remonte dans l'historique scellé d'une ancienne session masque les lignes au
lieu de les effacer, et le journal reste cohérent. C'est la version candidate à la bascule
vers `main`.

#### Journal : la seule source de la conversation (#208, T16, T19)

- **Plus aucune écriture de la conversation sans son événement** : messages, contextes
  figés, résumés, fork et retour arrière entrent au journal d'abord, et les tables
  `messages`, `message_context`, `lcm_nodes`, `prompt_snapshots` n'en sont que les
  caches, écrits par le moteur de contexte seul (un test d'architecture l'interdit
  ailleurs). Un `/fork` et l'archive d'un `/rewind` sont refaits depuis le journal, lignes
  scellées comprises.
- **La clé `history.source` est retirée** : la conversation se relit toujours depuis le
  journal. Un fichier de configuration qui la porte encore se charge, avec un
  avertissement ; la ligne peut être effacée.
- Les clés de travail `turn.recorded.*` et `prompt.prefix.*` disparaissent : le message
  d'un tour déjà écrit et le préfixe retenu se lisent dans le journal. La rétention efface
  celles qui restent. Un projet fixé à la main entre au journal (`session.project`).
- Décision [0017](decisions/0017-journal-source-unique.md) : le journal d'événements est
  la source unique de la conversation.

#### Journal : un retour arrière dans l'historique d'avant la mise à jour (#208)

- **Un `/rewind` sur une ancienne session ne casse plus sa lecture** : revenir de
  quelques échanges juste après la mise à jour, avant tout message nouveau, retirait des
  messages que le scellement de l'historique compte ; la session se relisait alors dans
  ses caches, avec une erreur au journal, et `penelope history verify` la signalait. Les
  messages scellés défaits restent en base, masqués : la conversation vue par le modèle
  ne change pas, l'empreinte du scellement se vérifie de nouveau, et l'archive du retour
  arrière garde ce qu'elle gardait.

#### Corrections d'intégration (#208)

- Les lignes masquées par un rewind (`sealed = 2`) sont ignorées aussi par le titre de
  repli d'une session, la numérotation de `self_status` et l'audit du cache.
- Le Trousseau : `security` reçoit une entrée vide pour lire et effacer un item ; un test
  bloquait indéfiniment quand l'entrée standard restait ouverte.

### 1.0.0-alpha.16

**Le juge d'approbation (#203)** : la mesure préalable sur l'instance réelle (112 cartes sans
motif en 30 jours, 26 par semaine, 111 commandes distinctes, 96 % de « oui ») a donné go.
Par défaut (`approval.judge = "explain"`), un modèle auxiliaire traité comme hostile décrit
sur la carte ce que fait réellement une ligne composée et propose « Toujours pour ces
pouvoirs » ; il n'autorise rien seul. Et le premier test de la V1 sur une copie des données
réelles a trouvé un défaut du scellement, corrigé : `history verify` passe de 21 divergences
à 0 sur la copie (69 sessions, 8 793 nœuds).

#### Juge d'approbation (#203, #208, T22 et T23)

- Une ligne `shell_exec` sans motif possible (`;`, `||`, `$(…)`, redirection…) n'est
  plus une carte muette : un modèle auxiliaire (rôle `approval_judge`, alias `fast`)
  dit ce qu'elle fait réellement (« lecture sur `tmp` », « réseau vers
  `api.github.com` ») et donne un avis. Il est appelé seulement sous les planchers :
  politique `Ask`, classe non destructive, aucune règle du propriétaire qui refuse une
  famille de la ligne.
- `approval.judge = "explain"` (défaut) : la carte est enrichie et propose « ♾️ Toujours
  pour ces pouvoirs », une règle dérivée des pouvoirs reconnus et non de la forme de la
  ligne. `auto_read` laisse en plus passer sans carte une lecture pure dans le
  workspace. `off` : la carte d'avant.
- Le texte jugé est traité comme hostile (secrets masqués, commentaires retirés, bloc
  délimité, aucun outil) ; modèle absent, délai de 10 s, sortie hors schéma : la carte
  d'avant, sans message. `rm`, `sudo`, `curl … | sh` et un avis `dangereux` ne sont
  jamais automatisés.
- Événement `approval.judged` ; usage compté sous `approval_judge` ; `/policies` et
  `penelope policies` disent d'une règle qu'elle est née d'un jugement ; `penelope
  doctor` donne le mode et la part des cartes jugées sur sept jours. `config_set` sur
  `approval.*` demande deux confirmations.

#### Journal : les résumés scellés vérifient (#208)

- `penelope history verify` ne signale plus « jetons, ancres » sur chaque session scellée
  qui porte un résumé actif : la relecture du préfixe scellé rend `tokens_src` et les
  ancres du nœud. L'empreinte des `conv.import` existants est inchangée ; une base déjà
  scellée vérifie sans rien refaire.
- Une compaction qui prolonge un résumé scellé compte sa source héritée.
- Le fork d'une session scellée garde ses lignes scellées : `verify` les apparie et
  `reindex` accepte de refondre la fille.

### 1.0.0-alpha.15

Quatorzième vague de la V1, la clôture du code : les réexports de transition du daemon
disparaissent (chaque consommateur nomme la crate d'origine), et presque tous les fichiers
source passent sous 800 lignes, tests en fichiers frères (cible de sortie de la V1). Aucun
changement de comportement. Restent au-dessus de 800 : `engine.rs` (1 091) et
`supervisor.rs` (930) du daemon, dont le découpage demande de sortir le moteur de tours de
`impl Daemon` (T33, après V1).

#### Le daemon ne réexporte plus rien (#208, T30)

- Les réexports de transition posés pendant le découpage (T21 à T29) disparaissent :
  `penelope_daemon` n'exporte plus que ses modules propres, `Daemon` et `VERSION`. La
  passerelle Telegram, les évaluations, la CLI, les tests et l'exemple du daemon nomment
  la crate où vit le code (`penelope_app::services::Services`,
  `penelope_orchestrator::workflow::start_run`, `penelope_ops::session_ops`…).
- Les enveloppes `&Arc<Daemon>` de `workflow`, `scheduler` et `dream` sont retirées :
  un seul adaptateur, `workflow::context_of`, dérive du daemon le contexte de
  l'orchestrateur. Le module `scheduler` du daemon disparaît.
- `penelope-cli/src/commands.rs` passe de 2 128 à 225 lignes en six modules
  (`cli`, `route`, `offline`, `interactive`, `restore`, `upgrade`) ;
  `penelope-daemon/src/tool_jobs.rs` de 1 135 à 345, ses tests à côté. Les deux sortent
  de la liste de référence du gel.
- Aucun changement de comportement.

#### Plafonds : les crates socle sous 800 lignes par fichier (#208, T32)

- `penelope-kernel`, `penelope-llm`, `penelope-tools`, `penelope-hitl`, `penelope-store` :
  plus aucun fichier au-dessus de 800 lignes (charte V1 §3.4). Déplacements seuls, tests
  en fichiers frères, chemins publics réexportés ; aucun changement de comportement.
- Sept fichiers sortent de la liste de référence du gel (`config.rs`, `turn.rs`,
  `provider.rs`, `codex.rs`, `spec.rs`, `policy.rs`, `penelope-store/src/lib.rs`) ; le
  catalogue d'outils perd son `allow(clippy::too_many_lines)`.

#### Plafonds : les autres crates sous 800 lignes par fichier (#208, T32)

Vingt-cinq fichiers de `penelope-memory`, `penelope-mcp`, `penelope-context`,
`penelope-workflow`, `penelope-platform`, `penelope-telegram`, `penelope-dream`,
`penelope-executor`, `penelope-mcp-host`, `penelope-gateway-telegram` et `penelope-ops`
passent sous 800 lignes (charte §3.4) : tests en fichiers frères et, pour les plus gros,
une responsabilité sortie en module enfant (recherche de l'index mémoire, transport stdio,
recherche du registre MCP, recherche de l'historique). Déplacements seuls, aucun chemin
public ne change. Seize fichiers sortent de la liste de référence du gel.

#### Archtest sous 800 lignes (#208, T32)

Les tests de `penelope-archtest/src/lib.rs` sortent dans `tests.rs` : 912 → 578 lignes,
mêmes 69 tests. Le `.gitignore`, qui ignorait tout chemin nommé `spec`, suit désormais
`crates/penelope-tools/src/spec/`.

### 1.0.0-alpha.14

Treizième vague de la V1 : `penelope approvals stats` mesure, en lecture seule, si le juge
d'approbation (#203) vaudrait la peine d'être construit ; l'API de la boucle est nettoyée
et la couture du PTC posée (décision 0016) ; `docs/architecture.md` et les décisions 0013
et 0014 décrivent le code tel qu'il est.

#### Mesure avant le juge d'approbation (#208, #203, T21)

- `penelope approvals stats [--days 30] [--json]` compte, en lecture seule et sans daemon,
  les cartes `shell_exec` pour lesquelles « Toujours » n'écrit aucune règle, celles que le
  juge de #203 verrait, les commandes distinctes, les dix plus fréquentes (tronquées,
  secrets masqués, hachées) et la part de « oui », avec un verdict go/no-go pour T22.
- La base est ouverte en `SQLITE_OPEN_READ_ONLY` et `query_only` : les tests prouvent
  qu'elle ne change pas, daemon lancé ou arrêté.
- `docs/install-headless.md` : « Mesurer avant d'activer le juge », avec les seuils motivés.

#### Boucle : couture PTC, API réduite, cache audit sans port (#208, T24, T25, T27)

- **Couture PTC** (décision [0016](decisions/0016-ptc-hors-v1.md)) : `run_code` reste hors
  V1. Chaque appel porte son `CallContext { call_id, parent, root }` dans le pipeline ; un
  appel imbriqué dont la politique demanderait une approbation est refusé sans carte, car
  une approbation suspend le tour, pas un programme en vol.
- **API de la boucle** : `TurnRequest`, `AgentLoop::run` et l'alias
  `resume_after_approval` sont retirés ; un tour se lance par `TurnSpec` et une
  `Conversation`, une approbation se tranche par `decide_approval`.
- **Cache de prompt** : l'empreinte, le fournisseur collant et la cause d'un raté vivent
  dans `penelope_llm::cache` ; le dernier appel d'une session se lit dans le
  `BudgetLedger` (`previous_call`). Le port `CacheAudit` disparaît. Aucun changement
  visible : même requête, mêmes causes de raté.

#### Architecture documentée, décisions 0013 et 0014 (#208, T31)

- Nouveau guide `docs/architecture.md` : les crates et leurs couches, les ports de
  `penelope-app` et qui les implémente, la frontière canal, le journal comme source de
  lecture, les règles d'architecture et les valeurs du gel, ce qui reste avant la
  bascule. Pénélope le lit aussi, embarqué dans son binaire.
- Décision [0013](decisions/0013-decoupage-du-daemon.md) : le daemon découpé en crates, la
  passerelle Telegram au-dessus de lui.
- Décision [0014](decisions/0014-boucle-pipeline.md) : la boucle d'agent est un pipeline
  d'étapes typées.

### 1.0.0-alpha.13

Douzième vague de la V1 : **le cœur ne nomme plus le canal** (`penelope-app`, le daemon et
l'exécuteur ne dépendent plus de `penelope-telegram` ; les mentions du canal dans le cœur
passent de 166 à 74), la boucle écrit ses tentatives par un port et ne voit plus aucun type
du moteur de contexte, et les jobs d'outils tiennent tous les critères de la
spécification : un redémarrage pendant un job ne pose plus de carte d'effet incertain.
Aucun texte ne change sur Telegram ; hors Telegram, « Telegram non configuré » devient
« canal (non configuré) », et `self_status` rend `channel_commands` au lieu de
`telegram_commands`.

#### Frontière canal : le cœur ne nomme plus Telegram (#208, T36)

- Les gabarits de cartes et les jetons de boutons quittent `Services` pour la
  passerelle ; le cœur lit le texte d'un gabarit, le catalogue que valident les
  workflows, les liens profonds et la liste des commandes par un port `Cards`.
- La carte de rafale, le nom des conversations où livrent les planifications, le
  contrôle des conversations autorisées et le formulaire d'une élicitation MCP sont
  décidés par la passerelle, derrière `ChannelDelivery` et `OwnerChannel`.
- `penelope-app`, `penelope-executor` et `penelope-daemon` ne dépendent plus de
  `penelope-telegram` (règles d'architecture) ; les mentions du canal dans le cœur
  passent de 166 à 74.
- Aucun texte ne change sur Telegram. Sans canal, les messages disent « canal » au lieu
  de « Telegram ».

#### Tentatives par un port, boucle coupée du moteur de contexte (#208, T15, T26)

- **Une réponse vide est épinglée à sa requête** : sa tentative (`conv.attempt`, cause
  `empty_answer`) porte le `llm_request_id` de l'appel qui l'a rendue, comme les autres
  causes ; l'audit retrouve la requête exacte au lieu de la recalculer.
- La boucle remet ses tentatives au port `AttemptSink` ; le journal les écrit comme avant.
  La table `turn_attempts` prévue n'est pas créée : le journal la remplace.
- La boucle d'agent ne voit plus aucun type du moteur de contexte, même par les ports : le
  vocabulaire du journal qu'elle écrit (bornes de tour, provenance, tentatives, tuiles du
  préfixe) vit dans le noyau. Une règle d'architecture le vérifie.
- Les événements de la boucle sont nommés par un type, et `docs/runtime-events.md` décrit
  désormais `turn.merged`, `turn.empty_answer`, `turn.loop_aborted`, `tool.result`,
  `llm.retried`, `llm.fallback_used` et `approval.decided`.

#### Jobs d'outils : écarts de T17 à T20 comblés (#208)

- Un redémarrage pendant un job d'outil ne pose plus la carte « C'est fait / Relancer /
  Ignorer » : le job et son effet passent à `failed` avec la raison, et la conversation
  reçoit le résultat comme pour tout job fini. Aucune relance automatique, comme avant
  (décision 0012, révisée).
- Nouveaux tests de bout en bout : un sous-agent long lancé en job et interrompu par
  `/stop` (#57), le groupe de processus d'un job entièrement tué par `/stop`, les
  plafonds par défaut (3 par conversation, 10 en tout) refusés avec leur texte.

### 1.0.0-alpha.12

Onzième vague de la V1 : **un message qui arrive pendant un lot d'outils est pris en
compte proprement** (l'outil en cours finit, les autres ne sont pas lancés, le modèle est
rappelé une fois) et `/stop` pendant des outils laisse une note au tour suivant ; les
workflows et la planification sortent du daemon (`penelope-orchestrator`), ainsi que la
purge et les opérations de session (`penelope-ops`). Toutes les crates du découpage sont
désormais sorties : le daemon passe de 22 863 à 15 109 lignes (86 462 au plus haut).

#### Boucle d'agent : steering explicite (#208, T12 à T14)

- Un message arrivé pendant un tour est réclamé par la boucle à des points nommés
  (`Inbox`, avant chaque appel au modèle et entre deux appels d'outils) ; lire la
  conversation n'absorbe plus rien.
- Un message arrivé pendant un lot d'outils n'attend plus la fin du lot : l'appel en
  cours finit, les suivants reçoivent « Non exécuté : nouveau message du propriétaire. »,
  et le modèle repart avec le message.
- `/stop` pendant un lot donne « Non exécuté : arrêté par le propriétaire. » aux appels
  non démarrés ; le tour suivant sait que le précédent a été interrompu, sans toucher au
  préfixe du prompt.
- Nouveau scénario rejouable `message-pendant-un-lot`.

#### Orchestrateur : crate `penelope-orchestrator` (#208, T27 et T11)

- Le moteur de workflows et l'ordonnanceur quittent le daemon pour la crate
  `penelope-orchestrator`, au-dessus de la boucle d'agent, de l'exécuteur, de la
  conversation et du rêve ; une règle d'architecture lui interdit le daemon, la
  passerelle et le canal, qu'il n'atteint que par les ports de `penelope-app`.
- Ils reçoivent un contexte (services, providers, état des runs, services de la boucle)
  au lieu du daemon ; les étapes `agent` et les sous-agents appellent la boucle
  directement. Un test vérifie qu'un sous-agent qui demande une approbation échoue au
  lieu d'attendre.
- `penelope-daemon` passe de 22 863 à 17 076 lignes. Aucun comportement visible ne
  change.

#### `penelope-ops` complète : purge et sessions, ordre de `doctor` rétabli (#208, T28)

- `purge` (purge d'une session, rétention, caviardage de la file sortante) et
  `session_ops` (fork, retour arrière, export, reconstruction) quittent le daemon pour
  `penelope-ops` ; le daemon les réexporte sous leurs anciens chemins. Leurs tests
  n'ouvrent plus de daemon.
- `penelope doctor` retrouve l'ordre de la 1.0.0-alpha.7 : stabilité du prompt,
  historique et jobs d'outils juste après la rétention, et non plus en fin de liste.
  Un test compare les identifiants.
- La règle HTTPS des adresses (OAuth MCP, téléchargement des releases) n'a plus qu'une
  copie, dans `penelope_app::helpers::check_endpoint`.
- `penelope-daemon` passe de 22 863 à 20 831 lignes.

#### Doctor : le test d'ordre ne dépend plus de la machine (#208)

Le test qui fige l'ordre de `penelope doctor` compare les contrôles de référence sans
interdire qu'un contrôle propre à la machine (`machine.missing`, `machine.gh`) s'intercale :
il échouait sur les runners de la CI.

### 1.0.0-alpha.11

Dixième vague de la V1 : trois crates du cœur sortent du daemon (la conversation et la
compaction, l'exécuteur des outils natifs, la consolidation nocturne entière), et trois
critères d'acceptation prouvent, à chaque appel des scénarios, que le modèle voit
exactement ce que le journal contient. Le daemon passe de 38 177 à 22 863 lignes. Aucun
changement de comportement en production.

#### Conversation : crate `penelope-conversation` (#208, T23)

- La conversation d'une session, la compaction de fond et son état, les titres de
  session et l'alerte de budget quittent le daemon pour la crate
  `penelope-conversation`, au-dessus de `penelope-app` et de `penelope-vault` ; une
  règle d'architecture lui interdit le daemon, la boucle d'agent et le canal.
- La compaction ne reçoit plus le daemon mais son contexte (services, providers, bus
  des tours, état) ; l'alias épinglé d'une session se lit dans `penelope-app`.
- `penelope-daemon` passe de 38 177 à 35 241 lignes. Aucun comportement visible ne
  change.

#### Exécuteur des outils natifs : crate `penelope-executor` (#208, T24)

- Les outils natifs, `self_status`, la documentation embarquée, la vision, les images,
  la voix, les outils à la demande et le magasin des jobs d'outils quittent le daemon
  pour la crate `penelope-executor`, qui ne dépend ni de la boucle d'agent ni de
  l'orchestrateur ; une règle d'architecture le vérifie.
- La planification (`schedule_*`) passe par le port `Orchestrator`, et ce que
  `self_status` lit du processus (mémoire, Codex, contexte) par le port `Admin`.
- `penelope-daemon` passe de 38 177 à 31 486 lignes. Aucun comportement visible ne
  change.

#### Crate `penelope-dream` : le rêve nocturne hors du daemon (#208, T26)

La consolidation nocturne, le digest du matin et les crons qui les déclenchent quittent
le daemon pour la crate `penelope-dream`, qui ne dépend pas de lui : ils reçoivent un
contexte (services, providers, embeddings), et le digest obtient ce qu'il lit des
planifications et du compactage par un port. Aucun comportement ne change ; les anciens
chemins restent réexportés (épopée #208, lot J, T26).

#### Journal d'événements : critères d'acceptation (#208, T22)

- **Ce que le modèle lit est journalisé, vérifié à chaque appel** : à la fin de chaque
  scénario enregistré, chaque requête reçue par le modèle est comparée, octet pour
  octet, à la conversation repliée depuis le journal juste avant sa réponse (critère
  CA 4.5).
- **Rien ne se réécrit au milieu d'un tour** : entre deux appels d'un même tour, le
  journal n'admet que le niveau 1 d'un résultat pas encore envoyé et le résumé d'un
  dépassement prouvé (CA 5.5). La refonte des caches depuis le journal redonne tout à
  l'identique (CA 4.6).
- Nouveau scénario `outils-niveau-1-et-compaction` : un tour avec lecture d'un gros
  fichier, niveau 1 et compaction d'urgence. 78 critères d'acceptation.

### 1.0.0-alpha.10

Neuvième vague de la V1, courte : la crate `penelope-dream` naît avec l'accueil et
l'ingestion, et le digest du matin reçoit ses entrées en données (`DigestInputs`). La
consolidation nocturne (`dream/`) reste au daemon pour la vague suivante. Le daemon passe
à 38 177 lignes.

#### Crate `penelope-dream` : accueil et ingestion hors du daemon (#208, T26)

L'entretien d'accueil et l'ingestion de documents quittent le daemon pour la crate
`penelope-dream`, qui ne dépend pas de lui : l'ingestion reçoit un contexte (services,
providers, embeddings) au lieu du daemon entier. Le digest du matin reçoit en données ce
qu'il lisait des planifications et du compactage, première étape de la descente du rêve
nocturne. Aucun comportement ne change ; les anciens chemins restent réexportés (épopée
#208, lot J, T26).

### 1.0.0-alpha.9

Huitième vague de la V1 : `penelope purge` prévient avant d'agir quand des forks vont
perdre leur début (arbitrage 3 du propriétaire), et la boucle d'agent sort du daemon dans
la crate `penelope-agent`, qui ne dépend ni du contexte, ni de la mémoire, ni de Telegram.
Le daemon passe à 39 727 lignes.

#### Purge : les forks sont prévenus avant (#208, arbitrage 3)

`penelope session purge` et `/purge` disent, avant de demander confirmation, quelles
sessions nées d'un fork perdront leur début (« Cette session a deux forks, ils perdront
leur début : … »). Sans terminal pour répondre, la commande refuse sans `--yes` au lieu
de ne rien faire en silence. Nouvelle méthode RPC `session.purge_preview`, lecture seule.

#### Boucle d'agent : crate `penelope-agent` (#208, T10)

- La boucle d'agent et le pipeline d'outils quittent le daemon pour la crate
  `penelope-agent`, qui ne dépend ni du moteur de contexte, ni de la mémoire, ni du
  canal ; une règle d'architecture le vérifie.
- Les charges du journal qu'écrit la boucle passent par `penelope-app`, inchangées.
- Aucun comportement visible ne change.

### 1.0.0-alpha.8

Septième vague de la V1 : la lecture du journal devient incrémentale (une session longue
ne ralentit plus chaque requête), `audit show` et l'export lisent le journal, la purge
emporte le journal et nomme les forks qui perdent leur début, et deux blocs sortent encore
du daemon : les opérations (`penelope-ops`) et les dépendances de la boucle d'agent,
désormais derrière des ports. Le daemon passe à 46 009 lignes. Changement visible : dans
`penelope doctor`, les contrôles MCP, stabilité du prompt, jobs d'outils et historique
apparaissent en fin de liste.

#### Journal d'événements : lecture incrémentale, audit exact, purge (#208, T15, T17)

- **Lecture incrémentale** : une requête ne replie plus tout le journal de la session et
  de ses ancêtres, seulement les événements arrivés depuis la précédente (8 ms au lieu de
  115 ms pour une session de 2 000 messages en compilation de test). Une purge fait tout
  relire.
- **`penelope audit show` exact après une compaction** : la requête d'un tour passé est
  repliée depuis le journal jusqu'à l'appel (résumés de l'époque, messages résumés depuis
  en clair) au lieu d'être relue dans les lignes d'aujourd'hui.
- **Export** : le préfixe scellé est marqué (`sealed`) et la conversation telle que le
  modèle la voit suit en lignes `surface`.
- **Purge** : le rapport nomme les sessions nées d'un fork de la session purgée, qui
  perdent le préfixe hérité ; les prompts système journalisés par la session partent
  avec elle. **Rétention** : le texte partiel des tentatives d'appel (`conv.attempt`)
  est purgé après `retention.days`, la chaîne d'audit reste vérifiable.

#### Crate `penelope-ops` : l'exploitation hors du daemon (#208, T28)

- Nouvelle crate `penelope-ops`, entre `penelope-app` et `penelope-vault` d'un côté et
  le daemon de l'autre : diagnostic (`doctor`), mise à jour et retour arrière du binaire,
  sauvegarde, import d'une instance Hermes, connexion et quota de l'abonnement Codex,
  installation de skills tierces et de leurs dépendances.
- La crate ne connaît ni le daemon ni l'hôte MCP : `penelope-archtest` le vérifie. Les
  contrôles de `doctor` qui lisent le daemon (stabilité du prompt, journal, jobs
  d'outils) ou l'hôte MCP (serveurs, bac à sable) restent au daemon, qui les ajoute à la
  méthode `doctor` ; dans la sortie, ils arrivent après les autres contrôles.
- `upgrade`, `hermes` et `codex_auth` sont découpés sous 800 lignes et quittent la liste
  des fichiers trop longs ; les tests de la crate n'ouvrent pas de daemon.
- La CLI importe `doctor::render` et `upgrade` de la crate ; le daemon réexporte tout sous
  les anciens chemins. `penelope-daemon` passe de 53 836 à 45 232 lignes. `purge` et
  `session_ops` suivront.

#### Boucle d'agent : ports (#208, T09)

- La boucle ne reçoit plus tous les services du daemon mais `AgentServices` : les
  registres qu'elle touche et cinq ports (mode d'approbation d'une session, nature d'une
  session, instantanés du prompt, dernier appel pour le cache, jobs d'outils).
- Plus aucun chemin du daemon dans `agent/` : le répertoire est prêt à devenir la
  crate `penelope-agent` (T10). Les tests de la boucle tournent sur une base en
  mémoire, sans daemon.
- Le formatage des montants (`usd`) descend dans `penelope-kernel`.
- Aucun comportement visible ne change.

### 1.0.0-alpha.7

Sixième vague de la V1 : **la conversation se relit depuis le journal d'événements**
(`history.source = journal`, nouveau défaut ; `tables` reste disponible), avec des
requêtes identiques octet pour octet à celles d'avant, et trois crates sortent du daemon :
le vault, l'hôte MCP et la passerelle Telegram, qui passe au-dessus du daemon. Le daemon
passe de 82 637 à 53 836 lignes. Point connu : chaque requête replie tout le journal de la
session ; la lecture incrémentale est la prochaine tâche du journal.

#### Journal d'événements : la conversation se relit depuis le journal (#208, T14)

- **Nouvelle clé `history.source`** (`journal` par défaut, `tables` pour revenir à la
  lecture d'avant) : chaque requête relit la conversation en pliant le journal
  d'événements de la session (préfixe scellé, mère d'un fork compris) au lieu des
  tables `messages`. Une session que le journal ne sait pas redonner se lit dans les
  tables.
- **Aucune requête ne change** : les seize scénarios enregistrés envoient les mêmes
  requêtes, octet pour octet, sous les deux sources ; en compilation de test, le mode
  `tables` compare chaque lecture à celle du journal et échoue à la première différence.
- Le journal garde désormais l'ordre exact des arguments d'appel et des blocs de
  raisonnement tels que le fournisseur les a envoyés, et le drapeau `eager` d'une
  réponse ; une fille de fork hérite des messages de sa mère sans leur contexte figé,
  comme avant ; l'instantané du prompt système est refait depuis le journal par
  `penelope history reindex`.

#### Crate `penelope-vault` : la mémoire en fichiers hors du daemon (#208, T22)

- Nouvelle crate `penelope-vault`, entre `penelope-app` et le daemon : vault et
  réindexation, wiki de concepts, inventaire, historique git, notes de travail, secrets
  mis de côté, embeddings, retour d'usage, sujet des sessions, découpage et audit de la
  mémoire, revue des tours, épisodes, instantanés mémoire et tuiles du prompt.
- Quatre dépendances circulaires coupées au passage : l'indexation d'une fiche source,
  le commit du vault, la mesure du budget Cœur et la clé du préfixe stable descendent
  vers ce qui les utilise.
- Les tests de la crate n'ouvrent pas de daemon : une session et un provider factice
  suffisent. `penelope-archtest` interdit à la crate de dépendre du daemon ou du canal.
- Le daemon réexporte tout sous les anciens chemins ; `penelope-daemon` passe de
  82 637 à 76 508 lignes.

#### Crate `penelope-mcp-host` : superviseur MCP et OAuth hors du daemon (#208, T25)

- Nouvelle crate `penelope-mcp-host`, au-dessus de `penelope-app` et sous le daemon :
  le superviseur des serveurs de `mcp.d/`, son connecteur de processus et
  l'autorisation OAuth des serveurs HTTP. Elle ne dépend pas du daemon (règle
  d'archtest) ; `McpSupervisor` est `McpGateway` et `McpAdmin`.
- L'autorisation OAuth ne prend plus le daemon : ses services, et pour le serveur de
  retour local un contexte (canal du propriétaire, administration MCP, supervision).
- Le daemon réexporte l'hôte sous `mcp` et `mcp_auth` : CLI, évaluations et tests
  inchangés. `penelope-daemon` passe de 82 637 à 78 275 lignes.

#### Passerelle Telegram en crate `penelope-gateway-telegram` (#208, T29)

- La passerelle Telegram (29 modules, 91 tests, le filet `telegram_e2e` et
  `ticket_to_deploy_e2e`) quitte `penelope-daemon` pour une crate au-dessus de lui ; le
  daemon passe de 82 637 à 64 364 lignes et n'a plus de module `telegram`.
- Le daemon ne la connaît que par ses ports : nouveau port `Gateway`
  (`penelope-app`), `Daemon::run(gateway)` au lieu d'une construction dans le
  superviseur. `penelope-cli` la compose ; archtest refuse toute autre crate qui en
  dépendrait (`GATEWAY_DEPENDENTS`).
- L'ordre de démarrage de l'issue #12 est conservé (propriétaire annoncé avant les
  serveurs MCP, passerelle démarrée après eux) et désormais testé.
- Aucun comportement visible ne change : commandes, cartes, journaux identiques.

### 1.0.0-alpha.6

Cinquième vague de la V1 : la première crate sort du daemon (`penelope-app`, sur laquelle
les suivantes s'appuieront), et le journal sait se vérifier et refaire ses caches. Les
tables restent la source de lecture ; `penelope history verify` et
`penelope history reindex` sont les deux commandes nouvelles.

#### Journal d'événements : vérification et projecteur (#208, T12, T13)

- **`penelope history verify [--session <id>]`** dérive chaque conversation de son
  journal (événements `conv.*`, préfixe d'avant le journal scellé, mère d'un fork) et la
  compare à ses tables : nombre et ordre des messages, contenu de chacun, drapeau de
  compaction, contextes figés, résumés actifs, empreinte du préfixe scellé. Rapport JSON ;
  code de sortie non nul dès la première divergence, qui nomme la session, le nœud et la
  ligne. L'archive d'un `/rewind` est vérifiée contre ce que la coupe a retiré. Méthode
  RPC `history.verify` ; `penelope doctor` vérifie les sessions de la semaine.
- **`penelope history reindex [--session <id>]`** efface les lignes de cache non scellées
  (messages et plein texte, contextes figés, résumés) et les réécrit depuis le journal,
  aux mêmes numéros ; une session que le journal ne sait pas refaire est laissée intacte
  et nommée. Méthode RPC `history.reindex`.
- **Les caches se rattrapent seuls** : à l'ouverture de chaque tour, ce qu'une écriture
  interrompue a laissé derrière le journal est refait depuis le filigrane
  (`projections_session`) ; un rattrapage en échec ne fait pas échouer le tour, il
  apparaît dans `doctor`. Une nouvelle version du pliage refond chaque session à son
  prochain tour.
- Corrigé : dans une session scellée, un message écrit après le scellement recevait une
  adresse de journal fausse (offset lu au mauvais endroit), et après un `/rewind` le
  message suivant héritait du contexte figé du message retiré.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.

#### Crate `penelope-app` : Services, ports et bus sous le daemon (#208, T21)

- Nouvelle crate `penelope-app`, sous le daemon, qui ne dépend que des crates métier
  (règle d'archtest) : `Services` et son assemblage, le bus des tours (`Origin`,
  `TurnOutcome`, `TurnEvent`), l'élicitation MCP, les boucles supervisées, les ports
  (`ProviderSource`, `McpAdmin`, `Messenger`, `McpGateway`, `Orchestrator`,
  `Conversation`, `Compactor`, `ToolExecutor`), les helpers sur `&Services` et les
  doubles de test ; `codex_scope`, `machine` et `media` avec eux.
- La skill livrée `wiki-markdown` suit `reload_skills` dans la nouvelle crate.
- Le daemon réexporte tout sous les anciens chemins : CLI, évaluations et tests
  inchangés. `penelope-daemon` passe de 86 462 à 82 409 lignes.
- `penelope-app` dépend encore de `penelope-telegram` (gabarits et actions de
  `Services`, lien profond, formulaire d'élicitation) jusqu'à T36.

#### Outillage : `make bump` suit le nombre de crates (#208)

`scripts/bump.sh` attendait seize lignes de version dans `Cargo.toml` ; avec
`penelope-app` il y en a dix-sept. Il réécrit maintenant toutes les occurrences de la
version courante et vérifie qu'aucune ne manque. Le plafond du crate daemon descend à
82 637 lignes.

### 1.0.0-alpha.5

Quatrième vague de la V1 : le journal d'événements reçoit tout ce qui change la
conversation (compaction, niveau 1, fork, retour arrière, tentatives), l'historique
d'avant le journal est scellé au premier démarrage, et les modules du daemon ne
reçoivent plus `Daemon` mais des ports. Les tables restent la source de lecture ; deux
comportements changent : le message d'un flux coupé cite le début gardé, et la consigne
de relance après une réponse vide n'accompagne plus que la requête qui suit.
**La migration 0021 scelle la base et supprime deux tables : une base passée en
1.0.0-alpha.5 ne se relit plus en 0.17.**

#### Journal d'événements, scellement de l'historique (#208, T11)

- **L'historique d'avant le journal est scellé au démarrage** : chaque session dont les
  messages n'ont pas d'événement reçoit un seul `conv.import`, qui porte le nombre de
  messages, de contextes figés, les nœuds de résumé actifs et l'empreinte sha256 de ce
  préfixe ; ses lignes sont marquées `sealed`. Aucun message n'est recopié dans le
  journal. L'étape est idempotente : le second démarrage ne scelle rien.
- La dérivation d'une session scellée (`derive`) repart de ce préfixe et redonne la
  projection d'avant, résumé actif compris ; les messages suivants viennent après.
- Migration 0021 : index des lignes à sceller ; les tables `projections_workflow` et
  `projections_approval`, jamais utilisées, sont supprimées.
- Le test de migration depuis une vraie base 0.17 vérifie maintenant le scellement.

#### Journal d'événements : compaction, niveau 1, fork et retour arrière (#208, T7, T8, T10, T18)

- **La compaction entre au journal** : un résumé publié est un `conv.summary` qui
  remplace la plage qu'il couvre (texte du nœud, ancres, résumé prolongé, modèle,
  déclencheur). Le nœud LCM cite son événement et la plage est marquée dans la même
  transaction que lui ; republier le même travail n'écrit rien de plus, et un arrêt
  entre l'événement et le nœud se répare depuis l'événement.
- **Le niveau 1 aussi** : un gros résultat d'outil parti en artefact est un
  `conv.tool_result` qui remplace ce seul nœud, avec l'artefact et son empreinte ; le
  nœud garde sa place.
- **Fork et retour arrière** : `/fork` écrit `conv.fork` en tête de la session fille
  (héritage par référence de toute la mère), `/rewind` écrit `conv.rewind` avant de
  couper. Les adresses d'une fille suivent son héritage (contexte figé, préfixe
  système, lignes copiées).
- **Flux runtime** : un consommateur ne reçoit un `conv.*` que s'il le nomme ou n'a pas
  de filtre, rédigé et borné à 64 Kio ; le catalogue des `conv.*` est dans
  `docs/runtime-events.md`.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.

#### Tentatives hors surface et reprise après crash (#208, T9, T20, #206)

- **Un flux coupé en cours d'écriture n'est plus perdu** : le début reçu est gardé dans
  le journal (`conv.attempt`, cause `stream_cut`), hors de l'historique ; le message au
  propriétaire le cite au lieu de parler d'un début « affiché ». Le tour suivant ne le
  renvoie pas au modèle.
- **Chaque appel sans réponse est relisible** : erreur avant le flux (`before_stream`),
  passage au modèle de repli (`fallback`), réponse vide relancée (`empty_answer`, avec
  la consigne de relance, l'usage et le coût déjà comptés : aucune seconde ligne
  d'usage). Dix tentatives gardées par tour au plus ; toutes sont dans
  `penelope logs --turn` (« tentative sans réponse » : modèle, cause, début du texte).
- La consigne de relance après une réponse vide n'accompagne plus que la requête qui
  suit : elle n'était auparavant retirée qu'en fin de tour.
- **Reprise après crash** : au démarrage, un tour que l'arrêt du processus a laissé
  ouvert est fermé `turn.finished {reason: interrupted}` ; le tour rejoué ouvre sa
  propre borne avec la tentative suivante et reprend l'appel resté sans résultat.

#### Ports du daemon : providers, supervision, canal, MCP (#208, T07 à T10)

- Nouveau module `ports` : `ProviderSource`, `Handle` (ex `DaemonHandle`), `Supervision`,
  `Slot` (branchement posé après le démarrage) et `McpAdmin`.
- Dix-huit modules (vault, concepts, embeddings, épisodes, revue, titres, vision, images,
  voix, purge, sauvegarde, accueil, sessions…) ne prennent plus `&Daemon` : 67 fonctions
  passent à `&Services` et aux ports.
- Plus aucun module hors de la colle ne lit `d.hooks` : canal, livraison, MCP et
  orchestrateur sont reçus en paramètre, par un `Slot` pour les boucles de fond, ou par
  l'état de la compaction et des workflows.
- `McpSupervisor` n'est plus nommé hors de `mcp/` et du superviseur : ses consommateurs
  passent par `McpAdmin`.
- Doubles de test partagés dans `testing` : `RecordingMessenger` remplace sept copies,
  `MockProviders` sert un `MockProvider` comme `ProviderSource`.
- Tests de six modules sortis dans `<module>/tests.rs` ; `hermes::yaml`, les gabarits de
  l'ordonnanceur et `context_view` en sous-modules. Occurrences de `Daemon` 254 → 148.

#### Gel : la liste de référence n'a plus de taille minimale (#208)

`the_budget_file_is_readable` exigeait trente fichiers en dépassement, ce qui empêchait
le cliquet de descendre sous ce nombre ; il vérifie désormais que chaque entrée dépasse
le plafond. Le plafond du crate daemon passe à 86 462 lignes (nouveaux modules `ports`,
`testing`, `history`, tests des lots) ; il redescendra avec l'extraction des crates.

### 1.0.0-alpha.4

Troisième vague de la V1 : les six plus gros fichiers du daemon découpés en modules, et
le journal d'événements écrit en double à côté des tables. La fusion de `main` 0.17.62
apporte le correctif SQLite (transactions `IMMEDIATE`, plus de « database is locked »
entre deux écrivains). Le seul changement visible est la double écriture : le journal
reçoit plus d'événements, et aucune requête envoyée au modèle ne change.

#### Passerelle Telegram en modules (épopée #208, lot G)

- `telegram.rs` (7 925 lignes) et `telegram/screens.rs` (2 437) deviennent `telegram/` :
  vingt-neuf modules, le plus gros à 614 lignes. Les deux fichiers sortent de la liste de
  référence du gel ; tous les chemins `crate::telegram::*` sont inchangés.
- Les 50 commandes `/…` et les 32 écrans ne sont plus deux `match` géants : une fonction
  par commande (`telegram/commands/`) et par écran (`telegram/screens/`), derrière un
  aiguillage court. Deux `#[allow(clippy::too_many_lines)]` disparaissent.
- Aucun comportement visible ne change : textes, boutons, cartes et ordre des envois sont
  ceux de la 1.0.0-alpha.3.

#### Consolidation nocturne découpée en modules (lot G)

`dream.rs` (3 457 lignes) devient le répertoire `dream/` : dix modules de 127 à 633 lignes
(passe et phases, candidats, contradictions, instantané du vault, appel du modèle, lots,
application au vault, ledger, digest, crons). Déplacement pur : aucun comportement ni aucun
chemin public ne change, et le fichier sort de la liste de référence du gel.

#### workflow.rs et executor.rs découpés en modules (lot G)

Les deux fichiers sortent de la liste de référence du gel : `workflow/` (onze modules,
le plus gros à 438 lignes) et `executor/` (treize modules, le plus gros à 520 lignes).
La table de dispatch des outils natifs (1 160 lignes) devient un aiguillage court vers
une fonction par famille d'outils ; son `allow(clippy::too_many_lines)` disparaît.
Déplacement pur, aucune signature publique changée, aucun comportement modifié.

#### Découpage de rpc.rs, mcp.rs et doctor.rs (lot G)

- `rpc.rs` devient `rpc/` : serveur, flux et méthodes rangées par domaine ; la table de
  dispatch de 1 190 lignes devient onze fonctions de moins de 200 lignes et un aiguillage
  sur le préfixe de la méthode.
- `mcp.rs` devient `mcp/` : connecteur, cycle de vie, appels, administration, passerelle,
  rendus.
- `doctor.rs` devient `doctor/` : contrôles rangés par famille (modèles, secrets, machine,
  mémoire, cohérence, MCP).
- Aucun comportement ne change : mêmes méthodes RPC, mêmes contrôles, mêmes tests.

#### Journal d'événements, double écriture (#208, T4, T5, T6)

- **Chaque tour est fermé** : `turn.finished` est écrit sur toutes les sorties de la
  boucle avec `reason` (`answered`, `awaiting_approval`, `cancelled`, `failed`,
  `budget_exceeded`, `loop_aborted`, `calls_exhausted`), y compris un tour tombé avant
  d'appeler le modèle. `turn.started` et `turn.finished` portent `turn_id`,
  `origin_turn`, `kind` et `attempt` ; leur forme vit dans `penelope_context::journal`.
  Un `turn.started` sans fin désigne désormais un arrêt du processus, et rien d'autre.
- **Double écriture de l'historique** : chaque message écrit dans `messages` l'est aussi
  dans le journal (`conv.user`, `conv.assistant`, `conv.tool_result`), l'événement
  d'abord, la ligne ensuite avec son `event_id` (migration 0020). La réponse du modèle
  porte son modèle, son usage, son coût et les empreintes du prompt. Un message de la
  file rejoué après un crash n'est ni réécrit ni rejournalisé.
- **Le prompt système et le contexte figé entrent au journal** : `conv.system` porte le
  texte entier du préfixe, une fois par changement (`first`, `cold`, `compaction`) ;
  `conv.context` le bloc volatil figé avec son message.
- Les tables restent la source de lecture : aucune requête envoyée au modèle ne change.
- Pas encore portés par les événements : `llm_request_id` et `projection.steps` de la
  réponse du modèle, `turn` et `step` du résultat d'outil.
- Les scénarios masquent `tokens_est` (`{{tokens}}`) quand l'objet cite la racine
  temporaire : sa longueur change d'une machine à l'autre, et le résultat d'outil
  désormais journalisé la contient.

#### Gel : la règle R5 ne compte que les modules de premier niveau du daemon (#208)

La liste blanche des modules du daemon ne lisait pas que `lib.rs` : chaque découpage en
sous-modules la faisait grossir. Elle ne compte plus que les `mod x;` de `lib.rs`.
Le plafond du crate daemon passe à 84 819 lignes (en-têtes des nouveaux modules et double
écriture) ; il redescendra avec l'extraction des crates.

### 1.0.0-alpha.3

Deuxième vague de la V1 : quatre lots développés en parallèle, chacun dans son worktree, et
la fusion de `main` 0.17.61 (jobs d'outils durables, #204). Les filets de la vague
précédente ont vu exactement ce que #204 changeait et rien d'autre (trois clés de
configuration, la méthode RPC `jobs`, `tool_jobs_lost` dans la reprise, quatre outils
`job_*` à la demande) : leurs attendus sont régénérés. Aucun comportement de production ne
change dans les lots eux-mêmes.

#### La branche v1 outillée : cliquet du budget en CI, fusion de main scriptée, critères de bascule vérifiés (#208, #209, #213)

Le cliquet de `budget.toml` n'était tenu qu'en local, la fusion de `main` dans `v1`
conflictait à chaque bump sur les seize lignes de version, et rien ne disait quand `v1`
pourrait devenir `main`. Le job `tests` de la CI lit tout l'historique et lance
`scripts/check-budget.sh` contre le commit d'avant (push) ou le point de fourche (pull
request) : une borne qui remonte sans `Dérogation-budget: #N` rend la CI rouge.
`scripts/sync-main.sh` fusionne `origin/main` dans `v1`, résout `Cargo.toml` et
`Cargo.lock` en gardant la `1.0.0-alpha.N` de v1 et les autres changements de main, et
ne commite que si `penelope-archtest` est vert (sinon il dit comment inscrire l'apport
de main avec le trailer) ; `--dry-run` montre les conflits sans rien toucher.
`scripts/switch-check.sh` vérifie les critères mesurables de la bascule (CI de v1,
budget sans dette, critères d'acceptation, fixture de migration, scénarios) et liste
ce qui manque. Le plancher de `tests/ca_matrix.rs` est désormais la liste figée
`[ca].required`, et `make bump` sur une `1.0.0-alpha.N` ne promet plus de release.

#### Une seule famille kv, et les helpers partagés sortent de leurs modules (T05, T06)

- `Services::kv_get`, `kv_set`, `kv_delete` remplacent `Daemon::kv_*` et
  `workflow::kv_get` / `kv_set`, qui dupliquaient la même requête ; 249 appels migrés,
  aucune clé renommée.
- Nouveau module `helpers` du daemon : clés kv du canal, `deep_link`, `seen_chats`,
  `vault_dir`, `local_now`, `owner_origin_of`, `round_usd`, `set_config_path`,
  `running_binary`, `is_source_build`, `last_model_key`, `step_done_key`. Six cycles de
  modules tombent (dream, compaction, selfknow, doctor, scheduler, session_project ne
  citent plus telegram, engine ni rpc).
- `deep_link`, `set_config_path` et l'origine du propriétaire prennent `&Services` ;
  `Services::publish_config` porte la publication de configuration.
- Gel : `helpers` est le seul module ajouté (dérogation #208) ; occurrences de `Daemon`
  261 → 254 ; telegram.rs, rpc.rs, workflow.rs, engine.rs, scheduler.rs, upgrade.rs et
  conversation.rs abaissés dans la liste de référence.

#### Journal d'événements, briques pures (#208, T1, T2, T3, T21)

- **`EventLog::append_in` et `append_with`** : un événement dans la transaction de
  l'appelant, ou un événement commité puis une seconde transaction (`after`) sur le même
  thread écrivain, sous le verrou d'ordre (#162), avant la diffusion. Une erreur de
  `after` laisse l'événement dans le journal et remonte. Les trois chemins d'écriture
  partagent la même insertion : une lecture en erreur ne forge jamais de maillon (#47).
- **Vocabulaire `conv.*`** (`penelope_context::journal`) : dix kinds (`conv.system`,
  `user`, `context`, `assistant`, `tool_result`, `attempt`, `summary`, `rewind`, `fork`,
  `import`), payloads typés, `"v": 1`, opération de surface (`append`, `replace`, `cut`,
  `inherit`, `seal`). Relecture stricte : un `v` futur, un kind inconnu non marqué
  `ignorable`, une opération qui ne va pas avec son kind sont refusés.
- **Pliage pur** (`penelope_context::derive`) : `derive(préfixe, événements) -> Surface`,
  sans base ; adresses `offset + seq` avec des trous ; résumé, niveau 1 (l'adresse reste),
  nouveau système, coupe, fork par référence récursif, préfixe V0 scellé, tentative dont
  la consigne de relance entre dans la requête suivante, note de fusion. Conversion en
  entrées (`compacted` = masqué par un résumé) et en requête, textes identiques à la
  projection V0. Un journal incohérent est une erreur ; seule une purge rend le pliage
  indulgent.
- **Numérotation à trous** : `SummaryJob` porte le nombre d'entrées de son lot et la fin
  du résumé qu'il prolonge ; `messages()` et les bornes montrées au résumeur ne se
  déduisent plus de `to - from + 1`. `numbering::uncovered` et `node_page` travaillent sur
  les adresses existantes.
- Rien n'est branché dans le daemon : la double écriture vient avec T5.

#### Boucle d'agent en modules (épopée #208, lot F : T02 à T06)

- `agent.rs` (2 513 lignes) devient `agent/` : douze modules, le plus gros à 571 lignes,
  tous les chemins `crate::agent::*` inchangés. Le fichier sort de la liste de référence
  du gel.
- La politique de nouvelles tentatives d'un appel au modèle est une table pure,
  `RetryPlan`, testée cas par cas ; `call_model` repasse sous 200 lignes.
- Avant chaque appel au modèle, quatre gardes nommées dans un ordre fixe : arrêt demandé,
  budget, plafond d'appels, palier de coût.
- Avant la politique, quatre gardes d'appel nommées : liste blanche, décision antérieure,
  garde de boucle, arguments ; les textes de refus sont typés (`Refusal`) et inchangés.
- La politique d'un appel dit quelle couche a tranché (`VerdictLayer`) ; ses huit raisons
  sont fixées par un test doré.
- Aucun comportement visible ne change : textes, cartes, événements et ordre des
  contrôles sont ceux de la 1.0.0-alpha.2.

### 1.0.0-alpha.2

Fin du lot A (gel de la dette) et fin du premier jalon du lot B (filets) sur `v1`. Aucun
comportement de production ne change : des tests changent de fichier, des lints et des
filets s'ajoutent.

#### Gel de la dette : tests inline sortis, lints de workspace, dépendance morte (#215, #211, #214)

- Les 269 tests inline des sept fichiers de plus de 3 000 lignes de `penelope-daemon`
  (`telegram.rs`, `dream.rs`, `agent.rs`, `workflow.rs`, `executor.rs`, `mcp.rs`,
  `engine.rs`) sont dans des fichiers frères, `<module>/tests.rs` ou
  `<module>/tests/<thème>.rs` (plus `mcp/testing.rs` pour les faux serveurs MCP et
  `agent/clone_policy_tests.rs`), par déplacement pur : même nombre de tests (538 dans
  le daemon, 1 777 dans le dépôt), aucune ligne de code de production déplacée, aucune
  signature changée. `telegram.rs` passe de 13 716 à 7 950 lignes, `dream.rs` de 5 859
  à 3 455, `agent.rs` de 4 240 à 2 482, `workflow.rs` de 4 044 à 2 914, `executor.rs`
  de 3 712 à 2 479, `mcp.rs` de 3 291 à 1 958, `engine.rs` de 3 173 à 1 220. Aucun
  fichier de tests créé ne dépasse 1 231 lignes.
- `clippy::too_many_lines` à 200 lignes dans tout le workspace (`[workspace.lints]`,
  `[lints] workspace = true` dans les 17 crates, `clippy.toml`) : une fonction nouvelle
  de plus de 200 lignes fait échouer `cargo clippy -- -D warnings`, la sortie est de la
  découper. Les 27 fonctions existantes au-dessus (23 dans le code, 4 tests de bout en
  bout) portent `#[allow(clippy::too_many_lines)] // gel 0.17 : <raison>`, comptés par
  archtest (`budget.toml [lints].allow_too_many_lines = 27`).
- `penelope-workflow` ne dépend plus de `penelope-telegram`, déclaré et jamais utilisé.

Le budget d'archtest est resserré dans le même lot (`UPDATE_BUDGET=1`) : les sept fichiers
descendent dans la liste de référence ; les six fichiers de la liste qui reçoivent un
`#[allow]` gagnent une ligne, inscrite avec `Dérogation-budget: #209` ; les modules de tests
`clone_policy_tests` et `testing` entrent dans la liste blanche ; `EffectKind::Telegram`
d'`agent.rs`, déjà là, devient visible pour la frontière canal/cœur une fois les tests sortis.

#### Filets de bout en bout sur l'API publique (#208, lot B)

Les filets avant découpage (épopée #208, tâches T8 et T9 de `design/v1/gel-et-outillage.md`) :
trois tests de bout en bout sur l'API publique du daemon, qui survivent aux déplacements
internes de la refonte.

- **Telegram de bout en bout** (`tests/telegram_e2e.rs`, CA 14.2) : un message du
  propriétaire sur un transport simulé donne une réponse (`tg_outbox`), un effet `completed`
  dans le ledger, les événements du tour ; le daemon reconstruit sur le même répertoire
  retrouve tout et ne rejoue rien (ni l'outil, ni l'envoi, ni l'update).
- **Approbation de bout en bout** (`tests/approval_e2e.rs`, CA 9.4 à 9.6) : un outil à
  risque arrête le tour sur une demande ; approuver puis reprendre exécute l'outil une
  fois ; refuser transmet « Non exécuté » au modèle sans rien écrire ; « pour cette
  session » crée une règle bornée à la session, un second appel passe sans demande, une
  autre session redemande.
- **Contrat RPC doré** (`crates/penelope-evals/tests/rpc_golden.rs`) : 104 fichiers
  `golden/<méthode>.json`, un par méthode de `method::ALL`, clés et types comparés à chaque
  test ; `UPDATE_GOLDEN=1` régénère et liste les cas d'erreur figés (24, dont les onze
  `mcp.*` sans superviseur). Une clé retirée d'une réponse est rouge.
- Matrice des CA régénérée : 75 tests d'acceptation.

#### Scénarios : redémarrage sans course et harnais sous le plafond (#208, lot B)

La CI macOS de la `1.0.0-alpha.1` a vu le scénario « crash en deux vies » rouvrir la base
pendant que la vie précédente relâchait encore son verrou SQLite (« database is locked »).
Le harnais relâche désormais le daemon avant le crash, attend que plus rien ne tienne les
services et que le `-wal` retombe à zéro, et réessaie une base encore verrouillée ; un
scénario `redemarrages-en-serie` enchaîne dix redémarrages. Le harnais, passé à 1 086
lignes, est découpé en quatre fichiers par déplacement pur : le gel l'avait attrapé.

La charte (`design/v1/README.md` §9) cesse de réserver 0011 et 0012, pris sur `main` par
#205 et #204 ; la décision sur le journal source unique prendra 0017.

### 1.0.0-alpha.1

Premier lot de la branche `v1` : fusion de cinq branches développées en parallèle dans la
nuit du 23 au 24 septembre, chacune dans son worktree et son périmètre de fichiers. Le gel
de la dette (lot A de l'épopée #208 : #209, #210, #212, #213, #214, #216 ; #211 et #215
suivent) et deux filets du lot B. Point de départ : la `0.17.59` de `main`. Ce lot ne
change aucun comportement de production : il pose des règles, des gardes et des tests.

#### Le gel écrit là où les sessions le lisent (#216)

- **`CLAUDE.md`** : sections « Gel de la dette » (règles R1 à R8 et frontière canal/cœur
  en une phrase chacune, `UPDATE_BUDGET=1`, `scripts/check-budget.sh`, le trailer
  `Dérogation-budget: #N` et ses trois cas légitimes, « un nouveau module ou une
  fonctionnalité va dans `v1` », « aucun tag `v1*` avant la bascule ») et « Travailler sur
  v1 » (versions `1.0.0-alpha.N` jamais taguées, un seul `progress.md`, fusion `main` →
  `v1` après chaque release, règle des 24 h). `AGENTS.md` ne fait plus que renvoyer à
  `CLAUDE.md`.
- **Modèle de pull request** : trois cases de gel (budget inchangé ou abaissé, aucun
  nouveau module dans le daemon, scénario ajouté ou mis à jour si le visible change).
- **`.github/workflows/README.md`** : trois workflows, tests sur Linux et macOS, tag posé
  par la CI (#147) ; la section « Poser un tag » à la main disparaît.
- **Décision [0015](decisions/0015-gel-0.17-et-branche-v1.md)** : gel de la 0.17 et
  branche `v1`. La table des décisions passe de 0008 à 0015 (0012 à 0014 et 0016
  réservés).
- **Deux incohérences corrigées** : le noyau d'outils compte 16 entrées (19 définitions
  avec les méta-outils), pas 17, depuis que `workflow_start` est passé à la demande
  (0.17.56) ; `telegram.max_fragments` n'est plus « sans effet », elle borne les avis
  internes (digest, veille) depuis la 0.17.27.

#### Gardes de version pour deux branches (#212)

La V1 vit sur une branche `v1` versionnée `1.0.0-alpha.N`, jamais taguée, pendant que la
0.17 continue de livrer sur `main`. Trois mécanismes ne le supportaient pas :
`parse_version` coupait le suffixe, donc un tag `v1.0.0-alpha.1` posé par erreur aurait
valu `1.0.0 > 0.17.59` et toutes les instances 0.17 l'auraient installé à leur prochaine
vérification ; le test `docs` n'acceptait que trois entiers ; la CI ne tournait que sur
`main`.

`penelope upgrade` ne juge plus jamais une version à suffixe « plus récente » : elle sort
de la liste des candidates et ne s'installe que par `--tag` (c'est ainsi que la
`1.0.0-rc.1` s'installera à la bascule) ; une instance dont le binaire est lui-même une
pré-release n'est pas rétrogradée vers une 0.17. Le test `docs` lit `x.y.z-<pré>` dans
l'ordre semver (`1.0.0-alpha.2 < 1.0.0-alpha.10 < 1.0.0-rc.1 < 1.0.0`) et refuse un bloc
`1.0.0-alpha.N` quand le workspace est en 0.17 (« section V1 sur main »). `ci.yml` tourne
sur `main` et `v1`, vérifie que chaque branche porte sa ligne de version (`0.` sur `main`,
`1.0.0-` sur `v1`) et réserve toujours `livraison` à `main` ; `release.yml` refuse tout
tag `v1*` tant que la variable de dépôt `V1_RELEASES` ne vaut pas `1`, `workflow_dispatch`
compris. Le `workflow_dispatch` réel avec `tag=v1.0.0-alpha.0` reste à rejouer par le
propriétaire.

#### Le gel de la dette est mécanique : budget.toml, huit règles, un cliquet (#209, #210, #213, #214)

Rien n'arrêtait la croissance : 42 fichiers au-dessus de 1 000 lignes portent 56 % du
workspace, `penelope-daemon` en fait 48 % à lui seul, 41 de ses fichiers nomment le type
`Daemon` et le cœur nomme Telegram dans 32 fichiers hors passerelle. `penelope-archtest`
vérifiait les dépendances, les motifs propres à un OS et `unsafe`, jamais une taille, un
module ou un couplage, et ne lisait que `src/`.

`crates/penelope-archtest/budget.toml` fige tout cela sur la mesure : plafond de 1 000
lignes par fichier source (tests inline compris) et 1 500 par fichier de tests, liste de
référence des 42 fichiers en dépassement avec leur borne, `penelope-daemon/src` à 81 168
lignes, liste blanche de ses 62 modules, budget d'occurrences de `Daemon` par fichier et
`impl Daemon` dans quatre fichiers, allows de `clippy::too_many_lines` comptés, les 70
critères `ca_*` figés (renommer interdit, déplacer permis), et la frontière canal / cœur
avec sa liste par fichier. Chaque règle a son test sur le workspace et son test de
détecteur ; `archtest` parcourt désormais `src/`, `tests/` et `examples/`. Les nombres ne
montent jamais : `UPDATE_BUDGET=1 cargo test -p penelope-archtest` resserre le fichier
vers le bas seulement, et `scripts/check-budget.sh`, à brancher en CI au prochain lot,
refuse toute remontée par rapport à la base git, sauf un commit portant
`Dérogation-budget: #<issue>`. Un seul relevé manuel dans ce lot : `upgrade.rs` passe de
1 926 à 2 034 lignes dans la liste, parce que #212 a ajouté ses tests avant le gel.

#### Filets de migration et de configuration (#208, lot B)

- **Une vraie base 0.17 dans le dépôt** : `crates/penelope-store/tests/fixtures/penelope-0.17.59.db`
  (216 064 octets, pages de 1 Kio), produite par la 0.17.59 : session `chat` liée à
  Telegram, douze messages dont deux résultats d'outil, index plein texte, tour terminé,
  nœud LCM actif, contexte figé, sept événements chaînés par `EventLog`, un effet
  `completed` par `EffectLedger`, usage et requête, souvenir avec provenance,
  planification, outbox envoyée, approbation décidée, règle, clés `kv`, génération de
  configuration. Le test `a_real_0_17_database_migrates_and_reads_back` ouvre une copie par
  `Store::open` et vérifie les migrations, le schéma (identique à une base neuve, table
  par table et index par index), les 45 tables, chaque donnée semée, la chaîne
  d'événements avant et après un événement de plus, l'intégrité. Jusque-là
  `upgrade_from_each_previous_version` ne rejouait que `0001`, sans base réelle.
- **Régénération** : `UPDATE_FIXTURE=1 cargo test -p penelope-store --test migration_from_0_17 -- --ignored`,
  une fois, quand la 0.17 finale est connue (`tests/fixtures/README.md`).
  `penelope-kernel` en dev-dependency de `penelope-store` pour semer et vérifier la
  chaîne par les vraies primitives ; le graphe livré ne change pas.
- **Configuration 0.17 complète relue** : `crates/penelope-kernel/tests/fixtures/config-0.17.toml`
  porte les 222 clés de la référence générée d'`install-headless.md` ; `config_0_17.rs`
  la charge sans clé inconnue, valide, sans contradiction, et refuse que le fichier prenne
  du retard sur la référence (la clé manquante est nommée).

#### Scénarios de session rejouables sans clé (#208, lot B)

- **Le format** (règle R11) : un répertoire `crates/penelope-evals/scenarios/<nom>/` par
  scénario, avec `scenario.toml` (messages, commandes `/compact` `/fork` `/rewind`
  `/purge`, avance de l'horloge, redémarrage, crash, approbation, configuration patchée,
  fichiers semés, serveur MCP simulé), `model.jsonl` (une réponse scriptée par appel de
  modèle) et deux attendus régénérés par `UPDATE_SCENARIOS=1` : `expected.jsonl`, le monde
  après le run (sessions, messages, résumés, événements, tours, effets, requêtes LLM,
  approbations, artefacts, usage, fichiers, audit), et `surface.jsonl`, chaque requête vue
  par le modèle. Identifiants, horodatages, chemins et hachages deviennent des jetons
  (`{{session:1}}`, `{{turn:1}}`, `{{ts+600s}}`, `{{home}}`, `{{hash}}`) : deux rejeux
  sont identiques octet pour octet, un test le vérifie.
- **Quinze scénarios** verts sans clé dans `cargo test --workspace` (suite `scenarios`) :
  tour simple, appel d'outil de lecture, lectures parallèles, niveau 1 et admission de
  groupe, compaction manuelle puis prolongation, session froide, dépassement prouvé, flux
  coupé, réponse vide relancée, messages fusionnés, fork, rewind, purge, crash en deux
  vies, approbation après redémarrage. Un diff nomme le scénario, le groupe de lignes ou
  l'appel qui divergent et dit comment régénérer.
- **`RECORD_SCENARIO=<nom>`** enveloppe le vrai fournisseur et réécrit `model.jsonl` :
  implémenté, pas encore exercé (aucune clé cette nuit).
- `penelope-evals` dépend de `toml`, `tempfile` et `async-trait`, déjà dans le workspace.

## Série 0.17

Les notes des versions 0.17.x, le résumé et les étapes du PRD sont archivés dans
[progress-0.17.md](progress-0.17.md).
