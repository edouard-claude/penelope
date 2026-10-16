//! Le port `HookGate` (#384) : les hooks `pre_tool` et `turn_end` de `hooks.d/`, lancés
//! pour la boucle, qui ne voit pas l'exécuteur.

use super::*;
use penelope_app::hook_gate::{HookGate, PreToolGate};
use penelope_tools::hooks::{Decision, Event, Hook};

/// Hooks lancés au plus par événement d'un appel ou d'une réponse.
const MAX_PER_EVENT: usize = 8;
/// Temps cumulé des hooks d'un événement.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Les hooks du propriétaire, lancés au-dessus de la boucle.
pub struct ExecHookGate(pub Arc<Services>);

/// Ce que `session_start` et `prompt_submit` décident d'un tour (#385).
#[derive(Debug, Clone, PartialEq)]
pub enum BeforeTurn {
    /// Le tour part ; les notes vont au contexte volatil (T4), jamais au préfixe.
    Go(Vec<String>),
    /// Le message n'est pas envoyé au modèle ; le propriétaire reçoit le motif.
    Block { hook: String, reason: String },
}

impl ExecHookGate {
    /// Le chemin visé par un appel, résolu comme l'outil le résoudra (normalisé, `..`
    /// compris, dans un workspace), puis relatif à son workspace : `infra/../infra/x` et
    /// `./infra/x` sont vus comme `infra/x` par un motif `infra/**`.
    fn file_of(&self, args: &Value) -> (Option<PathBuf>, Option<String>) {
        let workspaces = default_workspaces(&self.0);
        let Some(raw) = args.get("path").and_then(Value::as_str) else {
            return (workspaces.into_iter().next(), None);
        };
        let Ok(resolved) = penelope_tools::fs::resolve(raw, &workspaces) else {
            // Hors de tout workspace : l'outil refusera ; aucun motif de chemin ne s'y
            // applique.
            return (None, None);
        };
        for ws in &workspaces {
            let real = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.clone());
            for root in [ws, &real] {
                if let Ok(rel) = resolved.strip_prefix(root) {
                    return (Some(ws.clone()), Some(rel.to_string_lossy().to_string()));
                }
            }
        }
        (None, None)
    }

    /// Avant un tour : `session_start` à la première fois d'une session, `prompt_submit`
    /// pour un message du propriétaire. Le texte du message n'est passé qu'aux hooks qui
    /// le demandent (`input = "text"`).
    pub async fn before_turn(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        text: &str,
        owner_message: bool,
    ) -> BeforeTurn {
        let loaded = self.0.hooks.current();
        if loaded.hooks.is_empty() {
            return BeforeTurn::Go(Vec::new());
        }
        let workspaces = default_workspaces(&self.0);
        let started = std::time::Instant::now();
        let mut notes = Vec::new();
        let key = format!("hooks.session_started.{session_id}");
        let first = self.0.kv_get(&key).await.ok().flatten().is_none();
        if first {
            let _ = self.0.kv_set(&key, "1").await;
        }
        let events: Vec<Event> = [
            first.then_some(Event::SessionStart),
            owner_message.then_some(Event::PromptSubmit),
        ]
        .into_iter()
        .flatten()
        .collect();
        for event in events {
            for hook in loaded
                .hooks
                .iter()
                .filter(|h| h.applies(event, None, None))
                .take(MAX_PER_EVENT)
            {
                if started.elapsed() >= BUDGET {
                    break;
                }
                let prompt = (hook.input == penelope_tools::hooks::Input::Text).then_some(text);
                let launch = hook_exec::Launch {
                    event,
                    session_id,
                    run_id,
                    tool: None,
                    args: None,
                    result: None,
                    answer: None,
                    prompt,
                    workspaces: &workspaces,
                    workspace: workspaces.first().map(PathBuf::as_path),
                    file: None,
                    left: BUDGET.saturating_sub(started.elapsed()),
                };
                match self.run(hook, &launch).await {
                    // Une session s'ouvre toujours : `block` n'y existe pas (#385).
                    Decision::Block(note) if event == Event::PromptSubmit => {
                        return BeforeTurn::Block {
                            hook: hook.name.clone(),
                            reason: note.unwrap_or_else(|| "sans motif".into()),
                        };
                    }
                    Decision::Continue(Some(note)) => {
                        notes.push(penelope_observe::injection::wrap_untrusted(
                            &format!("hook {}", hook.name),
                            &note,
                        ))
                    }
                    _ => {}
                }
            }
        }
        BeforeTurn::Go(notes)
    }

    async fn run(&self, hook: &Hook, launch: &hook_exec::Launch<'_>) -> Decision {
        let t0 = std::time::Instant::now();
        let launched = hook_exec::launch(&self.0, hook, launch).await;
        let (decision, code) = hook_exec::decision_of(launch.event, hook, &launched);
        let verdict = match &decision {
            Decision::Continue(_) => "continue",
            Decision::Block(_) => "block",
            Decision::Deny(_) => "deny",
            Decision::Ask(_) => "ask",
            Decision::Args(_) => "args",
            Decision::Failed(_) => "échec",
        };
        let ms = t0.elapsed().as_millis() as u64;
        hook_exec::record(&self.0, hook, launch, verdict, code, ms).await;
        decision
    }
}

