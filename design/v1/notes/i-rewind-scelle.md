# Notes de livraison : retour arrière dans le préfixe scellé (épopée #208, lot I)

Branche `v1-i-rewind-scelle`, dérivée de `v1` à `7fe0e8a`. Aucune migration.

## Le défaut

Relevé par `i-retrait.md` (« Reste et risques ») : un `/rewind` qui coupe dans le préfixe
scellé d'une session d'avant le journal effaçait des lignes que son `conv.import` compte.
La mère ne se repliait plus (« préfixe fourni Import { messages: 6 }, le journal annonce
Import { messages: 8 } »), sa lecture retombait sur ses caches avec une erreur au journal
du daemon, et `verify` signalait une divergence `journal`. Cas probable : revenir de
quelques échanges sur une ancienne session juste après la mise à jour.

## Ce qui est livré

Décision de l'intégrateur appliquée : le préfixe scellé est une vérité, pas un cache.

- **Les lignes scellées coupées restent en base, masquées** : `sealed = 2` (la colonne
  existe depuis la migration 0019, aucune migration). Leurs contextes figés et leur
  entrée plein texte restent aussi. La coupe du `conv.rewind` s'applique par-dessus le
  préfixe au pliage, comme avant : la requête au modèle est octet pour octet celle d'un
  retour arrière V0 équivalent (test : même session, retour arrière fait avant le
  scellement).
- **Le préfixe se relit entier** (`LegacyPrefix::read_rows`, `sealed != 0`) : le
  `conv.import` compte de nouveau ses lignes, et son empreinte `digest`, inchangée, les
  couvre de nouveau. La note « empreinte non vérifiable » de `verify` et
  `Lineage::cuts_sealed_prefix` disparaissent : l'empreinte est vérifiée dans tous les cas.
- **Écritures** : `truncate_in` (seconde transaction de `rewind_from`) masque les lignes
  du préfixe scellé de la session (jusqu'à l'`offset` de son `conv.import`,
  `seal::sealed_offset_in`) et efface les autres, comme avant ; `Rewound::removed` compte
  les deux. Le rattrapage (`apply_in`, `cut_row`) fait de même ; la refonte
  (`write_plan`) masque tout le préfixe scellé puis démasque ce que la surface garde.
  Les copies scellées d'une fille de fork ou d'une archive restent des copies : coupées,
  elles s'effacent. `copy_sealed_row` recopie aussi une ligne masquée de la mère (refonte
  d'une archive).
- **Lectures des lignes** : `load`, `tail`, `recent_tool_results`, `load_episode`,
  `last_entry` et la recherche (`grep`) ignorent `sealed = 2`. La coupe suivante
  (`rewind_from`) cherche le nœud qui précède parmi les lignes non masquées.
- **Numérotation** (`replay::row_seqs`) : la première ligne neuve d'une session scellée
  suit le préfixe entier, masqué compris (`Lineage::sealed_seq`), comme `insert_row`
  (`MAX(seq) + 1`) l'écrit.
- **`verify`** : une ligne masquée est attendue pour chaque ligne du préfixe que la
  surface n'a plus (`Expected::masked`), avec son contexte ; une ligne masquée de trop est
  `extra_row`, une ligne coupée non masquée `missing_row` ou `extra_row`.

## Tests

- `store/rewrite/tests.rs` (rouge avant le correctif) :
  `a_rewind_inside_the_sealed_prefix_keeps_the_prefix_and_folds` (session scellée de 8
  messages et un résumé actif, un échange défait : la mère se replie sans repli sur les
  caches, lecture du journal et des caches identiques, requête identique à la V0,
  `verify` à zéro, rien à resceller ; un message nouveau numéroté 9 et 10, `verify` à
  zéro ; `reindex` de la mère puis de tout : caches identiques ; un fork après coup) et
  `a_lost_sealed_cut_is_caught_up_as_masked_rows` (le rattrapage masque au lieu
  d'effacer).
- `an_archive_cut_inside_the_sealed_prefix_keeps_its_sealed_rows` vérifie désormais la
  mère aussi.
- `sealed_with_summary` et `caches` rejoignent `replay/fixture.rs` (déplacement seul).

## Les choix

- **`sealed = 2` plutôt qu'une copie dans le `conv.rewind`** : la décision garde les
  lignes en base ; porter les lignes coupées dans l'événement aurait rendu le préfixe
  dépendant d'un payload que la rétention peut purger.
- **Plein texte gardé, filtré à la lecture** : `penelope-store` refait `messages_fts` de
  toutes les lignes (réparation d'intégrité) ; effacer l'entrée d'une ligne masquée
  aurait divergé à la première réparation. L'archive, elle, garde la ligne cherchable.

## Reste et risques

- Hors périmètre, trois lectures de `messages` ne filtrent pas `sealed = 2`, sans effet
  observé : le titre de repli d'une session (`penelope-kernel/src/budget.rs`, premier
  message utilisateur), `freeze_volatile` (`penelope-daemon/src/cache_audit.rs`, dernier
  message utilisateur : un tour écrit toujours le sien avant, numéroté après les lignes
  masquées) et la fenêtre de citation de l'exécuteur (`meta.rs`, `MAX(seq)`). Toute
  lecture nouvelle des lignes doit ajouter `sealed != 2`.
- `design/v1/source-de-verite.md` §4.5 dit « marquées `sealed = 1` » : à compléter par
  l'intégrateur (hors périmètre).
- Une base où l'ancien code a déjà effacé des lignes scellées (v1 non publiée, donc a
  priori aucune) reste en divergence `journal` et `digest` : rien ne les recrée.

## Vérifications

Pendant le lot : `penelope-context` (161 tests), `penelope-ops`, `penelope-evals --test
scenarios` (23 sur 23, CA 4.5, 4.6 et 5.5 compris ; aucun `surface.jsonl` modifié).
Fin de lot : voir le rapport.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Journal : un retour arrière dans l'historique d'avant la mise à jour (#208)

- **Un `/rewind` sur une ancienne session ne casse plus sa lecture** : revenir de
  quelques échanges juste après la mise à jour, avant tout message nouveau, retirait des
  messages que le scellement de l'historique compte ; la session se relisait alors dans
  ses caches, avec une erreur au journal, et `penelope history verify` la signalait. Les
  messages scellés défaits restent en base, masqués : la conversation vue par le modèle
  ne change pas, l'empreinte du scellement se vérifie de nouveau, et l'archive du retour
  arrière garde ce qu'elle gardait.
```
