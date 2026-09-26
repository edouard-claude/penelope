//! `penelope-archtest` : test d'architecture (§2.1, §3.1).
//!
//! Vérifie par lecture des manifestes et recherche de motifs :
//! - les règles de dépendance entre crates ;
//! - l'absence de chemins littéraux, d'appels shell, de signaux Unix et d'API Trousseau
//!   **hors** `penelope-platform` ;
//! - l'interdiction de `unsafe` hors des crates FFI explicitement listés ;
//! - les caches de la conversation écrits par `penelope-context` seule (`caches`) ;
//! - le gel de la dette (`freeze`, `budget`, `ratchet`) : plafonds de taille, liste
//!   blanche des modules du daemon, couplage au `Daemon`, allows comptés, critères
//!   d'acceptation figés et frontière canal/cœur, confrontés à `budget.toml` ;
//! - chaque surface visible exercée par un scénario rejouable (`scenarios`, R10).

#![forbid(unsafe_code)]

pub mod budget;
pub mod caches;
pub mod freeze;
pub mod ratchet;
pub mod reach;
pub mod scenarios;
pub mod snapshot;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Un crate du workspace.
#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    pub dir: PathBuf,
    /// Dépendances `penelope-*` déclarées.
    pub internal_deps: BTreeSet<String>,
    /// Dépendances externes déclarées.
    pub external_deps: BTreeSet<String>,
}

/// Racine du workspace, déduite de l'emplacement de ce crate.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Lit tous les crates du workspace.
pub fn crates() -> Vec<Crate> {
    let root = workspace_root().join("crates");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let dir = e.path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(parsed) = raw.parse::<toml::Value>() else {
            continue;
        };
        let name = parsed
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();

        let mut internal = BTreeSet::new();
        let mut external = BTreeSet::new();
        for table in ["dependencies", "dev-dependencies"] {
            let is_dev = table == "dev-dependencies";
            if let Some(deps) = parsed.get(table).and_then(|d| d.as_table()) {
                for k in deps.keys() {
                    if k.starts_with("penelope-") {
                        // Les dépendances de développement ne comptent pas dans les
                        // règles d'architecture : elles ne sont pas livrées.
                        if !is_dev {
                            internal.insert(k.clone());
                        }
                    } else if !is_dev {
                        external.insert(k.clone());
                    }
                }
            }
        }
        out.push(Crate {
            name,
            dir,
            internal_deps: internal,
            external_deps: external,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Fichiers sources d'un crate.
pub fn sources(c: &Crate) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(&c.dir.join("src"), &mut out);
    out.sort();
    out
}

pub(crate) fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// Une violation détectée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub crate_name: String,
    pub file: PathBuf,
    pub line: usize,
    pub rule: &'static str,
    pub text: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {}:{} — {} : {}",
            self.crate_name,
            self.file.display(),
            self.line,
            self.rule,
            self.text.trim()
        )
    }
}

/// Motifs interdits hors `penelope-platform` (§2.1).
pub const FORBIDDEN_PATTERNS: &[(&str, &str)] = &[
    ("chemin littéral macOS", "~/Library"),
    ("chemin littéral macOS", "/Library/Application Support"),
    ("chemin littéral Windows", "C:\\\\"),
    ("chemin absolu /tmp", "\"/tmp/"),
    ("appel shell", "Command::new(\"sh\")"),
    ("appel shell", "Command::new(\"bash\")"),
    ("appel shell", "Command::new(\"zsh\")"),
    ("appel shell", "Command::new(\"/bin/sh\")"),
    ("appel shell", "Command::new(\"/bin/bash\")"),
    ("signal Unix", "libc::kill"),
    ("signal Unix", "signal_hook"),
    ("API Trousseau", "security-framework"),
    ("API Trousseau", "SecKeychain"),
    ("séparateur de chemin en dur", "\"/\".to_string() +"),
];

/// Crates autorisés à contenir ces motifs.
///
/// `penelope-archtest` en fait partie parce qu'il **contient la table des motifs** :
/// s'exclure lui-même est la seule façon de ne pas se détecter soi-même.
pub const PLATFORM_CRATES: &[&str] = &["penelope-platform", "penelope-archtest"];

/// Recherche les motifs interdits.
pub fn forbidden_patterns() -> Vec<Violation> {
    let mut out = Vec::new();
    for c in crates() {
        if PLATFORM_CRATES.contains(&c.name.as_str()) {
            continue;
        }
        for file in sources(&c) {
            let Ok(raw) = std::fs::read_to_string(&file) else {
                continue;
            };
            out.extend(forbidden_patterns_in(&c.name, &file, &raw));
        }
    }
    out
}

