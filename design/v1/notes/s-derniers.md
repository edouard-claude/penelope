# Notes de livraison : les quatre dernières méthodes RPC en scénarios (épopée #208, critère 7)

Branche `v1-s-derniers`, dérivée de `v1` à `1f6fbc5`. Périmètre tenu :
`crates/penelope-evals/**` (moteur, quatre répertoires de scénarios, quatre cas dans
`tests/scenarios.rs`, golden de `config.get`), `budget.toml` par `UPDATE_BUDGET=1`
seulement, ce fichier ; puis, sur autorisation de l'intégrateur, deux modifications du
produit dans `penelope-ops` (`upgrade`, `skill_install`), `penelope-skills` (`install`)
et `penelope-kernel` (la clé `skills.archive_base_url`), avec `docs/install-headless.md`
régénéré par `UPDATE_DOCS=1`. Dépendance ajoutée à `penelope-evals` : `zip` (déjà dans
le workspace, raison écrite). Aucune ligne `version =`.

## Compte

`[scenarios].missing` : **4 → 0** (`rpc:doctor`, `rpc:mcp.auth`, `rpc:skill.install`,
`rpc:upgrade`), toutes jouées en entier.

## Les deux modifications du produit

- **`upgrade check` sans l'archive de l'OS** : la sélection de la release sort de
  `release()` dans `latest_tag` (tag, version, métadonnée) ; `release()` l'appelle puis
  cherche l'archive, `check` s'arrête à `latest_tag`. Vrai correctif : sous Linux,
  `penelope upgrade --check` échouait sur « pas d'artefact publié pour linux » au lieu de
  dire la dernière version. Test : `check_names_the_latest_version_without_an_archive_for_the_os`.
- **`skills.archive_base_url`** (défaut `https://codeload.github.com`, donc rien ne change
  pour une installation existante) : l'origine des archives de `skill install`, validée
  par `check_endpoint` (HTTPS, ou HTTP vers 127.0.0.1/localhost, jamais d'identifiants).
  `Source::archive_url` prend la base en paramètre. Test :
  `the_archive_base_is_https_or_loopback`.

## Le moteur

- **`[[http]]`, faux serveur HTTP local** (`src/scenario/http.rs`, avec ses tests) :
  `127.0.0.1`, port tiré au sort, une route par chemin exact (`path`, `method`,
  `status`, `body`, `content_type`, `location`, `zip` : une archive ZIP stockée,
  construite depuis `scenario.toml`, mêmes octets à chaque rejeu). `{{http}}` est sa base dans la
  configuration patchée, les paramètres RPC et les corps servis ; `{{q:nom}}` renvoie un
  paramètre de la requête reçue (un serveur d'autorisation rend le `state` qu'on lui
  donne). Les requêtes reçues entrent dans le monde (`http_request` : méthode, chemin,
  requête et corps lus en JSON ou en formulaire), `state`, `code_verifier` et
  `code_challenge` masqués. La normalisation rend la base, brute et encodée, en
  `{{http}}`.
- **Étape `open`** : le navigateur du propriétaire, un `GET` sans suivre de redirection
  (statut, `location`, corps), `bind` possible ; toute adresse hors du faux serveur est
  refusée, donc aucune requête ne sort.
- **`without` sur l'étape `rpc`** : `without = { id = ["service", "dep.*"] }` retire d'une
  réponse en tableau les éléments dont le champ répond à un motif (`*` final : préfixe).
  `pick` et `mask` ne suffisaient pas à `doctor` : le nombre même de contrôles change
  d'une machine à l'autre, donc les indices aussi.
- **Garde d'`upgrade`** dans le harnais : `switch` refusé (il réécrit le service de la
  machine), et hors `check` l'appel n'est permis que depuis un binaire de compilation
  (`target/debug`), que la méthode refuse de remplacer avant tout téléchargement.

## Les scénarios

