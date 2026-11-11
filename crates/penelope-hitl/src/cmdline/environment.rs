//! Ce que l'environnement change à une ligne (#444) : les variables qui détournent ce
//! qu'elle lance, et les mots-clés qui règlent le shell pour la suite.
//!
//! `export GIT_SSH_COMMAND=… && git fetch` partait sous une règle `git fetch` : l'étape
//! `export` était traversée sans qu'on regarde ce qu'elle posait, alors que la même
//! variable en préfixe (`GIT_SSH_COMMAND=… git fetch`) laissait la ligne composée. Les
//! deux écritures passent maintenant par [`hijacks`], et un réglage du shell n'est
//! traversé que sous une forme connue pour ne rien détourner : tout le reste laisse la
//! ligne composée, donc sans famille, sans règle possible, et soumise à carte.
//!
//! Rejoué le 11/11 sous zsh 5.9 et bash 3.2, git 2.54, ripgrep 15 : chaque forme refusée
//! ici lançait un script témoin ou un faux `ls`.

use super::SimpleCommand;

/// Une variable qui change ce que la ligne exécute vraiment : chemin de recherche,
/// bibliothèque préchargée, fichier de démarrage, options ou configuration d'un programme,
/// programme auxiliaire (pageur, éditeur, demande de mot de passe). `value` est ce qu'on
/// lui donne, quand la ligne le dit.
pub(super) fn hijacks(name: &str, value: Option<&str>) -> bool {
    const NAMES: &[&str] = &[
        "PATH",
        "HOME",
        "IFS",
        "ENV",
        "BASH_ENV",
        "SHELLOPTS",
        "BASHOPTS",
        "CDPATH",
        "PS4",
        // zsh lie ces tableaux à `PATH` et `CDPATH` : `path=/tmp ls` ne trouve plus `ls`.
        "path",
        "cdpath",
        "NODE_OPTIONS",
        "PYTHONSTARTUP",
        "PYTHONPATH",
        "PYTHONHOME",
        "PERL5OPT",
        "PERL5LIB",
        "RUBYOPT",
        "RUBYLIB",
        "VISUAL",
        // git lit `$XDG_CONFIG_HOME/git/config` : un `core.fsmonitor` y fait lancer un
        // programme par `git status`.
        "XDG_CONFIG_HOME",
        // Le fichier désigné porte des options de `rg`, `--pre=<programme>` comprise.
        "RIPGREP_CONFIG_PATH",
    ];
    const PREFIXES: &[&str] = &["DYLD_", "LD_", "BASH_FUNC_"];
    // Le nom dit qu'un programme sera lancé : `PAGER`, `GIT_PAGER`, `GH_PAGER`, `EDITOR`,
    // `GIT_SEQUENCE_EDITOR`, `SSH_ASKPASS`, `GIT_ASKPASS`, `BROWSER`…
    const SUFFIXES: &[&str] = &["PAGER", "EDITOR", "ASKPASS", "BROWSER"];
    // Couper le programme auxiliaire ne lance rien : `PAGER=cat git log`, `AWS_PAGER= aws …`,
    // `GIT_EDITOR=true git rebase --continue`, `BROWSER=/usr/bin/false <cli> login`.
    const OFF: &[&str] = &["", "cat", "true", "false", "echo", ":"];
    if SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return !value.is_some_and(|v| {
            let bare = v
                .strip_prefix("/usr/bin/")
                .or_else(|| v.strip_prefix("/bin/"))
                .unwrap_or(v);
            OFF.contains(&bare)
        });
    }
    NAMES.contains(&name)
        || PREFIXES.iter().any(|p| name.starts_with(p))
        || (name.starts_with("GIT_") && !GIT_INERT.contains(&name))
}

/// Les variables `GIT_*` qui ne font que renseigner git. Toute autre détourne : elle lance
/// un programme (`GIT_SSH`, `GIT_SSH_COMMAND`, `GIT_PROXY_COMMAND`, `GIT_EXTERNAL_DIFF`,
/// `GIT_EXEC_PATH`), désigne une autre configuration (`GIT_CONFIG*`, `GIT_DIR`), écrit un
/// fichier (`GIT_TRACE=/chemin`) ou ouvre un transport (`GIT_ALLOW_PROTOCOL`). La liste
/// est celle de ce qui est admis, pas de ce qui est refusé : git en ajoute à chaque
/// version.
const GIT_INERT: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_TERMINAL_PROMPT",
    "GIT_OPTIONAL_LOCKS",
    "GIT_MERGE_AUTOEDIT",
    "GIT_ADVICE",
    "GIT_PROGRESS_DELAY",
    "GIT_NO_LAZY_FETCH",
    "GIT_LFS_SKIP_SMUDGE",
];

/// Les mots-clés qui ne font que régler le shell pour la suite de la ligne, sous les
/// formes que [`obstacle`] laisse passer.
pub(super) const SETUP: &[&str] = &[
    "cd", "export", "set", "unset", "unalias", "umask", "ulimit", "pushd", "popd", "shopt",
    "local", "readonly",
];