/// Les motifs interdits d'un fichier.
///
/// Un fichier de tests (`tests.rs`, `tests/`, `*_tests.rs`, `testing.rs`) est ignoré en
/// entier, comme l'est ce qui suit un `#[cfg(test)]` : les tests sortis des gros fichiers
/// du daemon (#215) manipulent les mêmes chemins temporaires qu'avant, et l'attribut est
/// désormais sur la déclaration `mod` du parent.
pub fn forbidden_patterns_in(crate_name: &str, file: &Path, raw: &str) -> Vec<Violation> {
    if snapshot::is_test_path(&snapshot::relative_to_root(file)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut in_tests = false;
    for (i, line) in raw.lines().enumerate() {
        // Les blocs de test peuvent manipuler des chemins temporaires.
        if line.trim_start().starts_with("#[cfg(test)]") {
            in_tests = true;
        }
        if in_tests {
            continue;
        }
        // Une ligne de commentaire ou de documentation n'est pas du code.
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        for (rule, needle) in FORBIDDEN_PATTERNS {
            if line.contains(needle) {
                out.push(Violation {
                    crate_name: crate_name.to_string(),
                    file: file.to_path_buf(),
                    line: i + 1,
                    rule,
                    text: line.to_string(),
                });
            }
        }
    }
    out
}

/// Règles de dépendance (§3.1).
///
/// `penelope-store` est une **infrastructure**, pas un crate métier : `penelope-kernel`
/// peut en dépendre (voir `docs/decisions/0001-kernel-depend-de-store.md`).
pub fn dependency_rules() -> BTreeMap<&'static str, Vec<&'static str>> {
    let mut m = BTreeMap::new();
    m.insert("penelope-store", vec![]);
    m.insert("penelope-kernel", vec!["penelope-store"]);
    m.insert("penelope-observe", vec![]);
    m.insert("penelope-platform", vec![]);
    m.insert(
        "penelope-llm",
        vec![
            "penelope-kernel",
            "penelope-store",
            "penelope-observe",
            "penelope-platform",
        ],
    );
    m.insert(
        "penelope-context",
        vec![
            "penelope-kernel",
            "penelope-store",
            "penelope-llm",
            "penelope-observe",
        ],
    );
    m.insert(
        "penelope-hitl",
        vec!["penelope-kernel", "penelope-store", "penelope-observe"],
    );
    m.insert(
        "penelope-telegram",
        vec!["penelope-kernel", "penelope-store", "penelope-observe"],
    );
    // Le socle de l'application (épopée #208, T21) : les crates métier, jamais le daemon
    // ni une crate extraite du daemon, qui sont au-dessus de lui, ni le canal (T36 :
    // gabarits, actions et formulaires sont dans la passerelle, derrière les ports).
    m.insert("penelope-app", APP_ALLOWED_DEPS.to_vec());
    // L'hôte MCP (T25) : le socle et les crates métier dont il se sert, jamais le daemon,
    // qui le construit.
    m.insert("penelope-mcp-host", MCP_HOST_ALLOWED_DEPS.to_vec());
    // La mémoire en fichiers (T22) : le socle et les crates métier, sans le canal.
    m.insert("penelope-vault", VAULT_ALLOWED_DEPS.to_vec());
    // L'exploitation (T28) : le socle, le vault et les crates métier, jamais le daemon ni
    // l'hôte MCP (`McpAdmin` par le port), ni le canal.
    m.insert("penelope-ops", OPS_ALLOWED_DEPS.to_vec());
    // La boucle d'agent (T10) : le noyau, les crates métier de la boucle et les ports de
    // `penelope-app`. Jamais `penelope-context`, `penelope-memory`, `penelope-telegram`
    // (`design/v1/README.md` §3.2), ni le daemon, qui la compose.
    m.insert("penelope-agent", AGENT_ALLOWED_DEPS.to_vec());
    // La mémoire qui mûrit (T26) : le socle, le vault et les crates métier, jamais le
    // daemon ni l'orchestrateur, qui la déclenchent, ni le canal.
    m.insert("penelope-dream", DREAM_ALLOWED_DEPS.to_vec());
    // L'exécuteur des outils natifs (T24) : le socle, le vault et les crates métier ;
    // ni la boucle, qui l'appelle par `ToolExecutor`, ni l'orchestrateur (planification
    // par le port `Orchestrator`), ni les crates d'exploitation, ni le daemon.
    m.insert("penelope-executor", EXECUTOR_ALLOWED_DEPS.to_vec());
    // La conversation de session (T23) : le socle, le vault et les crates métier, jamais
    // le daemon, qui la compose, ni la boucle, qui la consomme par le port
    // `Conversation`, ni le canal (`design/v1/README.md` §3.2).
    m.insert("penelope-conversation", CONVERSATION_ALLOWED_DEPS.to_vec());
    // Le moteur de workflows et l'ordonnanceur (T27) : au-dessus de la boucle, de
    // l'exécuteur, de la conversation et du rêve, qu'ils déclenchent ; jamais le daemon,
    // qui les compose, ni le canal (`ChannelDelivery` et `Messenger` par les ports).
    m.insert("penelope-orchestrator", ORCHESTRATOR_ALLOWED_DEPS.to_vec());
    m
}

/// Ce dont `penelope-agent` peut dépendre (`design/v1/boucle-et-outils.md` §4.2, plus
/// `penelope-store` pour la base d'`AgentServices`).
pub const AGENT_ALLOWED_DEPS: &[&str] = &[
    "penelope-kernel",
    "penelope-llm",
    "penelope-tools",
    "penelope-hitl",
    "penelope-observe",
    "penelope-store",
    "penelope-app",
];

/// Ce dont `penelope-mcp-host` peut dépendre : le socle et six crates métier.
pub const MCP_HOST_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-mcp",
];

