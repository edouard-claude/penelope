//! `penelope-archtest` : test d'architecture (§2.1, §3.1).
//!
//! Vérifie par lecture des manifestes et recherche de motifs :
//! - les règles de dépendance entre crates ;
//! - l'absence de chemins littéraux, d'appels shell, de signaux Unix et d'API Trousseau
//!   **hors** `penelope-platform` ;
//! - l'interdiction de `unsafe` hors des crates FFI explicitement listés ;
//! - le gel de la dette (`freeze`, `budget`, `ratchet`) : plafonds de taille, liste
//!   blanche des modules du daemon, couplage au `Daemon`, allows comptés, critères
//!   d'acceptation figés et frontière canal/cœur, confrontés à `budget.toml`.

#![forbid(unsafe_code)]

pub mod budget;
pub mod freeze;
pub mod ratchet;
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
    // ni une crate extraite du daemon, qui sont au-dessus de lui. `penelope-telegram` en
    // sort avec T36 (gabarits et actions quittent `Services`).
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

/// Ce dont `penelope-app` peut dépendre : les treize crates métier.
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
    "penelope-telegram",
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
/// métier dont elle se sert. Ni le daemon, ni l'hôte MCP (§3.2 : `McpAdmin` par le port),
/// ni `penelope-telegram`, `penelope-workflow`, qu'elle ne lit que par `Services`.
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
/// agent, **pas** orchestrator). `penelope-telegram` pour la liste des commandes de
/// `self_status`, jusqu'à T36.
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
    "penelope-telegram",
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
pub const GATEWAY_DEPENDENTS: &[&str] = &["penelope-cli"];

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
mod tests {
    use super::*;

    #[test]
    fn the_workspace_is_discovered() {
        let all = crates();
        assert!(all.len() >= 15, "crates trouvés : {}", all.len());
        let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
        for expected in [
            "penelope-kernel",
            "penelope-store",
            "penelope-platform",
            "penelope-mcp",
            "penelope-memory",
            "penelope-daemon",
            "penelope-app",
            "penelope-mcp-host",
            "penelope-vault",
            "penelope-ops",
            "penelope-dream",
            "penelope-agent",
            "penelope-executor",
            "penelope-cli",
            "penelope-gateway-telegram",
        ] {
            assert!(names.contains(&expected), "crate manquant : {expected}");
        }
    }

    /// CA 3 : le test d'architecture échoue si une dépendance interdite est ajoutée.
    #[test]
    fn ca_3_1_dependency_rules_hold() {
        let v = dependency_violations();
        assert!(v.is_empty(), "violations :\n{}", v.join("\n"));
    }

    #[test]
    fn store_depends_on_no_business_crate() {
        let all = crates();
        let store = all.iter().find(|c| c.name == "penelope-store").unwrap();
        assert!(
            store.internal_deps.is_empty(),
            "penelope-store doit rester une infrastructure pure : {:?}",
            store.internal_deps
        );
    }

    #[test]
    fn kernel_depends_only_on_store() {
        let all = crates();
        let kernel = all.iter().find(|c| c.name == "penelope-kernel").unwrap();
        assert_eq!(
            kernel.internal_deps,
            ["penelope-store".to_string()].into_iter().collect(),
            "le noyau ne dépend que du stockage"
        );
    }

    /// T21 : le socle de l'application ne connaît pas le daemon.
    #[test]
    fn the_app_crate_does_not_depend_on_the_daemon() {
        let all = crates();
        let app = all.iter().find(|c| c.name == "penelope-app").unwrap();
        assert!(
            !app.internal_deps.contains("penelope-daemon"),
            "penelope-app est sous le daemon : {:?}",
            app.internal_deps
        );
    }

    /// T25 : l'hôte MCP ne connaît pas le daemon, qui le construit.
    #[test]
    fn the_mcp_host_crate_does_not_depend_on_the_daemon() {
        let all = crates();
        let host = all.iter().find(|c| c.name == "penelope-mcp-host").unwrap();
        assert!(
            !host.internal_deps.contains("penelope-daemon"),
            "penelope-mcp-host est sous le daemon : {:?}",
            host.internal_deps
        );
    }

    /// T29 : la passerelle Telegram n'est composée que par la CLI.
    #[test]
    fn only_the_cli_depends_on_the_gateway() {
        let v = gateway_dependent_violations();
        assert!(v.is_empty(), "violations :\n{}", v.join("\n"));
        let all = crates();
        let cli = all.iter().find(|c| c.name == "penelope-cli").unwrap();
        assert!(
            cli.internal_deps.contains("penelope-gateway-telegram"),
            "la CLI compose la passerelle : {:?}",
            cli.internal_deps
        );
    }

    /// T22 : la mémoire en fichiers ne connaît pas le daemon.
    #[test]
    fn the_vault_crate_does_not_depend_on_the_daemon() {
        let all = crates();
        let vault = all.iter().find(|c| c.name == "penelope-vault").unwrap();
        assert!(
            !vault.internal_deps.contains("penelope-daemon"),
            "penelope-vault est sous le daemon : {:?}",
            vault.internal_deps
        );
    }

