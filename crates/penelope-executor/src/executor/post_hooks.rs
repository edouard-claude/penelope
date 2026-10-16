//! Hooks `post_tool` du propriétaire (#383) : après un outil réussi, les hooks de
//! `hooks.d/` qui y correspondent tournent, sans shell, sous leur bac à sable ; leur
//! résultat est joint à celui de l'outil (`hooks`) et à son texte, encadré comme donnée.
//!
//! Codes de sortie : 0 = ok (stdout = note facultative), 2 = verdict JSON
//! `{"verdict":"continue|block","reason":…,"note":…}`, tout autre code ou le délai =
//! échec, tranché par `on_failure`. Un `block` fait de l'appel une erreur pour le modèle.
//! Bornes : 8 hooks par événement, 2 minutes cumulées par appel d'outil.

use super::*;
use penelope_tools::hooks::{Deliver, Event, Hook, OnFailure, Vars};

/// Hooks lancés au plus par événement d'un appel.
const MAX_PER_EVENT: usize = 8;
/// Temps cumulé des hooks d'un appel.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(120);
/// Résultat d'outil passé au hook sur son entrée, en caractères.
const RESULT_CHARS: usize = 8_000;

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
        let s = &self.services;
        let t0 = std::time::Instant::now();
        let fail = |why: String| (hook.on_failure, why);
        let result: Result<(i32, String), (OnFailure, String)> = async {
            // Une commande prise dans un workspace pourrait être écrite par l'agent : elle
            // est résolue comme au lancement (PATH, chemin réel) avant d'être jugée.
            if let Some(why) = command_in_workspace(&hook.command, &self.workspaces()) {
                return Err(fail(why));
            }
            let vars = Vars {
                file: file.map(String::from),
                workspace: workspace.map(|w| w.to_string_lossy().to_string()),
                session: Some(self.env.session_id.clone()),
                tool: Some(tool.to_string()),
                data: Some(s.platform.dirs.data().to_string_lossy().to_string()),
            };
            let rendered = hook.render(&vars).map_err(fail)?;
            let cwd = rendered
                .cwd
                .as_deref()
                .map(PathBuf::from)
                .or_else(|| workspace.map(Path::to_path_buf))
                .unwrap_or_else(|| s.platform.dirs.data());
            let mut env = Vec::new();
            for (k, v) in &hook.env {
                let value = s
                    .platform
                    .secrets
                    .expand(v)
                    .map_err(|e| fail(e.to_string()))?;
                penelope_observe::register_secret(&value);
                env.push((k.clone(), value));
            }
            let secrets: Vec<String> = env.iter().map(|(_, v)| v.clone()).collect();
            let profile = penelope_tools::shell::profile_with_denied_reads(
                hook.sandbox
                    .effective(&s.config.config().sandbox.default_profile),
                &cwd,
                false,
                &denied_reads(s),
            );
            let text: String = outcome.text.chars().take(RESULT_CHARS).collect();
            let input = json!({
                "event": "post_tool",
                "session_id": self.env.session_id,
                "run_id": self.env.run_id,
                "cwd": cwd,
                "tool": {"name": tool, "args": penelope_tools::args::without_intention(args)},
                "result": {"ok": true, "output": text},
            });
            let out = penelope_tools::shell::exec_argv_with_input(
                &s.platform.processes,
                &rendered.program,
                &rendered.args,
                penelope_tools::shell::ExecOptions {
                    profile: Some(&profile),
                    cwd: Some(&cwd),
                    timeout: hook.timeout.min(left),
                    max_output_bytes: penelope_tools::hooks::MAX_OUTPUT_BYTES,
                    env,
                    ..Default::default()
                },
                Some(input.to_string().into_bytes()),
            )
            .await
            .map_err(|e| fail(e.to_string()))?;
            // Un secret passé par `env` ne ressort ni au modèle ni dans une carte.
            let mut text = format!("{}{}", out.stdout, out.stderr);
            for secret in secrets.iter().filter(|v| v.len() >= 4) {
                text = text.replace(secret.as_str(), "‹secret›");
            }
            Ok((out.exit_code, penelope_observe::redact::redact(&text)))
        }
        .await;
        let ms = t0.elapsed().as_millis() as u64;
        let (verdict, exit_code, note) = match result {
            Ok((code, out)) => {
                let (verdict, note) =
                    penelope_tools::hooks::verdict_of(code, &out, hook.on_failure);
                (verdict, Some(code), note)
            }
            Err((OnFailure::Block, why)) => ("block", None, Some(why)),
            Err((OnFailure::Continue, why)) => ("échec", None, Some(why)),
        };
        self.record_hook(hook, verdict, exit_code, ms).await;
        Ran {
            name: hook.name.clone(),
            verdict,
            exit_code,
            ms,
            note,
            deliver: hook.deliver,
        }
    }

    /// Chaque exécution est un événement du journal, jamais un effet rejoué.
    async fn record_hook(&self, hook: &Hook, verdict: &str, exit_code: Option<i32>, ms: u64) {
        let mut draft = penelope_kernel::event::EventDraft::new(
            "hook.ran",
            json!({"name": hook.name, "event": "post_tool", "verdict": verdict,
                "exit_code": exit_code, "ms": ms}),
        )
        .session(&self.env.session_id);
        if let Some(run) = &self.env.run_id {
            draft = draft.run(run);
        }
        if let Err(e) = self.services.events.append(draft).await {
            tracing::warn!(error = %e, hook = %hook.name, "exécution de hook non journalisée");
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
