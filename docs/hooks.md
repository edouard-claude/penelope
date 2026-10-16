# Hooks de cycle de vie

Un hook est une commande que **le propriétaire** branche sur un moment de la boucle de
Pénélope : à l'ouverture d'une session, à l'arrivée d'un message, avant un outil
(garde-fou), après un outil (format, lint, test ciblé), en fin de tour. Un hook peut signaler et bloquer ; il ne peut jamais autoriser ce que la
politique d'approbation aurait demandé (décision [0023](decisions/0023-hooks-de-cycle-de-vie.md)).

## Déclarer un hook

Un fichier par hook dans `hooks.d/`, à côté de `mcp.d/` dans le dossier de données
(`penelope paths`). Le nom du fichier est le nom du hook. Le dossier est relu dès qu'il
change : déposer, modifier ou retirer un fichier prend effet au prochain appel, sans
redémarrage. Un fichier invalide est signalé par `penelope hooks list` et par `doctor` ;
les autres hooks tournent.

```toml
# hooks.d/fmt.toml
on      = "post_tool"
match   = { tool = "fs_write|fs_edit", path = "**/*.rs" }
command = "cargo"
args    = ["fmt", "--check", "--", "{file}"]
cwd     = "{workspace}"
timeout = "90s"
sandbox = "workspace-write"   # readonly | workspace-write
on_failure = "continue"       # continue | block
deliver = "note"              # note | card | silent
env     = { TOKEN = "${SECRET:mon_jeton}" }
```

| Champ | Rôle | Défaut |
|---|---|---|
| `on` | l'événement : `session_start`, `prompt_submit`, `pre_tool`, `post_tool`, `turn_end` | requis |
| `match.tool` | noms d'outils séparés par `\|` (`fs_write\|fs_edit`, `mcp__srv__outil`) | tous |
| `match.path` | motif du chemin visé, relatif au workspace : `**` pour des dossiers, `*` dans un nom | tous |
| `command` | l'exécutable, lancé **sans shell** | requis |
| `args` | ses arguments, un élément chacun | aucun |
| `cwd` | le dossier où le lancer | le workspace du fichier |
| `timeout` | le délai | `120s` |
| `sandbox` | le bac à sable : jamais plus large que `sandbox.default_profile` | `workspace-write` |
| `on_failure` | ce qu'un échec décide (code inattendu, délai, lancement impossible) | `continue` |
| `deliver` | où va le résultat : au modèle, en carte au propriétaire, ou au journal seul | `note` |
| `env` | variables ajoutées ; seul endroit où `${SECRET:nom}` est admis | aucune |
| `input` | `prompt_submit` seulement : `text` passe le texte du message au hook | `none` |

Valeurs substituées dans `args` et `cwd`, élément par élément : `{file}` (le chemin visé,
relatif au workspace), `{workspace}`, `{session}`, `{tool}`, `{data}`. Un argument n'est
jamais découpé : `a b.rs` reste un seul argument. Une valeur qui porte un caractère que le
shell interprète (`;`, `$`, guillemets, retour à la ligne…) ne lance pas le hook, et le
résultat le dit.

## Ce que le hook reçoit et rend

Sur son entrée standard, en JSON : `event`, `session_id`, `run_id`, `cwd`,
`tool: {name, args}`, `result: {ok, output}` (tronqué à 8 000 caractères).

- Code **0** : tout va bien ; la sortie, s'il y en a une, est une note pour le modèle.
- Code **2** : un verdict JSON sur la sortie, `{"verdict": "continue" | "block",
  "reason": "…", "note": "…"}`. `block` fait de l'appel une erreur pour le modèle, avec
  le motif.
- Tout autre code, ou le délai : un échec, tranché par `on_failure`.

Le résultat est joint à celui de l'outil (`hooks`) et à son texte, encadré comme une
donnée (`[hook <nom> : verdict]`), jamais comme une consigne du propriétaire.

## À l'ouverture d'une session : `session_start`

Le hook tourne une fois par session, au premier tour (lancer un `git fetch`, vérifier
qu'un service répond, préparer un dossier). Sa sortie (code 0) est une note ajoutée au
contexte volatil du tour (T4), encadrée comme donnée : jamais au préfixe du prompt, le
cache ne bouge pas. Pas de `block` : une session s'ouvre toujours.

## À l'arrivée d'un message : `prompt_submit`

Le hook passe sur chaque message du propriétaire accepté, avant la construction du
prompt. Le texte du message ne lui est passé que s'il le déclare (`input = "text"`,
montré par `penelope hooks show`). `{"verdict": "block", "reason": "…"}` : le message
n'est pas envoyé au modèle, le propriétaire reçoit le motif ; une sortie en code 0 est une
note ajoutée au contexte volatil (T4).

## Avant l'outil : `pre_tool`

