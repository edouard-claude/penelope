//! Les options sous lesquelles un programme ne fait que lire (#443).
//!
//! `sort`, `find`, `fd`, `rg`, `git`, `tree`, `date`, `uniq` et `file` lisent, sauf avec
//! une option qui écrit ou qui lance un programme. Une liste noire comparée mot à mot en
//! manquait la moitié : collée (`sort -ofichier`), groupée (`sort -uo fichier`), abrégée
//! (`sort --out=fichier`, `git grep --open=sh`), ou simplement inconnue de la liste
//! (`git grep -O`, `rg --hostname-bin`). La question est donc retournée : chaque programme
//! a la liste de ce qu'il accepte en restant une lecture, et **tout le reste demande une
//! carte**, une option nouvelle ou mal connue comprise. Une liste qui vieillit coûte une
//! carte de trop, jamais une exécution de trop.
//!
//! Chaque mot qui commence par `-` est jugé comme une option, même quand il est la valeur
//! de l'option précédente (`rg -e -x`) : sauter un mot sur la foi d'une table serait le
//! seul moyen de laisser passer une option qu'on n'a pas lue.

/// Ce qu'un programme accepte en restant une lecture.
struct Reader {
    /// Lettres sans valeur, seules ou groupées (`-rn`).
    flags: &'static str,
    /// Lettres dont la suite du mot est la valeur (`-k2`, `-A20`) : le groupe s'arrête là.
    /// N'y mettre qu'une lettre dont le programme prend **toujours** le reste du mot.
    valued: &'static str,
    /// Options longues écrites en entier, avec ou sans `=valeur`. Une abréviation
    /// (`--out`) n'y est pas, donc n'est pas une lecture.
    long: &'static [&'static str],
    /// `-5` : un nombre en option.
    numeric: bool,
}

impl Reader {
    fn allows(&self, word: &str) -> bool {
        if let Some(long) = word.strip_prefix("--") {
            // `--` seul : fin des options.
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            return long.is_empty() || self.long.contains(&name);
        }
        let Some(group) = word.strip_prefix('-') else {
            return true;
        };
        if self.numeric && !group.is_empty() && group.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
        for c in group.chars() {
            if self.valued.contains(c) {
                return true;
            }
            if !self.flags.contains(c) {
                return false;
            }
        }
        true
    }

    fn reads(&self, args: &[String]) -> bool {
        args.iter().all(|w| self.allows(w))
    }

    /// Les arguments qui ne sont ni une option, ni la valeur d'une option courte écrite à
    /// part (`-f 1`). Dans le doute, un mot compte : la valeur d'une option longue écrite
    /// à part (`--skip-fields 1`) en est un.
    fn positionals<'a>(&self, args: &'a [String]) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut value = false;
        for w in args {
            if std::mem::take(&mut value) {
                continue;
            }
            match w.strip_prefix('-') {
                Some(group) if !group.is_empty() => {
                    // La dernière lettre attend sa valeur dans le mot suivant, si elle est
                    // la seule du groupe à en prendre une (`-cf 1`, pas `-ff`).
                    value = !w.starts_with("--")
                        && group
                            .chars()
                            .last()
                            .is_some_and(|c| self.valued.contains(c))
                        && group.chars().filter(|c| self.valued.contains(*c)).count() == 1;
                }
                _ => out.push(w.as_str()),
            }
        }
        out
    }

    /// `flag` est posé, seul ou dans un groupe (`-j`, `-ju`).
    fn has_flag(&self, args: &[String], flag: char) -> bool {
        args.iter().any(|w| {
            w.strip_prefix('-').is_some_and(|group| {
                !group.starts_with('-')
                    && group
                        .chars()
                        .take_while(|c| !self.valued.contains(*c))
                        .any(|c| c == flag)
            })
        })
    }
}

static SORT: Reader = Reader {
    flags: "bcCdfghiMmnRrsuVz",
    valued: "ktS",
    long: &[
        "buffer-size",
        "check",
        "dictionary-order",
        "field-separator",
        "general-numeric-sort",
        "human-numeric-sort",
        "ignore-case",
        "ignore-leading-blanks",
        "ignore-nonprinting",
        "key",
        "merge",
        "month-sort",
        "numeric-sort",
        "parallel",
        "random-sort",
        "reverse",
        "sort",
        "stable",
        "unique",
        "version-sort",
        "zero-terminated",
    ],
    numeric: false,
};

