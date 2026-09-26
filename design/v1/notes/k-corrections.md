# Lot k-corrections : dix défauts relevés par les lots de tests

Épopée #208. Un commit par défaut, chacun avec un test rouge avant le correctif.

## Livré

1. **#219, « Ignorer » sur une contradiction** : `decide_approval` rend `Decided`
   (`Approved`, `Denied`, `AlreadyDecided`) au lieu d'un booléen lu tantôt comme
   « approuvée », tantôt comme « a gagné ». Tous les lecteurs relus : contradictions
   (le refus appelle désormais `apply_contradiction`), revue des propositions (« Rien »
   sur une demande déjà tranchée dit « Déjà tranché »), `finalize_decision` (un second
   refus dit « Déjà tranché » au lieu de redire « Refusé »), plafond de budget, harnais
   des scénarios (`resumable` = approuvée, inchangé). Tests
   `ignoring_a_contradiction_applies_it`, `a_second_deny_says_already_decided`.
2. **#220, raison d'un run bloqué** : `finish` écrit la raison quand l'état est déjà
   posé et que `run.error` est vide ; un état ou une raison déjà posés ne sont pas
   réécrits. Assertion ajoutée à `a_failing_step_is_retried_before_its_failure_counts`.
3. **#221, planification inconnue** : `Schedules::set_state` rend `bool` (une ligne non
   supprimée a changé) ; `schedule.pause`, `schedule.resume`, `schedule.rm` refusent par
   « planification inconnue : <id> », affiché tel quel par Telegram (`❌ …`) et la CLI.
   Une planification supprimée compte comme inconnue (on ne la reprend plus). Test
   `unknown_schedules_are_refused`.
4. **`embed_texts`** : un vecteur vide n'est jamais écrit dans `embeddings_cache`. Test
   `an_empty_vector_is_never_cached`.
5. **`why_composed`** : la boucle reprend après une quote simple fermée ; `echo 'a'; b`
   est nommé par `;`.
6. **`private_host`** : une entrée IPv6 (nue ou entre crochets avec port) est lue comme
   telle : `::1`, `::`, fc00::/7, fe80::/10, IPv4 mappées relues comme IPv4.
7. **`html_to_text`** : tout ce qui suit un `<!--` jamais fermé est écarté.
8. **Préfixe de chemin vide** : `describe_pattern` dit « path à la racine de l'espace de
   travail » (le préfixe vide ne vise que les fichiers directement à la racine).
9. **`tools_on_demand::touch`** : lecture, modification et écriture dans une seule
   transaction du rédacteur unique (`update`), pour `touch` comme pour
   `exposed_for_turn`. Test de concurrence `parallel_touches_keep_every_promotion`.
10. **`chat.stream`** : la vidange finale saute l'issue du tour comme la boucle ; plus de
    `done` (ni d'`error`) en notification avant la réponse finale. Test
    `chat_stream_never_sends_a_done_notification` (seize tours sur la vraie socket).

## Attendus changés

- `commandes-approbations` (étape 4, `/policies`, et sa ligne d'outbox) : « path sous
  «  » » devient « path à la racine de l'espace de travail ». Seul ce libellé bouge.
- Harnais des scénarios (`harness/rpc.rs`) : le filtre qui écartait `done` des
  notifications de `chat.stream` est retiré ; `rpc-conversation` garde ses attendus
  (cinq rejeux identiques).

## Choix

- #219 : un enum plutôt qu'un changement de sens du booléen : le compilateur a désigné
  chaque lecteur, aucun ne peut garder silencieusement l'ancienne lecture.
- #221 : l'outil `schedule_delete` (côté modèle) garde son comportement (`Ok` sur un
  identifiant inconnu) : hors brief.

## Relevés, non corrigés

- `html_to_text("a < b")` rend « a a < b » : le texte avant un `<` jamais fermé est
  poussé deux fois (même `break` que le commentaire, autre branche).
- L'outil `schedule_delete` répond `{"deleted": true}` pour un identifiant inconnu.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Dix défauts relevés par les tests de la V1 (épopée #208, lot k-corrections)

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
```
