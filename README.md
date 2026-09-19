# Pénélope

[![CI](https://github.com/edouard-claude/penelope/actions/workflows/ci.yml/badge.svg)](https://github.com/edouard-claude/penelope/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/edouard-claude/penelope?include_prereleases&sort=semver&label=version)](https://github.com/edouard-claude/penelope/releases)
[![Rust](https://img.shields.io/badge/rust-2024-000?logo=rust)](https://www.rust-lang.org)
[![Plateforme](https://img.shields.io/badge/plateforme-macOS%20arm64-000?logo=apple)](docs/install-headless.md)
[![Documentation](https://img.shields.io/badge/docs-index-informational)](docs/README.md)

Un agent personnel qui tourne en permanence sur un Mac sans écran. On lui parle depuis
Telegram ou en SSH. Elle garde ce qu'elle apprend dans des fichiers Markdown que l'on
peut relire et corriger à la main, appelle des serveurs MCP, exécute des workflows qui
survivent à un redémarrage, et demande l'accord de son propriétaire avant tout ce qui
engage.

Rust, 17 crates, `#![forbid(unsafe_code)]` dans chacun, 1510 tests qui ne touchent pas
au réseau.

## Pourquoi celle-ci

Il existe des agents personnels bien plus connus, avec dix fois plus de connecteurs.
Pénélope ne joue pas sur ce terrain : un seul canal (Telegram), une seule plateforme
(macOS), un seul propriétaire. Elle traite en revanche sérieusement ce qui casse au bout
de six mois d'usage réel, et qui ne se voit pas dans une démo de cinq minutes.

### La mémoire est un objet versionné, pas un fichier de notes

Ce que l'agent apprend ne s'écrit jamais directement en mémoire. Chaque relecture
d'épisode produit au plus cinq candidats typés (fait, préférence, décision, correction,
écart) posés dans le journal du jour. La nuit, à 3 h 30, une passe les juge sur cinq
critères (durable, utile, précis, introuvable ailleurs, endossé) : le modèle argumente,
le code décide. Un souvenir promu peut en remplacer un autre (`supersede`, `replace`,
`retire`), pas seulement s'empiler ; chaque opération garde sa pré-image dans
`mem_history` et le vault est commité dans git.

Le vault est un wiki Markdown valide à tout instant : frontmatter YAML, identifiants de
bloc `^uid`, wikilinks. On le lit et on le corrige en SSH pendant que l'agent tourne,
l'écriture est optimiste et rejoue l'opération ligne à ligne si le fichier a bougé.
Sept niveaux, de l'instruction permanente à l'épisodique, avec la provenance de chaque
entrée : ce qui vient d'un document ingéré est marqué comme non fiable. Les clés et les
jetons repérés dans un candidat partent au magasin de secrets, la mémoire ne garde que
`${SECRET:nom}`.

Le rappel ne dépend pas d'un modèle : recherche déterministe bornée à 150 ms au début du
tour, recherche sémantique quand la phrase montre une intention de rappel, embeddings
calculés en fond avec repli lexical si le budget de 1,5 s est dépassé.

### Le contexte est découpé, et le préfixe ne bouge pas

Le prompt est bâti en cinq tuiles, de l'identité (T0) au volatil de fin de prompt (T4).
Le préfixe T0 à T2 est identique octet pour octet d'un tour à l'autre : ce n'est pas une
intention, c'est un test (`ca_5_3_prefix_is_byte_identical_across_turns`). La
conséquence est assumée et écrite : un souvenir, une skill ou un serveur MCP ajoutés en
pleine conversation n'entrent dans le prompt qu'au prochain cache froid ou à la
compaction suivante, parce que les faire entrer tout de suite coûterait plus cher que ce
qu'ils rapportent.

Cinq niveaux de compaction, dont un seul appelle un modèle. `context.max_prompt_tokens`
borne le contexte indépendamment de la fenêtre annoncée par le modèle, la compaction de
fond se déclenche sur le prompt réellement facturé au dernier appel, et une réserve
budgétaire empêche le plafond du jour de bloquer les résumés. Chaque raté de cache est
attribué à une cause et visible par `penelope usage --by miss`.

### Une session longue ne se perd pas

Le journal d'événements est chaîné par hachage : une altération est détectée, et une
erreur de lecture fait refuser l'écriture plutôt que forger un maillon. Tout effet passe
par un ledger avant exécution ; un effet dont l'issue est incertaine devient une
question posée au propriétaire, jamais une relance automatique. Un run de workflow écrit
son étape courante, ses sorties et son journal dans la même transaction, et reprend à
cette étape après un redémarrage. Le tour interrompu est remis en file une seule fois.

### Une autorisation a des bornes

Répondre « Toujours » ne donne pas un blanc-seing sur un outil : la règle créée est
bornée par un motif d'arguments, famille de commande pour `shell_exec`, répertoire pour
une écriture, remote et branche pour un `git_push`, hôte pour une requête HTTP, clé pour
un changement de configuration. Les motifs refusent tout enchaînement (`;`, `&&`, `|`,
substitution, redirection). Une règle est visible et révocable, la première décision
gagne, et le contenu rapporté par un outil est une donnée, jamais une instruction.

### Le coût est mesuré, pas estimé

Le coût enregistré est celui facturé par le fournisseur, pas une multiplication de
tokens par un tarif de catalogue. Il se lit par session, par tour, par modèle, par jour,
par rôle, par fournisseur amont et par cause de raté de cache. Plafonds jour, session et
run, alerte à 80 %, point de contrôle au-delà d'un dollar dans un même tour, délégation
à un sous-agent après dix appels d'outils.

### Elle sait ce qu'elle est

Sa documentation est compilée dans son binaire : elle la cherche, la lit par section, et
cite le lien GitHub au tag de la version qui tourne. `self_status` lui rend sa version,
son modèle du tour, sa configuration effective, ses coûts, sa file, l'état de la machine
et l'inventaire de ses outils, workflows, skills, serveurs MCP et limites. Les tables de
référence de la documentation (54 outils, 193 clés de configuration) sont générées
depuis le code, et un test refuse une section « limites » qui décrirait comme manquant
quelque chose de livré.

## Comparaison

Les deux projets les plus proches sont [OpenClaw](https://github.com/openclaw/openclaw)
(TypeScript, 390 000 étoiles) et [Hermes](https://github.com/NousResearch/hermes-agent)
(Python, 246 000 étoiles). Pénélope en compte zéro, tourne sur une seule machine et n'a
qu'un utilisateur. La comparaison qui suit ne porte donc pas sur l'écosystème, où il n'y
a pas de match, mais sur les mécanismes. Relevé du 18 septembre 2026, sources dans la
documentation de chaque projet.

### Mémoire

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Forme | wiki Markdown, 7 niveaux, identifiants de bloc et wikilinks, index SQLite | fichiers Markdown (`USER.md`, `MEMORY.md`, notes du jour), index SQLite | 2 fichiers Markdown plafonnés à 2 200 et 1 375 caractères, SQLite FTS5 |
| Écriture | candidats typés, jamais écrits directement en mémoire | l'agent écrit, tour silencieux de rappel avant compaction | l'agent écrit, erreur rendue si le plafond est franchi |
| Consolidation automatique | oui, 3 h 30, grille à 5 critères, le modèle argumente, le code décide | oui, 3 h, score à 6 signaux et triple seuil | non, l'agent doit faire le ménage lui-même |
| Réversibilité | `supersede`, `replace`, `retire`, pré-image de chaque opération, vault commité dans git | suppression par session, édition manuelle | édition manuelle, revue par `/journey` |
| Provenance | origine par entrée, contenu ingéré marqué non fiable | classe d'origine, consolidation protégée du contenu non fiable | non documenté |
| Secrets rencontrés | extraits vers le magasin, la mémoire ne garde que `${SECRET:nom}` | non documenté | non documenté |

### Contexte et sessions longues

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Découpage | 5 tuiles T0 à T4, stabilité déclarée par tuile | moteur enfichable, pas de couches documentées | pas de couches, double seuil 85 % puis 50 % |
| Compaction | 5 niveaux, un seul appelle un modèle | automatique et `/compact`, garde les tours récents | résumé structuré à gabarit fixe, mis à jour d'une compression à l'autre, repli déterministe |
| Préfixe de cache | identique octet pour octet, vérifié par un test | élagage au TTL, battement conseillé pour garder le cache chaud | règle écrite de non-mutation, 4 points de rupture |
| Plafond de contexte | indépendant de la fenêtre du modèle, déclenché sur le prompt réellement facturé, seuil abaissé sur fenêtre courte ; chiffres dans [docs/context.md](docs/context.md), vérifiés contre le code | plafonds de contexte configurables | seuils par modèle, queue bornée |
| Journal | événements chaînés par hachage, altération détectée, écriture refusée plutôt que maillon forgé | événements typés, « best-effort », rétention 30 jours | SQLite WAL, pas de registre d'audit |
| Effets | ledger avant exécution, un effet incertain devient une question | non | non |
| Reprise | run repris à son étape, écrit dans la même transaction, tests dédiés | deux bugs de reprise fautive ouverts | instantanés git avant écriture et `/rollback`, suite crash/reprise encore à l'état de demande |

### Sécurité et coût

| | Pénélope | OpenClaw | Hermes |
|---|---|---|---|
| Approbation | bornée par motif d'arguments, enchaînements refusés, règle visible et révocable | demande hors liste d'autorisation, registre des octrois | mode « smart » par défaut, liste noire infranchissable |
| Bac à sable | Seatbelt, toujours appliqué, profil `workspace-write` par défaut, réseau fermé et accordé appel par appel, aucun interrupteur pour le couper | désactivé par défaut | durcissement conteneur si le backend Docker est choisi |
| Secrets | trousseau du système | fichiers en clair, permissions restreintes | `.env` en clair, coffre chiffré optionnel |
| Contenu non fiable | donnée jamais instruction, détecteur d'injection, adresses privées revérifiées à chaque redirection | balisage explicite du contenu externe | scan des fichiers de contexte avant inclusion |
| Coût | celui facturé par le fournisseur, lisible par session, tour, modèle, jour, rôle, fournisseur amont et cause de raté de cache | suivi par message et session | suivi par session |
| Plafonds de dépense | jour, session, run, alerte à 80 %, point de contrôle dans le tour | aucun | aucun, hors plafond de compte |

### Ce que les autres font mieux

Il faut le dire aussi, sinon le tableau ci-dessus ne vaut rien.

- **Écosystème et portabilité.** OpenClaw tourne sur macOS, Linux, Windows, Docker et
  Nix, avec une douzaine de canaux et autant de fournisseurs de modèles. Hermes offre
  sept backends d'exécution dont des bacs à sable distants qui hibernent. Pénélope fait
  macOS et Telegram, point.
- **Scoring de mémoire.** Les six signaux de promotion d'OpenClaw sont plus riches que
  notre grille à cinq critères, même si notre chemin d'écriture est plus réversible.
  L'usage mesuré (rappels jugés utiles sur la réponse, succès) ordonne désormais le
  rappel, borné, et sert de preuve à la grille ; il ne promeut encore rien seul.
- **Bac à sable par défaut.** Comme Codex CLI, le shell n'a plus le réseau par défaut :
  une commande le demande, la carte d'approbation le dit, et « Toujours » l'accorde à une
  famille de commandes. Codex coupe aussi ses autres outils ; nos appels MCP et
  `http_fetch` passent par leurs propres garde-fous, pas par le bac à sable.
- **Maturité.** Ces projets encaissent des millions d'heures d'usage et publient leurs
  vulnérabilités. Pénélope a une instance en service : ce qu'elle sait de ses propres
  défauts vient d'une revue adversariale de son code, pas encore de l'usage de milliers
  de gens.

## Ce qu'elle ne fait pas

Version 0.x, publiée en pre-release, une seule instance réelle en service.

- macOS seulement. Les backends Linux et Windows compilent et renvoient `Unsupported`.
- Telegram seulement, en long polling. Le mode webhook n'est pas servi.
- Un seul propriétaire par instance, tout autre expéditeur est refusé.
- Les requêtes `sampling/createMessage` d'un serveur MCP sont refusées.
- L'OCR ne se déclenche que si un PDF n'a aucune couche texte : un document mixte garde
  ses pages scannées illisibles.
- La recherche vectorielle est exhaustive, à revoir au-delà de 200 000 entrées.
- La signature minisign des releases est implémentée mais la clé n'est pas créée : seule
  la somme SHA-256 est vérifiée aujourd'hui.
- Les suites qui parlent à de vrais services sont écrites et n'ont pas encore tourné.
- Le bac à sable n'existe que sur macOS : la suite entière tourne aussi sur Linux (la CI
  la rejoue sur `ubuntu-latest`), mais les tests de Seatbelt, de launchd et du trousseau
  ne tournent que sur `macos-14`.
- Les manques et les écarts trouvés en revue sont suivis dans les
  [issues](https://github.com/edouard-claude/penelope/issues) du dépôt, lisibles par
  tout le monde.

## Démarrer

```bash
cargo build --release
```

```bash
./target/release/penelope paths
```

L'installation complète sur un Mac sans écran (service `launchd`, secrets, Telegram,
veille, signature) est décrite dans [docs/install-headless.md](docs/install-headless.md).

Toutes les commandes acceptent `--home <répertoire>` (ou `PENELOPE_HOME`) pour déplacer
l'intégralité de l'état : c'est ce qui rend les bacs à sable et les tests possibles sans
toucher au vrai profil.

## Comment c'est fait

17 crates, dépendances orientées, aucune dépendance circulaire. La règle est vérifiée
par un test (`cargo test -p penelope-archtest`), pas par la discipline.

```
penelope-store ──────────► penelope-kernel ──┬──► penelope-llm ────┐
   (SQLite, écrivain          (événements,    │                     │
    unique, pool de            effets, config,│    penelope-context ┤
    lecture)                   sessions, tours)│                     │
                                              ├──► penelope-memory  │
penelope-platform            penelope-observe ├──► penelope-mcp     ├──► penelope-daemon ──► penelope-cli
   (OS : chemins,               (traces,      ├──► penelope-tools   │       (composition,
    service, bac à sable,        redaction,   ├──► penelope-hitl    │        RPC, boucle
    processus, secrets,          injection)   ├──► penelope-skills  │        d'agent)
    surveillance)                             ├──► penelope-telegram│
                                              └──► penelope-workflow┘
                                                   penelope-evals ──┘  penelope-archtest
```

- `penelope-store` ne dépend de rien et réexporte `rusqlite` : aucun crate métier ne
  connaît le pilote SQL.
- `penelope-platform` isole tout ce qui est spécifique à un OS. Un chemin littéral, un
  appel shell, un signal Unix ou une API Keychain ailleurs fait échouer le test
  d'architecture.
- Le client MCP, le client Bot API, le validateur JSON Schema, la recherche vectorielle
  et la surveillance de fichiers sont écrits ici : 31 dépendances directes, aucun
  framework d'agent, aucune bibliothèque C ajoutée.
- Chaque écart assumé par rapport à la spécification est un fichier de
  [docs/decisions/](docs/decisions/), avec son contexte, ses conséquences et parfois son
  point de bascule.

## Tests

```bash
cargo test --workspace
```

1510 tests, aucun ne touche au réseau, donc la CI n'a besoin d'aucun secret. Les suites
nommées sont des filtres sur cette même commande, ce qui évite qu'un chemin de test
diverge de l'autre.

```bash
cargo test -p penelope-evals --test mcp_conformance
```

La matrice des critères d'acceptation, [docs/ca-matrix.md](docs/ca-matrix.md), est
**générée** depuis les sources : tout test nommé `ca_<section>_<n>_<nom>` y entre
automatiquement.

Les suites qui parlent à de vrais services (`live-openrouter`, `live-telegram`,
`ctx-recall`, `mem-longitudinal`, `ab-hermes`) sont ignorées par la CI et se lancent
depuis le dépôt avec leurs variables d'environnement.

La chaîne d'outils est épinglée dans `rust-toolchain.toml` : le lint local rend
exactement le même verdict que la CI.

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo deny check
```

## Documentation

- [docs/README.md](docs/README.md) : index, par besoin et par fichier.
- [docs/install-headless.md](docs/install-headless.md) : installation, configuration,
  référence des clés et des outils natifs.
- [docs/context.md](docs/context.md) : compression du contexte, seuils, budgets, cache.
- [docs/mcp.md](docs/mcp.md) : versions, transports, OAuth, registre paresseux.
- [docs/workflows.md](docs/workflows.md) : schéma complet et cycle de vie d'un run.
- [docs/telegram.md](docs/telegram.md) : commandes, gabarits, rendu.
- [docs/ca-matrix.md](docs/ca-matrix.md) : critères d'acceptation et tests qui les
  couvrent.
- [docs/progress.md](docs/progress.md) : avancement, décisions, reste à faire.

Un test vérifie que cet index cite chaque page, qu'aucun lien n'est mort, et que les
tables de référence correspondent au code.

## Principes qui ne se négocient pas

1. Rien d'irréversible sans accord explicite, et l'accord est traçable.
2. Un effet incertain ne se relance jamais tout seul : il devient une question.
3. Le savoir durable est un fichier Markdown que l'on peut lire et corriger en SSH.
4. Les données rapportées par un outil ne sont jamais des instructions.
5. Le chemin d'écriture de la mémoire est une frontière de sécurité, pas un cache.
