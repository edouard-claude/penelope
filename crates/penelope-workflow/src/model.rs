//! Schéma des workflows (§12.2, §12.3, §12.4).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DONE: &str = "$done";
pub const BLOCKED: &str = "$blocked";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub metadata: Metadata,
    #[serde(rename = "entryStep")]
    pub entry_step: String,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default = "default_start_condition", rename = "startCondition")]
    pub start_condition: Value,
    pub steps: Vec<Step>,
}

fn default_start_condition() -> Value {
    serde_json::json!({"type":"always"})
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub color: String,
    pub parameters: Vec<Parameter>,
    /// `macos`, `linux`, `windows` (§2.10).
    pub platforms: Vec<String>,
    /// Configuration propre au workflow (`settings.tracker` du §12.10, par exemple).
    pub config: Value,
}

impl Default for Metadata {
    fn default() -> Self {
        Metadata {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            version: "1.0.0".into(),
            color: "#3b82f6".into(),
            parameters: Vec::new(),
            platforms: Vec::new(),
            config: Value::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Parameter {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub required: bool,
    pub default: Option<Value>,
    pub description: String,
}

impl Default for Parameter {
    fn default() -> Self {
        Parameter {
            id: String::new(),
            label: String::new(),
            kind: "string".into(),
            required: false,
            default: None,
            description: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    #[serde(rename = "maxIterations")]
    pub max_iterations: u32,
    pub budget: Budget,
    pub concurrency: Concurrency,
    /// `ephemeral` ou `persistent:<nom>`.
    pub workspace: String,
    /// Outil du tracker, pour les workflows livrés (§12.10).
    pub tracker: String,
    pub deploy_workflow: String,
    /// Formulaires des étapes `user` (`input: "form:<id>"`) : JSON Schema d'objet, un champ
    /// par écran sur Telegram.
    pub forms: std::collections::BTreeMap<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_iterations: 40,
            budget: Budget::default(),
            concurrency: Concurrency::default(),
            workspace: "ephemeral".into(),
            tracker: String::new(),
            deploy_workflow: String::new(),
            forms: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    #[serde(rename = "maxUsd")]
    pub max_usd: f64,
    #[serde(rename = "maxTokens")]
    pub max_tokens: u64,
    #[serde(rename = "maxWallMs")]
    pub max_wall_ms: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_usd: 5.0,
            max_tokens: 2_000_000,
            max_wall_ms: 7_200_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Concurrency {
    #[serde(rename = "maxConcurrent")]
    pub max_concurrent: u32,
    /// `parallel` | `hold` | `coalesce` | `drop`.
    pub admission: String,
}

impl Default for Concurrency {
    fn default() -> Self {
        Concurrency {
            max_concurrent: 1,
            admission: "hold".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Plan,
    Build,
    Verification,
    Waiting,
    Deploy,
    Done,
}

impl Phase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Build => "build",
            Phase::Verification => "verification",
            Phase::Waiting => "waiting",
            Phase::Deploy => "deploy",
            Phase::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub phase: Phase,
    #[serde(rename = "subGroup")]
    pub sub_group: String,
    pub transitions: Vec<Transition>,
    pub budget: Option<Budget>,
    pub retry: Option<Retry>,
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
    /// Alias de modèle.
    pub model: String,

    // --- agent / sub_agent ---
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "subAgentType")]
    pub sub_agent_type: String,
    pub prompt: String,
    #[serde(rename = "nudgePrompt")]
    pub nudge_prompt: String,
    pub tools: Vec<String>,
    #[serde(rename = "outputSchema")]
    pub output_schema: Option<Value>,

    // --- shell ---
    /// Chaîne, ou table par OS (`unix`, `macos`, `linux`, `windows`), §2.10.
    pub command: Value,
    pub cwd: String,
    #[serde(rename = "successExitCodes")]
    pub success_exit_codes: Vec<i32>,

    // --- tool ---
    pub tool: String,
    pub args: Value,

    // --- user ---
    pub template: String,
    pub choices: Vec<String>,
    /// `none` | `text` | `form:<schema>`.
    pub input: String,

    // --- parallel ---
    pub children: Vec<Step>,
    #[serde(rename = "maxConcurrency")]
    pub max_concurrency: Option<u32>,

    // --- workflow ---
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    pub params: Value,

    // --- wait ---
    pub on: Value,

    // --- verify ---
    #[serde(rename = "criteriaKey")]
    pub criteria_key: String,
    pub verifier: String,
    pub checks: Vec<Value>,
}

impl Default for Step {
    fn default() -> Self {
        Step {
            id: String::new(),
            name: String::new(),
            kind: "agent".into(),
            phase: Phase::Build,
            sub_group: String::new(),
            transitions: Vec::new(),
            budget: None,
            retry: None,
            timeout_ms: None,
            model: String::new(),
            agent_id: String::new(),
            sub_agent_type: String::new(),
            prompt: String::new(),
            nudge_prompt: String::new(),
            tools: Vec::new(),
            output_schema: None,
            command: Value::Null,
            cwd: String::new(),
            success_exit_codes: vec![0],
            tool: String::new(),
            args: Value::Null,
            template: String::new(),
            choices: Vec::new(),
            input: "none".into(),
            children: Vec::new(),
            max_concurrency: None,
            workflow_id: String::new(),
            params: Value::Null,
            on: Value::Null,
            criteria_key: "criteria".into(),
            verifier: String::new(),
            checks: Vec::new(),
        }
    }
}

impl Step {
    /// Commande effective pour l'OS courant (§2.10).
    pub fn command_for_os(&self, os: &str) -> Option<String> {
        match &self.command {
            Value::String(s) => Some(s.clone()),
            Value::Object(m) => {
                // La clé la plus spécifique gagne.
                for key in [os, unix_family(os)] {
                    if key.is_empty() {
                        continue;
                    }
                    if let Some(Value::String(s)) = m.get(key) {
                        return Some(s.clone());
                    }
                }
                None
            }
            _ => None,
        }
    }

    pub fn is_terminal_kind(&self) -> bool {
        matches!(self.kind.as_str(), "user" | "wait")
    }
}

fn unix_family(os: &str) -> &'static str {
    match os {
        "macos" | "linux" | "freebsd" => "unix",
        _ => "",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retry {
    pub max: u32,
    #[serde(rename = "backoffMs")]
    pub backoff_ms: u64,
}

impl Default for Retry {
    fn default() -> Self {
        Retry {
            max: 0,
            backoff_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    /// Étape cible, `$done` ou `$blocked`.
    pub goto: String,
    #[serde(default = "default_condition")]
    pub condition: Value,
    /// Marque d'échappement d'un sous-groupe (§12.7, sémantique OpenFox).
    #[serde(default)]
    pub tag: String,
}

fn default_condition() -> Value {
    serde_json::json!({"type":"always"})
}

impl Transition {
    pub fn always(goto: &str) -> Transition {
        Transition {
            goto: goto.to_string(),
            condition: default_condition(),
            tag: String::new(),
        }
    }
    pub fn on_result(goto: &str, result: &str) -> Transition {
        Transition {
            goto: goto.to_string(),
            condition: serde_json::json!({"type":"step_result","result":result}),
            tag: String::new(),
        }
    }
    pub fn is_always(&self) -> bool {
        self.condition.get("type").and_then(|t| t.as_str()) == Some("always")
    }
}

/// Résultat d'une étape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepResult {
    Success,
    Failure,
    Completed,
    Error,
    Passed,
    Failed,
    Partial,
    Blocked,
    Fired,
    Timeout,
    /// Choix d'un utilisateur : le libellé est le résultat.
    Choice(String),
}

impl StepResult {
    pub fn as_str(&self) -> &str {
        match self {
            StepResult::Success => "success",
            StepResult::Failure => "failure",
            StepResult::Completed => "completed",
            StepResult::Error => "error",
            StepResult::Passed => "passed",
            StepResult::Failed => "failed",
            StepResult::Partial => "partial",
            StepResult::Blocked => "blocked",
            StepResult::Fired => "fired",
            StepResult::Timeout => "timeout",
            StepResult::Choice(s) => s,
        }
    }
    pub fn parse(s: &str) -> StepResult {
        match s {
            "success" => StepResult::Success,
            "failure" => StepResult::Failure,
            "completed" => StepResult::Completed,
            "error" => StepResult::Error,
            "passed" => StepResult::Passed,
            "failed" => StepResult::Failed,
            "partial" => StepResult::Partial,
            "blocked" => StepResult::Blocked,
            "fired" => StepResult::Fired,
            "timeout" => StepResult::Timeout,
            other => StepResult::Choice(other.to_string()),
        }
    }
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            StepResult::Success | StepResult::Completed | StepResult::Passed | StepResult::Fired
        )
    }
}

/// Types d'étapes reconnus.
pub const STEP_KINDS: &[&str] = &[
    "agent",
    "sub_agent",
    "shell",
    "tool",
    "user",
    "parallel",
    "workflow",
    "wait",
    "verify",
];

/// Types autorisés comme enfants d'un `parallel` (§12.6).
pub const PARALLEL_CHILD_KINDS: &[&str] = &["sub_agent", "shell", "tool"];

impl Workflow {
    pub fn from_json(raw: &str) -> Result<Workflow, String> {
        serde_json::from_str(raw).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }

    pub fn step_ids(&self) -> Vec<String> {
        self.steps.iter().map(|s| s.id.clone()).collect()
    }

    /// Le workflow peut-il tourner sur cet OS ? (§2.10)
    pub fn runs_on(&self, os: &str) -> bool {
        self.metadata.platforms.is_empty() || self.metadata.platforms.iter().any(|p| p == os)
    }

    /// Graphe texte, pour l'aperçu Telegram (§12.8, template `workflow_preview`).
    pub fn render_graph(&self) -> String {
        let mut s = String::new();
        for st in &self.steps {
            s.push_str(&format!(
                "{} [{}] ({})\n",
                st.id,
                st.kind,
                st.phase.as_str()
            ));
            for t in &st.transitions {
                let cond = t
                    .condition
                    .get("type")
                    .and_then(|x| x.as_str())
                    .unwrap_or("always");
                s.push_str(&format!("    --{cond}--> {}\n", t.goto));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal() -> Workflow {
        Workflow {
            metadata: Metadata {
                id: "demo".into(),
                name: "Démonstration".into(),
                ..Default::default()
            },
            entry_step: "un".into(),
            settings: Settings::default(),
            start_condition: json!({"type":"always"}),
            steps: vec![Step {
                id: "un".into(),
                kind: "shell".into(),
                command: json!("echo bonjour"),
                transitions: vec![Transition::always(DONE)],
                ..Default::default()
            }],
        }
    }

    #[test]
    fn json_roundtrip() {
        let w = minimal();
        let back = Workflow::from_json(&w.to_json()).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = r#"{"metadata":{"id":"x"},"entryStep":"a","steps":[],"inconnu":1}"#;
        assert!(Workflow::from_json(raw).unwrap_err().contains("inconnu"));
    }

    #[test]
    fn os_specific_commands() {
        let mut s = Step {
            command: json!({"unix":"go test ./...","windows":"go test ./..."}),
            ..Default::default()
        };
        assert_eq!(s.command_for_os("macos").as_deref(), Some("go test ./..."));
        assert_eq!(
            s.command_for_os("windows").as_deref(),
            Some("go test ./...")
        );

        // La clé la plus spécifique gagne.
        s.command = json!({"unix":"générique","macos":"spécifique"});
        assert_eq!(s.command_for_os("macos").as_deref(), Some("spécifique"));
        assert_eq!(s.command_for_os("linux").as_deref(), Some("générique"));

        // Absence de commande pour l'OS courant.
        s.command = json!({"windows":"x"});
        assert!(s.command_for_os("macos").is_none());

        // Chaîne simple.
        s.command = json!("make build");
        assert_eq!(s.command_for_os("linux").as_deref(), Some("make build"));
    }

    #[test]
    fn platform_gating() {
        let mut w = minimal();
        assert!(w.runs_on("macos"), "sans `platforms`, tout OS est permis");
        w.metadata.platforms = vec!["linux".into()];
        assert!(!w.runs_on("macos"));
        assert!(w.runs_on("linux"));
    }

    #[test]
    fn step_results_roundtrip() {
        for r in [
            StepResult::Success,
            StepResult::Failure,
            StepResult::Completed,
            StepResult::Passed,
            StepResult::Partial,
            StepResult::Fired,
            StepResult::Timeout,
        ] {
            assert_eq!(StepResult::parse(r.as_str()), r);
        }
        assert_eq!(
            StepResult::parse("Déployer"),
            StepResult::Choice("Déployer".into())
        );
        assert!(StepResult::Success.is_ok());
        assert!(!StepResult::Failure.is_ok());
    }

    #[test]
    fn defaults_follow_the_prd() {
        let s = Settings::default();
        assert_eq!(s.max_iterations, 40);
        assert_eq!(s.budget.max_usd, 5.0);
        assert_eq!(s.budget.max_tokens, 2_000_000);
        assert_eq!(s.budget.max_wall_ms, 7_200_000);
        assert_eq!(s.concurrency.max_concurrent, 1);
        assert_eq!(s.concurrency.admission, "hold");
        assert_eq!(s.workspace, "ephemeral");
    }

    #[test]
    fn graph_rendering_lists_steps_and_transitions() {
        let g = minimal().render_graph();
        assert!(g.contains("un [shell] (build)"));
        assert!(g.contains("--always--> $done"));
    }

    #[test]
    fn transitions_helpers() {
        assert!(Transition::always("x").is_always());
        assert!(!Transition::on_result("x", "success").is_always());
    }
}
