# 0007 — `deploy-generic` passe par les cibles `make` du dépôt

Statut : acceptée. Portée : §12.10 (workflows livrés).

## Contexte

Le PRD décrit `deploy-generic` comme « piloté par la config du dépôt
(`.penelope/deploy.toml` : commandes, environnements, vérifications post-déploiement,
rollback) ». Une étape `shell` exécute une commande écrite dans le workflow ; elle ne sait
pas lire un fichier TOML pour en extraire une autre commande.

## Décision

`deploy-generic` exige `.penelope/deploy.toml` (lu, et visible dans la trace du run) et
exécute les cibles `make deploy`, `make smoke` et `make rollback` du dépôt, avec
`ENV=<environnement>`. Les tests et le lint de `ticket-to-deploy` suivent la même
convention (`make test`, `make lint`), avec repli sur l'outil de l'écosystème détecté
(`cargo`, `npm`, `go`).

## Raisons

1. **Pas de commande arbitraire lue dans un fichier du dépôt** : les commandes
   exécutées restent celles, lisibles, du workflow validé au chargement ; le dépôt ne
   fournit que des cibles nommées.
2. **Le Makefile est la convention la plus répandue** pour « déployer ce dépôt » et se
   relit en SSH.
3. **Aucun outil natif de plus** à spécifier, classer par risque et sandboxer.

## Conséquences

Un dépôt sans Makefile doit en ajouter un de quelques lignes, ou l'utilisateur dépose un
workflow `deploy-generic` à lui, qui remplace celui livré (même identifiant). Le contenu de `deploy.toml` n'est
pas encore interprété : c'est un marqueur de dépôt déployable.
