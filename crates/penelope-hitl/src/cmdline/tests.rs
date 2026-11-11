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

/// Commentaire de #141 : un tube dont toutes les étapes suivantes ne font que lire
/// garde la famille de sa première étape ; une étape qui écrit ou lance autre chose
/// compose la ligne.
#[test]
fn a_pipe_into_pure_reads_keeps_the_family_of_its_first_stage() {
    let p = pipeline("glab api --hostname h \"pipelines?per_page=30\" | jq -r '.[].id'")
        .expect("tube de lecture");
    assert_eq!(p.head.program(), Some("glab"));
    assert_eq!(p.head.words[1], "api");
    assert_eq!(p.filters.len(), 1);
    assert_eq!(p.filters[0].program(), Some("jq"));

    for read in [
        "glab api h \"p\" | cat",
        "glab api h \"p\" | grep -inE \"error|failed\"",
        "cat f | grep x | head -20 | wc -l",
        "ls | sort | uniq -c",
        "git log | tr -d x | cut -c1-20",
    ] {
        let p = pipeline(read).unwrap_or_else(|| panic!("{read}"));
        assert!(!p.filters.is_empty(), "{read}");
    }
    // Ce qui écrit, lance autre chose, ou lit ailleurs, reste composé.
    for composed in [
        "glab api h \"p\" | sh",
        "glab api h \"p\" | xargs rm",
        "glab api h \"p\" | tee /tmp/x",
        "glab api h \"p\" | python3 -",
        "glab api h \"p\" | sed -i s/a/b/ f",
        "cat f | jq --rawfile x /etc/passwd .",
        "cat f | sort -o /tmp/vol",
        "cat f | cat /etc/passwd",
        "cat f | rg --pre danger x",
        "cat f | grep x > sortie",
        "cat f || rm -rf ~",
        "ls |",
        "| ls",
        "cat f | TZ=UTC grep x",
    ] {
        assert!(pipeline(composed).is_none(), "{composed}");
    }
    // `simple` reste strict : une seule commande, pas de tube.
    assert!(simple("cat f | grep x").is_none());
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

fn heads(line: &str) -> Option<Vec<String>> {
    Some(
        list(line)?
            .steps
            .iter()
            .map(|s| s.head.words.join(" "))
            .collect(),
    )
}

/// #150 : trois commandes nommables collées par `&&` font trois étapes.
#[test]
fn a_chain_of_ands_is_a_list_of_steps() {
    assert_eq!(
        heads("yt-dlp --print x https://y && yt-dlp -o tmp/z https://y && ls -la tmp/z*"),
        Some(vec![
            "yt-dlp --print x https://y".into(),
            "yt-dlp -o tmp/z https://y".into(),
            "ls -la tmp/z*".into(),
        ])
    );
    // Une ligne simple est une liste d'une étape : un seul chemin pour les deux cas.
    assert_eq!(heads("ls -la"), Some(vec!["ls -la".into()]));
    // Un tube de lecture pure reste une étape, avec la famille de sa tête (#141).
    assert_eq!(
        heads("glab api projects | jq -r .id && echo ok"),
        Some(vec!["glab api projects".into(), "echo ok".into()])
    );
}

/// Ce que #67 exclut reste exclu : la liste n'ouvre que `&&`.
#[test]
fn everything_but_and_stays_composed() {
    for line in [
        "cargo test; rm -rf ~",
        "a || b",
        "a && $(b)",
        "a && b > f",
        "a && sh -c \"b\"",
        "a && b &",
        "a && b | sh",
        "a &&& b",
        "&& b",
        "a &&",
        "a && ! b",
        "a && PATH=/tmp ls",
        "a && b `c`",
        "a\n&& b",
    ] {
        assert!(list(line).is_none(), "« {line} » doit rester composée");
    }
}

/// Un `&` entre guillemets reste un caractère (#141) : une URL n'enchaîne rien.
#[test]
fn an_ampersand_in_quotes_is_not_a_chain() {
    assert_eq!(
        heads("glab api \"projects?membership=true&per_page=100\""),
        Some(vec![
            "glab api projects?membership=true&per_page=100".into()
        ])
    );
    assert_eq!(
        heads("curl 'https://x?a=1&&b=2' && ls"),
        Some(vec!["curl https://x?a=1&&b=2".into(), "ls".into()])
    );
}

/// `pipeline` répond toujours pour une seule commande : une liste n'en est pas une.
#[test]
fn pipeline_still_refuses_a_list() {
    assert!(pipeline("a && b").is_none());
    assert!(pipeline("ls -la").is_some());
}

/// #150 : la carte nomme ce qui empêche une règle ; une ligne à famille n'a rien à
/// nommer.
#[test]
fn the_obstacle_to_a_family_is_named() {
    assert_eq!(why_composed("cargo test && cargo build"), None);
    for (line, why) in [
        ("a; b", "`;`"),
        ("a || b", "`||`"),
        ("a & b", "`&` (arrière-plan)"),
        ("a > f", "une redirection"),
        ("(a)", "un sous-shell"),
        ("echo $(id)", "`$(…)`"),
        ("echo `id`", "`` ` ``"),
        ("echo a\\ b", "un échappement"),
        ("a\nb", "un retour à la ligne"),
        ("echo \"$HOME\"", "`$` entre guillemets"),
        ("echo \"abc", "guillemets non fermés"),
        ("echo 'abc", "guillemets non fermés"),
        ("echo 'a'; b", "`;`"),
    ] {
        assert_eq!(why_composed(line).as_deref(), Some(why), "{line}");
    }
}

/// `||`, `&` seul et `&&&` ne sont pas des listes de commandes nommables.
#[test]
fn only_a_double_ampersand_chains() {
    assert!(list("a && b").is_some());
    assert!(list("a || b").is_none());
    assert!(list("a & b").is_none());
    assert!(list("a &&& b").is_none());
    assert!(list("&& b").is_none());
}

/// Les formes de #442 : la ligne lue ne porte aucune option interdite, celle que le shell
/// lance en porte une.
const EXPANDING: &[&str] = &[
    "find . -maxdepth 0 -{print,exec} sh -c 'id' ';'",
    "find . -name '*.rs' -{delete,print}",
    "sort -{u,o} fichier /dev/null",
    "git diff --{output=fichier,stat}",
    "rg --{pre=./script,files} x",
    "cat f | sort -{u,o} fichier",
    "cat f | rg --{pre=./script,files} x",
    "find . -{print,\"exec\"} id ';'",
    "find . -{a,{print,exec}} id ';'",
    "tree -{a,o} sortie",
    "fd -{H,x} rm",
    "date -{u,s} 2020-01-01",
    "sort -{1..2}",
    // Un joker en tête de mot rend les noms du dossier, `-delete` compris.
    "find . *",
    "find . -name x -o ?delete",
    "find . [-]delete",
    "git log -*",
    "sort \"\"*",
];

/// #442 : un mot que le shell développera en d'autres mots ne laisse pas classer en
/// lecture un programme qui sait écrire ou lancer selon ses options.
#[test]
fn a_word_the_shell_will_expand_is_never_a_conditional_read() {
    for line in EXPANDING {
        assert!(!is_read_command(line), "{line}");
    }
    // Les mêmes sans développement restent ce qu'elles étaient.
    for line in [
        "find . -maxdepth 0 -print",
        "sort -u fichier",
        "git diff --stat",
        "rg --files x",
        "cat f | sort -u",
    ] {
        assert!(is_read_command(line), "{line}");
    }
}

/// #442 : ce qui ressemble à un développement sans en être un reste une lecture, comme
/// les programmes qui ne font que lire quels que soient leurs arguments (#111).
#[test]
fn quoted_braces_and_plain_readers_stay_reads() {
    for line in [
        "git show HEAD@{1}",
        "git log -1 stash@{0}",
        "jq '{a: .b}' f",
        "cat f | jq '{a: .b, c: [.d]}'",
        "find . -name '*.{rs,toml}'",
        "find . -name \"*.{rs,toml}\"",
        "find . -name '*.rs'",
        "find src/*.rs -newer Cargo.toml",
        "git diff src/*.rs",
        "git log --format='%h {%an,%ae}'",
        "rg 'a{1,3}' src",
        "rg \"fn [a-z]*\" src",
        "find . -exec-pas-une-option{}",
        "sort a,b}",
        // Lecture quels que soient les arguments : le développement n'y change rien.
        "ls *.rs",
        "ls -la tmp/x*",
        "cat src/{a,b}.rs",
        "wc -l *.rs",
        "grep -n x {a,b}.txt",
        "head -{1..3} f",
        "cat f | grep -{n,i} x",
    ] {
        assert!(is_read_command(line), "{line}");
    }
    assert!(!simple("git show HEAD@{1}").unwrap().expands);
    assert!(!simple("jq '{a,b}' f").unwrap().expands);
    assert!(simple("cat src/{a,b}.rs").unwrap().expands);
    assert!(simple("ls *").unwrap().expands);
    assert!(!simple("ls src/*").unwrap().expands);
}

/// #442 : le drapeau est celui de l'étape qui porte le mot, pas de ses voisines.
#[test]
fn the_expansion_belongs_to_its_own_step() {
    let l = list("find . -{print,exec} id ';' && git status && ls *").expect("liste");
    let flags: Vec<bool> = l.steps.iter().map(|s| s.head.expands).collect();
    assert_eq!(flags, [true, false, true]);
    let p = pipeline("git log | grep -{n,i} x").expect("tube");
    assert!(!p.head.expands && p.filters[0].expands);
}

/// #442 : une famille se relit telle qu'écrite ; un mot développable n'en nomme aucune.
#[test]
fn an_expanding_word_names_no_family() {
    assert!(family("git {status,push}").is_none());
    assert!(family("git *").is_none());
    assert!(family("git status").is_some());
}

/// #442, la preuve par le shell : pour chaque ligne, les mots que bash et zsh passent
/// vraiment au programme. S'ils diffèrent de ceux qu'on lit, `expands` doit l'avoir dit.
#[cfg(unix)]
#[test]
fn real_shells_expand_exactly_what_is_flagged() {
    let dir = tempfile::tempdir().expect("dossier");
    for name in ["-delete", "a.rs", "fichier"] {
        std::fs::write(dir.path().join(name), "").expect("fichier");
    }
    let lines: Vec<&str> = EXPANDING
        .iter()
        .copied()
        .chain([
            "git show HEAD@{1}",
            "jq '{a: .b}' f",
            "find . -name '*.{rs,toml}'",
            "find . -name \"*.{rs,toml}\"",
            "git log --format='%h {%an,%ae}'",
            "find . -exec-pas-une-option{}",
            "sort a,b}",
            "cat {a,b}.rs",
        ])
        .collect();
    let mut ran = 0;
    for shell in ["bash", "zsh"] {
        for line in &lines {
            // Une étape seulement : le tube rendrait la sortie de la dernière.
            let step = line.rsplit(" | ").next().unwrap_or(line);
            let read = simple(step).expect("ligne simple");
            // Le programme devient `printf` : le shell développe ses arguments de la même
            // façon, et les rend un par ligne.
            let (_, args) = step.split_once(' ').expect("arguments");
            let Ok(out) = std::process::Command::new(shell)
                .args(["-c", &format!("printf '%s\\n' {args}")])
                .current_dir(dir.path())
                .output()
            else {
                break;
            };
            // Une ligne que ce shell refuse (`a,b}` sous zsh) ne lance rien.
            if !out.status.success() {
                continue;
            }
            ran += 1;
            let run: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(String::from)
                .collect();
            if run != read.words[1..] {
                assert!(
                    read.expands,
                    "{shell} : `{line}` lance {run:?}, non signalé"
                );
            }
        }
    }
    // Sans aucun des deux shells (image minimale), le test ne prouve rien : le dire.
    assert!(ran > 0 || cfg!(not(target_os = "macos")), "ni bash ni zsh");
    // Et la forme du constat, mot pour mot, là où bash existe.
    if let Ok(out) = std::process::Command::new("bash")
        .args(["-c", "printf '%s ' -{print,exec}"])
        .output()
    {
        assert_eq!(String::from_utf8_lossy(&out.stdout), "-print -exec ");
    }
}