/// Ce dont `penelope-app` peut dépendre : les douze crates métier, sans le canal.
pub const APP_ALLOWED_DEPS: &[&str] = &[
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-context",
    "penelope-memory",
    "penelope-mcp",
    "penelope-skills",
    "penelope-tools",
    "penelope-hitl",
    "penelope-workflow",
];

/// Ce dont `penelope-vault` peut dépendre : `penelope-app` et les crates métier, sauf
/// `penelope-telegram` (le vault ne nomme pas de canal) et `penelope-skills`,
/// `penelope-workflow`, qu'il ne lit que par `Services`.
pub const VAULT_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-context",
    "penelope-memory",
    "penelope-mcp",
    "penelope-tools",
    "penelope-hitl",
];

/// Ce dont `penelope-ops` peut dépendre : `penelope-app`, `penelope-vault` et les crates
/// métier dont elle se sert, dont `penelope-context`, seule à écrire les caches de la
/// conversation que la purge efface (T16). Ni le daemon, ni l'hôte MCP (§3.2 : `McpAdmin`
/// par le port), ni `penelope-telegram`, `penelope-workflow`, qu'elle ne lit que par
/// `Services`.
pub const OPS_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-vault",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-memory",
    "penelope-mcp",
    "penelope-skills",
    "penelope-context",
];

/// Ce dont `penelope-dream` peut dépendre (§3.2 : app, vault et métier). Pas
/// `penelope-telegram` : les liens du digest arrivent en données (`DigestInputs`).
pub const DREAM_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-vault",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-context",
    "penelope-memory",
    "penelope-mcp",
    "penelope-tools",
    "penelope-hitl",
];

/// Ce dont `penelope-executor` peut dépendre (§3.2 : app, vault et métier ; **pas**
/// agent, **pas** orchestrator, **pas** le canal depuis T36).
pub const EXECUTOR_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-vault",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-context",
    "penelope-memory",
    "penelope-mcp",
    "penelope-skills",
    "penelope-tools",
    "penelope-workflow",
];

/// Ce dont `penelope-conversation` peut dépendre (§3.2 : `context`, `penelope-vault`,
/// `penelope-app`, et les crates métier dont elle se sert). Ni `penelope-agent`, ni
/// `penelope-telegram`, ni le daemon.
pub const CONVERSATION_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-vault",
    "penelope-kernel",
    "penelope-store",
    "penelope-observe",
    "penelope-llm",
    "penelope-context",
];

/// Ce dont `penelope-orchestrator` peut dépendre (`decoupage-daemon.md` §3.2 : app,
/// vault, agent, executor, dream, et la conversation dont les étapes `agent` lisent le
/// prompt ; plus les crates métier). Ni le daemon, ni l'exploitation, ni l'hôte MCP (par
/// `McpAdmin`), ni `penelope-telegram`, ni la passerelle.
pub const ORCHESTRATOR_ALLOWED_DEPS: &[&str] = &[
    "penelope-app",
    "penelope-vault",
    "penelope-agent",
    "penelope-executor",
    "penelope-conversation",
    "penelope-dream",
    "penelope-kernel",
    "penelope-store",
    "penelope-platform",
    "penelope-observe",
    "penelope-llm",
    "penelope-mcp",
    "penelope-tools",
    "penelope-hitl",
    "penelope-workflow",
];

