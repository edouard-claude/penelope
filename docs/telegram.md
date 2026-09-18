# Telegram

Telegram est l'interface principale : c'est par là que Pénélope demande, rend compte et
reçoit des ordres. Le client Bot API est écrit à la main, sans bibliothèque tierce, pour
que les nouveautés du protocole soient adoptées le jour où elles sortent.

Un seul propriétaire est autorisé (`owner.telegram_user_id`). Tout autre expéditeur,
message ou clic de bouton, est classé `Unauthorized` et ne déclenche rien.

## Deux rendus, un seul arbre

Un message est rendu **une fois** en arbre Markdown, puis projeté deux fois :

```
  Markdown (sortie du modèle)
        │
        ▼
   arbre d'événements  (pulldown-cmark)
        │
        ├──► blocs riches   (Bot API 10.1+ : titres, listes, code, citations, tableaux)
        └──► HTML de repli  (le sous-ensemble que Telegram accepte depuis toujours)
```

Le repli HTML n'est pas une seconde implémentation : c'est la même traversée, avec un
autre émetteur. Une divergence entre les deux est donc impossible par construction, et un
test vérifie que **chaque** gabarit du catalogue rend quelque chose dans les deux formes.

Le repli se déclenche quand le serveur refuse une capacité (par exemple parce que le
client du propriétaire est trop ancien), jamais sur une erreur de transport : une erreur
« chat introuvable » n'a rien à voir avec le rendu et ne doit pas dégrader silencieusement
tous les messages suivants.

Le HTML brut présent dans la sortie du modèle est **échappé**, pas supprimé : ce que le
modèle a écrit reste visible, sans pouvoir injecter de balise.

## Découpe

Un message dépasse 4096 caractères ? Il est découpé sur des frontières sûres. Un bloc de
code n'est jamais coupé en deux : la clôture est réécrite dans le fragment courant et
rouverte dans le suivant, avec le même langage. Sinon le lecteur reçoit un fragment qui
s'affiche comme du texte brut, puis un fragment qui s'affiche comme du code.

## Gabarits

25 gabarits livrés, surchargeables par un fichier TOML dans `templates/` portant le même
identifiant. Un gabarit du disque prime sur celui livré, et la substitution est chargée à
chaud.

```toml
# templates/tool_approval.toml
id = "tool_approval"
body = "{{intention}}\n\n{{action}}"
variables = ["intention", "action"]
```

| Identifiant | Quand | Boutons |
|---|---|---|
| `answer` | Réponse ordinaire | Régénérer, Modèle supérieur, Mémoriser |
| `tool_approval` | Un outil demande l'autorisation | Autoriser, Pour ce run, Toujours, Refuser, Refuser avec raison |
| `destructive_confirm` | Seconde confirmation d'un geste destructeur | Confirmer, Annuler |
| `plan_proposal` | Un plan est proposé avant exécution | Appliquer, Réviser, Rejeter |
| `deploy_gate` | Porte avant déploiement | Déployer, Attendre la review, Annuler |
| `run_card` | Carte vivante d'un run | Pause, Reprendre, Annuler, Trace, Relancer l'étape |
| `run_done` | Run terminé | Trace, Relancer |
| `run_blocked` | Run bloqué | Réessayer, Passer l'étape, Annuler |
| `incident` | Incident pendant un run | Rollback, Réessayer, Laisser |
| `ticket_detected` | Un ticket est détecté | Lancer le workflow, Ignorer, Plus tard |
| `question` | Question libre au propriétaire | Répondre |
| `form` | Un champ de formulaire | Précédent, Suivant, Envoyer, Renoncer |
| `mcp_oauth_required` | Un serveur MCP demande une autorisation | Ouvrir, Coller l'URL, Annuler |
| `mcp_url_elicitation` | Un serveur demande une action à l'humain | Ouvrir le lien, J'ai terminé, Annuler |
| `sampling_request` | Un serveur demande une génération (non annoncé, toujours refusé) | Autoriser, Refuser |
| `effect_unknown` | Un effet est resté incertain après un crash (poussé au démarrage, une fois par effet) | C'est fait, Relancer, Ignorer (jamais de règle) |
| `skill_proposal` | Une skill est proposée | Accepter, Refuser |
| `memory_proposal` | Des souvenirs sont proposés à la promotion (un changement de défaut de pratique, par exemple) | Accepter, Modifier, Refuser |
| `learned` | Ce qui a été appris récemment | — |
| `schedule_preview` | Un déclencheur planifié est proposé | Créer, Annuler |
| `workflow_preview` | Un workflow est proposé | Enregistrer, Lancer une fois, Rejeter |
| `budget_alert` | Un seuil de budget est franchi | — |
| `mcp_status` | Tableau des serveurs MCP | — |
| `digest` | Digest du matin | — |
| `heartbeat` | Battement de cœur | — |
| `stopped` | Génération interrompue | — |

