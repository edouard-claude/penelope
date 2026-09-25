//! Étapes `shell` et `tool`, via le ledger d'effets.

use super::*;

/// `shell` : commande dans le workspace du run, via le ledger d'effets.
pub(super) async fn shell_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let cfg = s.config.config();
    let step = ctx.step;
    let Some(raw) = step.command_for_os(s.platform.os_name()) else {
        return Ok(done(
            StepResult::Error,
            json!({"error": format!("pas de commande pour {}", s.platform.os_name())}),
        ));
    };
    let command = ctx.render_command(&raw).await;
    let workdir = ctx.workdir();
    let cwd = if step.cwd.is_empty() {
        workdir.clone()
    } else {
        let c = std::path::PathBuf::from(ctx.render(&step.cwd).await);
        if c.is_absolute() { c } else { workdir.join(c) }
    };
    let cwd = penelope_platform::sandbox::normalise(&cwd);
    let timeout = step
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(SHELL_TIMEOUT);
    let effect = EffectSpec::new(
        penelope_kernel::effects::EffectKind::Shell,
        "shell",
        json!({"command": command, "cwd": cwd.to_string_lossy()}),
    )
    .run(&ctx.run.id)
    .session(&ctx.run.session_id)
    .step(&step.id)
    .attempt(ctx.attempt);
    let value = match s.effects.plan(effect).await? {
        Planned::Replayed(v) => v,
        Planned::InFlight(_) => return Ok(StepOutcome::Waiting("commande en cours".into())),
        Planned::NeedsDecision(id) => {
            return Ok(done(
                StepResult::Error,
                json!({"error": format!("commande peut-être déjà exécutée (effet {id}) : décision requise")}),
            ));
        }
        Planned::Fresh(id) => {
            s.effects.dispatching(&id).await?;
            // Réseau déclaré par l'étape, visible dans l'aperçu validé au lancement (#106).
            let profile = penelope_tools::shell::profile_with_denied_reads(
                &cfg.sandbox.default_profile,
                &cwd,
                cfg.sandbox.shell_network || step.network,
                &penelope_executor::executor::denied_reads(s),
            );
            let _ = std::fs::create_dir_all(&cwd);
            match penelope_tools::shell::exec(
                &s.platform.processes,
                &command,
                penelope_tools::shell::ExecOptions {
                    profile: Some(&profile),
                    cwd: Some(&cwd),
                    timeout,
                    max_output_bytes: cfg.tools.max_output_bytes,
                    shell: penelope_executor::executor::shell_override(&cfg.tools.shell),
                    cancel: Some(ctx.cancel),
                },
            )
            .await
            {
                Ok(out) => {
                    let v = out.to_json();
                    s.effects.complete(&id, v.clone()).await?;
                    v
                }
                Err(e) => {
                    s.effects.fail(&id, e.to_string()).await?;
                    return Ok(done(StepResult::Failure, json!({"error": e.to_string()})));
                }
            }
        }
    };
    let code = value["exitCode"].as_i64().unwrap_or(-1) as i32;
    let ok = step.success_exit_codes.contains(&code);
    let mut out = json!({"stdout": value["stdout"], "stderr": value["stderr"], "exitCode": code});
    // Réseau coupé : l'échec le dit, au lieu d'être relancé à l'identique (#106).
    if !ok
        && !(cfg.sandbox.shell_network || step.network)
        && cfg.sandbox.default_profile != "full"
        && penelope_tools::shell::looks_like_network_failure(
            &command,
            code,
            value["stdout"].as_str().unwrap_or_default(),
            value["stderr"].as_str().unwrap_or_default(),
        )
    {
        out["note"] = json!(
            "Réseau coupé pour cette étape par le bac à sable : elle ne déclare pas \
             `network: true` (sandbox.shell_network est fermé)."
        );
    }
    Ok(done(
        if ok {
            StepResult::Success
        } else {
            StepResult::Failure
        },
        out,
    ))
}