/// `uniq` : son second argument est un fichier de sortie.
static UNIQ: Reader = Reader {
    flags: "cdDiuz",
    valued: "fsw",
    long: &[
        "all-repeated",
        "check-chars",
        "count",
        "group",
        "ignore-case",
        "repeated",
        "skip-chars",
        "skip-fields",
        "unique",
        "zero-terminated",
    ],
    numeric: false,
};

/// `tree` lit ses lettres une à une et prend la valeur dans le mot suivant : aucune n'arrête
/// le groupe (`-Lo fichier` écrit). Sans `-o` (sortie) ni `-R` (un `00Tree.html` par
/// dossier).
static TREE: Reader = Reader {
    flags: "aAcCdDfFghiIJlLnNpPqQrsStuUvxX0123456789",
    valued: "",
    long: &[
        "charset",
        "device",
        "dirsfirst",
        "du",
        "filelimit",
        "filesfirst",
        "gitignore",
        "ignore-case",
        "inodes",
        "matchdirs",
        "metafirst",
        "noreport",
        "prune",
        "si",
        "sort",
        "timefmt",
    ],
    numeric: false,
};

/// `date` : sans `-s` ni `--set`. Un argument nu règle aussi l'horloge (`date 010112002020`),
/// voir `date_reads`.
static DATE: Reader = Reader {
    flags: "jnRu",
    valued: "dfIrvz",
    long: &[
        "date",
        "debug",
        "file",
        "iso-8601",
        "reference",
        "rfc-2822",
        "rfc-3339",
        "rfc-email",
        "universal",
        "utc",
    ],
    numeric: false,
};

/// `file` : sans `-C` (compile un fichier magique à côté de sa source), sans `-z` ni `-Z`
/// (peuvent lancer les décompresseurs du `PATH`), sans `-p`.
static FILE: Reader = Reader {
    flags: "bdEhiIkLnNrsSv0",
    valued: "eFfmP",
    long: &[
        "apple",
        "brief",
        "dereference",
        "exclude",
        "extension",
        "files-from",
        "keep-going",
        "magic-file",
        "mime",
        "mime-encoding",
        "mime-type",
        "no-buffer",
        "no-dereference",
        "no-pad",
        "print0",
        "raw",
        "separator",
        "special-files",
    ],
    numeric: false,
};

/// `fd` : sans `-x`, `-X`, `--exec`, `--exec-batch`.
static FD: Reader = Reader {
    flags: "aFgHiIlLpqsu01",
    valued: "cdeEjoSt",
    long: &[
        "absolute-path",
        "and",
        "base-directory",
        "case-sensitive",
        "changed-before",
        "changed-within",
        "color",
        "exact-depth",
        "exclude",
        "extension",
        "fixed-strings",
        "follow",
        "format",
        "full-path",
        "glob",
        "hidden",
        "ignore-case",
        "ignore-file",
        "list-details",
        "max-depth",
        "max-results",
        "min-depth",
        "newer",
        "no-follow",
        "no-hidden",
        "no-ignore",
        "no-ignore-parent",
        "no-ignore-vcs",
        "no-require-git",
        "older",
        "one-file-system",
        "owner",
        "print0",
        "prune",
        "quiet",
        "regex",
        "relative-path",
        "search-path",
        "show-errors",
        "size",
        "strip-cwd-prefix",
        "threads",
        "type",
        "unrestricted",
    ],
    numeric: false,
};

/// `rg` : sans `--pre` ni `--hostname-bin` (lancent un programme), sans `-z` (lance les
/// décompresseurs du `PATH`).
static RG: Reader = Reader {
    flags: "abcFHhIiLlNnoPpqSsUuVvwx0.",
    valued: "ABCdEefgjMmrTt",
    long: &[
        "after-context",
        "before-context",
        "binary",
        "block-buffered",
        "byte-offset",
        "case-sensitive",
        "color",
        "colors",
        "column",
        "context",
        "context-separator",
        "count",
        "count-matches",
        "crlf",
        "dfa-size-limit",
        "encoding",
        "engine",
        "field-context-separator",
        "field-match-separator",
        "file",
        "files",
        "files-with-matches",
        "files-without-match",
        "fixed-strings",
        "follow",
        "glob",
        "glob-case-insensitive",
        "heading",
        "hidden",
        "iglob",
        "ignore-case",
        "ignore-file",
        "include-zero",
        "invert-match",
        "json",
        "line-buffered",
        "line-number",
        "line-regexp",
        "max-columns",
        "max-columns-preview",
        "max-count",
        "max-depth",
        "max-filesize",
        "mmap",
        "multiline",
        "multiline-dotall",
        "no-column",
        "no-config",
        "no-filename",
        "no-heading",
        "no-ignore",
        "no-ignore-parent",
        "no-ignore-vcs",
        "no-line-number",
        "no-messages",
        "no-mmap",
        "no-require-git",
        "no-unicode",
        "null",
        "null-data",
        "one-file-system",
        "only-matching",
        "passthru",
        "path-separator",
        "pcre2",
        "pretty",
        "quiet",
        "regex-size-limit",
        "regexp",
        "replace",
        "smart-case",
        "sort",
        "sortr",
        "stats",
        "text",
        "threads",
        "trim",
        "type",
        "type-list",
        "type-not",
        "unrestricted",
        "vimgrep",
        "with-filename",
        "word-regexp",
    ],
    numeric: false,
};

