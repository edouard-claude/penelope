# 0018 : Un déclencheur `webhook` entrant, sur un listener dédié et local

Statut : acceptée (3 octobre 2026). Portée : §12.9 (déclencheurs et jobs planifiés),
§13.3 (contenu non fiable), §14 (canal). Issue #294.

## Contexte

Le PRD §12.9 prévoit cinq déclencheurs (`cron`, `interval`, `mcp_poll`, `watch_file`,
`event`) et aucune porte entrante : rien ne pouvait pousser un événement dans Pénélope
depuis l'extérieur. Les services qui savent appeler une URL (forges, suivi de tickets,
formulaires, domotique, un script sur une autre machine) n'avaient que la scrutation
(`mcp_poll`) ou rien. Deux promesses de la configuration restaient par ailleurs sans
suite depuis la 0.17 : `telegram.mode = "webhook"` et `telegram.webhook_url`, acceptés à
la validation et jamais servis ; un bot ainsi réglé ne recevait aucun message, sans un
mot.

Le daemon ouvre déjà deux ports locaux : le retour OAuth des serveurs MCP
(`mcp.callback_port`, un lecteur de 8 Ko dans `penelope-mcp-host`) et le flux runtime
(`observability.runtime_stream_bind`, WebSocket).

## Décision

1. **Un sixième déclencheur, `webhook`**, dans l'ordonnanceur. À la création, Pénélope
   attribue le chemin (`/hook/<jeton>`, 24 caractères tirés au sort) et un secret de 48
   caractères, rangé dans le magasin de secrets sous `webhook_<jeton>` ; la spécification
   ne porte que le chemin et le nom du secret (`secret_ref`), jamais sa valeur. Le secret
   est montré une fois, à une création en ligne de commande ; une création par l'outil
   `schedule_create` ne le montre pas au modèle (la réponse d'un outil entre dans le
   journal de la conversation) : le propriétaire pose le sien avec `penelope secret set`.
2. **Chaque requête est un `POST` signé et horodaté** : `X-Penelope-Timestamp: <secondes
   Unix>` et `X-Penelope-Signature: sha256=<hex>`, HMAC-SHA256 par le secret de
   `<horodatage>.<corps brut>`, comparé en temps constant. Un horodatage à plus de cinq
   minutes de l'horloge, dans un sens ou dans l'autre, vaut 401 ; une signature déjà admise
   dans la fenêtre vaut 409 (rejeu). Rien d'autre ne déclenche : un `GET` reçoit 405, un
   chemin inconnu ou en pause 404, une signature fausse 401. Le corps
   est du JSON ; il devient l'événement, filtrable par `filter` comme un élément de
   `mcp_poll`, et passe aux mêmes cibles (`notify`, `prompt`, `workflow`).
