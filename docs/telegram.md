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
body = "Autoriser **{{outil}}** sur {{serveur}} ?"
variables = ["outil", "serveur"]
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
| `mcp_url_elicitation` | Un serveur demande une action à l'humain | selon la demande |
| `sampling_request` | Un serveur demande une génération | Autoriser, Refuser |
| `effect_unknown` | Un effet est resté incertain après un crash | Vérifier, Relancer, Ignorer |
| `skill_proposal` | Une skill est proposée | Accepter, Refuser |
| `memory_proposal` | Des souvenirs sont proposés à la promotion | Accepter, Modifier, Refuser |
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
┌──────────────────────────────────────────────┐
│ Approbation demandée                         │
│                                              │
│ Outil : mcp__forge__create_pr                │
│ Serveur : forge                              │
│ Risque : write                               │
│                                              │
│ Arguments :                                  │
│ ┌──────────────────────────────────────────┐ │
│ │ {                                        │ │
│ │   "title": "corrige la TVA",             │ │
│ │   "branch": "penelope/4312"              │ │
│ │ }                                        │ │
│ └──────────────────────────────────────────┘ │
│                                              │
│ Raison donnée : le ticket 4312 est prêt      │
├──────────────────────────────────────────────┤
│ [✅ Autoriser]        [✅ Pour ce run]        │
│ [♾️ Toujours] [❌ Refuser] [✏️ Avec raison]   │
└──────────────────────────────────────────────┘
```

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

## Formulaires

Un formulaire est engendré depuis un **JSON Schema**, pas écrit à la main : c'est le même
schéma qui sert à l'élicitation MCP et aux paramètres de workflow. Les champs requis
apparaissent dans l'ordre du tableau `required`, puis les optionnels par ordre
alphabétique, pour que l'ordre soit stable d'une fois sur l'autre.

Un champ à la fois, avec sa valeur par défaut préremplie et sa progression affichée. Les
saisies sont validées contre le schéma avant d'être acceptées.

Depuis Bot API 9.3, un brouillon est proposé dans la zone de saisie plutôt que dans un
message : la réponse se corrige avant envoi.

## Commandes

Une quarantaine de commandes, groupées par famille : session, modèles, mémoire, MCP,
skills, workflows, planification, HITL, système. Chacune est reliée à une méthode RPC, et
un test vérifie que **toutes** le sont : une commande sans méthode serait une impasse.

```
/new /sessions /switch /title /fork /rewind /compact /export /stop
/model /models /budget
/note /retiens /oublie /recall /appris /pratique /dream /intentions /mien /forget
/mcp /mcp auth /p
/skills /skill
/wf /run /runs /resume
/schedules
/approvals /policies /quiet
/status /doctor /config /logs /restart /upgrade /secret
```

`/secret` liste ou supprime, jamais ne saisit : un secret ne transite pas par une
conversation.

Une session reçoit un titre de quelques mots après son premier échange ; `/sessions` les
liste avec leur date, `/title <texte>` renomme la session courante. `/upgrade` indique
la dernière version publiée, `/upgrade install` l'installe (retour automatique à
l'ancienne si elle ne démarre pas), `/upgrade rollback` revient au binaire précédent.

Un tour qui échoue arrive avec un bouton « 🔁 Réessayer » : la réponse est relancée sur
la même conversation, sans renvoyer le message. Quand le plafond d'une session, du jour
ou d'un run est atteint, le message donne la clé exacte à relever (`budget.session_usd`,
`budget.daily_usd` ou `budget.run_usd`).

Vocaux, photos (albums compris) et documents (PDF, `.docx`, HTML, Markdown, texte) sont reçus : un vocal
est transcrit, une photo montrée au modèle s'il lit les images, un document versé dans
les sources du vault.

Une adresse de retour OAuth collée (`code=` et `state=`, avec ou sans `http://`) termine
l'autorisation en attente et ne part jamais vers le modèle.

Une question de workflow qui attend un formulaire (`input: "form:<id>"`) s'ouvre au
clic sur le choix : un champ par écran, boutons pour une énumération ou un booléen, un
message pour le reste, « Précédent » et « Passer », récapitulatif puis « Envoyer ». La
saisie est validée contre son schéma avant de repartir au workflow.

## Sujets

Dans un groupe avec sujets activés, chaque run ou session longue peut recevoir son propre
fil. Les cartes du run y restent groupées au lieu de se mélanger à la conversation.

## Limites et reprise

Le client respecte les limites de débit de Telegram sans perdre de message : un `429`
avec `retry_after` est attendu puis rejoué. La file d'envoi est durable, donc un
redémarrage ne fait pas disparaître un message en attente. Un `update_id` déjà traité ne
crée pas un second tour, même après un crash.

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