/// `jq` en fin de tube : ce qui met en forme ce qu'il reçoit. Sans `-f` ni `--rawfile`,
/// `--slurpfile`, `--args`, `--jsonargs` (lisent ou prennent autre chose que le tube).
static JQ_FILTER: Reader = Reader {
    flags: "aCcejMnRrSs",
    valued: "",
    long: &[
        "arg",
        "argjson",
        "ascii-output",
        "color-output",
        "compact-output",
        "exit-status",
        "indent",
        "join-output",
        "monochrome-output",
        "null-input",
        "raw-input",
        "raw-output",
        "raw-output0",
        "seq",
        "slurp",
        "sort-keys",
        "stream",
        "tab",
        "unbuffered",
    ],
    numeric: false,
};

/// `git log`, `git diff`, `git show` : sans `--output` (écrit), `--ext-diff`, `--textconv`
/// ni `--show-signature` (lancent le programme que nomme la configuration), sans `-O`.
static GIT_HISTORY: Reader = Reader {
    flags: "abcEFghiPpqRrstuWwz",
    valued: "BCGLlMnSUX",
    long: &[
        "abbrev",
        "abbrev-commit",
        "after",
        "all",
        "all-match",
        "ancestry-path",
        "author",
        "author-date-order",
        "before",
        "binary",
        "boundary",
        "branches",
        "break-rewrites",
        "cached",
        "cc",
        "check",
        "cherry",
        "cherry-mark",
        "cherry-pick",
        "children",
        "color",
        "color-moved",
        "color-words",
        "committer",
        "compact-summary",
        "date",
        "date-order",
        "decorate",
        "dense",
        "diff-algorithm",
        "diff-filter",
        "diff-merges",
        "dirstat",
        "dst-prefix",
        "encoding",
        "exclude",
        "exit-code",
        "expand-tabs",
        "extended-regexp",
        "find-copies",
        "find-copies-harder",
        "find-renames",
        "first-parent",
        "fixed-strings",
        "follow",
        "format",
        "full-diff",
        "full-history",
        "full-index",
        "function-context",
        "glob",
        "graph",
        "grep",
        "histogram",
        "ignore-all-space",
        "ignore-blank-lines",
        "ignore-cr-at-eol",
        "ignore-space-at-eol",
        "ignore-space-change",
        "ignore-submodules",
        "indent-heuristic",
        "inter-hunk-context",
        "invert-grep",
        "left-right",
        "line-prefix",
        "max-count",
        "max-parents",
        "merge-base",
        "merges",
        "min-parents",
        "minimal",
        "name-only",
        "name-status",
        "no-abbrev-commit",
        "no-color",
        "no-commit-id",
        "no-decorate",
        "no-diff-merges",
        "no-expand-tabs",
        "no-ext-diff",
        "no-index",
        "no-mailmap",
        "no-merges",
        "no-notes",
        "no-patch",
        "no-prefix",
        "no-renames",
        "no-textconv",
        "no-walk",
        "not",
        "notes",
        "numstat",
        "oneline",
        "parents",
        "patch",
        "patch-with-stat",
        "patience",
        "perl-regexp",
        "pickaxe-all",
        "pickaxe-regex",
        "pretty",
        "quiet",
        "raw",
        "reflog",
        "regexp-ignore-case",
        "relative",
        "relative-date",
        "remerge-diff",
        "remotes",
        "reverse",
        "right-only",
        "root",
        "shortstat",
        "show-pulls",
        "simplify-by-decoration",
        "simplify-merges",
        "since",
        "skip",
        "source",
        "sparse",
        "src-prefix",
        "staged",
        "stat",
        "stat-count",
        "stat-name-width",
        "stat-width",
        "submodule",
        "summary",
        "tags",
        "text",
        "topo-order",
        "unified",
        "until",
        "use-mailmap",
        "walk-reflogs",
        "word-diff",
        "word-diff-regex",
        "ws-error-highlight",
    ],
    numeric: true,
};

