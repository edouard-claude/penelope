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
        "find . -name sans-virgule{}",
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
            "find . -name sans-virgule{}",
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

/// #443 : une option qui écrit ou qui lance, sous toutes ses écritures (collée, groupée,
/// abrégée, ou inconnue de l'ancienne liste noire), n'est jamais une lecture.
#[test]
fn an_option_that_writes_or_runs_is_never_a_read_however_it_is_spelled() {
    for line in [
        // `sort` : fichier de sortie, programme de compression.
        "sort -o out.txt in.txt",
        "sort -osrc/main.rs /dev/null",
        "sort -uo src/main.rs /dev/null",
        "sort --output=src/main.rs /dev/null",
        "sort --out=src/main.rs /dev/null",
        "sort --o src/main.rs /dev/null",
        "sort --compress-program=./script f",
        "cat f | sort -osortie",
        "cat f | sort --out=sortie",
        // `uniq` : le second argument est la sortie.
        "uniq /dev/null src/main.rs",
        "uniq -c entrée sortie",
        "uniq -f 1 entrée sortie",
        "cat f | uniq - sortie",
        // `git` : un programme lancé sur les fichiers trouvés, un fichier de sortie.
        "git grep -Oless motif",
        "git grep -O motif",
        "git grep -nO motif",
        "git grep '--open-files-in-pager=sh -c \"id\" x' motif",
        "git grep --open-files-in-pager motif",
        "git grep --open=less motif",
        "git diff --output=x",
        "git diff --outp=x",
        "git log --ext-diff -p",
        "git show --textconv HEAD:f",
        "git log --show-signature",
        "git branch nouvelle",
        "git branch -v nouvelle",
        "git branch -D main",
        // `tree`, `fd`, `rg`, `date`, `file`, `find`.
        "tree -o sortie",
        "tree -osortie",
        "tree -ao sortie",
        "tree -Lo 2 sortie",
        "tree -R -H .",
        "fd -x rm",
        "fd -xsh",
        "fd -Hx rm",
        "fd -X rm",
        "fd --exec rm",
        "fd --exec-batch=rm x",
        "rg --pre ./script x",
        "rg --pre=./script x",
        "rg --hostname-bin=./script --hyperlink-format=default x",
        "rg --hostname-bin ./script x",
        "rg -z x archive.gz",
        "cat f | rg --pre=./script x",
        "date -s 2020-01-01",
        "date -us 2020-01-01",
        "date --set=2020-01-01",
        "date 010112002020",
        "date -u 010112002020.30",
        "date -r 1700000000 010112002020",
        "file -C -m magique",
        "file --compile -m magique",
        "file -z archive.gz",
        "file -bz archive.gz",
        "find . -delete",
        "find . -exec rm {} +",
        "find . -fprint sortie",
        "find . -fls sortie",
        "find . -okdir rm {} ';'",
        // Dans le doute : une option inconnue de la table demande une carte.
        "sort --option-de-demain f",
        "git log --option-de-demain",
        "rg --option-de-demain x",
        "find . -primaire-de-demain",
        "cat f | jq --rawfile a b .",
        "cat f | jq -rf prog.jq",
    ] {
        assert!(!is_read_command(line), "{line}");
    }
}

/// #443 : les lectures courantes le restent. La liste reprend chaque option relevée dans
/// les `shell_exec` d'une instance réelle, le 11/11 : aucune n'a repris de carte.
#[test]
fn the_options_seen_in_real_reads_stay_reads() {
    for line in [
        "sort fichier",
        "sort -u fichier",
        "sort -rn -k2 fichier",
        "sort -t, -k1,1 fichier",
        "sort -to fichier",
        "sort --reverse --key=2 fichier",
        "uniq fichier",
        "uniq -c",
        "uniq -f 1 fichier",
        "cat f | sort | uniq -c | sort -rn | head -5",
        "cat f | uniq -c -",
        "tree -L 2",
        "tree -a -I node_modules --dirsfirst",
        "fd motif",
        "fd -e rs -t f motif src",
        "fd -tx motif",
        "fd --extension=rs --hidden",
        "date -u",
        "date -Iseconds",
        "date -v-48H",
        "date -j -f %s 0 +%Y",
        "date -r 1700000000",
        "date -r 1700000000 +%F",
        "date -ju 010112002020 +%F",
        "date -d yesterday +%F",
        "date '+%Y-%m-%d %H:%M'",
        "TZ=UTC date +%s",
        "file --mime-type x",
        "file -I x",
        "find . -type f -maxdepth 2 -name '*.rs' -print",
        "find . -path ./target -prune -o -not -iname '*.md' -print",
        "find . -mmin -30 -newermt 2026-01-01 -perm -111",
        "find . -newer Cargo.toml -mindepth 1 -size -10k",
        "git status --short --branch",
        "git status -sb --porcelain",
        "git status -uno",
        "git log --oneline --all --decorate --graph -30",
        "git log --format=%h --pretty=oneline --date=short --since=1.week -n 5",
        "git log --grep=x --author=y -i --regexp-ignore-case --stat --name-status",
        "git log --left-right --simplify-by-decoration --topo-order -1 -- src",
        "git log -S'fn main' -G regex -L 1,5:f",
        "git diff --stat --check --unified=0 --name-status --find-renames --find-copies",
        "git diff --exit-code --numstat --binary --cached --name-only --no-index a b",
        "git diff --no-ext-diff -U5 -M90% -w HEAD~1 -- src",
        "git diff --text",
        "git show --stat --format=%s --oneline --decorate --summary --no-patch HEAD",
        "git show --no-ext-diff --unified=3 --name-only --no-renames --patch HEAD@{1}",
        "git grep -n motif",
        "git grep -n -E -i -iE -e motif -- src",
        "git grep -eOk -A3 --count",
        "git branch -a",
        "git branch -vv --all",
        "git branch -avv",
        "git branch --all --verbose --no-abbrev",
        "git branch --show-current",
        "git remote -v",
        "git rev-parse --short HEAD",
        "git rev-parse --abbrev-ref HEAD",
        "git describe --always --tags",
        "git ls-files -o --exclude-standard",
        "git blame -L 10,20 -w f",
        "rg -n -i --glob '*.rs' -g '!target' -l --hidden motif",
        "rg -C 3 --files --iglob x --max-count 2 -ln -m 1 --no-heading --no-messages x",
        "rg -A140 -A20 -S motif src",
        "rg -e -motif src",
        "cat f | rg -n motif",
        "cat f | jq -r '.a'",
        "cat f | jq -e -c --arg n v '.[$n]'",
    ] {
        assert!(is_read_command(line), "{line}");
    }
}

/// #443 : `uniq` écrit dans son second argument, sous bash comme sous zsh. La forme que
/// le classement refuse est bien celle qui écrit.
#[cfg(unix)]
#[test]
fn uniq_really_writes_its_second_argument() {
    let dir = tempfile::tempdir().expect("dossier");
    std::fs::write(dir.path().join("entrée"), "a\na\n").expect("entrée");
    let Ok(status) = std::process::Command::new("uniq")
        .args(["entrée", "sortie"])
        .current_dir(dir.path())
        .status()
    else {
        return;
    };
    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sortie")).expect("sortie"),
        "a\n"
    );
}
