//! Hooks `post_tool` du propriétaire (#383) : après un outil réussi, les hooks de
//! `hooks.d/` qui y correspondent tournent, sans shell, sous leur bac à sable ; leur
//! résultat est joint à celui de l'outil (`hooks`) et à son texte, encadré comme donnée.
//!
//! Codes de sortie : 0 = ok (stdout = note facultative), 2 = verdict JSON
//! `{"verdict":"continue|block","reason":…,"note":…}`, tout autre code ou le délai =
//! échec, tranché par `on_failure`. Un `block` fait de l'appel une erreur pour le modèle.
//! Bornes : 8 hooks par événement, 2 minutes cumulées par appel d'outil.

use super::*;
use penelope_tools::hooks::{Deliver, Event, Hook};

/// Hooks lancés au plus par événement d'un appel.
const MAX_PER_EVENT: usize = 8;
/// Temps cumulé des hooks d'un appel.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Ce qu'un hook a rendu.
struct Ran {
    name: String,
    verdict: &'static str,
    exit_code: Option<i32>,
    ms: u64,
    note: Option<String>,
    deliver: Deliver,
}

impl NativeToolExecutor {
    /// Lance les hooks `post_tool` de cet appel ; modifie le résultat si l'un d'eux parle.
    pub(super) async fn post_tool_hooks(
        &self,
        tool: &str,
        args: &Value,
        outcome: &mut ToolOutcome,
    ) {
        let loaded = self.services.hooks.current();
        if loaded.hooks.is_empty() || outcome.is_error {
            return;
        }
        // Le fichier visé, relatif à son workspace, pour `match.path` et `{file}`.
        let (workspace, file) = self.file_of(args);
        let wanted: Vec<&Hook> = loaded
            .hooks
            .iter()
            .filter(|h| h.applies(Event::PostTool, Some(tool), file.as_deref()))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let started = std::time::Instant::now();
        let mut ran = Vec::new();
        let mut skipped = Vec::new();
        for hook in wanted {
            if ran.len() >= MAX_PER_EVENT || started.elapsed() >= BUDGET {
                skipped.push(hook.name.clone());
                continue;
            }
            let left = BUDGET.saturating_sub(started.elapsed());
            ran.push(
                self.run_hook(
                    hook,
                    tool,
                    args,
                    outcome,
                    workspace.as_deref(),
                    file.as_deref(),
                    left,
                )
                .await,
            );
        }
        self.attach(outcome, &ran, &skipped);
    }

