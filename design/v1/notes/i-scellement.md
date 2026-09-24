# Notes de livraison : lot E suite, scellement de l'historique (épopée #208, T11)

Branche `v1-i-scellement`, dérivée de `v1` à `fbf1fe5` (1.0.0-alpha.4), poussée sur
`origin`. Deux commits de code, un de notes. T12 n'est pas codé : son plan est plus bas.

## Commits

| Commit | Quoi |
|---|---|
| `Migration 0021 : index des lignes V0 à sceller, projections mortes supprimées` | `0021_history_seal` et son test |
| `Journal : l'historique V0 est scellé au démarrage par un conv.import par session` | `store/seal.rs`, `daemon/src/history.rs`, un appel dans `runtime.rs`, filet de la fixture 0.17 |

## Ce qui est livré

- **Migration `0021_history_seal`** (`migrations/since_0011.rs`). La 0020 avait déjà posé
  `messages.sealed` et `messages.event_id` : rien n'y est repris. La 0021 ajoute l'index
  partiel `messages_unsealed(session_id, seq) WHERE event_id IS NULL AND sealed = 0`, que
  l'étape de boot relit à chaque démarrage (vide dès que tout est scellé ; la requête le
  force par `INDEXED BY`, sans quoi SQLite prend `messages_event`, qui parcourt aussi les
  lignes déjà scellées), et supprime `projections_workflow` et `projections_approval` :
  aucun code ne les lit ni ne les écrit (`grep` sur `crates/`, `docs/`, `scripts/`).
  `projections_session` reste, pour le filigrane de T13.
- **Cœur du scellement** (`penelope-context/src/store/seal.rs`) :
  `HistoryStore::seal_legacy() -> SealReport`, `HistoryStore::sealed_prefix(session) ->
  Option<(ImportPayload, LegacyPrefix)>`, `LegacyPrefix::{offset, digest, payload,
  sealed}`, `SEAL_FORMAT`.
- **Étape de boot** `history::seal_legacy(&Services)` (`penelope-daemon/src/history.rs`,
  module nouveau prévu par la spécification), appelée en tête de `Daemon::recover` : une
  ligne dans `runtime.rs`.
- **Tests** : `history/tests.rs` (base de trois sessions V0, une simple, une compactée,
  une vide, plus une session déjà journalisée : premier démarrage deux `conv.import`,
  second aucun, chaîne vérifiée ; `derive` d'une session scellée redonne ses lignes et la
  projection V0, un message postérieur vient après l'`offset` ; l'empreinte suit le
  contenu, pas la compaction) ; `migrations/tests.rs` (0021) ;
  `tests/migration_from_0_17.rs` (étape 6 nouvelle : la fixture migrée est scellée, le
  second passage ne fait rien, l'empreinte se relit, la dérivation donne le nœud actif
  puis les messages 9 à 12, la chaîne compte un événement de plus).

## Les choix

- **Le cœur est dans `penelope-context`, pas dans le daemon.** Le test de la fixture
  (`penelope-store`) doit rejouer le scellement ; il ne pouvait l'appeler dans le daemon
  qu'en compilant tout le daemon comme dépendance de développement. `penelope-context`
  dépend déjà du store et du noyau ; le test du store le prend en dépendance de
  développement, comme `penelope-kernel` (cycle toléré par Cargo, ignoré par archtest).
  `history.rs` ne garde que l'étape de boot et sa journalisation. Question posée au lead
  en début de lot, sans réponse, option appliquée comme annoncé.
- **Quelles sessions.** Candidate : une session qui a au moins une ligne `event_id IS
  NULL AND sealed = 0`. Scellée si son journal n'a **aucun** `conv.*` ; sinon laissée
  telle quelle et listée dans `SealReport::skipped` (avertissement au démarrage) : un
  `conv.import` doit être en tête du journal (§2.3), il ne peut plus l'être. Ce cas
  couvre un fork ou une archive de retour arrière écrits après T10, et une base d'alpha
  qui a tourné en double écriture avant T11. Une session sans message n'est pas candidate.
