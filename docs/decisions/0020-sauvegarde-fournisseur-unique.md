# 0020 : Une sauvegarde, un fournisseur ; GitHub retiré, secrets dans l'archive

Statut : acceptée (7 octobre 2026). Portée : §15 (sauvegarde et restauration), §2.4
(secrets), §6 (vault). Issues #327, #328, #329, #330.

## Contexte

La sauvegarde était éclatée. Le vault partait **en clair** toutes les 15 minutes vers un
dépôt GitHub (`memory.vault_git_remote`) ; une archive chiffrée partait chaque nuit vers un
autre dépôt GitHub (`backup.git_remote`) et, ou, un bucket S3 (`[backup.s3]`). Un
`backup.git_remote` vide retombait sur le dépôt du vault : l'archive partait alors dans un
dépôt en clair, sans test pour le voir.

Depuis le 4 octobre, l'archive dépassait la limite de 100 Mo d'un fichier GitHub : la
sauvegarde nocturne échouait chaque nuit, et l'historique git gardait les archives que la
rotation retirait (dépôt à 860 Mo). Plusieurs jours d'échecs sont passés inaperçus :
`doctor` n'alertait qu'au-delà de 48 h.

L'archive elle-même ne suffisait pas à repartir : ni les valeurs des secrets (jetons OAuth
des serveurs MCP, connexion Codex, clés), ni `data/workspace`, ni `data/mcp-data` (dont la
session d'un pont de messagerie). La phrase de passe ne vivait que dans le trousseau de la
machine à sauvegarder : Mac perdu, archives perdues. `restore-all` finissait par une liste
d'étapes à la main.

Promesse du propriétaire : **une seule sauvegarde, un seul fournisseur ; sur un ordinateur
neuf, on la tire et ça repart comme hier.**

## Décision

1. **Un fournisseur unique**, `backup.provider` : `s3` (Scaleway documenté par défaut,
   AWS, MinIO), `dir` (disque, NAS, volume monté) ou `icloud` (un dossier d'iCloud
   Drive). GitHub n'est plus une destination : `backup.git_remote` et
   `backup.max_push_bytes` sont des clés retirées, lues sans erreur, ignorées avec un
   avertissement au démarrage et une ligne de `doctor`. Le repli sur le dépôt du vault
   disparaît. Un fichier d'avant qui a `[backup.s3]` renseignée et pas de
   `backup.provider` vaut `s3`.
2. **Une archive complète** : base, vault, `config.toml`, skills, workflows, gabarits,
   `mcp.d`, `data/workspace`, `data/mcp-data`, et les **valeurs** des secrets. Restent
   dehors, nommés au manifeste avec leur raison : les modèles locaux (`data/models`,
   rechargeables), les journaux, les index dérivés de la base (#289), les artefacts et
   les médias sauf `backup.include_media`. Sous une racine, aucun lien symbolique n'est
   suivi (il est nommé au manifeste) et la collecte est bornée (2 Gio) ; la restauration
   refuse une archive qui porte un chemin absolu, un `..` ou un lien.
3. **Les secrets sous une double couche.** Ils sont lus dans le magasin (trousseau) au
   moment de la sauvegarde, sérialisés, chiffrés par une clé dérivée de la même phrase
   de passe avec son propre sel (en-tête `PNLPSK01`), puis rangés dans l'archive, elle-même
   chiffrée (`PNLPBK01`). Un tar extrait par erreur ne livre aucune valeur. La phrase de
   passe des sauvegardes n'y est pas : la restauration la reçoit de qui la tape.
4. **Le vault garde son git local** (historique, retour arrière, `mem diff`). Son push
   devient optionnel, `memory.vault_git_push`, **faux par défaut** : il partirait en
   clair, et la sauvegarde chiffrée le couvre déjà.
5. **Le kit de secours** (`penelope backup setup`, `penelope backup kit`) : phrase de passe
   générée (six mots de trois syllabes, plus de 110 bits) ou saisie, fournisseur,
   emplacement, clés S3 et commande de restauration, affichés une fois pour un
   gestionnaire de mots de passe ; la ressaisie de quatre mots le confirme. Chaque
   sauvegarde vérifie qu'elle se déchiffre, et `doctor` date le dernier contrôle.
6. **Une restauration qui finit le travail** (`penelope restore`, `restore-all` en
   alias) : fichiers à leur place (le vault à son chemin), secrets dans le magasin,
   service réinstallé et démarré, `doctor` ; il ne reste que ce qu'elle ne peut pas faire
   (modèles à retélécharger, binaires MCP absents). Tout fichier déchiffré est effacé en
   fin de commande, succès ou échec.
7. **Une alerte à 24 h** sans sauvegarde réussie, au foyer, une fois par jour, avec la
   cause et la commande ; `doctor` en rouge au même seuil ; une ligne au digest du matin.
   Le dossier local `backups/` ne garde que la dernière archive (`backup.keep_local`).

## Raisons

Un fournisseur fait de stockage ne connaît pas de limite de 100 Mo, ne garde pas ce que
la rotation retire, et se résume à « où » : S3 pour hors de la maison, un dossier pour un
NAS, iCloud Drive pour qui n'a rien d'autre. GitHub n'apportait que des contraintes.

Les secrets sont ce qui manquait le plus à une machine neuve : sans eux, chaque serveur
MCP se réautorise à la main, Codex se reconnecte, chaque clé se recherche. Les mettre
dans l'archive ne change pas le modèle de menace : l'archive était déjà ce qui protège la
base et le vault, dont le contenu vaut plus qu'une clé révocable ; la seconde couche
empêche qu'une extraction de travail laisse des valeurs en clair sur un disque.

La phrase de passe devient le seul point de défaillance : d'où le kit, la ressaisie qui
prouve qu'il a été noté, et une doc qui dit sans détour qu'une phrase perdue rend les
archives illisibles.

## Conséquences assumées

- Les anciens dépôts GitHub (vault et sauvegardes) ne sont plus alimentés. La doc dit
  comment les archiver ; une ancienne archive se restaure encore, `git clone` puis
  `penelope restore <archive>`.
- Une archive porte des secrets : la phrase de passe protège désormais aussi les jetons.
  Elle n'est jamais écrite ailleurs que dans le magasin de secrets et le kit.
- Un secret illisible au moment de la sauvegarde (trousseau verrouillé) ne la fait pas
  échouer : son nom est au manifeste, à ressaisir.
- La restauration réinstalle le service sur macOS ; sous Linux, le gestionnaire de
  service n'est pas livré et le compte rendu le dit.

## Alternatives écartées

- **Garder GitHub avec Git LFS** : un quota, un coût, et toujours un historique qui garde
  tout ; c'est un stockage d'objets qu'il fallait.
- **Plusieurs fournisseurs à la fois** (#289 l'avait permis) : la promesse est « une
  sauvegarde » ; deux destinations doublaient les messages d'échec partiel sans protéger
  mieux qu'une rotation sur un stockage fiable.
- **Exporter les secrets en clair dans le kit** : le kit porte la phrase de passe et les
  clés du fournisseur, pas les jetons ; ceux-là restent dans l'archive.
