//! Le jeu de décisions du juge (issue #233) : un échantillon par ligne `shell_exec` vue
//! par la politique, carte ou pas, quand `observability.dataset.approvals` est vrai.
//!
//! La ligne y est **telle que le juge la reçoit** (`hostile_text` : secrets masqués,
//! commentaires retirés), par la même fonction : pas une seconde rédaction à maintenir.
//! L'échantillon s'écrit après la décision (la carte est déjà posée), et un échec
//! d'écriture ne change rien au tour (#179) : il est journalisé, c'est tout.

use super::policy::{Verdict, VerdictLayer};
use super::*;
use penelope_hitl::samples::{ApprovalSamples, SampleDraft};
use std::path::Path;

/// Ce que la politique et le juge ont vu d'un appel.
pub(crate) struct Seen<'a> {
    pub call_id: &'a str,
    pub workspace: Option<&'a Path>,
    pub info: &'a CallInfo,
    pub args: &'a Value,
    pub verdict: &'a Verdict,
    pub judge: Option<&'a Value>,
}

/// L'issue connue au moment de la décision.
pub(crate) enum Issue {
    /// Parti sans carte : la couche, ou l'automatisme du juge, qui l'a permis.
    Auto,
    /// Refusé par la politique.
    Denied,
    /// Carte posée : l'issue viendra de sa décision (`penelope_hitl::samples`).
    Card(String),
}

/// Nom stable de la couche qui a fixé la décision.
fn layer_name(layer: &VerdictLayer) -> &'static str {
    match layer {
        VerdictLayer::Rule { .. } => "regle",
        VerdictLayer::Default => "defaut",
        VerdictLayer::ServerDeclaration => "declaration_serveur",
        VerdictLayer::DeclaredAllow => "autorise_d_avance",
        VerdictLayer::SessionMode => "mode_session",
        VerdictLayer::SensitiveConfig => "reglage_sensible",
        VerdictLayer::Judge => "juge",
    }
}

/// Code de sortie et durée d'une exécution `shell_exec`, s'ils sont dans son résultat.
pub(crate) fn execution_of(value: &Value) -> (Option<i64>, Option<i64>) {
    (value["exitCode"].as_i64(), value["durationMs"].as_i64())
}

fn collecting(s: &AgentServices, tool: &str) -> bool {
    tool == "shell_exec" && s.config.config().observability.dataset.approvals
}

impl AgentLoop {
    /// Écrit l'échantillon d'un appel décidé.
    pub(crate) async fn sample_call(&self, spec: &TurnSpec, seen: &Seen<'_>, issue: Issue) {
        let s = &self.services;
        if !collecting(s, &seen.info.effective_name) {
            return;
        }
        let Some(command) = seen.args.get("command").and_then(|v| v.as_str()) else {
            return;
        };
        let workspaces: Vec<String> = seen
            .workspace
            .map(|w| w.to_string_lossy().into_owned())
            .into_iter()
            .collect();
        let rule = match &seen.verdict.layer {
            VerdictLayer::Rule { id } => Some(id.clone()),
            _ => None,
        };
        let (approval_id, outcome) = match issue {
            Issue::Auto => {
                // Sans carte malgré un `Ask` : c'est l'automatisme du juge qui l'a permis.
                let via = match seen.verdict.decision {
                    PolicyDecision::Auto => layer_name(&seen.verdict.layer).to_string(),
                    _ => seen
                        .judge
                        .and_then(|j| j["outcome"].as_str())
                        .unwrap_or("juge")
                        .to_string(),
                };
                (None, Some(("auto".to_string(), via)))
            }
            Issue::Denied => (None, Some(("denied".to_string(), "politique".to_string()))),
            Issue::Card(id) => (Some(id), None),
        };
        let draft = SampleDraft {
            session_id: spec.session_id.clone(),
            turn_id: spec.turn_id.clone(),
            call_id: seen.call_id.to_string(),
            command_sha: judge::command_sha(command),
            input: penelope_observe::redact_json(&json!({
                "command": penelope_app::judge::hostile_text(command),
                "cwd": judge::cwd_of(seen.args, seen.workspace),
                "workspaces": workspaces,
                "network": seen.args.get("network") == Some(&Value::Bool(true)),
            })),
            floors: json!({
                "policy": seen.verdict.decision.as_str(),
                "layer": layer_name(&seen.verdict.layer),
                "risk": seen.info.risk.as_str(),
                "rule": rule,
                "sans_motif": crate::always_creates_no_rule(&seen.info.effective_name, Some(seen.args)),
            }),
            judge: seen.judge.map(penelope_observe::redact_json),
            approval_id,
            outcome,
        };
        if let Err(e) = ApprovalSamples::new(s.store.clone(), s.clock.clone())
            .record(draft)
            .await
        {
            tracing::warn!(session = %spec.session_id, erreur = %e, "échantillon d'approbation non écrit");
        }
    }

    /// L'exécution d'un appel échantillonné.
    pub(crate) async fn sample_execution(
        &self,
        spec: &TurnSpec,
        call_id: &str,
        info: &CallInfo,
        outcome: &ToolOutcome,
    ) {
        let s = &self.services;
        if !collecting(s, &info.effective_name) {
            return;
        }
        let (exit, duration) = execution_of(&outcome.value);
        if let Err(e) = ApprovalSamples::new(s.store.clone(), s.clock.clone())
            .executed(&spec.session_id, call_id, exit, duration)
            .await
        {
            tracing::warn!(session = %spec.session_id, erreur = %e, "exécution non échantillonnée");
        }
    }
}