| Répertoire | Ce qui est vérifié |
|---|---|
| `rpc-diagnostic` | `doctor` joué en entier après un tour, une skill déposée, un serveur MCP déclaré. Retirés : service, disque, dépendances et inventaire, contrôles de l'OS (`macos.*`, `platform.*`), DNS (`net.*`), signature et mode d'installation du binaire, alimentation. Masquées : dérive de l'horloge de test, durée de la synthèse d'essai. Restent, dans l'ordre, 44 contrôles propres à Pénélope (base, audit, rétention, prompt, journal, jobs, bac à sable, skills, juge, rédacteur, mémoire, OAuth et serveur MCP, embeddings, cohérence, vault, mises à jour, planifications, voix, sauvegarde, boucles de fond). |
| `rpc-autorisation-mcp` | Parcours OAuth complet contre le faux serveur, ressource protégée et serveur d'autorisation à la fois : serveur inconnu refusé ; découverte RFC 9728 puis RFC 8414, enregistrement dynamique, URL d'autorisation PKCE S256, demande en attente ; `open` de l'URL, retour 302 avec code et même `state` ; échange du code (indicateur de ressource compris), secret `mcp.notes.oauth` rangé, serveur redémarré ; même adresse rejouée refusée ; seconde demande sans nouvel enregistrement du client. |
| `rpc-mise-a-jour` | Trois sources locales, changées par `config.set` : brouillon, pré-release (#212) et tag illisible, rien de retenu ; une 0.17, « à jour » ; une `v9.9.9` sans archive pour l'OS, « mise à jour disponible », identique sous Linux et macOS. Installer (dernière ou tag explicite, `force`) et revenir en arrière refusés sur un binaire de compilation, avant tout téléchargement : trois requêtes reçues, les trois listes, rien de remplacé. |
| `rpc-installation-skill` | Dépôt servi en ZIP par le faux serveur : source mal écrite refusée avant tout téléchargement, dépôt absent (404) dit, skill absente de l'archive refusée avec les disponibles ; la skill demandée posée avec son script, chargée, listée, montrée, l'autre skill du dépôt non posée ; seconde installation refusée sans `force`, remplacement avec ; `skill.installed` relevé. |

Chaque scénario a été régénéré puis rejoué au moins trois fois de suite sans différence.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Les dernières méthodes RPC rejouées par des scénarios (#208, critère 7)

- **Faux serveur HTTP local des scénarios** (`[[http]]`) : ce qu'une méthode va chercher
  sur le réseau lui est servi depuis 127.0.0.1, les requêtes reçues relevées dans le
  monde ; l'étape `open` joue le navigateur du propriétaire, `without` retire d'une
  réponse ce qui décrit l'hôte.
- `doctor`, `mcp.auth` (parcours OAuth complet : découverte, enregistrement, PKCE,
  échange du code), `upgrade` (« à jour » et « mise à jour disponible » contre une
  source locale, rien de remplacé) et `skill.install` (dépôt servi en local, skill
  posée) ont leur scénario : plus aucune surface RPC sans scénario.
- **`penelope upgrade --check` marche sous Linux** : la vérification dit la dernière
  version sans exiger l'archive de l'OS, qui n'est cherchée qu'à l'installation.
- **`skills.archive_base_url`** : l'origine des archives de `penelope skill install`
  (défaut `https://codeload.github.com`), HTTPS ou boucle locale seulement ; un miroir
  devient possible.
```

## Vérifications

Pendant l'itération : chaque scénario régénéré puis rejoué au moins trois fois sans
différence ; `cargo test -p penelope-archtest` vert après `UPDATE_BUDGET=1` ;
`scripts/switch-check.sh` : « 7. [scenarios].missing vide ».

Premier lot (avant les modifications du produit), finales : `cargo fmt --all --check` propre ; `cargo clippy --workspace
--all-targets -- -D warnings` propre ; `cargo test --workspace --no-fail-fast` : 81
suites, 2 135 verts, 20 ignorés, 2 échecs sous charge hors périmètre, tous deux verts
rejoués seuls trois fois : `a_restart_during_a_job_fails_it_and_says_so_without_a_card`
(`penelope-daemon`, `tool_jobs_e2e`, déjà relevé par s-rpc) et
`a_burst_asks_before_answering_and_can_be_ingested` (`penelope-gateway-telegram`).
Aucun fichier de ces deux crates n'est touché ici : aléas préexistants, à surveiller.

Second lot (modifications du produit et scénarios complets), finales : `cargo fmt --all
--check` propre ; `cargo clippy --workspace --all-targets -- -D warnings` propre ;
`cargo test --workspace --no-fail-fast` : 81 suites, 2 140 verts, 0 échec, 20 ignorés.
`rpc-mise-a-jour` et `rpc-installation-skill` rejoués trois fois sans différence.