Voici ce que donne `tool_approval` une fois rendu, tel qu'il apparaît dans la
conversation :

```
┌──────────────────────────────────────────────────┐
│ Approbation demandée                             │
│                                                  │
│ Je vérifie si la correction du nom d'expéditeur  │
│ est déjà partie en revue.                        │
│                                                  │
│ ┌──────────────────────────────────────────────┐ │
│ │ gh pr list --repo Fidelatoo/cron-send-add-   │ │
│ │ sender --state all --limit 10                │ │
│ └──────────────────────────────────────────────┘ │
│                                                  │
│ réseau · sortie complète · classe external ·     │
│ politique par défaut pour la classe external     │
│ 🌐 Accès au réseau demandé.                      │
├──────────────────────────────────────────────────┤
│ [✅ Autoriser]      [✅ Pour cette session]       │
│ [♾️ Toujours pour « gh pr » (réseau)] [❌ Refuser] │
└──────────────────────────────────────────────────┘
```

En tête, ce que Pénélope cherche à faire : la phrase qu'elle passe à l'appel
(`pourquoi`), sinon ton message qui a lancé le tour (« Pour ta demande : … »), jamais la
raison de la politique. Puis l'action telle qu'elle sera faite : la commande exacte, sans
échappement JSON, ou l'outil et ses valeurs sur une ligne (`issue_id = 7653 · status =
Résolu`). La dernière ligne regroupe les qualificatifs (réseau, sortie complète,
répertoire, serveur MCP), la classe de risque et la politique. « Toujours » dit sur quoi il
porte : une famille de commandes, un répertoire, un hôte. Variables : `intention`,
`action`, `details`, `alerte` ; un gabarit surchargé plus ancien garde `outil`, `serveur`,
`risque`, `arguments` et `raison`.

Une carte restée sans réponse revient : « ⏰ Rappel 1/2 » au bout d'une heure, « Rappel
2/2 » au bout de six, chaque fois avec des boutons neufs, dans la conversation d'origine.
Au bout de 24 h, la demande expire et le tour reprend en le disant au modèle.

Et `run_blocked`, qui porte la dernière sortie dans un bloc de code :

```
┌──────────────────────────────────────────────┐
│ ⛔ ticket-to-deploy bloqué                    │
│                                              │
│ Raison : aucune transition vraie à l'étape   │
│ « verifier »                                 │
│                                              │
│ Dernière sortie :                            │
│ ┌──────────────────────────────────────────┐ │
│ │ error[E0308]: mismatched types           │ │
│ │   --> src/tva.rs:42:9                    │ │
│ └──────────────────────────────────────────┘ │
├──────────────────────────────────────────────┤
│ [🔁 Réessayer] [⏭ Passer] [⏹ Annuler]        │
└──────────────────────────────────────────────┘
```

## Boutons et jetons

Telegram plafonne `callback_data` à 64 octets. Un identifiant de run, une étape et une
action n'y tiennent pas. Chaque bouton porte donc un **jeton opaque** en base62, court,
qui indexe une action stockée en base :

```
[✅ Autoriser]  ──►  callback_data = "a:7fK2pQ"
                          │
                          ▼
                 table `actions` : { kind, cible, run, session, expiration, propriétaire }
