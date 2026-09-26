//! Pouvoirs d'une ligne de commande et règles qui en dérivent (issue #203).
//!
//! Une ligne composée (`;`, `||`, `$(…)`, redirection…) n'a pas de famille : aucune règle
//! « Toujours » ne peut la couvrir (#67, #150). Le juge d'approbation décrit ce qu'elle
//! fait réellement, en **pouvoirs** (lecture, écriture, réseau, processus, paquet), avec
//! les chemins et les hôtes touchés. Une règle de pouvoirs couvre ensuite une ligne dont
//! les pouvoirs jugés sont **contenus** dans ceux que le propriétaire a lus et accordés.
//!
//! Ce module ne fait confiance à aucun texte : ni à la ligne (écrite par le modèle), ni au
//! jugement (rendu par un autre modèle). Les vetos déterministes passent avant tout
//! automatisme, et un doute vaut refus d'automatisme, jamais refus de carte.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

/// Opérateur d'une règle de pouvoirs dans `arg_match`. Une telle règle ne correspond
/// jamais par l'évaluation ordinaire : seule l'étape du juge la lit.
pub const POWERS_OP: &str = "$powers";

/// Chemins, et hôtes, qu'une règle de pouvoirs peut nommer : au-delà, la carte ne se lit
/// plus d'un coup d'œil (même esprit que `MAX_FAMILIES_PER_CLICK`).
pub const MAX_GRANT_ITEMS: usize = 3;

/// Un pouvoir demandé par une ligne.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Power {
    #[serde(rename = "lecture")]
    Read,
    #[serde(rename = "ecriture")]
    Write,
    #[serde(rename = "reseau")]
    Network,
    #[serde(rename = "processus")]
    Process,
    #[serde(rename = "paquet")]
    Package,
}

impl Power {
    pub const ALL: [Power; 5] = [
        Power::Read,
        Power::Write,
        Power::Network,
        Power::Process,
        Power::Package,
    ];

    /// Nom du schéma de sortie du juge.
    pub fn as_str(self) -> &'static str {
        match self {
            Power::Read => "lecture",
            Power::Write => "ecriture",
            Power::Network => "reseau",
            Power::Process => "processus",
            Power::Package => "paquet",
        }
    }

    pub fn parse(s: &str) -> Option<Power> {
        Power::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Nom lisible, accentué.
    pub fn label(self) -> &'static str {
        match self {
            Power::Read => "lecture",
            Power::Write => "écriture",
            Power::Network => "réseau",
            Power::Process => "processus",
            Power::Package => "paquet",
        }
    }
}

/// Ce qu'une règle de pouvoirs accorde : des pouvoirs, sous des chemins, vers des hôtes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PowerGrant {
    pub powers: Vec<Power>,
    /// Chemins absolus normalisés : un chemin jugé doit être dessous.
    pub paths: Vec<String>,
    /// Hôtes exacts, en minuscules.
    pub hosts: Vec<String>,
    /// Demande d'approbation dont le jugement a fait naître la règle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judged: Option<String>,
}

impl PowerGrant {
    /// Une règle accordable : des pouvoirs, peu de chemins et d'hôtes, rien d'illisible.
    pub fn readable(&self) -> bool {
        !self.powers.is_empty()
            && self.paths.len() <= MAX_GRANT_ITEMS
            && self.hosts.len() <= MAX_GRANT_ITEMS
            && self.paths.iter().all(|p| p.starts_with('/'))
            && self.hosts.iter().all(|h| valid_host(h))
            // Réseau sans hôte nommé : ce serait le réseau entier.
            && (!self.powers.contains(&Power::Network) || !self.hosts.is_empty())
            // Lecture ou écriture sans chemin : ce serait la machine entière.
            && (!self.touches_files() || !self.paths.is_empty())
    }

    fn touches_files(&self) -> bool {
        self.powers.contains(&Power::Read) || self.powers.contains(&Power::Write)
    }

    /// Le motif stocké dans `policies.arg_match`.
    pub fn to_pattern(&self) -> Value {
        json!({ POWERS_OP: self })
    }

    /// La règle de pouvoirs d'un motif, s'il en est une.
    pub fn from_pattern(pattern: &Value) -> Option<PowerGrant> {
        serde_json::from_value(pattern.get(POWERS_OP)?.clone()).ok()
    }

