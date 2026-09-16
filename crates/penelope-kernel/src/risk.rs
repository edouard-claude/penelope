//! Classes de risque et politique par défaut (§8.10, §11).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// `readOnlyHint` et non `openWorldHint`.
    Read,
    Write,
    Destructive,
    /// `openWorldHint` : atteint des systèmes hors du périmètre connu.
    External,
    /// Aucune annotation : traité comme `write`.
    Unknown,
}

impl RiskClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskClass::Read => "read",
            RiskClass::Write => "write",
            RiskClass::Destructive => "destructive",
            RiskClass::External => "external",
            RiskClass::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<RiskClass> {
        Some(match s {
            "read" => RiskClass::Read,
            "write" => RiskClass::Write,
            "destructive" => RiskClass::Destructive,
            "external" => RiskClass::External,
            "unknown" => RiskClass::Unknown,
            _ => return None,
        })
    }

    /// Couleur d'affichage des cartes Telegram (§14.5).
    pub fn colour(&self) -> &'static str {
        match self {
            RiskClass::Read => "#22c55e",
            RiskClass::Write => "#3b82f6",
            RiskClass::Destructive => "#ef4444",
            RiskClass::External => "#f59e0b",
            RiskClass::Unknown => "#a855f7",
        }
    }

    pub fn label_fr(&self) -> &'static str {
        match self {
            RiskClass::Read => "lecture",
            RiskClass::Write => "écriture",
            RiskClass::Destructive => "destructif",
            RiskClass::External => "externe",
            RiskClass::Unknown => "non annoté",
        }
    }
}

/// Déduit la classe de risque des annotations d'outil MCP.
///
/// **Les annotations d'un serveur sont des indices, pas des garanties** (§8.10) : la
/// surcharge de configuration prévaut toujours sur ce calcul.
pub fn classify_annotations(ann: &Value) -> RiskClass {
    let get = |k: &str| ann.get(k).and_then(|v| v.as_bool());
    let read_only = get("readOnlyHint");
    let destructive = get("destructiveHint");
    let open_world = get("openWorldHint");

    match (read_only, destructive, open_world) {
        (_, Some(true), _) => RiskClass::Destructive,
        (Some(true), _, Some(true)) => RiskClass::External,
        (Some(true), _, _) => RiskClass::Read,
        (Some(false), _, Some(true)) => RiskClass::External,
        (Some(false), _, _) => RiskClass::Write,
        (None, _, Some(true)) => RiskClass::External,
        (None, None, None) => RiskClass::Unknown,
        _ => RiskClass::Unknown,
    }
}

/// Décision de politique pour un appel (§8.10, §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Exécution immédiate, sans demander.
    Auto,
    /// Demande d'approbation simple.
    Ask,
    /// Demande d'approbation avec seconde confirmation (§14.5 `destructive_confirm`).
    AskTwice,
    /// Refus systématique.
    Deny,
}

impl PolicyDecision {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyDecision::Auto => "auto",
            PolicyDecision::Ask => "ask",
            PolicyDecision::AskTwice => "ask_twice",
            PolicyDecision::Deny => "deny",
        }
    }
    pub fn parse(s: &str) -> Option<PolicyDecision> {
        Some(match s {
            "auto" => PolicyDecision::Auto,
            "ask" => PolicyDecision::Ask,
            "ask_twice" => PolicyDecision::AskTwice,
            "deny" => PolicyDecision::Deny,
            _ => return None,
        })
    }
    pub fn needs_approval(&self) -> bool {
        matches!(self, PolicyDecision::Ask | PolicyDecision::AskTwice)
    }
}

/// Fenêtre d'autorisation choisie par le propriétaire (§8.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyWindow {
    Once,
    Run,
    Session,
    Always,
}

impl PolicyWindow {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolicyWindow::Once => "once",
            PolicyWindow::Run => "run",
            PolicyWindow::Session => "session",
            PolicyWindow::Always => "always",
        }
    }
    pub fn parse(s: &str) -> Option<PolicyWindow> {
        Some(match s {
            "once" => PolicyWindow::Once,
            "run" => PolicyWindow::Run,
            "session" => PolicyWindow::Session,
            "always" => PolicyWindow::Always,
            _ => return None,
        })
    }
    /// Une fenêtre `always` crée une règle visible et révocable dans `/policies`.
    pub fn creates_rule(&self) -> bool {
        matches!(self, PolicyWindow::Always)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_only_is_read() {
        assert_eq!(
            classify_annotations(&json!({"readOnlyHint": true})),
            RiskClass::Read
        );
    }

    #[test]
    fn read_only_but_open_world_is_external() {
        assert_eq!(
            classify_annotations(&json!({"readOnlyHint": true, "openWorldHint": true})),
            RiskClass::External
        );
    }

    #[test]
    fn destructive_wins() {
        assert_eq!(
            classify_annotations(&json!({"readOnlyHint": true, "destructiveHint": true})),
            RiskClass::Destructive
        );
    }

    #[test]
    fn no_annotation_is_unknown() {
        assert_eq!(classify_annotations(&json!({})), RiskClass::Unknown);
    }

    #[test]
    fn explicit_write_is_write() {
        assert_eq!(
            classify_annotations(&json!({"readOnlyHint": false})),
            RiskClass::Write
        );
    }

    #[test]
    fn decisions_roundtrip() {
        for d in [
            PolicyDecision::Auto,
            PolicyDecision::Ask,
            PolicyDecision::AskTwice,
            PolicyDecision::Deny,
        ] {
            assert_eq!(PolicyDecision::parse(d.as_str()), Some(d));
        }
        assert!(PolicyDecision::AskTwice.needs_approval());
        assert!(!PolicyDecision::Auto.needs_approval());
    }
}