3. **Un listener dédié**, `webhooks.listen`, `127.0.0.1:7778` par défaut, servi par
   l'orchestrateur (`scheduler/webhook.rs`) et composé par le daemon comme les autres
   boucles. Le retour OAuth n'est pas partagé : il vit dans la crate MCP, lit une seule
   trame de 8 Ko sans corps, répond à un `GET` de navigateur, et son hôte peut être
   `localhost` ; le webhook lit un corps borné, refuse tout `GET`, et c'est lui, pas le
   retour OAuth, qu'un tunnel exposera. Deux portes aux politiques différentes ne
   partagent pas un port. Pas de serveur HTTP ajouté : le strict nécessaire de HTTP/1.1
   (`Content-Length`, `Expect: 100-continue`, réponse, fermeture) tient en un module, et
   l'en-tête est lu par `httparse`, l'analyseur de `hyper` et de `tokio-tungstenite` déjà
   dans le graphe. Tout ce qu'un intermédiaire pourrait lire autrement (CR ou LF nus,
   ligne repliée, `Content-Length` répété ou non décimal, `Transfer-Encoding` avec
   `Content-Length`) est refusé en 400 et la connexion fermée après chaque réponse. Le
   chemin et le secret sont tirés de l'aléa du système, sans repli : s'il manque, la
   création échoue. Le HMAC est celui de la signature SigV4 des sauvegardes (#289), remonté dans
   `penelope_kernel::hmac` et partagé.
4. **L'exposition n'est pas l'affaire de Pénélope.** Le serveur n'écoute que l'adresse
   donnée ; aucun port n'est ouvert, aucun pare-feu touché. La doc dit comment faire par
   un tunnel sortant ou un réseau privé ; la signature reste obligatoire dans tous les cas.
5. **Garde-fous** : corps borné (`webhooks.max_body_bytes`, 64 Ko, refusé avant lecture),
   débit par hook (`webhooks.rate_per_minute`, 60), plafond de tours `prompt` par heure
   pour l'ensemble des hooks (`webhooks.prompt_turns_per_hour`, 20), connexion coupée
   après 20 s. Chaque réception, acceptée ou refusée, est un événement `webhook.received`
   de la chaîne d'audit : statut, motif, taille, empreinte SHA-256 du corps, adresse ;
   jamais le corps ni les en-têtes.
6. **Le corps est non fiable.** Dans un prompt, il n'est jamais substitué dans le texte :
   il arrive après, encadré par `wrap_untrusted` avec l'alerte du détecteur local, comme
   une description MCP (#92) ou un message transféré. Une notification ou un workflow le
   reçoivent comme élément (`{{payload}}`, champs de premier niveau, `{{item}}`) : aucun
   modèle entre les deux.
7. **`telegram.mode = "webhook"` est refusé à la validation**, avec la raison : seul
   `polling` est servi. Servir le mode webhook de Telegram sur ce listener aurait demandé
   une URL publique en HTTPS (exigence de la Bot API), `setWebhook`, le jeton secret de
   Telegram et un port de la passerelle vers le daemon ; ce n'est pas peu coûteux, et
   personne ne l'a demandé. Les deux clés restent lues (un fichier ancien se charge), la
   doc dit qu'elles sont sans effet.

## Raisons

Un webhook est la forme que prennent les intégrations des services courants ; sans lui,
chaque source devait être sondée, au prix d'appels inutiles et d'un délai. La signature
HMAC du corps est la convention de ces services (GitHub, GitLab, Stripe), connue des
appelants et vérifiable sans état partagé autre que le secret. Rangé dans le magasin, le
secret suit la règle de toute la configuration (`${SECRET:…}`) : le store SQLite et le
journal ne portent jamais une valeur secrète.

La frontière du contenu non fiable est la même que partout ailleurs : un prompt qui
substituerait `{{title}}` depuis un corps venu d'Internet exécuterait une consigne venue
d'Internet. Les plafonds sont là parce qu'un hook branché sur une cible `prompt` coûte un
tour de modèle par réception : une source bavarde ou un appelant malveillant qui connaît
le secret ne peut pas dépenser au-delà de vingt tours par heure.

## Conséquences assumées

- **Le débit se compte en mémoire** : un redémarrage remet les fenêtres à zéro. C'est
  borné par le redémarrage lui-même, et un compteur durable aurait coûté une écriture par
  réception.
- **Le rejeu est refusé, en mémoire** : les signatures admises sont retenues par hook
  jusqu'à la sortie de leur horodatage de la fenêtre de ±5 min. Après un redémarrage,
  une requête capturée dans les cinq dernières minutes passerait une fois de plus ; un
  cache durable aurait coûté une écriture par réception. Deux livraisons légitimes du même
  corps dans la même seconde ont la même signature : la seconde est un 409, l'appelant
  ré-horodate. Une livraison refusée par le débit n'est pas retenue, elle peut revenir.
- **La signature passe avant le débit** : un flot non signé n'épuise pas le budget du
  vrai appelant ; en retour, il n'est pas limité, et c'est au tunnel ou au réseau privé
  de le contenir. Un HMAC ne coûte rien, la lecture du secret dans le trousseau coûte
  une entrée par requête.
- **La suppression d'un hook efface son secret** (`scheduler::remove`), par la RPC et par
  l'outil ; un hook en pause garde le sien et répond 404.
- **Un hook `prompt` ouvre un tour par réception**, dédoublonné par l'identifiant de la
  livraison : deux réceptions à la seconde font deux tours, dans le plafond.

## Alternatives écartées

- **Partager le port du retour OAuth** (`mcp.callback_port`) : un seul port à exposer,
  mais exposer le retour OAuth avec les webhooks, un lecteur sans corps à réécrire dans
  la crate MCP, et deux politiques (hôte `localhost` admis, `GET` servi) mêlées.
- **Un serveur HTTP de bibliothèque** (`axum`, `hyper` en direct) : une dépendance de plus
  pour un `POST` borné ; le workspace n'a qu'un client HTTP et le garde ainsi.
- **Le secret dans la spécification** (`spec.secret`) : montré à chaque `schedule_list`,
  dans chaque réponse d'outil, dans le store et les sauvegardes. Refusé à la validation.
- **Servir le mode webhook de Telegram sur ce listener** : voir le point 7. Retirer les
  deux clés plutôt que les refuser aurait cassé la lecture d'un fichier ancien sans
  l'expliquer.