/// `tool` : outil natif ou MCP, politique et approbation comme en conversation.
pub(super) async fn tool_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let cfg = s.config.config();
    let step = ctx.step;
    let args = ctx.render_json(&step.args).await;
    let args = if args.is_null() { json!({}) } else { args };
    let exec = ctx.executor().await;
    use penelope_agent::ToolExecutor;
    // Même forme qu'en conversation : `cd <workspace> && …` porte son `cwd` (#123).
    let args = exec.normalise_call(&step.tool, &args).unwrap_or(args);
    let info = exec.describe_call(&step.tool, &args).await;
    let call_id = format!(
        "wf-{}-{}-{}-{}",
        ctx.run.id, step.id, ctx.run.iterations, ctx.attempt
    );
    // Arguments vérifiés avant toute carte : une étape qui ne pourrait pas aboutir échoue
    // sans rien demander au propriétaire (issue #117).
    if let Err(e) = exec.precheck(&step.tool, &args).await {
        return Ok(done(StepResult::Error, json!({"error": e.for_model()})));
    }

    match s
        .approvals
        .find_for_call(&ctx.run.session_id, &call_id)
        .await?
    {
        Some(a) => match a.state {
            ApprovalState::Pending => {
                send_approval_once(ctx, a.id.as_str()).await;
                return Ok(StepOutcome::Waiting(format!("approbation {}", a.id)));
            }
            ApprovalState::Approved => {}
            _ => {
                return Ok(done(
                    StepResult::Failure,
                    json!({"error": "appel refusé par le propriétaire"}),
                ));
            }
        },
        None => {
            let verdict = s
                .policies
                .evaluate_in(
                    &cfg.mcp.policy,
                    &info.effective_name,
                    penelope_agent::server_of(&info.effective_name).as_deref(),
                    &args,
                    info.risk,
                    Some(&ctx.run.id),
                    Some(&ctx.run.session_id),
                    Some(&ctx.workdir()),
                )
                .await?;
            let decision = match info.policy {
                Some(forced) if forced == PolicyDecision::Deny || verdict.rule_id.is_none() => {
                    forced
                }
                _ => verdict.decision,
            };
            match decision {
                PolicyDecision::Deny => {
                    return Ok(done(
                        StepResult::Failure,
                        json!({"error": format!("refusé par la politique : {}", verdict.reason)}),
                    ));
                }
                PolicyDecision::Ask | PolicyDecision::AskTwice => {
                    let approval = s
                        .approvals
                        .create(
                            ApprovalKind::ToolCall,
                            &info.effective_name,
                            info.risk,
                            json!({
                                "tool": info.effective_name,
                                "arguments": penelope_observe::redact_json(&args),
                                "reason": verdict.reason,
                                "double": decision == PolicyDecision::AskTwice,
                                "call_id": call_id,
                                "run": ctx.run.id,
                                "step": step.id,
                            }),
                            vec!["Autoriser".into(), "Refuser".into()],
                            Some(&ctx.run.session_id),
                            Some(&ctx.run.id),
                            false,
                        )
                        .await?;
                    send_approval_once(ctx, approval.id.as_str()).await;
                    return Ok(StepOutcome::Waiting(format!("approbation {}", approval.id)));
                }
                PolicyDecision::Auto => {}
            }
        }
    }

    let effect = EffectSpec::new(
        penelope_agent::effect_kind(&info.effective_name),
        info.effective_name.clone(),
        args.clone(),
    )
    .run(&ctx.run.id)
    .session(&ctx.run.session_id)
    .step(&call_id)
    .idempotent(info.idempotent);
    let (ok, value, text) = match s.effects.plan(effect).await? {
        Planned::Replayed(v) => (true, v, String::new()),
        Planned::InFlight(_) => return Ok(StepOutcome::Waiting("appel en cours".into())),
        Planned::NeedsDecision(id) => {
            return Ok(done(
                StepResult::Error,
                json!({"error": format!("appel peut-être déjà passé (effet {id}) : décision requise")}),
            ));
        }
        Planned::Fresh(id) => {
            s.effects.dispatching(&id).await?;
            match exec.execute(&step.tool, &args).await {
                Ok(o) if !o.is_error => {
                    s.effects.complete(&id, o.value.clone()).await?;
                    (true, o.value, o.text)
                }
                Ok(o) => {
                    s.effects.fail(&id, o.text.clone()).await?;
                    (false, o.value, o.text)
                }
                Err(e) => {
                    s.effects.fail(&id, e.to_string()).await?;
                    (false, Value::Null, e.to_string())
                }
            }
        }
    };
    let structured = value
        .get("structuredContent")
        .cloned()
        .unwrap_or(Value::Null);
    let mut output = json!({"data": value, "text": text});
    if !structured.is_null() {
        output["structuredContent"] = structured;
    }
    if !ok {
        output["error"] = json!(text);
    }
    Ok(done(
        if ok {
            StepResult::Success
        } else {
            StepResult::Failure
        },
        output,
    ))
}
