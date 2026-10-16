# Hooks de cycle de vie

Un hook est une commande que **le propriétaire** branche sur un moment de la boucle de
Pénélope : après un outil (format, lint, test ciblé), et, aux lots suivants de l'épopée
#386, avant un outil, en fin de tour, à l'ouverture d'une session ou à l'arrivée d'un
message. Un hook peut signaler et bloquer ; il ne peut jamais autoriser ce que la
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
| `on` | l'événement ; seul `post_tool` est servi pour l'instant (#384, #385 pour les autres) | requis |
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

## Bornes et sûreté

- 8 hooks au plus par événement d'un appel, 2 minutes cumulées ; au-delà, les suivants sont
  sautés et le résultat le dit.
- Chaque exécution est un événement du journal (`hook.ran` : nom, verdict, code, durée),
  jamais un effet rejoué après un redémarrage.
- Un hook vient du propriétaire, jamais d'un dépôt : `fs_write` et `fs_edit` refusent
  d'écrire dans `hooks.d/`, même si un workspace englobe le dossier de données, et une
  `command` située dans un workspace est refusée. `hooks.d/` est aussi interdit en lecture
  aux commandes du shell.
- Le bac à sable du hook s'applique là où la plateforme sait le faire (macOS) ; sur une
  plateforme sans bac à sable où la configuration dit `full`, le hook tourne comme le
  shell.

## Commandes

```bash
penelope hooks list
penelope hooks show fmt
penelope hooks test fmt --tool fs_edit --file src/lib.rs
penelope hooks edit fmt timeout 30s
```

`test` lance le hook sur une entrée synthétique, sous son bac à sable, et montre l'entrée,
la sortie, le code et le verdict, sans rien écrire au journal. `edit` suit les règles de
`mcp edit` : une valeur seule pour `args` vaut une liste d'un élément, plusieurs en JSON
(`'["--check","{file}"]'`) ; le fichier n'est réécrit que si la déclaration reste valide.

`self_status --section tools` liste les hooks et leurs événements.

## Le `--hook` d'une mission

`/mission … --hook "<commande>"` (#366) est un hook éphémère de la mission : relancé après
chaque `fs_write` ou `fs_edit` du dépôt de la mission. C'est la seule ligne de shell
admise (`sh -c`), parce que le propriétaire la tape au lancement ; un hook de `hooks.d/`
n'a pas cette porte.
