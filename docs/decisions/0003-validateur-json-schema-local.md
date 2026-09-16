# 0003 — Validateur JSON Schema local plutôt qu'une bibliothèque

Statut : acceptée. Portée : §3.2, §8.4, §13.3, §14.7.

## Contexte

Il faut valider les `inputSchema` et `outputSchema` MCP, le `structuredContent` renvoyé
par un serveur, les paramètres de workflow et les formulaires Telegram. Des crates de
validation JSON Schema existent et sont matures.

## Décision

Un validateur maison, dans `penelope-kernel::schema`, couvrant un sous-ensemble de
JSON Schema 2020-12.

## Raisons

1. **La résolution `$ref` doit être bornée.** Un schéma MCP est du contenu non fiable
   (§13.3). Une bibliothèque généraliste résout volontiers un `$ref` distant, ou boucle
   sur une référence cyclique. Ici, un `$ref` non local échoue avec le message « non
   résoluble (externe ou cyclique) » et la profondeur est plafonnée. Un test le vérifie :
   aucun schéma ne peut déclencher une requête réseau.
2. **Le moteur de formulaires a besoin des annotations.** `default`, `description`,
   `enumNames` et l'ordre des champs requis servent à construire une carte Telegram. Une
   API de validation qui rend seulement « valide / invalide » oblige à reparcourir le
   schéma en parallèle, donc à le réinterpréter deux fois.
3. **Les messages d'erreur partent au modèle.** Ils sont rédigés en français, avec le
   chemin JSON Pointer de l'erreur, parce que le modèle doit pouvoir corriger son appel
   d'outil sans intervention humaine.

## Conséquences

- Mots-clés couverts : `type`, `enum`, `const`, `properties`, `required`,
  `additionalProperties`, `patternProperties`, `items`, `prefixItems`, `minItems`,
  `maxItems`, `uniqueItems`, `minimum`, `maximum`, `exclusiveMinimum`,
  `exclusiveMaximum`, `multipleOf`, `minLength`, `maxLength`, `pattern`, `allOf`,
  `anyOf`, `oneOf`, `not`, `$ref` local, `$defs`, `definitions`.
- Non couverts, donc ignorés comme des annotations : `if` / `then` / `else`,
  `dependentSchemas`, `unevaluatedProperties`, `contains`, `format` (non contraignant).
  Un schéma qui s'appuie sur eux est accepté plus largement qu'il ne devrait : c'est une
  validation permissive, jamais un refus injustifié.
- `schemas/workflow.schema.json` se tient volontairement dans ce sous-ensemble, pour
  rester vérifiable par l'outil du projet.