    /// Vrai si ces pouvoirs, chemins (absolus normalisés) et hôtes sont tous accordés.
    pub fn covers(&self, powers: &[Power], paths: &[String], hosts: &[String]) -> bool {
        !powers.is_empty()
            && powers.iter().all(|p| self.powers.contains(p))
            && paths
                .iter()
                .all(|p| self.paths.iter().any(|root| within(p, root)))
            && hosts
                .iter()
                .all(|h| self.hosts.contains(&h.to_ascii_lowercase()))
    }

    /// Une ligne pour une carte ou `/policies` : « lecture, écriture sous /x/out ;
    /// réseau vers api.github.com ».
    pub fn describe(&self) -> String {
        let powers: Vec<&str> = self.powers.iter().map(|p| p.label()).collect();
        let mut out = format!("pouvoirs {}", powers.join(", "));
        if !self.paths.is_empty() {
            out.push_str(&format!(" sous {}", self.paths.join(", ")));
        }
        if !self.hosts.is_empty() {
            out.push_str(&format!(" ; hôtes {}", self.hosts.join(", ")));
        }
        if let Some(id) = &self.judged {
            out.push_str(&format!(" (née du jugement de la demande {id})"));
        }
        out
    }
}

/// Un nom d'hôte simple : lettres, chiffres, `-`, `.`, `:` pour un port.
fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-.:".contains(&c))
}

/// Un hôte jugé, ramené à sa forme comparable ; `None` s'il n'en est pas un.
pub fn normalise_host(raw: &str) -> Option<String> {
    let h = raw.trim().to_ascii_lowercase();
    let h = h.split_once("://").map(|(_, rest)| rest).unwrap_or(&h);
    let h = h.split(['/', '?', '#']).next().unwrap_or_default();
    let h = h.rsplit('@').next().unwrap_or_default();
    valid_host(h).then(|| h.to_string())
}

/// Un chemin jugé, absolu et normalisé (`..` résolu dans le texte). Un chemin relatif est
/// pris depuis `cwd` ; un motif (`tmp/*.log`) vaut son répertoire. `None` : illisible
/// (`~`, variable, relatif sans répertoire de travail).
pub fn normalise_path(raw: &str, cwd: Option<&Path>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('~') || raw.contains(['$', '`', '\n']) {
        return None;
    }
    // Un motif touche un répertoire : on garde la partie fixe.
    let fixed = match raw.find(['*', '?', '[', '{']) {
        Some(i) => {
            let head = &raw[..i];
            match head.rfind('/') {
                Some(j) => &head[..=j],
                None => ".",
            }
        }
        None => raw,
    };
    let base: PathBuf = if Path::new(fixed).is_absolute() {
        PathBuf::from("/")
    } else {
        cwd.filter(|c| c.is_absolute())?.to_path_buf()
    };
    let mut out = base;
    for component in Path::new(fixed).components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::Prefix(_) => return None,
        }
    }
    Some(out.to_string_lossy().into_owned())
}

/// Vrai si `path` est `root` ou dessous, à la frontière d'un composant.
pub fn within(path: &str, root: &str) -> bool {
    Path::new(path).starts_with(Path::new(root))
}

/// Mots d'une ligne, sans guillemets, coupés aussi aux opérateurs : de quoi repérer un
/// programme **n'importe où** dans la ligne, y compris derrière `;` ou dans `$(…)`. Ce
/// n'est pas un analyseur : il ne sert qu'à refuser, jamais à autoriser.
pub fn words(line: &str) -> Vec<String> {
    line.split(|c: char| {
        c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '<' | '>' | '{' | '}')
    })
    .map(|w| w.trim_matches(|c| matches!(c, '\'' | '"' | '$' | '\\')))
    .filter(|w| !w.is_empty())
    .map(|w| w.rsplit('/').next().unwrap_or(w).to_string())
    .collect()
}

/// Premiers mots de chaque commande d'une ligne, affectations de tête ôtées : `a; b | c`
/// rend `a`, `b` et `c`. Sert à trouver une règle `Deny` du propriétaire sur une famille
/// qui paraît dans une ligne composée.
pub fn command_heads(line: &str) -> Vec<Vec<String>> {
    let mut heads = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let flush_word = |word: &mut String, current: &mut Vec<String>| {
        if !word.is_empty() {
            current.push(std::mem::take(word));
        }
    };
    let flush_cmd = |current: &mut Vec<String>, heads: &mut Vec<Vec<String>>| {
        let words: Vec<String> = std::mem::take(current)
            .into_iter()
            .skip_while(|w| w.contains('=') && !w.starts_with('='))
            .collect();
        if !words.is_empty() {
            heads.push(words);
        }
    };
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, ';' | '&' | '|' | '(' | ')' | '`' | '\n' | '{' | '}') => {
                flush_word(&mut word, &mut current);
                flush_cmd(&mut current, &mut heads);
            }
            (None, '$') => {}
            (None, c) if c.is_whitespace() => flush_word(&mut word, &mut current),
            (None, c) => word.push(c),
        }
    }
    flush_word(&mut word, &mut current);
    flush_cmd(&mut current, &mut heads);
    heads
}