```

Trois propriétés en découlent, chacune couverte par un test :

- **Idempotence.** Un double clic ne déclenche qu'une fois ; le second reçoit le même
  résultat, pas une seconde exécution.
- **Autorisation.** Un clic d'un autre utilisateur est refusé, même s'il a vu la carte.
- **Expiration.** Un jeton périmé répond que la fenêtre est passée, il ne rejoue rien.

Outre `callback_data` et les liens, un bouton peut copier un texte (`copy_text`) : une
commande à compléter, comme `/retiens `. Les textes longs (digest, audit) renvoient vers un
écran précis par lien profond `https://t.me/<bot>?start=<écran>` (`approvals`,
`runs_stuck`, `audit`…), une fois le nom du bot connu.

## Formulaires

Un formulaire est engendré depuis un **JSON Schema**, pas écrit à la main : c'est le même
schéma qui sert à l'élicitation MCP et aux paramètres de workflow. Les champs requis
apparaissent dans l'ordre du tableau `required`, puis les optionnels par ordre
alphabétique, pour que l'ordre soit stable d'une fois sur l'autre.

Un champ à la fois, avec sa valeur par défaut préremplie et sa progression affichée. Les
saisies sont validées contre le schéma avant d'être acceptées.

Depuis Bot API 9.3, un brouillon est proposé dans la zone de saisie plutôt que dans un
message : la réponse se corrige avant envoi.

Une demande d'élicitation MCP ouvre une carte qui nomme le serveur et cite sa demande :

```
🔐 Le serveur MCP redmine demande ta confirmation
│ Modifier le ticket 42 ?
Sans réponse d'ici 10 min, la demande est annulée.
[ ✅ Accepter ] [ 🚫 Refuser ]
[ ✖️ Annuler ]
```

