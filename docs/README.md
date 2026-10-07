# Documentation de Pénélope

Où lire quoi. Chaque page décrit la version qui l'accompagne ; Pénélope lit la même
documentation, embarquée dans son binaire (`self_docs`). Le test `docs` vérifie que cet
index cite chaque page, qu'aucun lien n'est mort et que les références de configuration et
d'outils sont à jour.

## Par besoin

| Besoin | Où lire |
|---|---|
| Installer et mettre à jour | [Compiler et installer](install-headless.md#2-compiler-et-installer), [Diagnostic](install-headless.md#3-diagnostic), [Démarrer et surveiller](install-headless.md#8-démarrer-et-surveiller), [Mise à jour](install-headless.md#10-mise-à-jour), [Passer aux releases](install-headless.md#passer-dune-installation-source-aux-releases), [Signature locale](install-headless.md#signature-locale) |
| Configurer | [Configuration](install-headless.md#5-configuration), [Référence des clés](install-headless.md#référence-des-clés), [Secrets](install-headless.md#4-secrets) |
| Choisir les modèles | [Modèles](install-headless.md#6-modèles), [Qui répond à un message](install-headless.md#qui-répond-à-un-message), [Inférence locale sur Mac](install-headless.md#inférence-locale-sur-mac), [OpenRouter](install-headless.md#openrouter), [Codex](install-headless.md#codex--les-modèles-dun-abonnement-chatgpt) |
| Utiliser sur Telegram | [Commandes](telegram.md#commandes), [Gabarits](telegram.md#gabarits), [Formulaires](telegram.md#formulaires), [Sujets](telegram.md#sujets), [Signes de vie et trace des outils](telegram.md#signes-de-vie), [Messages vocaux](install-headless.md#messages-vocaux), [Photos et documents](install-headless.md#photos-et-documents) |
| Écrire ou lancer un workflow | [Fichier](workflows.md#fichier), [Types d'étapes](workflows.md#les-dix-types-détapes), [Transitions](workflows.md#transitions), [Workflows livrés](workflows.md#workflows-livrés), [Lancer, planifier, suivre](workflows.md#lancer-planifier-suivre), [Plan approuvé exécuté en phases](workflows.md#plan-approuvé-exécuté-en-phases), [Livraison en dev](workflows.md#livraison-en-dev), [Gate de production](workflows.md#gate-de-production), [Workflows sur la machine](install-headless.md#workflows) |
| Brancher des serveurs MCP | [Déclarer un serveur](mcp.md#déclarer-un-serveur), [Risque et approbation](mcp.md#risque-et-approbation), [OAuth 2.1](mcp.md#oauth-21), [Serveurs MCP sur la machine](install-headless.md#serveurs-mcp), [Agenda CalDAV](install-headless.md#agenda-caldav), [L'agenda, un serveur livré](mcp.md#lagenda-caldav-un-serveur-livré), [Venir d'Hermes](install-headless.md#venir-dhermes) |
| Mémoire et vault | [Mémoire qui apprend](install-headless.md#mémoire-qui-apprend), [Rappels et tâches planifiées](install-headless.md#rappels-et-tâches-planifiées), [Retrouver une conversation](install-headless.md#retrouver-une-conversation), [Sauvegarde et audit](install-headless.md#9-sauvegarde-et-audit), [Sauvegarde complète chiffrée](install-headless.md#sauvegarde-complète-chiffrée-hors-de-la-machine), [Remonter une instance](install-headless.md#remonter-une-instance-sur-une-machine-neuve) |
| Coûts et budget | [Coûts](install-headless.md#coûts), [Gros résultats d'outils](install-headless.md#gros-résultats-doutils), [Longues conversations](install-headless.md#longues-conversations), [Le cache](context.md#le-cache), [Cache de prompt](decisions/0008-cache-de-prompt.md) |
| Travaux longs sans bloquer le tour | [Jobs d'outils](install-headless.md#jobs-doutils), [Jobs durables](decisions/0012-jobs-outils-durables.md) |
| Relire une requête envoyée | [Relire ce que le modèle a lu](context.md#relire-ce-que-le-modèle-a-lu), [Prompt système journalisé](decisions/0011-prompt-systeme-journalise.md) |
| Observer le daemon en direct | [Flux runtime](runtime-events.md#flux-dévénements-runtime), [Contrat](runtime-events.md#contrat), [Démonstration Pathlayer](runtime-events.md#démonstration-pathlayer) |
| Comprendre la compression de contexte | [Ce qu'un appel envoie](context.md#ce-quun-appel-envoie), [Les chiffres par fenêtre](context.md#les-chiffres-par-fenêtre), [Une session jusqu'à la cinquième compaction](context.md#une-session-du-premier-tour-à-la-cinquième-compaction), [Quand le résumé échoue](context.md#quand-le-résumé-échoue), [Le cache](context.md#le-cache) |
| Sécurité | [Secrets](install-headless.md#4-secrets), [Shell et bac à sable](install-headless.md#shell-et-bac-à-sable), [Le juge d'approbation](install-headless.md#le-juge-dapprobation), [Garder un jeu de décisions](install-headless.md#garder-un-jeu-de-décisions), [Outils natifs](install-headless.md#outils-natifs), [Risque et approbation MCP](mcp.md#risque-et-approbation) |
| Ce que Pénélope sait d'elle-même et de sa machine | [Inventaire et documentation](install-headless.md#ce-que-pénélope-sait-delle-même), [Ce que Pénélope sait de sa machine](install-headless.md#ce-que-pénélope-sait-de-sa-machine), [La carte de l'environnement](install-headless.md#la-carte-de-lenvironnement) |
| Comprendre le code | [Couches](architecture.md#les-couches), [Crates](architecture.md#les-crates), [Ports](architecture.md#les-ports), [Règles d'architecture](architecture.md#les-règles-darchitecture), [Découpage du daemon](decisions/0013-decoupage-du-daemon.md), [Boucle en pipeline](decisions/0014-boucle-pipeline.md) |
| Se situer face aux autres agents | [Comparaison avec OpenClaw et Hermes](comparaison.md) (relevé du 18/09/2026, non revérifié depuis) |
| État d'avancement | [Avancement](progress.md#version-1), [Ce qui reste](architecture.md#ce-qui-reste), [Ce qui n'est pas encore branché](install-headless.md#11-ce-qui-nest-pas-encore-branché), [Critères d'acceptation](ca-matrix.md), [Archive de la 0.17](progress-0.17.md#résumé) |

## Commandes en ligne

Ce que `penelope --help` liste à la 1.0.22, et où chaque commande est expliquée. Toutes
acceptent `--home <répertoire>` (ou `PENELOPE_HOME`), `--json` et `--timeout <s>` ; celles
marquées « sans daemon » lisent ou écrivent l'état elles-mêmes, les autres parlent au
daemon par la socket locale.

| Commande | Ce qu'elle fait | Où lire |
|---|---|---|
| `install`, `uninstall`, `start`, `stop`, `restart`, `daemon` | le service `launchd` ; `daemon` lance le processus au premier plan | [Compiler et installer](install-headless.md#2-compiler-et-installer), [Démarrer et surveiller](install-headless.md#8-démarrer-et-surveiller) |
| `status`, `metrics`, `logs [--turn\|--session]`, `paths` | état, compteurs Prometheus, journaux JSON filtrés, répertoires effectifs | [Démarrer et surveiller](install-headless.md#8-démarrer-et-surveiller) |
| `doctor` | diagnostic complet : configuration, secrets, modèles, MCP, machine, mémoire, veille | [Diagnostic](install-headless.md#3-diagnostic) |
| `chat [message]`, `onboard [partie]` | converser en SSH ; l'entretien d'accueil | [Premier essai en CLI](install-headless.md#7-premier-essai-en-cli) |
| `config get\|set\|status\|reload\|validate` | la configuration ; `validate` sans daemon | [Configuration](install-headless.md#5-configuration), [Référence des clés](install-headless.md#référence-des-clés) |
| `secret list\|backend\|set\|rm` | le magasin de secrets ; `set` lit la valeur à l'invite, jamais en argument | [Secrets](install-headless.md#4-secrets) |
| `model list\|set\|auth` | catalogue, alias, connexion d'un fournisseur à compte (`codex`) | [Modèles](install-headless.md#6-modèles), [Codex](install-headless.md#codex--les-modèles-dun-abonnement-chatgpt) |
| `local install\|status\|uninstall` | `mlx_lm.server` en LaunchAgent sur l'adresse d'un endpoint ; sans daemon | [Inférence locale sur Mac](install-headless.md#inférence-locale-sur-mac) |
| `usage [--by session\|turn\|model\|day\|role\|provider\|upstream\|run\|miss]` | consommation et coûts facturés, ratés de cache par cause | [Coûts](install-headless.md#coûts), [Le cache](context.md#le-cache) |
| `session list\|new\|close\|title\|fork\|rewind\|budget\|model\|compact\|mode\|project\|purge\|export` | les sessions | [Retrouver une conversation](install-headless.md#retrouver-une-conversation), [Choisir le modèle d'une session](install-headless.md#choisir-le-modèle-dune-session), [Longues conversations](install-headless.md#longues-conversations), [Effacer une conversation](install-headless.md#effacer-une-conversation) |
| `approvals [stats]`, `approve`, `deny`, `policies` | cartes en attente, décision, règles « toujours » ; `stats` mesure les cartes sans motif, sans daemon | [Shell et bac à sable](install-headless.md#shell-et-bac-à-sable), [Mesurer avant d'activer le juge](install-headless.md#mesurer-avant-dactiver-le-juge) |
| `dataset export --kind approvals` | le jeu de décisions du juge en JSONL, sans daemon | [Garder un jeu de décisions](install-headless.md#garder-un-jeu-de-décisions) |
| `jobs [--all]` | les appels d'outils sortis de leur tour | [Jobs d'outils](install-headless.md#jobs-doutils) |
| `mcp list\|show\|add\|edit\|rm\|enable\|disable\|restart\|auth\|test\|logs` | les serveurs de `mcp.d` | [Serveurs MCP](install-headless.md#serveurs-mcp), [Déclarer un serveur](mcp.md#déclarer-un-serveur), [OAuth 2.1](mcp.md#oauth-21), [Diagnostiquer un serveur](mcp.md#diagnostiquer-un-serveur-qui-ne-démarre-pas) |
| `agenda-mcp` | le serveur MCP d'agenda CalDAV, en lecture, sur stdio ; lancé par Pénélope depuis `mcp.d/agenda.toml`, sans daemon | [Agenda CalDAV](install-headless.md#agenda-caldav), [L'agenda, un serveur livré](mcp.md#lagenda-caldav-un-serveur-livré) |
| `wf list\|show\|validate\|runs\|run\|trace\|control` | workflows et runs ; `validate` sans daemon | [Workflows sur la machine](install-headless.md#workflows), [Lancer, planifier, suivre](workflows.md#lancer-planifier-suivre), [Cycle de vie d'un run](workflows.md#cycle-de-vie-dun-run) |
| `schedule list\|add\|pause\|resume\|rm\|run\|move` | les déclencheurs planifiés | [Rappels et tâches planifiées](install-headless.md#rappels-et-tâches-planifiées) |
| `mem search\|show\|history\|restore\|reindex\|forget\|candidates\|split\|audit\|retry-rejected\|reclaim\|diff\|dream\|learned\|signals` | la mémoire et sa consolidation | [Mémoire qui apprend](install-headless.md#mémoire-qui-apprend) |
| `vault sync\|check\|lint` | git, contrôle et lint du wiki | [Mémoire qui apprend](install-headless.md#mémoire-qui-apprend), [Diagnostic](install-headless.md#3-diagnostic) |
| `skill list\|show\|rollback\|reload\|install` | les skills, depuis un dossier ou un dépôt GitHub | [Déposer une skill](install-headless.md#déposer-une-skill), [Importer des skills d'un dépôt](install-headless.md#importer-des-skills-dun-dépôt) |
| `import hermes` | skills, mémoire et serveurs MCP d'une instance Hermes | [Venir d'Hermes](install-headless.md#venir-dhermes) |
| `audit show [--turn]`, `audit-verify` | ce que le modèle a lu ; la chaîne d'audit | [Relire une requête envoyée](install-headless.md#relire-une-requête-envoyée), [Sauvegarde et audit](install-headless.md#9-sauvegarde-et-audit) |
| `history verify\|reindex` | la conversation dérivée du journal contre ses caches | [Remonter une instance](install-headless.md#remonter-une-instance-sur-une-machine-neuve), [Le journal, source unique](architecture.md#le-journal-source-unique) |
| `backup [--local --db --media]`, `backup setup\|kit`, `restore [source]` (`restore-all`) | sauvegarde complète chiffrée vers un fournisseur, kit de secours, restauration sur machine neuve | [Sauvegarde et audit](install-headless.md#9-sauvegarde-et-audit), [Sauvegarde complète chiffrée](install-headless.md#sauvegarde-complète-chiffrée-hors-de-la-machine), [Remonter une instance](install-headless.md#remonter-une-instance-sur-une-machine-neuve) |
| `export session\|run\|all`, `store rebuild` | export JSONL ; reconstruction des index dérivés | `penelope export --help`, `penelope store --help` |
| `upgrade [--check\|--rollback\|--tag\|--force\|--switch]` | mise à jour depuis les releases, retour arrière | [Mise à jour](install-headless.md#10-mise-à-jour), [Passer aux releases](install-headless.md#passer-dune-installation-source-aux-releases) |
| `eval <suite>` | une suite d'évaluation depuis les sources, sans daemon | [Tests](../README.md#tests), [Inférence locale sur Mac](install-headless.md#inférence-locale-sur-mac) |

## Par fichier

- [install-headless.md](install-headless.md) : installer, configurer et exploiter Pénélope sur un Mac sans écran ; référence des clés de configuration et des outils natifs. Sections : [Prérequis](install-headless.md#1-prérequis), [Compiler et installer](install-headless.md#2-compiler-et-installer), [Secrets](install-headless.md#4-secrets), [Configuration](install-headless.md#5-configuration), [Modèles](install-headless.md#6-modèles), [Inférence locale sur Mac](install-headless.md#inférence-locale-sur-mac), [Premier essai en CLI](install-headless.md#7-premier-essai-en-cli), [Mise à jour](install-headless.md#10-mise-à-jour).
- [telegram.md](telegram.md) : le canal Telegram, rendu, gabarits, boutons, formulaires et catalogue des commandes. Sections : [Deux rendus, un seul arbre](telegram.md#deux-rendus-un-seul-arbre), [Gabarits](telegram.md#gabarits), [Boutons et jetons](telegram.md#boutons-et-jetons), [Commandes](telegram.md#commandes), [Signes de vie](telegram.md#signes-de-vie), [Limites actuelles](telegram.md#limites-actuelles).
- [workflows.md](workflows.md) : référence complète du schéma de workflow et du cycle de vie d'un run. Sections : [Fichier](workflows.md#fichier), [Métadonnées](workflows.md#métadonnées), [Réglages](workflows.md#réglages), [Types d'étapes](workflows.md#les-dix-types-détapes), [Transitions](workflows.md#transitions), [Reprise](workflows.md#reprise), [Workflows livrés](workflows.md#workflows-livrés), [Plan approuvé exécuté en phases](workflows.md#plan-approuvé-exécuté-en-phases), [Livraison en dev](workflows.md#livraison-en-dev), [Gate de production](workflows.md#gate-de-production).
- [mcp.md](mcp.md) : le client MCP, versions de protocole, transports, OAuth, registre paresseux, élicitation et le serveur d'agenda livré. Sections : [Versions de protocole](mcp.md#versions-de-protocole), [Transports](mcp.md#transports), [Déclarer un serveur](mcp.md#déclarer-un-serveur), [L'agenda CalDAV](mcp.md#lagenda-caldav-un-serveur-livré), [Registre paresseux](mcp.md#registre-paresseux), [OAuth 2.1](mcp.md#oauth-21), [Élicitation](mcp.md#élicitation).
- [context.md](context.md) : la compression de contexte de bout en bout, seuils, queue verbatim, gabarit de résumé, échecs et cache, chiffres vérifiés contre le code. Sections : [Ce qu'un appel envoie](context.md#ce-quun-appel-envoie), [Les chiffres par fenêtre](context.md#les-chiffres-par-fenêtre), [Une session](context.md#une-session-du-premier-tour-à-la-cinquième-compaction), [Quand le résumé échoue](context.md#quand-le-résumé-échoue), [Le cache](context.md#le-cache), [Observer](context.md#observer).
- [runtime-events.md](runtime-events.md) : activation du flux WebSocket, contrat du replay et démonstration Pathlayer. Sections : [Activer](runtime-events.md#activer-un-consommateur), [Contrat](runtime-events.md#contrat), [Démonstration](runtime-events.md#démonstration-pathlayer).
- [comparaison.md](comparaison.md) : Pénélope face à OpenClaw et Hermes, mécanisme par mécanisme ; relevé du 18 septembre 2026, non revérifié depuis. Sections : [Mémoire](comparaison.md#mémoire), [Contexte et sessions longues](comparaison.md#contexte-et-sessions-longues), [Sécurité et coût](comparaison.md#sécurité-et-coût), [Ce que les autres font mieux](comparaison.md#ce-que-les-autres-font-mieux).
- [progress.md](progress.md) : avancement, notes de chaque version de la V1. Sections : [Version 1](progress.md#version-1), [Série 0.17](progress.md#série-017).
- [progress-0.17.md](progress-0.17.md) : archive des notes de la série 0.17. Sections : [Résumé](progress-0.17.md#résumé), [Suites du §20.1](progress-0.17.md#suites-du-201), [Ce qui reste à faire](progress-0.17.md#ce-qui-reste-à-faire), [Routine de livraison](progress-0.17.md#routine-de-livraison), [Décisions](progress-0.17.md#décisions).
- [architecture.md](architecture.md) : les crates et leurs couches, les ports de `penelope-app` et qui les implémente, la frontière canal, le journal source unique de la conversation, les règles d'architecture et le gel. Sections : [Couches](architecture.md#les-couches), [Crates](architecture.md#les-crates), [Ports](architecture.md#les-ports), [Frontière canal](architecture.md#la-frontière-canal), [Journal](architecture.md#le-journal-source-unique), [Règles](architecture.md#les-règles-darchitecture), [Ce qui reste](architecture.md#ce-qui-reste).
- [ca-matrix.md](ca-matrix.md) : critères d'acceptation et tests qui les couvrent, générée depuis les sources.

## Décisions

Écarts assumés par rapport à un « DEVRAIT » de la spécification.

- [0001](decisions/0001-kernel-depend-de-store.md) : `penelope-kernel` dépend de `penelope-store`.
- [0002](decisions/0002-pas-de-sqlite-vec.md) : pas de `sqlite-vec`, recherche vectorielle exhaustive en Rust.
- [0003](decisions/0003-validateur-json-schema-local.md) : validateur JSON Schema local plutôt qu'une bibliothèque.
- [0004](decisions/0004-ipc-tokio-unix-socket.md) : IPC par `tokio::net::UnixListener` plutôt qu'un crate d'abstraction.
- [0005](decisions/0005-watcher-par-scrutation.md) : surveillance des fichiers par scrutation, pas par `notify`.
- [0006](decisions/0006-changement-de-sujet-lexical.md) : changement de sujet mesuré sans modèle.
- [0007](decisions/0007-deploiement-par-makefile.md) : `deploy-generic` passe par les cibles `make` du dépôt.
- [0008](decisions/0008-cache-de-prompt.md) : cache de prompt, rien ne bouge avant le dernier message ; complétée par #236 (1.0.12), la différence part en fin de prompt.
- [0009](decisions/0009-pas-d-emulation-d-outils.md) : pas d'émulation d'outils, un modèle sans tool calling est refusé.
- [0010](decisions/0010-fournisseur-codex-oauth.md) : fournisseur Codex — identité empruntée, périmètre du propriétaire, quota du plan.
- [0011](decisions/0011-prompt-systeme-journalise.md) : le prompt système est journalisé en clair, adressé par son empreinte.
- [0012](decisions/0012-jobs-outils-durables.md) : un job d'outil mort au redémarrage n'est jamais relancé d'office.
- [0013](decisions/0013-decoupage-du-daemon.md) : le daemon découpé en crates, la passerelle Telegram au-dessus de lui, les ports dans `penelope-app`.
- [0014](decisions/0014-boucle-pipeline.md) : la boucle d'agent est un pipeline d'étapes typées, en chaînes fixes, qui ne connaît que des ports.
- [0015](decisions/0015-gel-0.17-et-branche-v1.md) : gel de la 0.17 et branche `v1` ; close par la bascule du 27 septembre 2026 (la V1 sur `main`, 1.0.0-rc.1), ses règles du gel restent en vigueur.
- [0016](decisions/0016-ptc-hors-v1.md) : le PTC (`run_code`) est hors V1 ; un appel imbriqué qui demanderait une approbation est refusé sans carte.
- [0017](decisions/0017-journal-source-unique.md) : le journal d'événements est la source unique de la conversation ; les tables de messages sont des caches que seul `penelope-context` écrit.
- [0018](decisions/0018-agenda-caldav-en-lecture.md) : l'agenda, hors PRD, entre par CalDAV en lecture seule, dans un serveur MCP à part servi par le même binaire (`penelope agenda-mcp`) ; rien n'en entre en mémoire durable.
- [0019](decisions/0019-webhook-entrant.md) : un déclencheur `webhook` entrant, signé en HMAC, sur un listener local dédié (`[webhooks]`) ; le mode webhook de Telegram est refusé plutôt que promis.
- [0020](decisions/0020-sauvegarde-fournisseur-unique.md) : une sauvegarde, un fournisseur (S3, dossier ou iCloud Drive) ; GitHub retiré, les secrets dans l'archive sous une seconde couche, le vault en clair plus poussé par défaut.