Le hook passe après la politique d'approbation, avant la carte ou l'exécution. Code 2 et
verdict JSON :

- `{"verdict": "deny", "reason": "…"}` : l'appel est refusé, le motif va au modèle ;
- `{"verdict": "ask", "reason": "…"}` : une carte est forcée, même si une règle
  « Toujours » ou le mode `auto` l'aurait laissé passer ; dans un tour autonome (mission,
  run autonome), personne ne peut répondre : l'appel est refusé et la mission passe
  `blocked` avec le motif ;
- `{"verdict": "args", "args": {…}}` : nouveaux arguments pour **le même outil** ; l'appel
  modifié repasse toute la politique, et finit au moins aussi strict que l'original ;
- `{"verdict": "continue"}` ou code 0 : rien ne change.

Le plus strict l'emporte toujours (`ca_9_7_hook_cannot_loosen`) ; la couche `hook` est
nommée dans le motif de la carte et le jeu de décisions. Un `allow` n'existe pas : un JSON
qui le dit est une erreur du hook, tranchée par `on_failure` (`block` refuse l'appel).

Exemple, « jamais `git push` vers `main` » :

```toml
# hooks.d/garde-git.toml
on      = "pre_tool"
match   = { tool = "shell_exec" }
command = "sh"
args    = ["-c", "input=$(cat); case \"$input\" in *'git push'*main*) echo '{\"verdict\":\"deny\",\"reason\":\"jamais vers main\"}'; exit 2;; esac"]
on_failure = "block"
```

## En fin de tour : `turn_end`

Le hook reçoit la réponse prête (`answer`). `{"verdict": "block", "note": "…"}` relance
**une seule fois** le modèle avec la note, encadrée comme donnée ; si le hook retient aussi
la seconde réponse, elle part avec la note en pied.

## Bornes et sûreté

- 8 hooks au plus par événement d'un appel, 2 minutes cumulées ; au-delà, les suivants sont
  sautés et le résultat le dit.
- Chaque exécution est un événement du journal (`hook.ran` : nom, verdict, code, durée),
  jamais un effet rejoué après un redémarrage.
- Un hook vient du propriétaire, jamais d'un dépôt : `fs_write` et `fs_edit` refusent
  d'écrire dans `hooks.d/`, même si un workspace englobe le dossier de données ; sous le
  bac à sable de macOS, `hooks.d/` (comme les autres chemins de `sandbox.deny_read`) est
  interdit en lecture **et en écriture** aux commandes du shell. Une `command` qui, résolue
  comme au lancement (PATH, chemin réel), se trouve dans un workspace est refusée.
- Sous le profil `full` (aucun bac à sable), une commande du shell peut écrire où elle
  veut, `hooks.d/` compris : l'agent y a déjà les pouvoirs du propriétaire. C'est le prix
  assumé de ce profil.
- Un secret passé par `env` est caviardé dans la sortie du hook avant d'aller au modèle,
  dans une carte ou au journal.
- Le bac à sable du hook s'applique là où la plateforme sait le faire (macOS) ; sur une
  plateforme sans bac à sable où la configuration dit `full`, le hook tourne comme le
  shell.

## Limites actuelles

- 8 hooks au plus par événement, 2 minutes cumulées par événement (par appel d'outil pour
  `pre_tool` et `post_tool`) ; au-delà, les suivants sont sautés.
- `session_start` ne tourne qu'au premier tour d'une session ; la reprise d'un tour après
  un redémarrage ne le relance pas.
- `turn_end` relance le modèle une seule fois par tour.
- Le temps des hooks se lit par hook (`penelope usage --by hook`), pas encore dans la
  bulle de réponse.

## Commandes

```bash
penelope hooks list
penelope hooks show fmt
penelope hooks test fmt --tool fs_edit --file src/lib.rs
penelope hooks edit fmt timeout 30s
penelope usage --by hook --since 2026-10-16
```

`test` lance le hook sur une entrée synthétique, sous son bac à sable, et montre l'entrée,
la sortie, le code et le verdict, sans rien écrire au journal. `edit` suit les règles de
`mcp edit` : une valeur seule pour `args` vaut une liste d'un élément, plusieurs en JSON
(`'["--check","{file}"]'`) ; le fichier n'est réécrit que si la déclaration reste valide.

`self_status --section tools` liste les hooks et leurs événements ; `penelope usage --by
hook` donne, par hook, le nombre d'exécutions et le temps cumulé (journal `hook.ran`).

## Le `--hook` d'une mission

`/mission … --hook "<commande>"` (#366) est un hook éphémère de la mission : relancé après
chaque `fs_write` ou `fs_edit` du dépôt de la mission. C'est la seule ligne de shell
admise (`sh -c`), parce que le propriétaire la tape au lancement ; un hook de `hooks.d/`
n'a pas cette porte.
