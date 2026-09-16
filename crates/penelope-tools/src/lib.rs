//! `penelope-tools` : outils natifs (§11).
//!
//! Chaque appel d'outil non `readOnly` passe par le ledger d'effets **avant** exécution
//! (§4.2) : c'est le daemon qui orchestre, ce crate fournit les implémentations et les
//! gardes (workspace, commandes interdites, SSRF, détection de boucles).

#![forbid(unsafe_code)]

pub mod error;
pub mod fs;
pub mod git;
pub mod http;
pub mod loops;
pub mod shell;
pub mod spec;

pub use error::{ToolError, ToolResult};
pub use loops::{LoopDetector, LoopVerdict};
pub use spec::{ToolSpec, all as all_tools, always_exposed, get as tool_spec};

use penelope_kernel::risk::RiskClass;
use serde_json::Value;

/// Contexte d'exécution d'un appel d'outil.
pub struct ToolContext {
    pub session_id: String,
    pub run_id: Option<String>,
    pub workspaces: Vec<std::path::PathBuf>,
    pub sandbox_profile: String,
    pub http_allowlist: Vec<String>,
    pub block_private_ips: bool,
    pub max_output_bytes: usize,
    pub shell_timeout: std::time::Duration,
    pub in_workflow: bool,
}

impl ToolContext {
    pub fn workspace(&self) -> std::path::PathBuf {
        self.workspaces
            .first()
            .cloned()
            .unwrap_or_else(std::env::temp_dir)
    }
}

/// Résultat normalisé d'un appel d'outil.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub value: Value,
    /// Erreur d'exécution renvoyée au modèle plutôt qu'au harnais.
    pub is_error: bool,
    /// Texte prêt à insérer dans le transcript.
    pub text: String,
    /// Le résultat est volatil : candidat à la micro-compaction (§5.4 niveau 0).
    pub eager: bool,
}

impl ToolOutcome {
    pub fn ok(value: Value) -> Self {
        let text = render(&value);
        ToolOutcome {
            value,
            is_error: false,
            text,
            eager: false,
        }
    }
    pub fn eager(mut self) -> Self {
        self.eager = true;
        self
    }
    pub fn error(e: &ToolError) -> Self {
        ToolOutcome {
            value: serde_json::json!({"error": e.to_string()}),
            is_error: true,
            text: e.for_model(),
            eager: false,
        }
    }
}

/// Rendu lisible d'un résultat d'outil.
pub fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            // Cas fréquents : on privilégie le champ le plus parlant.
            for k in ["content", "stdout", "body", "text"] {
                if let Some(Value::String(s)) = o.get(k)
                    && !s.is_empty()
                {
                    return s.clone();
                }
            }
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        }
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Classe de risque effective : surcharge de configuration d'abord, spécification ensuite.
pub fn effective_risk(
    tool: &str,
    override_map: &std::collections::BTreeMap<String, String>,
) -> RiskClass {
    if let Some(r) = override_map.get(tool).and_then(|s| RiskClass::parse(s)) {
        return r;
    }
    spec::get(tool)
        .map(|s| s.risk)
        .unwrap_or(RiskClass::Unknown)
}

/// Un outil est-il sur la liste blanche d'une étape de workflow ou d'une skill ?
pub fn is_allowed(tool: &str, allow: &[String]) -> bool {
    if allow.is_empty() {
        return true;
    }
    allow
        .iter()
        .any(|a| a == tool || (a.ends_with('*') && tool.starts_with(a.trim_end_matches('*'))))
}

/// Valide les arguments contre le schéma de l'outil.
pub fn validate_args(tool: &str, args: &Value) -> ToolResult<()> {
    let Some(s) = spec::get(tool) else {
        return Err(ToolError::Unknown(tool.to_string()));
    };
    penelope_kernel::schema::validate_ok(&s.schema, args).map_err(ToolError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_prefers_the_readable_field() {
        assert_eq!(render(&json!({"content":"bonjour","lines":3})), "bonjour");
        assert_eq!(render(&json!({"stdout":"sortie","exitCode":0})), "sortie");
        assert!(render(&json!({"a":1})).contains("\"a\": 1"));
        assert_eq!(render(&json!("texte brut")), "texte brut");
    }

    #[test]
    fn empty_readable_field_falls_back_to_json() {
        let r = render(&json!({"stdout":"","exitCode":1}));
        assert!(r.contains("exitCode"));
    }

    #[test]
    fn risk_overrides_win_over_annotations() {
        let mut o = std::collections::BTreeMap::new();
        assert_eq!(effective_risk("fs_read", &o), RiskClass::Read);
        o.insert("fs_read".to_string(), "destructive".to_string());
        assert_eq!(effective_risk("fs_read", &o), RiskClass::Destructive);
        assert_eq!(effective_risk("inconnu", &o), RiskClass::Unknown);
    }

    #[test]
    fn allowlists_support_prefixes() {
        assert!(is_allowed("fs_read", &[]));
        assert!(is_allowed("fs_read", &["fs_read".to_string()]));
        assert!(is_allowed("fs_read", &["fs_*".to_string()]));
        assert!(!is_allowed("shell_exec", &["fs_*".to_string()]));
    }

    #[test]
    fn arguments_are_validated_against_the_spec() {
        validate_args("fs_read", &json!({"path":"a.rs"})).unwrap();
        assert!(validate_args("fs_read", &json!({})).is_err());
        assert!(validate_args("outil_inexistant", &json!({})).is_err());
    }

    #[test]
    fn outcome_carries_error_text_for_the_model() {
        let o = ToolOutcome::error(&ToolError::Denied("hors workspace".into()));
        assert!(o.is_error);
        assert!(o.text.contains("Ne réessaie pas"));
    }
}
