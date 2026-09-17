# Pénélope

Agent personnel autonome, écrit en Rust, conçu pour tourner **sans écran** sur un
MacBook Pro M1 branché en permanence. On lui parle par Telegram ou en SSH ; elle garde
sa mémoire dans des fichiers Markdown lisibles, appelle des serveurs MCP, exécute des
workflows durables et demande l'avis de son propriétaire avant tout ce qui engage.

La spécification de référence est le PRD, gardé hors du dépôt (`spec/` est ignoré par
git). Ce README dit comment le dépôt est fait et comment le faire tourner ; les détails
vivent dans `docs/`, dont l'[index](docs/README.md) dit quoi lire pour quel besoin.

## Ce qui est là

| Domaine | État |
|---|---|
| Journal d'événements chaîné, ledger d'effets, générations de configuration | complet |
| Moteur de contexte : tuiles T0..T4, compaction 0 à 4, LCM, ancres | complet |
| Client MCP : 5 versions de protocole, 3 transports, OAuth 2.1 + PKCE, registre paresseux | complet |
| Mémoire : vault Markdown, règles défaisables, provenance, rappel, consolidation | complet |
| HITL, politiques, bac à sable, outils natifs | complet |
| Telegram : rendu, gabarits, CTA, formulaires, mock Bot API | complet |
| Workflows : 9 types d'étapes, conditions, reprise, workflows livrés | complet |
| Conversation : `penelope chat`, pool de runners, reprise après approbation, titres de session | branché |
| Passerelle Telegram : commandes, cartes d'approbation, brouillons, file d'envoi, vocaux, photos, documents | branchée |
| Catalogue de modèles OpenRouter, routage par complexité, repli, budgets | branché |
| Superviseur MCP et OAuth depuis Telegram, ordonnanceur, moteur de workflows, sous-agents, images | branché |
| Mémoire qui apprend : revue de fond, consolidation nocturne, digest du matin | branché |
| Exploitation : `upgrade` avec retour arrière automatique, `import hermes`, sauvegarde, export | branché |

`docs/progress.md` tient la liste exacte de ce qui reste.

## Démarrer

```bash
cargo build --release
```

```bash
./target/release/penelope paths
```

L'installation headless complète (service `launchd`, secrets, Telegram, veille) est
décrite dans [docs/install-headless.md](docs/install-headless.md).

Toutes les commandes acceptent `--home <répertoire>` (ou la variable `PENELOPE_HOME`)
pour déplacer l'intégralité de l'état : c'est ce qui rend les tests et les bacs à sable
possibles sans toucher au vrai profil.

## Organisation du dépôt

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
- `penelope-kernel` dépend de `penelope-store` et de rien d'autre du projet, voir
  [docs/decisions/0001-kernel-depend-de-store.md](docs/decisions/0001-kernel-depend-de-store.md).
- `penelope-platform` isole tout ce qui est spécifique à un OS. Un chemin littéral, un
  appel shell, un signal Unix ou une API Keychain ailleurs fait échouer le test
  d'architecture.
- `#![forbid(unsafe_code)]` dans chaque crate, vérifié lui aussi par un test.

## Tests

```bash
cargo test --workspace
```

1313 tests, tous hors réseau. Les suites nommées du PRD §20.1 sont des filtres sur cette
même commande, ce qui évite qu'un chemin de test diverge de l'autre :

```bash
cargo test -p penelope-evals --test mcp_conformance
```

```bash
cargo test -p penelope-evals --test resilience --test security --test hot_reload
```

Les suites réseau (`live-openrouter`, `live-telegram`, `ctx-recall`, `mem-longitudinal`,
`ab-hermes`) parlent à de vrais services : ignorées par la CI, elles se lancent depuis le
dépôt avec leurs variables d'environnement (clé OpenRouter, bot de test, commande Hermes) :

```bash
OPENROUTER_API_KEY=… penelope eval live-openrouter
```

`ab-hermes` rejoue 30 tâches vérifiables sur Hermes et Pénélope et écrit
`target/ab-hermes.md` (réussite, latence, tokens, coût).

La matrice des critères d'acceptation, [docs/ca-matrix.md](docs/ca-matrix.md), est
**générée** depuis les sources : tout test nommé `ca_<section>_<n>_<nom>` y entre
automatiquement.

```bash
UPDATE_CA_MATRIX=1 cargo test -p penelope-evals --test ca_matrix
```

La chaîne d'outils est épinglée dans `rust-toolchain.toml` : le lint local rend
exactement le même verdict que la CI.

Lint et dépendances :

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo deny check
```

## Intégration continue

[`ci.yml`](.github/workflows/ci.yml) rejoue exactement les commandes ci-dessus sur
`macos-14`, plus `cargo deny check` sur Linux. Aucune suite ne demande le réseau, donc
la CI n'a besoin d'aucun secret.

[`release.yml`](.github/workflows/release.yml) se déclenche sur un tag `vX.Y.Z` : il
vérifie, construit `aarch64-apple-darwin` et `x86_64-apple-darwin`, fusionne les deux en
un binaire universel, et publie trois archives avec leurs sommes de contrôle.

```bash
git tag -a v0.1.1 -m "Pénélope 0.1.1" && git push origin v0.1.1
```

Détails dans [.github/workflows/README.md](.github/workflows/README.md).

## Documentation

- [docs/README.md](docs/README.md) : index de la documentation, par besoin et par fichier.
- [docs/install-headless.md](docs/install-headless.md) : installation sur un Mac sans écran.
- [docs/mcp.md](docs/mcp.md) : client MCP, versions, transports, OAuth, registre paresseux.
- [docs/workflows.md](docs/workflows.md) : référence complète du schéma de workflow, dont [schemas/workflow.schema.json](schemas/workflow.schema.json).
- [docs/telegram.md](docs/telegram.md) : commandes, gabarits et rendu.
- [docs/ca-matrix.md](docs/ca-matrix.md) : critères d'acceptation et tests qui les couvrent.
- [docs/progress.md](docs/progress.md) : avancement, décisions, reste à faire.
- [docs/decisions/](docs/decisions/) : écarts assumés par rapport à un « DEVRAIT » du PRD.

## Principes qui ne se négocient pas

1. Rien d'irréversible sans accord explicite, et l'accord est traçable.
2. Un effet incertain ne se relance jamais tout seul : il devient une question.
3. Le savoir durable est un fichier Markdown que l'on peut lire et corriger en SSH.
4. Les données rapportées par un outil ne sont jamais des instructions.
5. Le chemin d'écriture de la mémoire est une frontière de sécurité, pas un cache.