/// `git grep` : sans `-O` ni `--open-files-in-pager` (lancent un programme sur les
/// fichiers trouvés), sans `--textconv`.
static GIT_GREP: Reader = Reader {
    flags: "acEFGHhIiLlnopPqrvWwz",
    valued: "ABCefm",
    long: &[
        "after-context",
        "all-match",
        "and",
        "basic-regexp",
        "before-context",
        "break",
        "cached",
        "color",
        "column",
        "context",
        "count",
        "exclude-standard",
        "extended-regexp",
        "files-with-matches",
        "files-without-match",
        "fixed-strings",
        "full-name",
        "function-context",
        "heading",
        "ignore-case",
        "invert-match",
        "line-number",
        "max-count",
        "max-depth",
        "name-only",
        "no-color",
        "no-exclude-standard",
        "no-index",
        "no-textconv",
        "not",
        "null",
        "only-matching",
        "or",
        "perl-regexp",
        "quiet",
        "recurse-submodules",
        "show-function",
        "text",
        "threads",
        "untracked",
        "word-regexp",
    ],
    numeric: true,
};

static GIT_STATUS: Reader = Reader {
    flags: "bsvz",
    valued: "u",
    long: &[
        "ahead-behind",
        "branch",
        "column",
        "find-renames",
        "ignore-submodules",
        "ignored",
        "long",
        "no-ahead-behind",
        "no-column",
        "no-renames",
        "porcelain",
        "renames",
        "short",
        "show-stash",
        "untracked-files",
        "verbose",
    ],
    numeric: false,
};

static GIT_LS_FILES: Reader = Reader {
    flags: "cdikmostuvz",
    valued: "xX",
    long: &[
        "abbrev",
        "cached",
        "debug",
        "deduplicate",
        "deleted",
        "directory",
        "eol",
        "error-unmatch",
        "exclude",
        "exclude-from",
        "exclude-per-directory",
        "exclude-standard",
        "format",
        "full-name",
        "ignored",
        "killed",
        "modified",
        "no-empty-directory",
        "others",
        "recurse-submodules",
        "sparse",
        "stage",
        "unmerged",
    ],
    numeric: false,
};

static GIT_BLAME: Reader = Reader {
    flags: "bcefklnpstw",
    valued: "CLMS",
    long: &[
        "abbrev",
        "color-by-age",
        "color-lines",
        "date",
        "first-parent",
        "ignore-rev",
        "ignore-revs-file",
        "incremental",
        "line-porcelain",
        "minimal",
        "porcelain",
        "reverse",
        "root",
        "show-email",
        "show-name",
        "show-number",
        "show-stats",
        "since",
    ],
    numeric: false,
};

static GIT_REV_PARSE: Reader = Reader {
    flags: "q",
    valued: "",
    long: &[
        "abbrev-ref",
        "absolute-git-dir",
        "all",
        "branches",
        "default",
        "git-common-dir",
        "git-dir",
        "git-path",
        "is-bare-repository",
        "is-inside-git-dir",
        "is-inside-work-tree",
        "is-shallow-repository",
        "path-format",
        "quiet",
        "remotes",
        "short",
        "show-cdup",
        "show-prefix",
        "show-superproject-working-tree",
        "show-toplevel",
        "symbolic",
        "symbolic-full-name",
        "tags",
        "verify",
    ],
    numeric: false,
};

static GIT_DESCRIBE: Reader = Reader {
    flags: "",
    valued: "",
    long: &[
        "abbrev",
        "all",
        "always",
        "broken",
        "candidates",
        "contains",
        "dirty",
        "exact-match",
        "exclude",
        "first-parent",
        "long",
        "match",
        "tags",
    ],
    numeric: false,
};

static GIT_SHORTLOG: Reader = Reader {
    flags: "cens",
    valued: "w",
    long: &[
        "after",
        "all",
        "author",
        "before",
        "committer",
        "email",
        "format",
        "group",
        "no-merges",
        "numbered",
        "since",
        "summary",
        "until",
    ],
    numeric: true,
};

/// `git branch` qui liste. Avec un nom il crée, d'où aucun argument hors option.
static GIT_BRANCH: Reader = Reader {
    flags: "arv",
    valued: "",
    long: &[
        "abbrev",
        "all",
        "color",
        "column",
        "format",
        "ignore-case",
        "list",
        "no-abbrev",
        "no-color",
        "no-column",
        "remotes",
        "show-current",
        "sort",
        "verbose",
    ],
    numeric: false,
};

