//! Lancement d'un hook, commun à tous les événements (#383, #384) : commande jugée,
//! rendu, bac à sable, secrets d'`env`, contexte en JSON sur l'entrée standard, sortie
//! caviardée ; et son journal (`hook.ran`).

use super::*;
use penelope_tools::hooks::{Event, Hook, Vars};

/// Ce que le hook reçoit d'un moment de la boucle.
pub(crate) struct Launch<'a> {
    pub event: Event,
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub tool: Option<&'a str>,
    pub args: Option<&'a Value>,
    /// Résultat de l'outil (`post_tool`).
    pub result: Option<&'a str>,
    /// Réponse prête (`turn_end`).
    pub answer: Option<&'a str>,
    pub workspaces: &'a [PathBuf],
    pub workspace: Option<&'a Path>,
    pub file: Option<&'a str>,
    pub left: std::time::Duration,
}

/// Texte passé au hook sur son entrée (résultat ou réponse), en caractères.
const INPUT_CHARS: usize = 8_000;

fn event_name(e: Event) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Lance le hook ; `Ok((code, sortie caviardée))`, ou `Err(motif)` s'il n'a pas pu partir.
pub(crate) async fn launch(
    s: &Services,
    hook: &Hook,
    l: &Launch<'_>,
) -> Result<(i32, String), String> {
    // Une commande prise dans un workspace pourrait être écrite par l'agent : elle est
    // résolue comme au lancement (PATH, chemin réel) avant d'être jugée.
    if let Some(why) = super::post_hooks::command_in_workspace(&hook.command, l.workspaces) {
        return Err(why);
    }
    let vars = Vars {
        file: l.file.map(String::from),
        workspace: l.workspace.map(|w| w.to_string_lossy().to_string()),
        session: Some(l.session_id.to_string()),
        tool: l.tool.map(String::from),
        data: Some(s.platform.dirs.data().to_string_lossy().to_string()),
    };
    let rendered = hook.render(&vars)?;
    let cwd = rendered
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| l.workspace.map(Path::to_path_buf))
        .unwrap_or_else(|| s.platform.dirs.data());
    let mut env = Vec::new();
    for (k, v) in &hook.env {
        let value = s.platform.secrets.expand(v).map_err(|e| e.to_string())?;
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
    let clip = |t: Option<&str>| t.map(|t| t.chars().take(INPUT_CHARS).collect::<String>());
    let input = json!({
        "event": event_name(l.event),
        "session_id": l.session_id,
        "run_id": l.run_id,
        "cwd": cwd,
        "tool": l.tool.map(|name| json!({
            "name": name,
            "args": l.args.map(penelope_tools::args::without_intention),
        })),
        "result": clip(l.result).map(|o| json!({"ok": true, "output": o})),
        "answer": clip(l.answer),
    });
    let out = penelope_tools::shell::exec_argv_with_input(
        &s.platform.processes,
        &rendered.program,
        &rendered.args,
        penelope_tools::shell::ExecOptions {
            profile: Some(&profile),
            cwd: Some(&cwd),
            timeout: hook.timeout.min(l.left),
            max_output_bytes: penelope_tools::hooks::MAX_OUTPUT_BYTES,
            env,
            ..Default::default()
        },
        Some(input.to_string().into_bytes()),
    )
    .await
    .map_err(|e| e.to_string())?;
    // Un secret passé par `env` ne ressort ni au modèle ni dans une carte.
    let mut text = format!("{}{}", out.stdout, out.stderr);
    for secret in secrets.iter().filter(|v| v.len() >= 4) {
        text = text.replace(secret.as_str(), "‹secret›");
    }
    Ok((out.exit_code, penelope_observe::redact::redact(&text)))
}

/// La décision d'un hook lancé (ou qui n'a pas pu partir : un échec).
pub(crate) fn decision_of(
    event: Event,
    hook: &Hook,
    launched: &Result<(i32, String), String>,
) -> (penelope_tools::hooks::Decision, Option<i32>) {
    match launched {
        Ok((code, out)) => (
            penelope_tools::hooks::decide(event, *code, out, hook.on_failure),
            Some(*code),
        ),
        Err(why) => (
            penelope_tools::hooks::decide(event, -1, why, hook.on_failure),
            None,
        ),
    }
}

/// Chaque exécution est un événement du journal, jamais un effet rejoué.
pub(crate) async fn record(
    s: &Services,
    hook: &Hook,
    l: &Launch<'_>,
    verdict: &str,
    exit_code: Option<i32>,
    ms: u64,
) {
    let mut draft = penelope_kernel::event::EventDraft::new(
        "hook.ran",
        json!({"name": hook.name, "event": event_name(l.event), "verdict": verdict,
            "exit_code": exit_code, "ms": ms}),
    )
    .session(l.session_id);
    if let Some(run) = l.run_id {
        draft = draft.run(run);
    }
    if let Err(e) = s.events.append(draft).await {
        tracing::warn!(error = %e, hook = %hook.name, "exécution de hook non journalisée");
    }
}
