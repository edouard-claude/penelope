# Notes de livraison : les quatre dernières méthodes RPC en scénarios (épopée #208, critère 7)

Branche `v1-s-derniers`, dérivée de `v1` à `1f6fbc5`. Périmètre tenu :
`crates/penelope-evals/**` (moteur, quatre répertoires de scénarios, quatre cas dans
`tests/scenarios.rs`), `budget.toml` par `UPDATE_BUDGET=1` seulement, ce fichier. Aucun
fichier du produit, aucune dépendance, aucune ligne `version =`.

## Compte

`[scenarios].missing` : **4 → 0** (`rpc:doctor`, `rpc:mcp.auth`, `rpc:skill.install`,
`rpc:upgrade`). Deux le sont en entier, deux en partie, faute d'un point d'accroche dans
le produit (plus bas).

## Le moteur

- **`[[http]]`, faux serveur HTTP local** (`src/scenario/http.rs`, avec ses tests) :
  `127.0.0.1`, port tiré au sort, une route par chemin exact (`path`, `method`,
  `status`, `body`, `content_type`, `location`). `{{http}}` est sa base dans la
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
| `rpc-mise-a-jour` | `upgrade.base_url` vers le faux serveur, qui ne publie qu'un brouillon, une pré-release (`v1.0.0-alpha.99`, #212) et un tag illisible : `check` n'en retient aucune. Installer (dernière ou tag explicite, `force`) et revenir en arrière refusés sur un binaire de compilation, avant tout téléchargement : une seule requête reçue. |
| `rpc-installation-skill` | Sources mal écrites refusées (sans propriétaire, remontée de répertoire, slug invalide, espace), rien de posé. |

Chaque scénario a été régénéré puis rejoué au moins trois fois de suite sans différence.

## Blocages : ce qui manque au produit (pour l'intégrateur)

1. **`skill.install` réussi** : `penelope_skills::install::Source::archive_url` construit
   toujours `https://codeload.github.com/...` ; ni clé de configuration, ni source
   locale, et le HTTPS exclut un faux serveur. Plus petite modification : une clé
   `skills.archive_base_url` (défaut `https://codeload.github.com`, validée par
   `check_endpoint`, donc `http` seulement en boucle locale), lue par
   `penelope_ops::skill_install::install` et passée à `archive_url(base)`. Le scénario
   servirait alors un zip semé par `[[http]]` (le serveur sert du texte : il faudrait
   aussi un `body_base64` à la route) et relèverait la skill posée.
2. **`upgrade` « une mise à jour existe » / « à jour »** : `check` passe par `release()`,
   qui exige l'archive de l'OS (`asset_name`) et échoue sous Linux (« pas d'artefact
   publié pour linux ») : la réponse diffère entre la CI Linux et macOS. Plus petite
   modification : dans `penelope_ops::upgrade::check`, résoudre tag et version sans
   l'archive (extraire de `release()` une fonction qui rend la release retenue ;
   `release()` l'appelle puis cherche l'archive). Le scénario ajouterait alors une
   release `v1.0.0` (plus haute que `1.0.0-alpha.N`) puis une liste sans elle.

## Notes de version, à coller dans `docs/progress.md`

```markdown
#### Les dernières méthodes RPC rejouées par des scénarios (#208, critère 7)

- **Faux serveur HTTP local des scénarios** (`[[http]]`) : ce qu'une méthode va chercher
  sur le réseau lui est servi depuis 127.0.0.1, les requêtes reçues relevées dans le
  monde ; l'étape `open` joue le navigateur du propriétaire, `without` retire d'une
  réponse ce qui décrit l'hôte.
- `doctor`, `mcp.auth` (parcours OAuth complet : découverte, enregistrement, PKCE,
  échange du code), `upgrade` (source de releases locale, rien de remplacé) et
  `skill.install` (sources refusées) ont leur scénario : plus aucune surface RPC sans
  scénario.
```