/// Ce que `find` accepte en restant une lecture : ses tests, ses réglages et ses sorties
/// vers le terminal. Sans `-exec`, `-execdir`, `-ok`, `-okdir`, `-delete`, ni les `-f…`
/// qui écrivent un fichier.
const FIND: &[&str] = &[
    "-a",
    "-amin",
    "-and",
    "-anewer",
    "-atime",
    "-cmin",
    "-cnewer",
    "-ctime",
    "-d",
    "-daystart",
    "-depth",
    "-E",
    "-empty",
    "-executable",
    "-false",
    "-follow",
    "-fstype",
    "-gid",
    "-group",
    "-H",
    "-ilname",
    "-iname",
    "-inum",
    "-ipath",
    "-iregex",
    "-iwholename",
    "-L",
    "-links",
    "-lname",
    "-ls",
    "-maxdepth",
    "-mindepth",
    "-mmin",
    "-mount",
    "-mtime",
    "-name",
    "-newer",
    "-nogroup",
    "-not",
    "-nouser",
    "-o",
    "-or",
    "-P",
    "-path",
    "-perm",
    "-print",
    "-print0",
    "-printf",
    "-prune",
    "-quit",
    "-readable",
    "-regex",
    "-regextype",
    "-s",
    "-samefile",
    "-size",
    "-true",
    "-type",
    "-uid",
    "-user",
    "-wholename",
    "-writable",
    "-x",
    "-xdev",
    "-xtype",
];

/// Un mot de `find` : un test connu, `-newerXY`, ou une valeur négative (`-mtime -7`,
/// `-size -10k`, `-perm -111`).
fn find_allows(word: &str) -> bool {
    let Some(rest) = word.strip_prefix('-') else {
        return true;
    };
    let digits = rest.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let negative = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && rest.len() - digits.len() <= 1;
    negative
        || FIND.contains(&word)
        || rest.strip_prefix("newer").is_some_and(|xy| {
            xy.len() == 2 && xy.chars().all(|c| matches!(c, 'a' | 'B' | 'c' | 'm' | 't'))
        })
}

/// `git <sous-commande> …` : une sous-commande de lecture, sans option globale (`git -c …`
/// peut lancer une commande), et rien que des options connues.
fn git_reads(args: &[String]) -> bool {
    let Some((sub, rest)) = args.split_first() else {
        return false;
    };
    let reader = match sub.as_str() {
        "log" | "diff" | "show" => &GIT_HISTORY,
        "grep" => &GIT_GREP,
        "status" => &GIT_STATUS,
        "ls-files" => &GIT_LS_FILES,
        "blame" => &GIT_BLAME,
        "rev-parse" => &GIT_REV_PARSE,
        "describe" => &GIT_DESCRIBE,
        "shortlog" => &GIT_SHORTLOG,
        // Avec un nom, `git branch` crée et `git remote` configure.
        "branch" => return GIT_BRANCH.reads(rest) && GIT_BRANCH.positionals(rest).is_empty(),
        "remote" => return rest.iter().all(|w| w == "-v"),
        _ => return false,
    };
    reader.reads(rest)
}

/// `date` règle l'horloge avec un argument nu (`date 010112002020`), sous root, chez GNU
/// comme chez BSD : hors `-j` (BSD, « ne règle rien »), seul un format `+…` est une
/// lecture.
fn date_reads(args: &[String]) -> bool {
    DATE.reads(args)
        && (DATE.has_flag(args, 'j') || DATE.positionals(args).iter().all(|w| w.starts_with('+')))
}

/// `uniq` écrit dans son second argument.
fn uniq_reads(args: &[String]) -> bool {
    UNIQ.reads(args) && UNIQ.positionals(args).len() <= 1
}

/// Un programme de tête qui ne lit que selon ses options, avec ces arguments. `None` :
/// le programme n'est pas de ceux-là.
pub(super) fn head_reads(program: &str, args: &[String]) -> Option<bool> {
    Some(match program {
        "sort" => SORT.reads(args),
        "uniq" => uniq_reads(args),
        "tree" => TREE.reads(args),
        "date" => date_reads(args),
        "file" => FILE.reads(args),
        "fd" => FD.reads(args),
        "rg" => RG.reads(args),
        "find" => args.iter().all(|w| find_allows(w)),
        "git" => git_reads(args),
        _ => return None,
    })
}

/// La même question pour une étape de fin de tube.
pub(super) fn filter_reads(program: &str, args: &[String]) -> Option<bool> {
    Some(match program {
        "sort" => SORT.reads(args),
        "uniq" => uniq_reads(args),
        "rg" => RG.reads(args),
        "jq" => JQ_FILTER.reads(args),
        _ => return None,
    })
}
