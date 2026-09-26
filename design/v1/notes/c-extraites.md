# Notes de livraison : c-extraites (couverture des crates sorties du daemon, critère 7)

Branche `v1-c-extraites`, sur `v1` à `1f6fbc5` (1.0.0-alpha.17), sans rebase. Périmètre :
tests des crates `penelope-gateway-telegram`, `penelope-ops`, `penelope-orchestrator`,
`penelope-mcp-host`, `penelope-dream` ; ces notes. Aucun changement de comportement du
produit, aucune ligne de `budget.toml`, rien dans `upgrade` ni `skill_install` de
`penelope-ops` (lot `s-derniers`).

## Résultat

Mesure `cargo llvm-cov --workspace`, lignes, avant (`1f6fbc5`) et après (tête de la
branche) :

| Crate | Avant | Après | Lignes non couvertes |
|---|---|---|---|
| gateway-telegram | 74,5 % | **87,9 %** | 2 602 → 1 231 |
| ops | 78,5 % | **87,0 %** | 1 335 → 805 |
| orchestrator | 80,6 % | **87,4 %** | 649 → 422 |
| mcp-host | 80,2 % | **89,1 %** | 434 → 240 |
| dream | 82,2 % | **87,8 %** | 716 → 491 |
| daemon | 87,6 % | 88,2 % | 788 → 752 |
| app | 87,1 % | 87,2 % | 374 → 370 |
| workspace | 86,6 % | **89,2 %** | |

Objectif du lot : chaque crate extraite au moins au niveau du daemon de `main` (86,3 %),
le daemon au moins à 86,3, `app` sans baisse. Tenu partout. Le total du workspace dépasse
celui de `main` au point de fourche (88,3 %).

## Pourquoi les crates extraites avaient baissé

Ce n'était pas du code devenu non couvert. Lignes non couvertes, modules du daemon de
`main` contre crate de `v1` : Telegram 2 585 contre 2 602, rêve et ingestion 683 contre
716, workflows et ordonnanceur 679 contre 649. Au total, `v1` avait **moins** de lignes non
couvertes que `main` (13 647 contre 14 045), mais 18 700 lignes comptées de moins.

La différence est le dénominateur : `cargo llvm-cov` compte les tests écrits dans le
fichier source (`#[cfg(test)] mod tests { … }`, couverts à 100 % par construction) et
ignore les fichiers `tests.rs` et `tests/`. Sur `main`, `telegram.rs` portait ses tests
en ligne (11 559 lignes comptées) ; l'extraction les a rangés dans `tests/`, hors du
compte. Le pourcentage a baissé mécaniquement. Viser 86,3 % par crate revenait donc à
couvrir environ 2 200 lignes de produit que `main` ne couvrait pas non plus.

Conséquence de méthode : tous les tests ajoutés vivent dans des fichiers de tests
(`tests.rs`, `tests/*.rs`, `<module>/<nom>_tests.rs`), jamais en ligne. Ils ne gonflent
pas le chiffre par leurs propres lignes : ce qui monte est du code du produit exercé.

## Ce qui est testé, par crate

- **gateway-telegram** : les écrans cliquables (#30), construits avec des données et
  cliqués, confirmation comprise (mémoire, pratiques, intentions, règles, runs, serveur
  MCP, catalogue et affectation de modèle, skills, secrets, mise à jour, heures
  silencieuses, retour en arrière, aide, état, journal, prompts MCP) ; le démarrage de la
  passerelle (commandes publiées, effets incertains annoncés une fois, réception malgré une
  coupure, arrêt avec le daemon) ; cartes particulières (point de contrôle de coût,
  alertes réseau, lancement de workflow, OAuth impossible) ; commandes `/upgrade`,
  `/stop tout`, `/schedules`, `/mcp`, `/secret`, `/new`, `/rewind`, `/model` ; boutons hors
  du chemin nominal ; formulaire d'un prompt MCP ; relance d'une demande MCP expirée ;
  rapport de `/stop` et envoi en pièce jointe. Aides `screen_of`, `token_of`, `press`,
  `confirm` dans le banc d'essai.
- **ops** : connexion Codex hors nominal et boucle hors tour, contrôles de `doctor`
  (secrets, mémoire, cohérence, machine, passe complète), sauvegarde poussée dans un dépôt
  local et sauvegarde nocturne en échec, opérations de session, sous-ensemble YAML et
  fichiers de Hermes, sondes pip et npm des dépendances de skill.
- **orchestrator** : `retry-step` et `skip-step`, admission des runs en file, relances
  d'une étape, étapes shell et tool en échec, attentes (événement, cron, délai, tâche MCP
  mal désignée), livrables d'un prompt planifié, gabarits des paramètres.
