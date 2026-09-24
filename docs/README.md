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
| Choisir les modèles | [Modèles](install-headless.md#6-modèles), [Qui répond à un message](install-headless.md#qui-répond-à-un-message), [OpenRouter](install-headless.md#openrouter) |
| Utiliser sur Telegram | [Commandes](telegram.md#commandes), [Gabarits](telegram.md#gabarits), [Formulaires](telegram.md#formulaires), [Sujets](telegram.md#sujets), [Messages vocaux](install-headless.md#messages-vocaux), [Photos et documents](install-headless.md#photos-et-documents) |
| Écrire ou lancer un workflow | [Fichier](workflows.md#fichier), [Types d'étapes](workflows.md#les-neuf-types-détapes), [Transitions](workflows.md#transitions), [Workflows livrés](workflows.md#workflows-livrés), [Lancer, planifier, suivre](workflows.md#lancer-planifier-suivre), [Workflows sur la machine](install-headless.md#workflows) |
| Brancher des serveurs MCP | [Déclarer un serveur](mcp.md#déclarer-un-serveur), [Risque et approbation](mcp.md#risque-et-approbation), [OAuth 2.1](mcp.md#oauth-21), [Serveurs MCP sur la machine](install-headless.md#serveurs-mcp), [Venir d'Hermes](install-headless.md#venir-dhermes) |
| Mémoire et vault | [Mémoire qui apprend](install-headless.md#mémoire-qui-apprend), [Rappels et tâches planifiées](install-headless.md#rappels-et-tâches-planifiées), [Retrouver une conversation](install-headless.md#retrouver-une-conversation), [Sauvegarde et audit](install-headless.md#9-sauvegarde-et-audit), [Sauvegarde complète chiffrée](install-headless.md#sauvegarde-complète-chiffrée-hors-de-la-machine), [Remonter une instance](install-headless.md#remonter-une-instance-sur-une-machine-neuve) |
| Coûts et budget | [Coûts](install-headless.md#coûts), [Gros résultats d'outils](install-headless.md#gros-résultats-doutils), [Longues conversations](install-headless.md#longues-conversations), [Cache de prompt](decisions/0008-cache-de-prompt.md) |
| Travaux longs sans bloquer le tour | [Jobs d'outils](install-headless.md#jobs-doutils), [Jobs durables](decisions/0012-jobs-outils-durables.md) |
| Relire une requête envoyée | [Relire ce que le modèle a lu](context.md#relire-ce-que-le-modèle-a-lu), [Prompt système journalisé](decisions/0011-prompt-systeme-journalise.md) |
| Observer le daemon en direct | [Flux runtime](runtime-events.md#flux-dévénements-runtime), [Contrat](runtime-events.md#contrat), [Démonstration Pathlayer](runtime-events.md#démonstration-pathlayer) |
| Comprendre la compression de contexte | [Ce qu'un appel envoie](context.md#ce-quun-appel-envoie), [Les chiffres par fenêtre](context.md#les-chiffres-par-fenêtre), [Une session jusqu'à la cinquième compaction](context.md#une-session-du-premier-tour-à-la-cinquième-compaction), [Quand le résumé échoue](context.md#quand-le-résumé-échoue), [Le cache](context.md#le-cache) |
| Sécurité | [Secrets](install-headless.md#4-secrets), [Shell et bac à sable](install-headless.md#shell-et-bac-à-sable), [Outils natifs](install-headless.md#outils-natifs), [Risque et approbation MCP](mcp.md#risque-et-approbation) |
| Ce que Pénélope sait d'elle-même | [Inventaire et documentation](install-headless.md#ce-que-pénélope-sait-delle-même) |
| État d'avancement | [Avancement](progress.md#résumé), [Ce qui reste à faire](progress.md#ce-qui-reste-à-faire), [Ce qui n'est pas encore branché](install-headless.md#11-ce-qui-nest-pas-encore-branché), [Critères d'acceptation](ca-matrix.md) |

## Par fichier

- [install-headless.md](install-headless.md) : installer, configurer et exploiter Pénélope sur un Mac sans écran ; référence des clés de configuration et des outils natifs. Sections : [Prérequis](install-headless.md#1-prérequis), [Compiler et installer](install-headless.md#2-compiler-et-installer), [Secrets](install-headless.md#4-secrets), [Configuration](install-headless.md#5-configuration), [Modèles](install-headless.md#6-modèles), [Premier essai en CLI](install-headless.md#7-premier-essai-en-cli), [Mise à jour](install-headless.md#10-mise-à-jour).
- [telegram.md](telegram.md) : le canal Telegram, rendu, gabarits, boutons, formulaires et catalogue des commandes. Sections : [Deux rendus, un seul arbre](telegram.md#deux-rendus-un-seul-arbre), [Gabarits](telegram.md#gabarits), [Boutons et jetons](telegram.md#boutons-et-jetons), [Commandes](telegram.md#commandes), [Limites actuelles](telegram.md#limites-actuelles).
- [workflows.md](workflows.md) : référence complète du schéma de workflow et du cycle de vie d'un run. Sections : [Fichier](workflows.md#fichier), [Métadonnées](workflows.md#métadonnées), [Réglages](workflows.md#réglages), [Types d'étapes](workflows.md#les-neuf-types-détapes), [Transitions](workflows.md#transitions), [Reprise](workflows.md#reprise), [Workflows livrés](workflows.md#workflows-livrés).
- [mcp.md](mcp.md) : le client MCP, versions de protocole, transports, OAuth, registre paresseux et élicitation. Sections : [Versions de protocole](mcp.md#versions-de-protocole), [Transports](mcp.md#transports), [Déclarer un serveur](mcp.md#déclarer-un-serveur), [Registre paresseux](mcp.md#registre-paresseux), [OAuth 2.1](mcp.md#oauth-21), [Élicitation](mcp.md#élicitation).
- [context.md](context.md) : la compression de contexte de bout en bout, seuils, queue verbatim, gabarit de résumé, échecs et cache, chiffres vérifiés contre le code. Sections : [Ce qu'un appel envoie](context.md#ce-quun-appel-envoie), [Les chiffres par fenêtre](context.md#les-chiffres-par-fenêtre), [Une session](context.md#une-session-du-premier-tour-à-la-cinquième-compaction), [Quand le résumé échoue](context.md#quand-le-résumé-échoue), [Le cache](context.md#le-cache), [Observer](context.md#observer).
- [runtime-events.md](runtime-events.md) : activation du flux WebSocket, contrat du replay et démonstration Pathlayer. Sections : [Activer](runtime-events.md#activer-un-consommateur), [Contrat](runtime-events.md#contrat), [Démonstration](runtime-events.md#démonstration-pathlayer).
- [progress.md](progress.md) : avancement, notes de chaque version, routine de livraison et manques connus. Sections : [Version 1](progress.md#version-1-branche-v1), [Résumé](progress.md#résumé), [Suites du §20.1](progress.md#suites-du-201), [Ce qui reste à faire](progress.md#ce-qui-reste-à-faire), [Routine de livraison](progress.md#routine-de-livraison), [Décisions](progress.md#décisions).
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
- [0008](decisions/0008-cache-de-prompt.md) : cache de prompt, rien ne bouge avant le dernier message.
- [0009](decisions/0009-pas-d-emulation-d-outils.md) : pas d'émulation d'outils, un modèle sans tool calling est refusé.
- [0010](decisions/0010-fournisseur-codex-oauth.md) : fournisseur Codex — identité empruntée, périmètre du propriétaire, quota du plan.
- [0011](decisions/0011-prompt-systeme-journalise.md) : le prompt système est journalisé en clair, adressé par son empreinte.
- [0012](decisions/0012-jobs-outils-durables.md) : un job d'outil mort au redémarrage n'est jamais relancé d'office.
- [0015](decisions/0015-gel-0.17-et-branche-v1.md) : gel de la 0.17 et branche `v1` ; `main` ne prend que des corrections, versions `1.0.0-alpha.N` jamais taguées.

Les numéros 0013, 0014, 0016 et 0017 sont réservés par la charte de la V1 (`design/v1/README.md` §9) et pas encore écrits (0017 : journal source unique).
