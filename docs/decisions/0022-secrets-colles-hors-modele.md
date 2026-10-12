# 0022 : Un secret peut entrer par la conversation, par un seul chemin, sans modèle

Statut : acceptée (12 octobre 2026). Portée : §13.1 (secrets), décision
[0020](0020-sauvegarde-fournisseur-unique.md) pour le magasin, et la règle « un secret ne
se saisit jamais dans une conversation » (#252, #328). Issue #350.

## Contexte

Le 12/10, le run `backlog` MentalEco devait déployer `dev`. Il lui manquait des clés
MinIO et des mots de passe que seul le propriétaire détient. Le propriétaire, joint
seulement par Telegram, les a collés dans le chat ; trois refus ont suivi :

- **Le modèle.** Codex a refusé de recopier un secret du chat dans une commande, quoi que
  dise le propriétaire.
- **Le produit.** `/secret` ne saisit jamais.
- **Le terminal.** `penelope secret set` demande un terminal sur la machine, accessible
  seulement en SSH, avec le Trousseau verrouillé.

Le collage était de toute façon parti en clair dans `tg_updates`, avant tout traitement.
Le run est resté bloqué, et la règle censée protéger les secrets ne les protégeait pas.

## Décision

1. **`/secrets <projet>`** (et `penelope secret import <projet>`) est le seul chemin d'un
   secret par une conversation. Le message suivant du propriétaire, dans ce chat et ce
   sujet, sous 5 minutes, est intercepté par la passerelle **avant** toute écriture :
   - la ligne de dédoublonnage `tg_updates` est posée sans contenu ;
   - aucun tour n'est mis en file et aucun modèle ne voit le message ;
   - le message est supprimé du chat.
2. **Découpage déterministe** (`penelope_platform::secrets::import`). Les sections donnent
   l'environnement, et chaque ligne `clé : valeur` ou `CLÉ=valeur` devient
   `ci.<projet>.<env>.<libellé>`. Une ligne illisible n'est rendue que par son numéro. Aucun
   modèle, même local : un modèle n'apporterait que du risque à un découpage que des
   règles font sans erreur.
3. **Usage par référence.** *Révisé en 1.0.54 (#352).* `shell_exec` reçoit
   `secrets: {"VARIABLE": "ci.projet.env.libellé"}`. Le daemon passe la valeur en
   variable d'environnement de la commande et l'ajoute au caviardage ; la commande n'écrit
   que `$VARIABLE`, et le résultat comme les journaux la masquent. C'est la règle générale
   pour toute commande (`glab variable set`, `psql`, `docker login`…). Elle remplace
   l'outil `ci_variable_set` de la 1.0.53, propre à GitLab, et sa liste `tools.ci_hosts`.
   Seuls les secrets collés (préfixe `ci.`) s'injectent ; un secret interne jamais. Le prix
   est assumé : une commande qui a la valeur peut l'envoyer où elle veut, ce que le
   propriétaire accepte pour les secrets qu'il colle (dev, qa).
4. **`/secret`** reste la liste et la suppression ; il renvoie à `/secrets`.

## Conséquences

- Un secret tapé ailleurs dans la conversation reste ce qu'il était : du texte que voit le
  modèle. La phrase de `/secret` le dit.
- En privé, Telegram ne permet de supprimer un message que dans les 48 h, et en groupe il
  faut le droit `can_delete_messages`. Une suppression refusée est journalisée, et le
  secret est rangé quand même.
- Un collage que Telegram coupe à 4 096 caractères est reçu en plusieurs messages. La
  capture reste armée une minute de plus, et chaque morceau est découpé seul : une ligne
  coupée en deux est signalée illisible.
