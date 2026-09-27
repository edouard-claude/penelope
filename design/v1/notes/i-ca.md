# Notes de livraison : lot I, critères d'acceptation du journal (épopée #208, T22)

Branche `v1-i-ca`, dérivée de `v1` à `d662ca4` (1.0.0-alpha.10), poussée sur `origin`.
Aucune migration, aucun changement hors de `penelope-evals`, de la matrice CA et de
`[ca].required`. Aucun défaut trouvé dans `penelope-context` : pas de commit correctif.

## Commits

| Commit | Quoi |
|---|---|
| `da6553e` | T22 : contrôles du journal à la fin de chaque scénario, scénario `outils-niveau-1-et-compaction`, `ca_4_5`, `ca_4_6`, `ca_5_5`, matrice et budget régénérés |
| `d3c8a79` | T22 : une tentative sans `llm_request_id` est aussi comparée |
| (ce commit) | ces notes, brouillon de la décision 0017 (T19) |

## Ce qui est livré

- **`harness/visible.rs`**, appelé à la fin de chaque scénario, avant la refonte de
  `journal.rs` (donc dans les dix-sept cas, `two_replays…` et `both_history_sources…`) :
  1. chaque appel du modèle (`conv.assistant` qui porte `request_hash`, et toute
     `conv.attempt`) est retrouvé dans les requêtes reçues par le mock par l'empreinte que
     le journal cite (`request_hash`, ou la ligne `llm_requests` de la tentative ; à
     défaut, celle du pliage, compté `unpinned`), puis comparé octet pour octet, corps
     « chat completions » (`to_openai_body`) compris, à
     `derive_until(journal, seq - 1).request_messages("")` ;
  2. entre deux appels d'un même tour (`turn.started` … `turn.finished`), aucun événement
     `conv.*` dont l'opération de surface n'est pas `append`, sauf le niveau 1 d'un
     résultat ajouté depuis l'appel précédent et le résumé `trigger: overflow` après une
     tentative `before_stream`.
- **Le pliage est refait avec les seules fonctions publiques et pures** de
  `penelope-context` (`derive_until`, `Sealed::fork`, préfixe d'un fork récursif,
  offset lu dans `surface.offset`) : il ne passe ni par `read.rs` ni par son cache, qu'il
  contrôle.
- **`Run.audit`** (`harness::Audit` : `Visible` et le nombre de lignes de cache redonnées
  par la refonte) et `scenario::audit(dir)` : les critères vérifient qu'ils n'ont pas
  réussi à vide.
- **Scénario `outils-niveau-1-et-compaction`** : deux échanges, puis un tour qui lit un
  gros fichier (niveau 1 : `conv.tool_result` ajouté puis remplacé), subit un dépassement
  prouvé (`conv.attempt` puis `conv.summary trigger overflow`) et répond ; un tour de plus.