Avec des champs, « 📝 Remplir » ouvre le formulaire ; le récapitulatif garde « Refuser ».
Pour un lien, la carte montre le domaine et l'adresse entière, et le bouton d'ouverture
n'apparaît qu'après accord. Détails dans [mcp.md](mcp.md#élicitation).

## Commandes

Une quarantaine de commandes, groupées par famille : session, modèles, mémoire, MCP,
skills, workflows, planification, HITL, système. Chacune est reliée à une méthode RPC, et
un test vérifie que **toutes** le sont : une commande sans méthode serait une impasse.

```
/new /sessions /switch /close /purge /title /fork /rewind /compact /export /stop
/model /models /mode /projet /budget /usage
/note /retiens /oublie /recall /appris /pratique /dream /intentions /mien /forget /accueil /audit
/mcp /mcp auth /p
/skills /skill
/wf /run /runs /resume
/schedules
/approvals /policies /quiet
/status /doctor /config /logs /restart /upgrade /secret
```

**Aucune commande ne répond par un « Usage : ».** Sans argument, chacune ouvre son écran :
un bouton par élément actionnable, le message redessiné en place après chaque action,
« Précédent / Suivant » au-delà de dix éléments, et un second écran « Confirmer /
Annuler » pour tout geste risqué (supprimer, oublier, fermer, redémarrer, installer ou
annuler une mise à jour).

```
Workflows (4)
[ ▶️ Construire puis vérifier ] [ ℹ️ ]
[ ▶️ Revue                     ] [ ℹ️ ]
```

| Commande | Écran |
|---|---|
| `/help` | familles, puis un bouton par commande |
| `/wf`, `/run` | ▶️ lance (les paramètres déclarés sont demandés un par un), ℹ️ étapes et paramètres ; `/run <workflow>` sans paramètres passe par la conversation, formulaire à un bouton |
| `/runs`, `/resume` | état et étape de chaque run, ⏸ ▶️ ⏹ (confirmé), 🔎 détail ; `/resume` ne montre que les runs en pause ou bloqués |
| `/schedules` | ⚡ déclencher, ⏸/▶️, 🗑 (confirmé) ; dernière erreur de chaque planification ; une exécution en échec arrive en alerte avec « Relancer maintenant » |
| `/mcp` | par serveur : détail, 🔄 redémarrer, 🧪 tester ; le détail ajoute 📜 journal, ⏻ activer ou désactiver, 🔐 autoriser |
| `/models` | un modèle, puis l'alias auquel l'affecter ; 🔎 chercher |
| `/projet` | sujet de travail de la session : un bouton par projet connu du vault, et « Aucun » ; la mémoire d'office s'y limite |
| `/mode` | ce qui part sans demande dans cette session : demander tout, lectures sans demande (défaut), tout sauf le destructif ; le mode actuel coché |
| `/skills`, `/skill` | 📖 voir, ⏪ version précédente (confirmé) |
| `/oublie`, `/forget` | une entrée (ou une session) par bouton, puis confirmation |
| `/appris`, `/pratique` | voir, ✅ valider, 🚫 rejeter |
| `/intentions`, `/policies` | ❌ annuler une intention, 🗑 retirer une règle (confirmé) ; une règle inutile (famille issue d'une commande composée, lecture déjà libre, jamais utilisée depuis une semaine) porte ⚠️ et la raison |
| `/status`, `/doctor` | résumé lisible, boutons vers l'écran de chaque alerte (MCP, dépenses, modèles) |
| `/config`, `/logs` | générations et sous-systèmes ; journal filtré par composant, « Plus » |
| `/restart`, `/close`, `/rewind` | confirmation |
| `/purge` | confirmation ; efface le contenu de la session (messages, résumés, artefacts, arguments et résultats d'outils, messages envoyés), la chaîne d'audit garde ses lignes sans leur contenu |
| `/fork` | ↪️ revenir à l'original |
| `/upgrade` | version installée et disponible, ⬆️ installer, ⏪ revenir (confirmés) ; sur une installation source, carte de bascule vers les releases |
| `/quiet`, `/secret`, `/p` | plages proposées ; 🗑 par secret (confirmé) ; serveurs puis prompts MCP, arguments par formulaire |
| `/retiens`, `/recall`, `/note`, `/title` | ✏️ bouton qui copie la commande à compléter |
| `/budget session <montant>` | plafond propre à la session ; au plafond, carte « continuer ? » avec +5 $, +20 $, Arrêter |

`/secret` liste ou supprime, jamais ne saisit : un secret ne transite pas par une
conversation.

### Messages envoyés coup sur coup

Un long texte collé arrive découpé par Telegram en messages de 4 096 caractères. Un
morceau à la limite, ou un message transféré, ouvre une fenêtre de
`telegram.text_group_window_ms` (2 s par défaut) : les morceaux qui suivent forment **un
seul tour**, recollés dans l'ordre (un morceau à la limite est la suite du précédent, les
autres sont séparés par une ligne vide), et un message court qui arrive pendant la fenêtre
la ferme après 300 ms de silence, comme dernier morceau. Un message court tapé seul, lui,
part **tout de suite** : un « merci » n'attend rien. Chaque morceau reçoit sa réaction
« reçu », mais une seule réponse part.

Au-delà de `telegram.burst_messages` (5) ou de `telegram.burst_chars` (20 000), Pénélope
ne répond pas d'elle-même : elle dit ce qu'elle a reçu et demande quoi en faire.

```
📥 Tu m'as envoyé 41 messages (150 000 caractères). Qu'est-ce que j'en fais ?
[ 📄 Un seul document        ]
[ 📥 Ingérer sans répondre   ]
[ 1️⃣ Un par un ] [ 🗑 Tout annuler ]
```

« Ingérer sans répondre » passe par l'ingestion de documents : fiche source dans le vault,
passages indexés, propositions de mémoire, sans réponse par morceau.

`/stop` arrête le tour en cours **et** vide la file de la session, puis dit ce qui a été
arrêté (« ⏹ Tour arrêté, 26 message(s) en attente annulé(s). ») et ce qui continue.
`/stop tout` vide en plus les files des autres sessions du chat et met en pause les runs
de workflow en cours.

Une session reçoit un titre de quelques mots après son premier échange ; `/title
<texte>` renomme la session courante. `/sessions` rend un bouton par session (▶️ celle du
chat, ⏳ un tour en cours ou en attente, heure de dernière activité pour la plus récente) :
un clic bascule le chat dessus et met le menu à jour, « ⋯ » ouvre Basculer, Forker,
Renommer et Fermer. Douze sessions par page ; les fermées sont masquées sauf « Voir les
fermées » (ou `/sessions all`). `/switch` accepte un identifiant, un préfixe unique ou
un titre, `/close [session]` arrête une session et vide sa file, `/purge [session]` en
efface le contenu (RGPD, sans retour).

```
Sessions (14 · page 1/2)
[ ▶️ Refonte du site (16/09) 18:42 ] [ ⋯ ]
[ ⏳ Budget 2027 (15/09)          ] [ ⋯ ]
[ Plus anciennes » ] [ Voir les fermées ]
```

Seule la session au **focus** écrit dans le chat. Une session quittée (origine d'un fork,
ou laissée par `/switch`) finit son tour en cours sans rien écrire : réponse, messages,
fichiers et approbations sont mis de côté derrière une seule notification silencieuse,
mise à jour au fil de l'eau.

```
📬 2 réponses et 1 approbation en attente dans « Budget 2027 »
[ ↪️ Basculer ]
```

« Basculer » revient sur la session et envoie tout dans l'ordre ; une approbation déjà
tranchée entre-temps n'est pas renvoyée. Les nouveaux messages vont toujours à la session
au focus. `/upgrade install` installe la dernière version publiée (retour automatique à
l'ancienne si elle ne démarre pas), `/upgrade rollback` revient au binaire précédent,
après confirmation.

Quand le détecteur arrête un outil appelé en boucle, Pénélope répond quand même, sans
outil : ce qu'elle a tenté, l'erreur exacte renvoyée, ce qu'elle a déjà obtenu, et deux ou
trois suites en boutons (« Chercher autrement », « Je te précise… », « Laisser tomber »).
Un clic envoie la suite comme un message. Le résultat réel et une note d'arrêt restent
dans la conversation, pour que le tour suivant ne recommence pas à l'identique ; le
rapport technique (compteurs d'appels) reste dans les événements et `/logs`.

Un tour qui échoue arrive avec un bouton « 🔁 Réessayer » : la réponse est relancée sur
la même conversation, sans renvoyer le message. Quand le plafond d'une session, du jour
ou d'un run est atteint, le message donne la clé exacte à relever (`budget.session_usd`,
`budget.daily_usd` ou `budget.run_usd`).

Vocaux, photos (albums compris) et documents (PDF, `.docx`, HTML, Markdown, texte) sont reçus : un vocal
est transcrit, une photo montrée au modèle s'il lit les images, un document versé dans
les sources du vault.

Une adresse de retour OAuth collée (`code=` et `state=`, avec ou sans `http://`) termine
l'autorisation en attente et ne part jamais vers le modèle.

Un workflow proposé en conversation (`workflow_start`) arrive en carte dédiée : nom et
rôle du workflow, paramètres complétés par Pénélope, brief de la discussion, et deux
boutons seulement, « ▶️ Lancer » et « ⏸ Pas encore » (pas de « Toujours » : chaque
lancement se valide). « Pas encore » rend la main à la conversation avec la raison du
refus. Le run parle ensuite dans le même chat et le même sujet, formulaire compris.

```
▶️ Lancer « Ticket → correctif → déploiement » ? (ticket-to-deploy)
Paramètres
- ticket_id : 7647
- ticket_url : https://…/issues/7647
Brief
Export CSV vide depuis la 2.3 ; piste : filtre de dates.
[ ▶️ Lancer ] [ ⏸ Pas encore ]
```

Une question de workflow qui attend un formulaire (`input: "form:<id>"`) s'ouvre au
clic sur le choix : un champ par écran, boutons pour une énumération ou un booléen, un
message pour le reste, « Précédent » et « Passer », récapitulatif puis « Envoyer ». La
saisie est validée contre son schéma avant de repartir au workflow.

## Sujets

Dans un groupe avec sujets activés, chaque run ou session longue peut recevoir son propre
fil. Les cartes du run y restent groupées au lieu de se mélanger à la conversation.

Changer de session dans un fil (`/switch`, `/sessions`, `/fork`) ne coupe plus celle
qu'on quitte : ses tours en file s'exécutent en fond, ce qu'elle produit est retenu, une
seule notification la signale avec un bouton pour y revenir, et tout est délivré au
retour. `/sessions` marque celles qui travaillent (⏳ et le nombre de tours en file). Une
session en fond reste soumise à son plafond : au-delà, elle s'arrête sans toucher à la
session du fil. `/new` sur une session qui travaille encore demande d'abord : la garder
en fond (la nouvelle session prend le fil, l'ancienne finit son travail) ou la fermer, en
disant combien de tours seraient perdus.

Chaque sujet porte sa propre session : plusieurs chantiers avancent en parallèle, un par
sujet. Chaque session a aussi un **sujet de travail** : le nom du sujet Telegram, le titre
de la session ou son premier message, quand il nomme un projet connu du vault (annotation
`projet`, section de `projets.md`), sinon `/projet`. La mémoire injectée d'office s'y
limite : le profil et les entrées sans projet partout, celles d'un projet seulement dans
ses sessions ; le reste revient par le rappel quand la question le vise, ou par
`mem_search`. `/sessions` le montre (📁), `/projet` le change.

Pour monter un tel groupe :

1. Créer un groupe, activer les sujets (il devient un supergroupe) et y ajouter le bot
   comme administrateur ; sinon, désactiver son mode privé chez @BotFather
   (`/setprivacy`, *Disable*), faute de quoi il ne voit que les commandes.
2. Écrire un premier message dans le groupe. Pénélope ne répond pas (le groupe n'est pas
   encore autorisé), mais `penelope doctor`, ligne « Conversations Telegram », donne son
   identifiant (`-100…`), son type et son titre ; le journal aussi.
3. Autoriser le groupe par cet identifiant :

```bash
penelope config set telegram.allowed_chats '[-1001234567890]'
```

Le changement s'applique sans redémarrer. Dans un groupe listé, seul le propriétaire est
écouté ; s'il écrit en administrateur anonyme, Telegram envoie ses messages au nom de
`GroupAnonymousBot` (`1087968824`), avec le groupe comme expéditeur : ce message-là, dans
ce groupe-là, vaut propriétaire. Un autre membre, ou l'anonyme d'une autre conversation,
est ignoré. Un groupe absent de la liste est ignoré en silence, même avec
`telegram.allow_groups` : l'interrupteur ne suffit plus, il faut l'identifiant.

## Limites et reprise

Le client respecte les limites de débit de Telegram sans perdre de message : un `429`
avec `retry_after` est attendu puis rejoué. La file d'envoi est durable, donc un
redémarrage ne fait pas disparaître un message en attente. Un `update_id` déjà traité ne
crée pas un second tour, même après un crash.

Les messages d'un même chat partent dans l'ordre : une erreur de transport isolée est
reprise tout de suite, et un envoi qui doit attendre sa nouvelle tentative retient ceux qui
le suivent dans ce chat (les autres chats ne l'attendent pas), si bien qu'une longue
réponse ne se lit jamais dans le désordre. Un refus définitif (message trop long, chat
introuvable) est dit dans le chat par une courte note en texte brut, et `/status` compte
les messages non envoyés.

## Tester sans réseau

Le mock Bot API implémente le même contrat que le vrai transport :

```bash
cargo test -p penelope-telegram
```

Il sert aussi aux suites transverses, par exemple pour dérouler le flux OAuth
`paste_back` de bout en bout dans la suite de conformance MCP.

## Limites actuelles

- Seul le long polling est lancé : `telegram.mode = "webhook"` n'est pas servi.
- Un PDF scanné est lu par OCR sur macOS seulement, et seulement s'il n'a aucune couche
  texte.

Voir [progress.md](progress.md).