- **Une transaction par session** : lecture du préfixe, `EventLog::append_in`, marquage
  `sealed`, commités ensemble ; pas de fenêtre de crash entre l'événement et le
  marquage, donc pas de réparation au démarrage suivant. `append_in` ne diffuse pas en
  direct : l'étape tourne dans `recover`, avant tout abonné.
- **Forme canonique** (documentée en tête de `seal.rs`) : un tableau JSON sans objet
  (le workspace active `preserve_order`, l'ordre des clés ne doit pas compter),
  `["penelope.seal.v1", lignes, contextes, nœuds]`, sha256 en hexadécimal. Lignes :
  `seq, role, content, tool_call_id, tool_name, tokens_est, ts, episode, eager,
  artifact_id`, `content` tel que stocké ; contextes `seq, context` jusqu'à l'offset ;
  nœuds actifs (règle de `Lcm::active_nodes`) `node_id, from, to, tokens_self, summary`.
- **Écart assumé au §4.5 : `compacted` n'entre pas dans l'empreinte.** C'est un état
  dérivé : une compaction du préfixe après le scellement (`mark_compacted`) le change
  sans rien changer au contenu, et `verify` crierait à la falsification à chaque
  compaction d'une vieille session. Il est porté par `lcm_active` ; la relecture
  (`LegacyPrefix::sealed`) le recalcule par couverture des nœuds actifs, ce que fait la
  projection V0 (`conversation.rs`, qui repart après la fin du dernier résumé). La
  fixture 0.17 en donne le cas : ses messages 1 à 8 sont couverts par le nœud actif sans
  porter le drapeau. `tokens_est` et `eager` y entrent en revanche, parce que la
  surface les relit (budget, niveau 0). À reprendre dans la décision de T19.
- **`sealed_prefix` relit les nœuds par l'identifiant que l'import nomme**, avec les
  bornes de l'import : un `superseded_by` posé depuis (prolongation) ne retire pas un
  nœud du préfixe.
- **Scénarios** : `cargo test -p penelope-evals --test scenarios` 18 sur 18 sans aucun
  attendu régénéré : dans les scénarios, tout message passe par la double écriture, il
  n'y a rien à sceller, donc aucun `conv.import`.

## Défaut repéré, hors périmètre (signalé au lead)

`HistoryStore::freeze_context` (`store/dual.rs`) journalise `conv.context.target` =
`events.seq` du `conv.user`, sans l'`offset` de la session. Pour une session scellée
(offset = `MAX(messages.seq)` V0) ou forkée (T10), l'adresse d'un nœud est
`offset + seq` : le contexte d'un message écrit après l'import est rangé sous une adresse
fausse, qui peut tomber sur un message du préfixe. Correctif : ajouter l'offset du
`conv.import` ou du `conv.fork` de tête. Tant qu'il manque, `verify` (T12) verra une
divergence de contexte sur toute session scellée qui reçoit un nouveau message.

Deuxième point pour T12 : une ligne écrite après le scellement reçoit
`messages.seq = MAX(seq) + 1` (`dual.rs`, `insert_row`), alors que son adresse dérivée est
`offset + events.seq`. Les deux numérotations divergent dès le premier message : la
comparaison doit apparier par `event_id`, jamais par `seq`.

## Plan de T12 (`penelope history verify`), non codé

Dépend de T7, T8, T10 (autre agent) : `conv.summary`, remplacement niveau 1, `conv.fork`
et `conv.rewind` doivent être journalisés pour qu'une session à jour se dérive sans
erreur.

1. **Lecture** (`penelope-context`, `store/verify.rs`, pur SQL et pliage, pour la même
   raison que `seal.rs` : testable sans daemon) : `HistoryStore::session_prefix(sid) ->
   Sealed` : `conv.fork` en tête → `Sealed::fork` récursif sur la mère (préfixe de la
   mère, événements de la mère, `up_to`) ; `conv.import` → `sealed_prefix(sid)?.sealed()`
   ; sinon `Sealed::none()`. Garde-fou de profondeur (fork de fork) et de cycle.
2. **Comparaison**, `verify_session(sid) -> SessionReport { session, divergences:
   Vec<Divergence> }`, `Divergence { node: Option<i64>, what, expected, found }` :
   - digest : `import.digest == sealed_prefix().1.digest()` ;
   - lignes : `derive(prefix, events).entries()` hors préfixe contre les lignes
     `messages` à `event_id` non nul, appariées par `event_id` (adresse = offset +
     `events.seq`), comparées par nombre, rôle, empreinte sha256 du contenu sérialisé
     (`serialise_content`), `artifact_id`, `compacted` (surface : masqué ; ligne :
     drapeau) ;
   - lignes du préfixe : `sealed = 1` toutes présentes (nombre = `import.messages`) ;
   - contextes : `surface.contexts` contre `message_context` (adresse traduite en `seq`
     de ligne par `event_id`) ; dépend du correctif d'offset ci-dessus ;
   - nœuds LCM : bornes des résumés actifs de la surface contre `Lcm::active_nodes`
     (bornes traduites de même) ;
   - une `DeriveError` est elle-même une divergence (le journal est incohérent).
   `verify_all()` itère sur `SELECT id FROM sessions` plus les `session_id` distincts
   de `messages` (une session supprimée peut garder des lignes).
3. **Daemon** : `history::verify(&Services, Option<&str>) -> Report` dans
   `daemon/src/history.rs` (le module existe) ; méthode RPC `history.verify` dans
   `rpc/methods/ops.rs`, à côté de `audit.verify` (`method::HISTORY_VERIFY` dans
   `penelope-kernel/src/api.rs`, liste des méthodes ligne 343) ; commande CLI
   `penelope history verify [--session <id>]` dans `penelope-cli/src/commands.rs`
   (rapport JSON, code de sortie non nul à la première divergence, comme
   `audit-verify`) ; ligne `doctor` dans `doctor/coherence.rs` (nombre de sessions
   divergentes, bornée en temps : sessions actives des sept derniers jours).
   `UPDATE_DOCS=1 cargo test -p penelope-evals --test docs` (commande et méthode
   nouvelles).
4. **Tests de fin** (§5 T12) : sur les bases produites par les scénarios (tour simple,
   outils, compaction, niveau 1, fork, rewind, purge) zéro divergence, via un crochet dans
   le harnais `penelope-evals/src/scenario/harness` qui appelle `verify_all` à la fin de
   chaque scénario ; `UPDATE messages SET content` manuel → une divergence qui nomme la
   session et le nœud ; ligne scellée modifiée → divergence de digest
   (`the_digest_follows_content_not_compaction` en donne déjà la moitié).

## Vérifications

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` :
verts. `cargo test --workspace --no-fail-fast` : vert sauf `penelope-archtest` (deux
rouges tolérés, ci-dessous). Ciblés pendant le lot :
`penelope-store` (migrations, `migration_from_0_17` et son générateur ignoré),
`penelope-daemon --lib history`, `penelope-evals --test scenarios` (18/18),
`penelope-archtest`. Rouges tolérés et seulement eux : `daemon_modules_are_whitelisted`
(module `history`, prévu par la spécification, à ajouter à `[daemon].modules`) et
`crates_stay_under_their_ceiling` (le daemon grossit de 257 lignes, dont 225 de tests).
`store.rs` (liste de référence) garde exactement sa taille.

## Notes de version, à coller dans `docs/progress.md`

```markdown
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
```

## Blocages

Aucun. Le lead ajuste `[daemon].modules` et le plafond `[crates]` du daemon.
