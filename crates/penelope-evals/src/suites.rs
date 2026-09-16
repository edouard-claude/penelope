//! Catalogue des suites d'évaluation (§20.1).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suite {
    pub name: &'static str,
    /// La suite a besoin du réseau.
    pub network: bool,
    pub description: &'static str,
    /// Section du PRD couverte.
    pub section: &'static str,
}

const fn s(
    name: &'static str,
    network: bool,
    description: &'static str,
    section: &'static str,
) -> Suite {
    Suite {
        name,
        network,
        description,
        section,
    }
}

/// Tableau du §20.1.
pub fn all_suites() -> Vec<Suite> {
    vec![
        s("unit", false, "Tests unitaires de tous les crates", "tous"),
        s(
            "arch",
            false,
            "Règles de dépendance et portabilité",
            "§2, §3",
        ),
        s(
            "ctx-safety",
            false,
            "Compaction 0..4, paires d'outils, reprise",
            "§5",
        ),
        s(
            "mem-learning",
            false,
            "Règles défaisables, promotion, corrections, provenance, anti-boucle",
            "§6",
        ),
        s(
            "mcp-conformance",
            false,
            "Matrice versions × transports × fonctionnalités, OAuth mock",
            "§8",
        ),
        s(
            "telegram",
            false,
            "Rendu, templates, CTA, limites, mock Bot API",
            "§14",
        ),
        s(
            "hitl",
            false,
            "Approbations, fenêtres, expiration, règles",
            "§9",
        ),
        s(
            "workflow",
            false,
            "Validation, exécution, reprise après crash, ticket-to-deploy",
            "§12",
        ),
        s(
            "hot-reload",
            false,
            "Skills, MCP, workflows, templates, config modifiés à chaud",
            "§4.4, §7, §8.7, §12.1",
        ),
        s(
            "resilience",
            false,
            "Crash, reprise, effets incertains",
            "§17",
        ),
        s(
            "security",
            false,
            "Injection, redaction, bac à sable, SSRF",
            "§13",
        ),
        s(
            "ctx-recall",
            true,
            "Rappel après compaction, modèle réel",
            "§5",
        ),
        s(
            "mem-longitudinal",
            true,
            "14 jours simulés de conversations",
            "§6",
        ),
        s(
            "live-openrouter",
            true,
            "Streaming, outils, raisonnement, image",
            "§10",
        ),
        s("live-telegram", true, "Bot de test réel", "§14"),
        s(
            "ab-hermes",
            true,
            "30 tâches rejouées sur Hermes et Pénélope",
            "§20.2",
        ),
    ]
}

/// Suites exécutables sans réseau : ce sont elles qui bloquent la livraison.
pub fn offline_suites() -> Vec<Suite> {
    all_suites().into_iter().filter(|x| !x.network).collect()
}

pub fn requires_network(name: &str) -> Option<bool> {
    all_suites()
        .into_iter()
        .find(|x| x.name == name)
        .map(|x| x.network)
}

pub fn find(name: &str) -> Option<Suite> {
    all_suites().into_iter().find(|x| x.name == name)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteResult {
    pub suite: String,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub duration_ms: u64,
    pub failures: Vec<String>,
}

impl SuiteResult {
    pub fn is_green(&self) -> bool {
        self.failed == 0
    }
    pub fn render(&self) -> String {
        let mark = if self.is_green() { "✅" } else { "❌" };
        let mut s = format!(
            "{mark} {} : {} réussis, {} échecs, {} ignorés ({} ms)",
            self.suite, self.passed, self.failed, self.skipped, self.duration_ms
        );
        for f in &self.failures {
            s.push_str(&format!("\n   - {f}"));
        }
        s
    }
}

/// Commande `cargo` correspondant à une suite sans réseau.
///
/// Les suites sont des **filtres de tests** : elles s'exécutent avec la même commande que
/// la CI, ce qui évite qu'un chemin de test diverge de l'autre.
pub fn cargo_filter(suite: &str) -> Option<(&'static str, Vec<String>)> {
    let args = |v: Vec<&str>| v.into_iter().map(String::from).collect::<Vec<String>>();
    Some(match suite {
        "unit" => ("test", args(vec!["--workspace"])),
        "arch" => ("test", args(vec!["-p", "penelope-archtest"])),
        "ctx-safety" => ("test", args(vec!["-p", "penelope-context", "ctx_safety"])),
        "mem-learning" => ("test", args(vec!["-p", "penelope-memory"])),
        "mcp-conformance" => (
            "test",
            args(vec!["-p", "penelope-evals", "--test", "mcp_conformance"]),
        ),
        "telegram" => ("test", args(vec!["-p", "penelope-telegram"])),
        "hitl" => ("test", args(vec!["-p", "penelope-hitl"])),
        "workflow" => ("test", args(vec!["-p", "penelope-workflow"])),
        "hot-reload" => (
            "test",
            args(vec!["-p", "penelope-evals", "--test", "hot_reload"]),
        ),
        "resilience" => (
            "test",
            args(vec!["-p", "penelope-evals", "--test", "resilience"]),
        ),
        "security" => (
            "test",
            args(vec!["-p", "penelope-evals", "--test", "security"]),
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_matches_the_prd_table() {
        let names: Vec<&str> = all_suites().iter().map(|s| s.name).collect();
        for expected in [
            "unit",
            "arch",
            "ctx-safety",
            "mem-learning",
            "mcp-conformance",
            "telegram",
            "hitl",
            "workflow",
            "hot-reload",
            "resilience",
            "security",
            "ctx-recall",
            "mem-longitudinal",
            "live-openrouter",
            "live-telegram",
            "ab-hermes",
        ] {
            assert!(names.contains(&expected), "suite manquante : {expected}");
        }
        assert_eq!(all_suites().len(), 16);
    }

    #[test]
    fn network_flags_follow_the_table() {
        for (name, network) in [
            ("unit", false),
            ("mcp-conformance", false),
            ("security", false),
            ("ctx-recall", true),
            ("live-openrouter", true),
            ("ab-hermes", true),
        ] {
            assert_eq!(requires_network(name), Some(network), "{name}");
        }
        assert_eq!(offline_suites().len(), 11);
    }

    #[test]
    fn every_offline_suite_has_a_runnable_command() {
        for s in offline_suites() {
            let cmd = cargo_filter(s.name);
            assert!(cmd.is_some(), "suite sans commande : {}", s.name);
        }
        assert!(cargo_filter("live-openrouter").is_none());
    }

    #[test]
    fn results_render_readably() {
        let r = SuiteResult {
            suite: "hitl".into(),
            passed: 16,
            failed: 0,
            skipped: 0,
            duration_ms: 270,
            failures: vec![],
        };
        assert!(r.is_green());
        assert!(r.render().starts_with("✅ hitl"));

        let r = SuiteResult {
            suite: "security".into(),
            passed: 4,
            failed: 1,
            skipped: 0,
            duration_ms: 90,
            failures: vec!["ssrf_is_blocked".into()],
        };
        assert!(!r.is_green());
        assert!(r.render().contains("ssrf_is_blocked"));
    }

    #[test]
    fn unknown_suites_are_reported() {
        assert!(find("inventee").is_none());
        assert!(requires_network("inventee").is_none());
    }
}
