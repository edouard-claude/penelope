//! Les méthodes `hooks.*` (#383) : lister, montrer, essayer et modifier les hooks de
//! `hooks.d/`, pour `penelope hooks` et `self_status`.

use crate::services::Services;
use penelope_tools::hooks::{Event, Hook, Vars};
use serde_json::{Value, json};

/// Une ligne de liste : ce qu'il faut pour reconnaître un hook.
fn summary(h: &Hook) -> Value {
    json!({
        "name": h.name, "on": h.on, "match": h.matcher, "command": h.command,
        "args": h.args, "timeout_s": h.timeout.as_secs(), "sandbox": h.sandbox,
        "on_failure": h.on_failure, "deliver": h.deliver,
    })
}

pub async fn rpc(s: &Services, method: &str, p: &Value) -> anyhow::Result<Value> {
    use penelope_kernel::api::method as m;
    let name = || {
        p.get("name")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("paramètre `name` manquant"))
    };
    match method {
        m::HOOKS_LIST => {
            let loaded = s.hooks.current();
            let mut text = if loaded.hooks.is_empty() {
                format!("Aucun hook dans {}.", s.hooks.dir().display())
            } else {
                loaded
                    .hooks
                    .iter()
                    .map(|h| {
                        format!(
                            "• {} ({:?}) : {} {}",
                            h.name,
                            h.on,
                            h.command,
                            h.args.join(" ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            for (file, error) in &loaded.invalid {
                text.push_str(&format!("\n⚠️ {file}.toml invalide : {error}"));
            }
            Ok(json!({
                "dir": s.hooks.dir(),
                "hooks": loaded.hooks.iter().map(summary).collect::<Vec<_>>(),
                "invalid": loaded.invalid.iter()
                    .map(|(f, e)| json!({"file": f, "error": e})).collect::<Vec<_>>(),
                "text": text,
            }))
        }
        m::HOOKS_SHOW => {
            let n = name()?;
            let hook = s
                .hooks
                .get(&n)
                .ok_or_else(|| anyhow::anyhow!("aucun hook `{n}` valide dans hooks.d/"))?;
            let path = s.hooks.dir().join(format!("{n}.toml"));
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            Ok(json!({"hook": summary(&hook), "path": path, "text": raw}))
        }
        m::HOOKS_TEST => test(s, &name()?, p).await,
        m::HOOKS_EDIT => {
            let n = name()?;
            let patch = p
                .get("patch")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("paramètre `patch` manquant"))?;
            let mut hook = None;
            for (field, value) in patch {
                hook = Some(
                    penelope_tools::hooks::edit(s.hooks.dir(), &n, field, value.clone())
                        .map_err(anyhow::Error::msg)?,
                );
            }
            let hook = hook.ok_or_else(|| anyhow::anyhow!("aucun champ à modifier"))?;
            Ok(json!({"hook": summary(&hook), "text": format!("Hook `{n}` modifié.")}))
        }
        other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
    }
}

/// Lance un hook sur une entrée synthétique, sous son profil, sans rien journaliser.
async fn test(s: &Services, name: &str, p: &Value) -> anyhow::Result<Value> {
    let hook = s
        .hooks
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("aucun hook `{name}` valide dans hooks.d/"))?;
    let tool = p.get("tool").and_then(Value::as_str).unwrap_or("fs_edit");
    let file = p.get("file").and_then(Value::as_str).map(String::from);
    let result = p.get("result").and_then(Value::as_str).unwrap_or("");
    let workspace = crate::helpers::default_workspaces(s).into_iter().next();
    let vars = Vars {
        file: file.clone(),
        workspace: workspace.as_ref().map(|w| w.to_string_lossy().to_string()),
        session: Some("essai".into()),
        tool: Some(tool.into()),
        data: Some(s.platform.dirs.data().to_string_lossy().to_string()),
    };
    let applies = hook.applies(Event::PostTool, Some(tool), file.as_deref());
    let rendered = match hook.render(&vars) {
        Ok(r) => r,
        Err(e) => return Ok(json!({"applies": applies, "skipped": e, "text": e})),
    };
    let cwd = rendered
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .or(workspace)
        .unwrap_or_else(|| s.platform.dirs.data());
    let input = json!({
        "event": "post_tool", "session_id": "essai", "cwd": cwd,
        "tool": {"name": tool, "args": {"path": file}},
        "result": {"ok": true, "output": result},
    });
    let profile = penelope_tools::shell::profile_with_denied_reads(
        hook.sandbox
            .effective(&s.config.config().sandbox.default_profile),
        &cwd,
        false,
        &crate::helpers::denied_reads(s),
    );
    let mut env = Vec::new();
    for (k, v) in &hook.env {
        let value = s.platform.secrets.expand(v)?;
        penelope_observe::register_secret(&value);
        env.push((k.clone(), value));
    }
    let secrets: Vec<String> = env.iter().map(|(_, v)| v.clone()).collect();
    let mask = |t: &str| {
        let mut t = t.to_string();
        for secret in secrets.iter().filter(|v| v.len() >= 4) {
            t = t.replace(secret.as_str(), "‹secret›");
        }
        penelope_observe::redact::redact(&t)
    };
    let out = penelope_tools::shell::exec_argv_with_input(
        &s.platform.processes,
        &rendered.program,
        &rendered.args,
        penelope_tools::shell::ExecOptions {
            profile: Some(&profile),
            cwd: Some(&cwd),
            timeout: hook.timeout,
            max_output_bytes: penelope_tools::hooks::MAX_OUTPUT_BYTES,
            env,
            ..Default::default()
        },
        Some(input.to_string().into_bytes()),
    )
    .await;
    let (exit_code, stdout, stderr) = match &out {
        Ok(o) => (Some(o.exit_code), mask(&o.stdout), mask(&o.stderr)),
        Err(e) => (None, String::new(), e.to_string()),
    };
    let (verdict, note) = match exit_code {
        Some(code) => {
            penelope_tools::hooks::verdict_of(code, &format!("{stdout}{stderr}"), hook.on_failure)
        }
        None => (
            match hook.on_failure {
                penelope_tools::hooks::OnFailure::Block => "block",
                penelope_tools::hooks::OnFailure::Continue => "échec",
            },
            Some(stderr.clone()),
        ),
    };
    let text = format!(
        "Hook `{name}` {} cet appel ; code {} ; verdict {verdict}{}",
        if applies { "vise" } else { "ne vise pas" },
        exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".into()),
        note.as_deref()
            .map(|n| format!("\n{n}"))
            .unwrap_or_default()
    );
    Ok(json!({
        "applies": applies, "program": rendered.program, "args": rendered.args,
        "input": input, "exitCode": exit_code, "stdout": stdout, "stderr": stderr,
        "verdict": verdict, "note": note, "text": text,
    }))
}
