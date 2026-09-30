# Comparaison avec OpenClaw et Hermes

Relevé du 18 septembre 2026, sources dans la documentation de chaque projet. C'est un
**instantané, non revérifié depuis** : les deux autres projets ont avancé, Pénélope aussi
(la colonne Pénélope décrit la 0.17 de cette date ; ce que la 1.0.x a ajouté depuis est
dans le [README](../README.md) et dans [progress.md](progress.md)). Ces tableaux ont
quitté le README le 30 septembre 2026 pour ne pas y vieillir en silence.

Les deux projets les plus proches sont [OpenClaw](https://github.com/openclaw/openclaw)
(TypeScript, 390 000 étoiles) et [Hermes](https://github.com/NousResearch/hermes-agent)
(Python, 246 000 étoiles). Pénélope en compte zéro, tourne sur une seule machine et n'a
qu'un utilisateur. La comparaison qui suit ne porte donc pas sur l'écosystème, où il n'y
a pas de match, mais sur les mécanismes.

## Mémoire

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Forme | wiki Markdown, 7 niveaux, identifiants de bloc et wikilinks, index SQLite | fichiers Markdown (`USER.md`, `MEMORY.md`, notes du jour), index SQLite | 2 fichiers Markdown plafonnés à 2 200 et 1 375 caractères, SQLite FTS5 |
| Écriture | candidats typés, jamais écrits directement en mémoire | l'agent écrit, tour silencieux de rappel avant compaction | l'agent écrit, erreur rendue si le plafond est franchi |
| Consolidation automatique | oui, 3 h 30, grille à 5 critères, le modèle argumente, le code décide | oui, 3 h, score à 6 signaux et triple seuil | non, l'agent doit faire le ménage lui-même |
| Réversibilité | `supersede`, `replace`, `retire`, pré-image de chaque opération, vault commité dans git | suppression par session, édition manuelle | édition manuelle, revue par `/journey` |
| Provenance | origine par entrée, contenu ingéré marqué non fiable | classe d'origine, consolidation protégée du contenu non fiable | non documenté |
| Secrets rencontrés | extraits vers le magasin, la mémoire ne garde que `${SECRET:nom}` | non documenté | non documenté |

## Contexte et sessions longues

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Découpage | 5 tuiles T0 à T4, stabilité déclarée par tuile | moteur enfichable, pas de couches documentées | pas de couches, double seuil 85 % puis 50 % |
| Compaction | 5 niveaux, un seul appelle un modèle | automatique et `/compact`, garde les tours récents | résumé structuré à gabarit fixe, mis à jour d'une compression à l'autre, repli déterministe |
| Préfixe de cache | identique octet pour octet, vérifié par un test | élagage au TTL, battement conseillé pour garder le cache chaud | règle écrite de non-mutation, 4 points de rupture |
| Plafond de contexte | indépendant de la fenêtre du modèle, déclenché sur le prompt réellement facturé, seuil abaissé sur fenêtre courte ; chiffres dans [context.md](context.md), vérifiés contre le code | plafonds de contexte configurables | seuils par modèle, queue bornée |
| Journal | événements chaînés par hachage, altération détectée, écriture refusée plutôt que maillon forgé | événements typés, « best-effort », rétention 30 jours | SQLite WAL, pas de registre d'audit |
| Effets | ledger avant exécution, un effet incertain devient une question | non | non |
| Reprise | run repris à son étape, écrit dans la même transaction, tests dédiés | deux bugs de reprise fautive ouverts | instantanés git avant écriture et `/rollback`, suite crash/reprise encore à l'état de demande |

## Sécurité et coût

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Approbation | bornée par motif d'arguments, enchaînements refusés, règle visible et révocable | demande hors liste d'autorisation, registre des octrois | mode « smart » par défaut, liste noire infranchissable |
| Bac à sable | Seatbelt, toujours appliqué, profil `workspace-write` par défaut, réseau fermé et accordé appel par appel, aucun interrupteur pour le couper | désactivé par défaut | durcissement conteneur si le backend Docker est choisi |
| Secrets | trousseau du système | fichiers en clair, permissions restreintes | `.env` en clair, coffre chiffré optionnel |
| Contenu non fiable | donnée jamais instruction, détecteur d'injection, adresses privées revérifiées à chaque redirection | balisage explicite du contenu externe | scan des fichiers de contexte avant inclusion |
| Coût | celui facturé par le fournisseur, lisible par session, tour, modèle, jour, rôle, fournisseur amont et cause de raté de cache | suivi par message et session | suivi par session |
| Plafonds de dépense | jour, session, run, alerte à 80 %, point de contrôle dans le tour ; sur abonnement ChatGPT, le quota du plan avec la même alerte | aucun | aucun, hors plafond de compte |

## Ce que les autres font mieux

Il faut le dire aussi, sinon les tableaux ci-dessus ne valent rien. Constats du 18/09 ; le
second a bougé depuis, c'est dit entre parenthèses.

- **Écosystème et portabilité.** OpenClaw tourne sur macOS, Linux, Windows, Docker et
  Nix, avec une douzaine de canaux et autant de fournisseurs de modèles. Hermes offre
  sept backends d'exécution dont des bacs à sable distants qui hibernent. Pénélope fait
  macOS et Telegram, point.
- **Scoring de mémoire.** Les six signaux de promotion d'OpenClaw sont plus riches que
  notre grille à cinq critères, même si notre chemin d'écriture est plus réversible. Le
  18/09, l'usage mesuré ordonnait le rappel sans rien promouvoir seul ; depuis la 1.0.8
  (#230), les réponses du propriétaire jugent chaque souvenir servi et un écart ne devient
  une exception qu'après des réponses acceptées dans deux sessions distinctes.
- **Bac à sable par défaut.** Comme Codex CLI, le shell n'a plus le réseau par défaut :
  une commande le demande, la carte d'approbation le dit, et « Toujours » l'accorde à une
  famille de commandes. Codex coupe aussi ses autres outils ; nos appels MCP et
  `http_fetch` passent par leurs propres garde-fous, pas par le bac à sable.
- **Maturité.** Ces projets encaissent des millions d'heures d'usage et publient leurs
  vulnérabilités. Pénélope a une instance en service : ce qu'elle sait de ses propres
  défauts vient d'une revue adversariale de son code, pas encore de l'usage de milliers
  de gens.