/// Vérifie les règles de dépendance.
pub fn dependency_violations() -> Vec<String> {
    let rules = dependency_rules();
    let mut out = Vec::new();
    for c in crates() {
        let Some(allowed) = rules.get(c.name.as_str()) else {
            continue;
        };
        for dep in &c.internal_deps {
            if !allowed.contains(&dep.as_str()) {
                out.push(format!(
                    "`{}` ne doit pas dépendre de `{dep}` (autorisés : {})",
                    c.name,
                    if allowed.is_empty() {
                        "aucun".to_string()
                    } else {
                        allowed.join(", ")
                    }
                ));
            }
        }
    }
    out
}

/// Crates qui peuvent dépendre de la passerelle Telegram : la composition seulement
/// (épopée #208, T29, `decoupage-daemon.md` §7 R2). Le daemon la connaît par ses ports
/// (`Gateway`, `ChannelDelivery`, `Messenger`, `OwnerChannel`), jamais par son type ; les
/// `dev-dependencies` restent hors règle, comme pour les autres.
/// `penelope-evals` y est aussi : l'étape `telegram` de ses scénarios joue les commandes
/// par la vraie passerelle, sur un transport simulé (critère 7 de bascule).
pub const GATEWAY_DEPENDENTS: &[&str] = &["penelope-cli", "penelope-evals"];

/// Crates hors `GATEWAY_DEPENDENTS` qui déclarent la passerelle dans `[dependencies]`.
pub fn gateway_dependent_violations() -> Vec<String> {
    crates()
        .into_iter()
        .filter(|c| {
            c.internal_deps.contains("penelope-gateway-telegram")
                && !GATEWAY_DEPENDENTS.contains(&c.name.as_str())
        })
        .map(|c| {
            format!(
                "`{}` dépend de `penelope-gateway-telegram` : seule la composition ({}) \
                 la connaît ; passer par les ports `Gateway`, `ChannelDelivery`, \
                 `Messenger`, `OwnerChannel`",
                c.name,
                GATEWAY_DEPENDENTS.join(", ")
            )
        })
        .collect()
}

/// Détecte les cycles de dépendance entre crates.
pub fn dependency_cycles() -> Vec<String> {
    let all = crates();
    let graph: BTreeMap<String, BTreeSet<String>> = all
        .iter()
        .map(|c| (c.name.clone(), c.internal_deps.clone()))
        .collect();

    let mut cycles = Vec::new();
    for start in graph.keys() {
        let mut stack = vec![(start.clone(), vec![start.clone()])];
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some((node, path)) = stack.pop() {
            for dep in graph.get(&node).cloned().unwrap_or_default() {
                if &dep == start {
                    cycles.push(format!("{} → {}", path.join(" → "), dep));
                    continue;
                }
                if seen.insert(dep.clone()) {
                    let mut p = path.clone();
                    p.push(dep.clone());
                    stack.push((dep, p));
                }
            }
        }
    }
    cycles.sort();
    cycles.dedup();
    cycles
}

/// Crates qui peuvent contenir `unsafe` (§3.2 : aucun, hors FFI explicitement listé).
pub const UNSAFE_ALLOWED: &[&str] = &[];

/// Vérifie que chaque crate interdit `unsafe`.
pub fn unsafe_violations() -> Vec<String> {
    let mut out = Vec::new();
    for c in crates() {
        if UNSAFE_ALLOWED.contains(&c.name.as_str()) {
            continue;
        }
        let lib = c.dir.join("src/lib.rs");
        let main = c.dir.join("src/main.rs");
        let entry = if lib.is_file() { lib } else { main };
        let Ok(raw) = std::fs::read_to_string(&entry) else {
            out.push(format!("`{}` : point d'entrée introuvable", c.name));
            continue;
        };
        if !raw.contains("#![forbid(unsafe_code)]") {
            out.push(format!(
                "`{}` : `#![forbid(unsafe_code)]` manquant dans {}",
                c.name,
                entry.display()
            ));
        }
    }
    out
}

/// Dépendances propres à un OS interdites hors `penelope-platform` (§2.1).
pub const OS_SPECIFIC_CRATES: &[&str] = &[
    "security-framework",
    "core-foundation",
    "windows",
    "windows-service",
    "landlock",
    "seccompiler",
    "nix",
];

pub fn os_dependency_violations() -> Vec<String> {
    let mut out = Vec::new();
    for c in crates() {
        if PLATFORM_CRATES.contains(&c.name.as_str()) {
            continue;
        }
        for dep in &c.external_deps {
            if OS_SPECIFIC_CRATES.contains(&dep.as_str()) {
                out.push(format!(
                    "`{}` dépend de `{dep}`, propre à un OS : cela doit rester dans \
                     penelope-platform",
                    c.name
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