- **Critères** (`tests/scenarios.rs`) : `ca_4_5_model_visible_is_logged` (six appels
  comparés, aucun niveau 0, 2 ou 4, aucun non épinglé, un niveau 1 et un résumé admis),
  `ca_5_5_replace_only_at_a_turn_boundary` (exactement ces deux kinds admis),
  `ca_4_6_reindex_is_lossless` (25 lignes de cache redonnées à l'identique).
- **Test unitaire** `only_two_replaces_may_fall_between_two_calls` : niveau 1 d'un nœud
  déjà envoyé, résumé après une réponse, résumé manuel après une tentative, prompt système
  remplacé et coupe au milieu d'un tour sont refusés ; frontière de tour et remplacement
  avant le premier appel passent.
- `UPDATE_CA_MATRIX=1` (75 → 78 critères) et `UPDATE_BUDGET=1` (les trois noms entrent
  dans `[ca].required`, seule liste qui grandit, comme pour les critères existants).

## Couverture mesurée sur les dix-sept scénarios

Appels comparés octet pour octet : 43, dont un non épinglé (la relance d'une réponse
vide). Aucun appel n'a porté de niveau 0, 2 ou 4. Remplacements admis dans un tour :
trois niveaux 1 (`niveau-1-et-groupe` 2, le nouveau 1), deux résumés de dépassement
(`depassement-prouve`, le nouveau). `purge` : rien à comparer (payloads purgés).

## Mutation

Plier jusqu'à l'événement de la réponse au lieu de celui d'avant fait échouer `ca_4_5`
au premier appel (« 3 messages dérivés, 2 envoyés », la réponse elle-même en trop).

## Les choix

- **Critères sur un scénario, contrôle sur tous.** Rejouer les dix-sept scénarios coûte
  environ 70 s par passe (la suite en fait déjà trois) ; les critères rejouent le seul
  scénario qui réunit outil, niveau 1 et compaction dans un tour, et exigent des
  compteurs non nuls. Le contrôle, lui, tourne à la fin de chaque scénario.
- **Marqueur `cache_control` retiré de la requête envoyée** avant comparaison : il se
  déplace d'un appel à l'autre sans être du contenu, et l'empreinte l'ignore déjà. Les
  scénarios épinglent `main` (deepseek), sans marqueur ; le retrait ne sert qu'à un
  scénario Anthropic futur.
- **Niveaux 0, 2, 4 comptés, pas comparés** : la réponse porte alors `projection`, et
  refaire ces fonctions pures demanderait les tuiles du prompt, absentes du journal
  (seul leur rendu y est). Le critère exige zéro ; un scénario qui les déclenche le dira.
- **`ca_5_5` lu à la lettre** (« entre deux appels d'un même tour ») : un remplacement
  avant le premier appel du tour (session froide, préfixe froid) ou après le dernier est
  à la frontière de tour.

## Reste et points ouverts

- **`conv.attempt` d'une réponse vide sans `llm_request_id`** (`penelope-agent`,
  `turn.rs`, `AttemptPayload::of_response`) : écart au §2.2, qui le prévoit. La requête
  reste retrouvable par le pliage, mais l'appel n'est pas épinglé. Hors périmètre (boucle) :
  à poser avec le port `AttemptSink` (lot K) ; le contrôle le compte (`unpinned`).
- Aucun scénario à préfixe scellé (`conv.import`) : le contrôle le refuse en le disant.
  Une fixture scellée dans la suite (risque 3 du §7) le couvrirait.

## Vérifications

Pendant le lot : `penelope-evals --test scenarios` (23 tests, dont les trois critères et
le nouveau scénario ; `two_replays…` et `both_history_sources…` inclus en fin de lot),
`--lib`, `--test ca_matrix`, `penelope-archtest`. En fin de lot : `cargo fmt --all
--check` et `cargo clippy --workspace --all-targets -- -D warnings` propres ;
`cargo test --workspace --no-fail-fast` vert, 2 009 tests, aucun échec (le plafond du
daemon compris : le lot ne touche pas au daemon).

## Notes de version, à coller dans `docs/progress.md`

```markdown
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
```

## Brouillon T19 : décision 0017

À poser avec T16, dans `docs/decisions/0017-journal-source-unique.md` (le §5 de la
spécification disait 0011, numéro pris depuis par #205). Titre au format de 0015 :
« 0017 : Le journal d'événements est la source unique de la conversation ». Les passages
entre crochets sont à confirmer à la livraison de T16. Le reste de T19 (`docs/context.md`
§ « Le journal », index de `docs/README.md`, `docs/install-headless.md`, `UPDATE_DOCS`)
n'est pas rédigé ici.

```markdown
# 0017 : Le journal d'événements est la source unique de la conversation

Statut : acceptée ([date de T16]). Portée : §4.1 (event log et chaîne d'audit), §5
(moteur de contexte), §19 (RGPD). Épopée #208, lots E et I ; issues #205 et #206.
Spécification : `design/v1/source-de-verite.md`.

## Contexte

Jusqu'à la 0.17, deux mondes cohabitaient sans se connaître. Le journal `events`
(chaîne de hachage) prouvait le contrôle : tours, effets, approbations. Le contenu de la
conversation vivait ailleurs, dans `messages`, `message_context`, `lcm_nodes` et
`prompt_snapshots`, réécrits en place : la compaction marquait `compacted`, le niveau 1
remplaçait un corps, `/rewind` tronquait, un fork copiait. Conséquences mesurées :

- aucune requête passée n'était reconstituable après une compaction : `audit show`
  relisait les lignes d'aujourd'hui (#205 a journalisé le prompt système, pas
  l'historique) ;
- une réponse vide ou un flux coupé ne laissait aucune trace de ce que le modèle avait
  rendu (#206) ;
- la chaîne d'audit prouvait tout sauf ce que le modèle avait lu, et une ligne modifiée
  à la main ne se voyait nulle part.

## Décision

1. **La conversation est une suite d'événements `conv.*`** (`system`, `user`,
   `context`, `assistant`, `tool_result`, `attempt`, `summary`, `rewind`, `fork`,
   `import`), versionnés (`"v": 1`), chacun avec son opération de surface : `append`,
   `replace` d'une plage, `cut`, `inherit`, `seal`. Un contenu ne change jamais en place :
   le niveau 1, la compaction, le retour arrière sont des événements qui remplacent.
2. **Ce que le modèle lit est un pliage pur du journal** (`derive`, dans
   `penelope-context`, sans base ni horloge). Les seules transformations non journalisées
   sont les niveaux 0, 2 et 4 et la réparation des paires, fonctions pures dont la
   réponse enregistre l'effet (`projection`).
3. **Les tables sont des caches** : `messages`, `messages_fts`, `message_context`,
   `lcm_nodes`, `prompt_snapshots` sont écrites par un projecteur dans la transaction qui
   suit l'événement, rattrapées depuis un filigrane (`projections_session`), refondues
   par `penelope history reindex` et comparées au journal par `penelope history verify`.
   [Après T16 : plus aucune écriture SQL sur ces tables hors du projecteur, tenu par
   `penelope-archtest`.]
4. **La conversation se relit depuis le journal** (`history.source = "journal"`, défaut
   depuis T14 ; [T16 : la clé est retirée]), par lecture incrémentale.
5. **L'historique d'avant le journal est scellé**, pas recopié : un `conv.import` porte
   l'empreinte du préfixe V0, que `verify` recalcule.
6. **Tentatives hors historique** (#206) : une réponse vide, un flux coupé, un repli
   laissent un `conv.attempt` sans effet sur la surface ; seule la consigne de relance
   entre dans la requête suivante.

## Raisons

C'est le principe que Pénélope applique déjà à sa mémoire (le vault est la vérité,
l'index se reconstruit) et celui de DeepSeek Harness : un journal append-only, une
dérivation pure, des projections jetables. Il donne trois preuves que la 0.17 n'avait
pas : chaque requête se recalcule depuis le journal (critère CA 4.5, comparé octet pour
octet à chaque appel des scénarios enregistrés), les caches se refont sans perte
(CA 4.6), et le cache de prompt ne peut être cassé au milieu d'un tour que par les deux
remplacements qui ne coûtent rien (CA 5.5 ; décision [0008](0008-cache-de-prompt.md)).

## Conséquences assumées

- **Le contenu entre dans la chaîne hachée** : `audit verify` le relit, la base grossit
  d'à peu près la taille de `messages`. Les gros résultats restent en artefact (niveau 1),
  l'événement ne porte que la tête, la queue et le pointeur.
- **Numérotation à trous** : l'adresse d'un nœud est le `seq` de son événement ; les
  événements d'observation consomment des numéros sans produire de nœud. Rien ne doit
  plus supposer `seq + 1`.
- **Un fork est une référence** : la fille plie sa mère jusqu'au point de fork. La purge
  de la mère emporte le préfixe hérité de ses filles ; `penelope purge` les nomme avant
  d'agir.
- **Rétention des tentatives par payload** : le texte partiel d'une `conv.attempt` part
  après `retention.days` par le mécanisme de purge (`event_purges`, hash conservé) ;
  `audit verify` compte alors des événements `purged` dans des sessions vivantes, ce qui
  est attendu.
- **Un journal incohérent est une erreur, jamais une devinette** : `verify` le dit ; en
  production, la lecture retombe sur les caches et `doctor` nomme la session.
- **Les sous-agents** (`MemoryConversation`) n'écrivent rien : leur conclusion entre
  dans le journal du parent comme résultat d'outil.
- [Écarts restants à la livraison de T16 : l'archive d'un `/rewind` encore copiée,
  `conv.attempt` d'une réponse vide sans `llm_request_id`, niveaux 0 et 2 non
  journalisés (à rouvrir si `audit show` doit rendre une requête dégradée sans recalcul).]
```

## Blocages

Aucun.