/// Programmes qui détruisent, élèvent leurs droits ou arrêtent d'autres processus.
const DESTRUCTIVE: &[&str] = &[
    "rm",
    "rmdir",
    "dd",
    "shred",
    "truncate",
    "mkfs",
    "sudo",
    "doas",
    "su",
    "chown",
    "kill",
    "killall",
    "pkill",
    "launchctl",
    "diskutil",
    "srm",
    "wipefs",
];

/// Programmes qui parlent au réseau.
const NETWORK: &[&str] = &[
    "curl", "wget", "ssh", "scp", "sftp", "rsync", "nc", "ncat", "netcat", "telnet", "ftp", "http",
    "https", "socat",
];

/// Interpréteurs : ce qu'ils exécutent ne se lit pas dans la ligne.
const INTERPRETERS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "fish",
    "ksh",
    "eval",
    "exec",
    "source",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "deno",
    "bun",
    "php",
    "osascript",
    "xargs",
    "env",
    "awk",
    "gawk",
];

/// Programmes qui écrivent, lancent en arrière-plan ou planifient.
const WRITERS: &[&str] = &[
    "mv", "cp", "tee", "ln", "touch", "mkdir", "chmod", "install", "patch", "nohup", "setsid",
    "disown", "screen", "tmux", "crontab", "at", "open", "git", "npm", "pnpm", "yarn", "pip",
    "pip3", "brew", "cargo", "make", "docker",
];

/// Ce qui interdit **tout** automatisme sur une ligne, quel que soit le jugement : un
/// programme destructeur, ou le réseau versé dans un interpréteur (`curl … | sh`).
pub fn never_automatic(line: &str) -> Option<String> {
    let words = words(line);
    if let Some(w) = words.iter().find(|w| DESTRUCTIVE.contains(&w.as_str())) {
        return Some(format!("`{w}` dans la ligne"));
    }
    let net = words.iter().find(|w| NETWORK.contains(&w.as_str()));
    let interp = words.iter().find(|w| INTERPRETERS.contains(&w.as_str()));
    if let (Some(n), Some(i)) = (net, interp) {
        return Some(format!("`{n}` et `{i}` dans la même ligne"));
    }
    None
}

/// Ce qui interdit de tenir une ligne pour une lecture pure, quel que soit le jugement :
/// réseau, interpréteur, écriture, redirection vers un fichier, arrière-plan,
/// substitution. Un faux positif garde la carte ; c'est voulu (#13).
pub fn not_pure_read(line: &str) -> Option<String> {
    if let Some(why) = never_automatic(line) {
        return Some(why);
    }
    let words = words(line);
    for list in [NETWORK, INTERPRETERS, WRITERS] {
        if let Some(w) = words.iter().find(|w| list.contains(&w.as_str())) {
            return Some(format!("`{w}` dans la ligne"));
        }
    }
    if words
        .iter()
        .any(|w| w == "-i" || w.starts_with("--in-place"))
    {
        return Some("modification sur place (`-i`)".into());
    }
    // Les redirections qui ne touchent aucun fichier sont tolérées.
    let rest = [
        "2>/dev/null",
        "&>/dev/null",
        ">/dev/null",
        "2>&1",
        "1>&2",
        ">&2",
    ]
    .iter()
    .fold(line.to_string(), |acc, ok| acc.replace(ok, " "));
    if rest.contains('>') {
        return Some("redirection vers un fichier".into());
    }
    if rest.contains("$(") || rest.contains('`') || rest.contains("<(") {
        return Some("substitution de commande".into());
    }
    let background = rest
        .char_indices()
        .any(|(i, c)| c == '&' && !rest[i..].starts_with("&&") && !rest[..i].ends_with('&'));
    if background {
        return Some("processus en arrière-plan (`&`)".into());
    }
    None
}

#[cfg(test)]
mod tests;