- **mcp-host** : serveur de retour OAuth local, appels d'outils hors nominal (MRTR sans
  fin, −32042 sans lien, refus d'en-têtes, trousseau fermé, tâches), entretien et refus de
  l'administration.
- **dream** : carte de contradiction et question rangée dans `DREAMS.md` (#145), aucune
  couverte sur `main` ; opérations `update_exception`, `record_ecart`, `link`,
  `create_entity` et leurs refus.

Les tests de la passerelle construisent un `Daemon::from_services` comme les autres tests
de la crate : `TelegramGateway` prend un `Core`, qui vit dans `penelope-daemon`. Les autres
crates n'en construisent aucun.

## Défauts relevés (non corrigés : aucun changement de comportement dans ce lot)

1. `penelope-gateway-telegram`, `callbacks.rs`, bras des contradictions (#145) :
   `decide_approval` rend `decision.approved`, lu comme « première décision ». « Ignorer »
   répond donc « ℹ️ Déjà tranché. » et n'appelle jamais `apply_contradiction` : le candidat
   reste en état `question`. Le test vérifie le refus de la demande, pas le message.
2. `penelope-orchestrator`, `driver.rs`, `finish` : quand `advance` a déjà mis le run en
   `blocked` (transition `$blocked`), `set_state` est sauté et la raison n'arrive jamais
   dans `run.error` ; elle n'est que dans l'événement `workflow.finished`.
3. `/schedules pause <identifiant inconnu>` répond « en pause » : `SCHEDULE_PAUSE` ne
   vérifie pas l'existence.
4. `penelope-vault`, `embeddings::embed_texts` : un vecteur vide rendu par le fournisseur
   est gardé en cache ; la sonde de `doctor` le montre encore une fois le fournisseur
   réparé.

## Laissé de côté

- `penelope-ops` : `upgrade` et `skill_install`, touchés par `s-derniers`.
- Écran `upgrade.switch` : son contrôle préalable interroge la machine réelle (signature,
  répertoire d'installation, service) ; `/upgrade install` et `upgrade.check` appellent le
  réseau.
- Effets incertains tranchés depuis Telegram : il faut un effet réel dans le ledger.

## Notes de version

#### Couverture des crates sorties du daemon (critère 7)

- Les crates extraites du daemon repassent au-dessus du niveau du daemon de `main`
  (86,3 %) : passerelle Telegram 87,9 %, ops 87,0 %, orchestrateur 87,4 %, hôte MCP
  89,1 %, rêve 87,8 % ; le workspace passe de 86,6 % à 89,2 % (88,3 % sur `main`).
- La baisse venait du dénominateur : les tests en ligne de `main` comptaient comme lignes
  couvertes, rangés dans `tests/` ils ne comptent plus. Les tests ajoutés vivent tous dans
  des fichiers de tests.
- Cent dix-neuf tests, sans changement de comportement : écrans cliquables de Telegram,
  démarrage de la passerelle, contrôles de `doctor`, connexion Codex, sauvegardes,
  contrôle des runs, attentes, OAuth local, contradictions de la mémoire.
- Quatre défauts relevés et consignés dans `design/v1/notes/c-extraites.md`, dont
  « Ignorer » une contradiction depuis Telegram qui répond « Déjà tranché » sans écarter
  le candidat.
