//! Les gestionnaires de paquets (issue #260) : ce qu'ils disent avoir installé, et de qui
//! vient chaque exécutable du PATH.
//!
//! Deux sources se complètent. La liste d'un gestionnaire donne les versions ; la cible
//! d'un lien du PATH (`Cellar/<formule>/<version>`, `node_modules/<paquet>`) dit à quel
//! paquet appartient un binaire dont le nom n'est pas celui du paquet (`rg` de
//! `ripgrep`). Les parseurs sont purs ; seul [`listed`] lance des processus.

use super::Found;
use penelope_platform::discover::Executable;
use penelope_platform::process::{probe_command, which_in};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::time::Duration;

/// Un paquet déclaré par un gestionnaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// `brew`, `brew-cask`, `npm`, `uv`, `pipx`, `cargo`.
    pub manager: &'static str,
    pub name: String,
    pub version: Option<String>,
    /// Exécutables annoncés, quand le gestionnaire les donne (`uv`, `cargo`).
    pub bins: Vec<String>,
}

/// Le programme, ses arguments et le parseur de chaque gestionnaire. Les listes
/// n'écrivent rien : `brew list`, `npm ls`, `uv tool list`, `pipx list`,
/// `cargo install --list`.
type Parser = fn(&str) -> Vec<Package>;
const MANAGERS: &[(&str, &[&str], Parser)] = &[
    ("brew", &["list", "--formula", "--versions"], parse_brew),
    ("brew", &["list", "--cask", "--versions"], parse_brew_cask),
    ("npm", &["ls", "-g", "--depth=0", "--json"], parse_npm),
    ("uv", &["tool", "list"], parse_uv),
    ("pipx", &["list", "--short"], parse_pipx),
    ("cargo", &["install", "--list"], parse_cargo),
];

/// Les paquets de chaque gestionnaire trouvé dans `path`, sondés en parallèle : `npm ls`
/// prend une seconde, `brew list` presque autant. Un gestionnaire absent, lent ou en
/// échec ne dit rien.
pub fn listed(path: &OsStr, timeout: Duration) -> Vec<Package> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = MANAGERS
            .iter()
            .filter_map(|(program, args, parse)| {
                let bin = which_in(program, path)?;
                Some(scope.spawn(move || {
                    let p = probe_command(&bin, args, timeout)?;
                    p.ok.then(|| parse(&p.stdout))
                }))
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().ok().flatten())
            .flatten()
            .collect()
    })
}

/// `brew list --formula --versions` : `ripgrep 14.1.0`, plusieurs versions possibles,
/// la dernière est l'active.
pub fn parse_brew(text: &str) -> Vec<Package> {
    name_version_lines(text, "brew")
}

pub fn parse_brew_cask(text: &str) -> Vec<Package> {
    name_version_lines(text, "brew-cask")
}

fn name_version_lines(text: &str, manager: &'static str) -> Vec<Package> {
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?.to_string();
            Some(Package {
                manager,
                name,
                version: words.last().map(str::to_string),
                bins: Vec::new(),
            })
        })
        .collect()
}

/// `npm ls -g --depth=0 --json` : `{"dependencies": {"@openai/codex": {"version": …}}}`.
pub fn parse_npm(text: &str) -> Vec<Package> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    v.get("dependencies")
        .and_then(|d| d.as_object())
        .into_iter()
        .flatten()
        .map(|(name, dep)| Package {
            manager: "npm",
            name: name.clone(),
            version: dep
                .get("version")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            bins: Vec::new(),
        })
        .collect()
}

/// `uv tool list` : `ruff v0.6.0` puis ses exécutables, `- ruff`.
pub fn parse_uv(text: &str) -> Vec<Package> {
    let mut out: Vec<Package> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some(bin) = l.strip_prefix("- ") {
            if let Some(p) = out.last_mut() {
                p.bins.push(bin.trim().to_string());
            }
            continue;
        }
        let mut words = l.split_whitespace();
        let (Some(name), Some(ver)) = (words.next(), words.next()) else {
            continue;
        };
        out.push(Package {
            manager: "uv",
            name: name.to_string(),
            version: Some(ver.trim_start_matches('v').to_string()),
            bins: Vec::new(),
        });
    }
    out
}

/// `pipx list --short` : `black 24.8.0`. Sans paquet, `pipx` écrit une phrase : une
/// version commence par un chiffre.
pub fn parse_pipx(text: &str) -> Vec<Package> {
    name_version_lines(text, "pipx")
        .into_iter()
        .filter(|p| {
            p.version
                .as_deref()
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        })
        .collect()
}