    /// T28 : l'exploitation ne connaît ni le daemon, qui compose `doctor`, ni l'hôte MCP.
    #[test]
    fn the_ops_crate_depends_neither_on_the_daemon_nor_on_the_mcp_host() {
        let all = crates();
        let ops = all.iter().find(|c| c.name == "penelope-ops").unwrap();
        for above in ["penelope-daemon", "penelope-mcp-host"] {
            assert!(
                !ops.internal_deps.contains(above),
                "penelope-ops est sous {above} : {:?}",
                ops.internal_deps
            );
        }
    }

    /// T26 : le rêve, l'ingestion et l'accueil ne connaissent pas le daemon.
    #[test]
    fn the_dream_crate_does_not_depend_on_the_daemon() {
        let all = crates();
        let dream = all.iter().find(|c| c.name == "penelope-dream").unwrap();
        assert!(
            !dream.internal_deps.contains("penelope-daemon"),
            "penelope-dream est sous le daemon : {:?}",
            dream.internal_deps
        );
    }

    /// T24 : l'exécuteur ne connaît ni le daemon, ni la boucle qui l'appelle, ni les
    /// crates qui sont au-dessus de lui ; il ne les atteint que par les ports.
    #[test]
    fn the_executor_crate_sees_neither_the_daemon_nor_the_agent_loop() {
        let all = crates();
        let executor = all.iter().find(|c| c.name == "penelope-executor").unwrap();
        for above in [
            "penelope-daemon",
            "penelope-agent",
            "penelope-dream",
            "penelope-ops",
            "penelope-mcp-host",
            "penelope-gateway-telegram",
        ] {
            assert!(
                !executor.internal_deps.contains(above),
                "penelope-executor dépend de {above} : {:?}",
                executor.internal_deps
            );
        }
    }

    /// T10 : la boucle ne voit ni le moteur de contexte, ni la mémoire, ni le canal, ni
    /// le daemon (`design/v1/README.md` §3.2).
    #[test]
    fn the_agent_crate_sees_neither_context_memory_channel_nor_daemon() {
        let all = crates();
        let agent = all.iter().find(|c| c.name == "penelope-agent").unwrap();
        for above in [
            "penelope-context",
            "penelope-memory",
            "penelope-telegram",
            "penelope-daemon",
        ] {
            assert!(
                !agent.internal_deps.contains(above),
                "penelope-agent dépend de {above} : {:?}",
                agent.internal_deps
            );
        }
    }

    #[test]
    fn there_is_no_dependency_cycle() {
        let c = dependency_cycles();
        assert!(c.is_empty(), "cycles :\n{}", c.join("\n"));
    }

    /// CA 2 : le test échoue si un chemin littéral, un appel shell ou une API propre à un
    /// OS apparaît hors de `penelope-platform`.
    #[test]
    fn ca_2_3_no_os_specific_code_outside_the_platform_crate() {
        let v = forbidden_patterns();
        assert!(
            v.is_empty(),
            "motifs interdits :\n{}",
            v.iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn no_os_specific_dependencies_outside_the_platform_crate() {
        let v = os_dependency_violations();
        assert!(v.is_empty(), "{}", v.join("\n"));
    }

    #[test]
    fn unsafe_is_forbidden_everywhere() {
        let v = unsafe_violations();
        assert!(v.is_empty(), "{}", v.join("\n"));
    }

    #[test]
    fn the_pattern_detector_actually_detects() {
        // Garde-fou : si la détection cassait, les tests ci-dessus passeraient à tort.
        let sample = "let p = \"~/Library/Application Support/x\";";
        assert!(
            FORBIDDEN_PATTERNS
                .iter()
                .any(|(_, needle)| sample.contains(needle)),
            "le détecteur de motifs ne détecte plus rien"
        );
        let shell = "Command::new(\"bash\").arg(\"-c\")";
        assert!(
            FORBIDDEN_PATTERNS
                .iter()
                .any(|(_, needle)| shell.contains(needle))
        );
    }

    #[test]
    fn test_files_are_exempt_from_the_forbidden_patterns() {
        let raw = "let p = \"/tmp/projet\";\n";
        for rel in [
            "crates/penelope-daemon/src/workflow/tests.rs",
            "crates/penelope-daemon/src/telegram/tests/commands.rs",
            "crates/penelope-daemon/src/agent/clone_policy_tests.rs",
            "crates/penelope-daemon/src/mcp/testing.rs",
        ] {
            let v = forbidden_patterns_in("penelope-daemon", Path::new(rel), raw);
            assert!(v.is_empty(), "{rel} est un fichier de tests : {v:?}");
        }
        let v = forbidden_patterns_in(
            "penelope-daemon",
            Path::new("crates/penelope-daemon/src/workflow.rs"),
            raw,
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].rule, "chemin absolu /tmp");
        assert_eq!(v[0].line, 1);
    }

    #[test]
    fn every_crate_has_sources() {
        for c in crates() {
            assert!(
                !sources(&c).is_empty(),
                "{} n'a aucun fichier source",
                c.name
            );
        }
    }
}