/// Les commandes internes du shell qui changent ce que la suite de la ligne lance, et
/// qu'aucune forme ne rend lisible : une règle à leur nom couvrirait n'importe quel
/// détournement. `hash -p ./x git && git status` et `typeset -x PATH=… && ls` lancent
/// autre chose que ce qu'on lit, `trap ./x EXIT && true` aussi.
const RESHAPES: &[&str] = &[
    "alias",
    "declare",
    "typeset",
    "float",
    "integer",
    "hash",
    "trap",
    "enable",
    "bind",
    "zmodload",
    "autoload",
    "functions",
    "emulate",
    "setopt",
    "unsetopt",
    "disable",
    "sched",
];

/// Ce qui, dans l'environnement d'une étape, rend la ligne composée, nommé pour le
/// propriétaire. `None` : l'étape ne détourne rien.
pub(super) fn obstacle(c: &SimpleCommand) -> Option<String> {
    if let Some((name, _)) = c
        .assignments
        .iter()
        .find(|(name, value)| hijacks(name, Some(value)))
    {
        return Some(variable(name));
    }
    let program = c.program()?;
    let args = &c.words[1..];
    match program {
        _ if RESHAPES.contains(&program) => Some(format!(
            "`{program}`, qui change ce que la suite de la ligne lance"
        )),
        "export" | "readonly" | "local" => declares(program, args, c.expands),
        "unset" => unsets(args, c.expands),
        "set" => (!set_is_inert(args) || c.expands).then(|| unknown(program, args)),
        // `shopt -so keyword` est `set -k` sous un autre nom.
        "shopt" if args.iter().any(|a| a.starts_with('-') && a.contains('o')) => {
            Some(unknown(program, args))
        }
        // `printf -v PATH %s ./bin && ls` : une « lecture » qui pose une variable du shell,
        // `PATH` comprise. `print -v` fait de même sous zsh.
        "printf" | "print" if assigns(args) => {
            Some(format!("`{program} -v`, qui pose une variable du shell"))
        }
        _ => None,
    }
}

/// `-v` parmi les options de tête de `printf` ou de `print`, seul, collé ou groupé.
fn assigns(args: &[String]) -> bool {
    args.iter()
        .take_while(|a| a.len() > 1 && a.starts_with('-') && a.as_str() != "--")
        .any(|a| a.contains('v'))
}

fn variable(name: &str) -> String {
    format!("la variable `{name}`, qui change ce que la commande lance")
}

fn unknown(program: &str, args: &[String]) -> String {
    format!(
        "`{program} {}`, un réglage du shell hors des formes connues",
        args.join(" ")
    )
}

fn is_name(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `export NOM=valeur`, `export NOM` : des noms seulement, aucun détournant, aucune
/// option (`export -f` exporte une fonction, `export -n` retire).
fn declares(program: &str, args: &[String], expands: bool) -> Option<String> {
    for arg in args {
        let (name, value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg.as_str(), None),
        };
        if expands || !is_name(name) {
            return Some(unknown(program, args));
        }
        if hijacks(name, value) {
            return Some(variable(name));
        }
    }
    None
}

/// `unset NOM…` : retirer une variable détournante change autant la suite que la poser.
fn unsets(args: &[String], expands: bool) -> Option<String> {
    for arg in args {
        if matches!(arg.as_str(), "-v" | "-f") {
            continue;
        }
        if expands || !is_name(arg) {
            return Some(unknown("unset", args));
        }
        if hijacks(arg, None) {
            return Some(variable(arg));
        }
    }
    None
}

/// `set` ne se traverse que pour ce qu'on écrit en tête d'un script : arrêt à la première
/// erreur, variable absente, trace, tube. `set -k` (bash) fait d'un argument `VAR=valeur`
/// une variable d'environnement de la commande, `set -a` exporte tout ce qui est affecté,
/// et un argument nu remplace les paramètres positionnels.
fn set_is_inert(args: &[String]) -> bool {
    const LETTERS: &str = "euxfCv";
    const NAMES: &[&str] = &[
        "errexit",
        "nounset",
        "xtrace",
        "pipefail",
        "noglob",
        "noclobber",
        "verbose",
    ];
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let Some(letters) = arg.strip_prefix(['-', '+']) else {
            return false;
        };
        if letters.is_empty() {
            return false;
        }
        // `-o nom` : la lettre `o` ferme le groupe, son nom est le mot suivant.
        let (plain, named) = match letters.strip_suffix('o') {
            Some(plain) => (plain, true),
            None => (letters, false),
        };
        if !plain.chars().all(|c| LETTERS.contains(c)) {
            return false;
        }
        if named && !args.next().is_some_and(|n| NAMES.contains(&n.as_str())) {
            return false;
        }
    }
    true
}