/// `cargo install --list` : `ripgrep v14.1.0:` puis ses exécutables indentés.
pub fn parse_cargo(text: &str) -> Vec<Package> {
    let mut out: Vec<Package> = Vec::new();
    for line in text.lines() {
        if line.starts_with(char::is_whitespace) {
            if let Some(p) = out.last_mut() {
                p.bins.push(line.trim().to_string());
            }
            continue;
        }
        let head = line.trim().trim_end_matches(':');
        let mut words = head.split_whitespace();
        let Some(name) = words.next() else { continue };
        out.push(Package {
            manager: "cargo",
            name: name.to_string(),
            version: words.next().map(|v| v.trim_start_matches('v').to_string()),
            bins: Vec::new(),
        });
    }
    out
}

/// Le paquet d'un exécutable d'après la cible de son lien : `(gestionnaire, paquet,
/// version si le chemin la porte)`.
fn by_target(target: &str) -> Option<(&'static str, String, Option<String>)> {
    let after = |marker: &str| -> Option<Vec<&str>> {
        let i = target.find(marker)?;
        Some(target[i + marker.len()..].split('/').collect())
    };
    if let Some(seg) = after("/Cellar/") {
        return Some((
            "brew",
            seg.first()?.to_string(),
            seg.get(1).map(|s| s.to_string()),
        ));
    }
    if let Some(seg) = after("/Caskroom/") {
        return Some((
            "brew-cask",
            seg.first()?.to_string(),
            seg.get(1).map(|s| s.to_string()),
        ));
    }
    if let Some(seg) = after("/node_modules/") {
        let first = seg.first()?;
        let name = if first.starts_with('@') {
            format!("{first}/{}", seg.get(1)?)
        } else {
            first.to_string()
        };
        return Some(("npm", name, None));
    }
    if let Some(seg) = after("/uv/tools/") {
        return Some(("uv", seg.first()?.to_string(), None));
    }
    if let Some(seg) = after("/pipx/venvs/") {
        return Some(("pipx", seg.first()?.to_string(), None));
    }
    None
}

/// La carte des outils : chaque exécutable du PATH avec sa source, puis les paquets
/// qu'aucun exécutable n'a réclamés (bibliothèques, applications de `brew --cask`).
pub fn tools(exes: &[Executable], packages: &[Package]) -> Vec<Found> {
    let find = |manager: &str, name: &str| {
        packages
            .iter()
            .find(|p| p.manager == manager && p.name == name)
    };
    let mut claimed: BTreeSet<(&str, String)> = BTreeSet::new();
    let mut out = Vec::new();
    for e in exes {
        let target = e.target.to_string_lossy();
        // Le lien d'abord ; puis un paquet qui annonce ce binaire ; puis un paquet du même
        // nom (`gh` de la formule `gh`, un faux `brew list` dans les tests).
        let pkg = by_target(&target)
            .map(|(m, name, ver)| {
                let listed = find(m, &name);
                (
                    m,
                    name,
                    ver.or_else(|| listed.and_then(|p| p.version.clone())),
                )
            })
            .or_else(|| {
                packages
                    .iter()
                    .find(|p| p.bins.contains(&e.name))
                    .or_else(|| packages.iter().find(|p| p.name == e.name))
                    .map(|p| (p.manager, p.name.clone(), p.version.clone()))
            });
        let found = match pkg {
            Some((manager, name, version)) => {
                claimed.insert((manager, name.clone()));
                Found {
                    name: e.name.clone(),
                    source: manager.to_string(),
                    package: (name != e.name).then_some(name),
                    version,
                    path: Some(e.path.to_string_lossy().to_string()),
                }
            }
            None => Found {
                name: e.name.clone(),
                source: if e.system { "système" } else { "PATH" }.to_string(),
                package: None,
                version: None,
                path: Some(e.path.to_string_lossy().to_string()),
            },
        };
        out.push(found);
    }
    for p in packages {
        if claimed.contains(&(p.manager, p.name.clone())) {
            continue;
        }
        out.push(Found {
            name: p.name.clone(),
            source: p.manager.to_string(),
            package: None,
            version: p.version.clone(),
            path: None,
        });
    }
    out.sort_by(|a, b| (&a.name, &a.source).cmp(&(&b.name, &b.source)));
    out.dedup_by(|a, b| a.name == b.name && a.source == b.source);
    out
}