#[async_trait::async_trait]
impl HookGate for ExecHookGate {
    async fn pre_tool(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        tool: &str,
        args: &Value,
    ) -> PreToolGate {
        let loaded = self.0.hooks.current();
        let (workspace, file) = self.file_of(args);
        let wanted: Vec<&Hook> = loaded
            .hooks
            .iter()
            .filter(|h| h.applies(Event::PreTool, Some(tool), file.as_deref()))
            .take(MAX_PER_EVENT)
            .collect();
        if wanted.is_empty() {
            return PreToolGate::Continue;
        }
        let workspaces = default_workspaces(&self.0);
        let started = std::time::Instant::now();
        let mut current = args.clone();
        let mut changed_by: Option<String> = None;
        let mut asked: Option<(String, String)> = None;
        for hook in wanted {
            if started.elapsed() >= BUDGET {
                break;
            }
            let launch = hook_exec::Launch {
                event: Event::PreTool,
                session_id,
                run_id,
                tool: Some(tool),
                args: Some(&current),
                result: None,
                answer: None,
                prompt: None,
                workspaces: &workspaces,
                workspace: workspace.as_deref(),
                file: file.as_deref(),
                left: BUDGET.saturating_sub(started.elapsed()),
            };
            match self.run(hook, &launch).await {
                Decision::Deny(reason) => {
                    return PreToolGate::Deny {
                        hook: hook.name.clone(),
                        reason,
                    };
                }
                Decision::Block(note) => {
                    return PreToolGate::Deny {
                        hook: hook.name.clone(),
                        reason: note.unwrap_or_else(|| "bloqué".into()),
                    };
                }
                Decision::Ask(reason) => {
                    asked.get_or_insert((hook.name.clone(), reason));
                }
                Decision::Args(new) => {
                    current = new;
                    changed_by = Some(hook.name.clone());
                }
                Decision::Continue(_) | Decision::Failed(_) => {}
            }
        }
        // Le plus strict l'emporte : une carte forcée passe avant des arguments modifiés.
        if let Some((hook, reason)) = asked {
            return PreToolGate::Ask { hook, reason };
        }
        match changed_by {
            Some(hook) if current != *args => PreToolGate::Args {
                hook,
                args: current,
            },
            _ => PreToolGate::Continue,
        }
    }

    async fn turn_end(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        answer: &str,
    ) -> Option<(String, String)> {
        let loaded = self.0.hooks.current();
        let workspaces = default_workspaces(&self.0);
        let started = std::time::Instant::now();
        for hook in loaded
            .hooks
            .iter()
            .filter(|h| h.applies(Event::TurnEnd, None, None))
            .take(MAX_PER_EVENT)
        {
            if started.elapsed() >= BUDGET {
                break;
            }
            let launch = hook_exec::Launch {
                event: Event::TurnEnd,
                session_id,
                run_id,
                tool: None,
                args: None,
                result: None,
                answer: Some(answer),
                prompt: None,
                workspaces: &workspaces,
                workspace: workspaces.first().map(PathBuf::as_path),
                file: None,
                left: BUDGET.saturating_sub(started.elapsed()),
            };
            if let Decision::Block(note) = self.run(hook, &launch).await {
                return Some((hook.name.clone(), note.unwrap_or_default()));
            }
        }
        None
    }

    async fn blocked(&self, session_id: &str, _run_id: Option<&str>, reason: &str) {
        if let Err(e) = penelope_app::mission::block(&self.0, session_id, reason).await {
            tracing::warn!(error = %e, session = session_id, "mission non bloquée");
        }
    }
}
