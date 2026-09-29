# Avancement

Tenu à jour conformément au §21 du PRD : étape, critères d'acceptation couverts,
décisions. Ce fichier dit aussi, sans détour, ce qui **n'est pas** fait.

Dernière mise à jour : 27 septembre 2026.

## Version 1

Une section `### x.y.z` par lot, la plus récente en tête (décision
[0015](decisions/0015-gel-0.17-et-branche-v1.md), épopée #208). Les versions `1.0.0-alpha.N`
ont été écrites sur la branche `v1`, sans tag ni release, avant la bascule vers `main`.
La charte et les spécifications sont dans `design/v1/`.

### 1.0.18

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
  ne vaut plus rien. La PR dev doit être fusionnée. Un refus bloque le run pour de bon.
- Au plus une PR prod par run : effet au ledger sous une clé qui ne dépend que du run,
  marque du run dans la description, recherche sur le forgeur avant l'ouverture ; un envoi
  interrompu par un redémarrage est terminé sans rejuger le bilan. Lien dans le sujet ; ni
  fusion ni déploiement, qui suivent la politique du projet.
- L'attente d'une étape `user` ne compte plus dans la borne de durée d'un run : elle mesure
  le travail, pas le temps d'une décision.
- Tests : logique pure du gate (chemin, branche de prod, fraîcheur, bilan), orchestrateur
  contre les faux forgeurs GitHub et GitLab (tout vert sans clic et redémarrage : zéro PR ;
  approbation puis redémarrage : une seule PR, lien dans le sujet ; PR ouverte avant un
  arrêt retrouvée ; bilan vieilli, commit avancé, CI rouge : aucune PR et bouton périmé ;
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