    /// Le workspace et le chemin relatif du fichier que l'appel vise (`path`).
    fn file_of(&self, args: &Value) -> (Option<PathBuf>, Option<String>) {
        let Some(path) = args
            .get("path")
            .and_then(Value::as_str)
            .and_then(|p| self.resolve_path(p).ok())
        else {
            return (self.workspaces().into_iter().next(), None);
        };
        for ws in self.workspaces() {
            if let Ok(rel) = path.strip_prefix(&ws) {
                return (Some(ws), Some(rel.to_string_lossy().to_string()));
            }
        }
        (None, None)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_hook(
        &self,
        hook: &Hook,
        tool: &str,
        args: &Value,
        outcome: &ToolOutcome,
        workspace: Option<&Path>,
        file: Option<&str>,
        left: std::time::Duration,
    ) -> Ran {
        use penelope_tools::hooks::Decision;
        let t0 = std::time::Instant::now();
        let workspaces = self.workspaces();
        let launch = super::hook_exec::Launch {
            event: Event::PostTool,
            session_id: &self.env.session_id,
            run_id: self.env.run_id.as_deref(),
            tool: Some(tool),
            args: Some(args),
            result: Some(&outcome.text),
            answer: None,
            prompt: None,
            workspaces: &workspaces,
            workspace,
            file,
            left,
        };
        let launched = super::hook_exec::launch(&self.services, hook, &launch).await;
        let ms = t0.elapsed().as_millis() as u64;
        let (decision, exit_code) = super::hook_exec::decision_of(Event::PostTool, hook, &launched);
        let (verdict, note) = match decision {
            Decision::Continue(n) => ("continue", n),
            Decision::Block(n) => ("block", n),
            Decision::Failed(n) => ("échec", n),
            // Verdicts d'un autre événement : un échec de ce hook.
            Decision::Deny(r) | Decision::Ask(r) => ("échec", Some(r)),
            Decision::Args(_) => ("échec", Some("`args` n'existe qu'avant l'outil".into())),
        };
        super::hook_exec::record(&self.services, hook, &launch, verdict, exit_code, ms).await;
        Ran {
            name: hook.name.clone(),
            verdict,
            exit_code,
            ms,
            note,
            deliver: hook.deliver,
        }
    }

    /// Joint ce que les hooks ont dit au résultat et à son texte ; un `block` en fait une
    /// erreur pour le modèle.
    fn attach(&self, outcome: &mut ToolOutcome, ran: &[Ran], skipped: &[String]) {
        let mut list = Vec::new();
        let mut text = String::new();
        let mut blocked = None;
        for r in ran {
            if r.verdict == "block" && blocked.is_none() {
                blocked = Some(r.name.clone());
            }
            if r.deliver == Deliver::Silent && r.verdict != "block" {
                continue;
            }
            if r.deliver == Deliver::Card
                && let Some(m) = &self.messenger
            {
                let m = m.clone();
                let origin = self.env.origin.clone();
                let card = format!(
                    "⚙ Hook `{}` : {}{}",
                    r.name,
                    r.verdict,
                    r.note
                        .as_deref()
                        .map(|n| format!("\n{n}"))
                        .unwrap_or_default()
                );
                tokio::spawn(async move {
                    let _ = m.send_text(&origin, &card).await;
                });
            }
            list.push(
                json!({"name": r.name, "verdict": r.verdict, "exitCode": r.exit_code,
                "ms": r.ms, "note": r.note}),
            );
            let head = format!("[hook {} : {}]", r.name, r.verdict);
            text.push_str("\n\n");
            match &r.note {
                Some(n) => text.push_str(&penelope_observe::injection::wrap_untrusted(&head, n)),
                None => text.push_str(&head),
            }
        }
        if !skipped.is_empty() {
            let why = format!(
                "hooks sautés (au plus {MAX_PER_EVENT} par événement, {} s cumulées) : {}",
                BUDGET.as_secs(),
                skipped.join(", ")
            );
            text.push_str(&format!("\n\n[{why}]"));
            list.push(json!({"skipped": skipped, "why": why}));
        }
        if list.is_empty() && blocked.is_none() {
            return;
        }
        if let Value::Object(o) = &mut outcome.value {
            o.insert("hooks".into(), Value::Array(list));
        }
        if let Some(name) = blocked {
            outcome.is_error = true;
            text.push_str(&format!(
                "\n\n[Le hook `{name}` bloque la suite : corrige ce qu'il signale avant de \
                 continuer.]"
            ));
        }
        outcome.text.push_str(&text);
    }
}

/// Le motif du refus si `command` ne peut pas être lancée sans risque : un chemin relatif
/// (qui dépendrait du dossier de lancement), une commande introuvable, ou une commande
/// dont le chemin réel (PATH résolu, liens suivis) se trouve dans un workspace.
pub(crate) fn command_in_workspace(command: &str, workspaces: &[PathBuf]) -> Option<String> {
    let path = Path::new(command);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else if command.contains('/') {
        return Some(format!(
            "commande `{command}` relative à un dossier : donner un nom du PATH ou un chemin \
             absolu"
        ));
    } else {
        match penelope_platform::which(command) {
            Some(p) => p,
            None => return Some(format!("commande `{command}` introuvable dans le PATH")),
        }
    };
    let Ok(real) = std::fs::canonicalize(&resolved) else {
        return Some(format!("commande `{command}` introuvable"));
    };
    workspaces
        .iter()
        .map(|w| std::fs::canonicalize(w).unwrap_or_else(|_| w.clone()))
        .any(|w| real.starts_with(&w))
        .then(|| format!("commande `{command}` située dans un workspace : refusée"))
}
