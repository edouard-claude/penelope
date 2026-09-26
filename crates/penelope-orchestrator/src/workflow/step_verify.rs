//! Étape `verify` : vérification indépendante du travail livré.

use super::*;

/// Commande réellement validée par le build, sinon contrat initial du plan (#167).
/// Le champ reste une commande shell, pour déclarer explicitement PATH et ulimit sans
/// recopier un environnement de processus contenant éventuellement des secrets.
pub(super) fn project_test_spec(metadata: &Value) -> (String, String) {
    let declared = |section: &str, key: &str| {
        metadata[section][key]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(String::from)
    };
    (
        declared("verification", "dir")
            .or_else(|| declared("project", "dir"))
            .unwrap_or_default(),
        declared("verification", "test_command")
            .or_else(|| declared("project", "test_command"))
            .unwrap_or_else(|| penelope_workflow::bundled::TEST_COMMAND.to_string()),
    )
}

pub(super) fn classify_project_test(output: &Value) -> &'static str {
    let value = output.get("data").unwrap_or(output);
    if value["exitCode"].as_i64() == Some(127)
        || output["error"].as_str().is_some_and(|e| {
            e.contains("No such file or directory") || e.contains("command not found")
        })
    {
        "prerequisite_missing"
    } else if value["exitCode"].as_i64() == Some(0) {
        "passed"
    } else {
        "test_failed"
    }
}

pub(super) fn evidence_matches_head(evidence: &Value, head: &str) -> bool {
    evidence["sha"].as_str() == Some(head)
}

pub(super) fn requires_current_sha(evidence: &Value) -> bool {
    matches!(evidence["kind"].as_str(), Some("pr" | "ci" | "tdd_green"))
}

fn limited_text(value: &Value, limit: usize) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(limit)
        .collect()
}

/// Seulement les champs utiles au vérificateur, bornés puis rédigés. Un environnement
/// entier n'est jamais transmis : il pourrait contenir des identifiants (#167).
fn verification_handoff(
    metadata: &Value,
    build_output: Option<&Value>,
    head: Option<&str>,
) -> Value {
    let v = &metadata["verification"];
    let (dir, command) = project_test_spec(metadata);
    let evidence: Vec<Value> = v["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .map(|e| {
            json!({
                "kind": limited_text(&e["kind"], 32),
                "ref": limited_text(&e["ref"], 512),
                "sha": limited_text(&e["sha"], 64),
            })
        })
        .collect();
    let prerequisites: Vec<String> = v["prerequisites"]
        .as_array()
        .into_iter()
        .flatten()
        .take(12)
        .map(|p| limited_text(p, 256))
        .collect();
    let output = build_output
        .map(|o| o.to_string().chars().take(6000).collect::<String>())
        .unwrap_or_default();
    penelope_observe::redact::redact_json(&json!({
        "dir": dir.chars().take(1024).collect::<String>(),
        "test_command": command.chars().take(2048).collect::<String>(),
        "prerequisites": prerequisites,
        "evidence": evidence,
        "head_sha": head,
        "build_output": output,
    }))
}

pub(super) fn repository_rules(project_dir: &str) -> Value {
    let root = std::path::Path::new(project_dir);
    let canonical_root = std::fs::canonicalize(root).ok();
    let mut rules = serde_json::Map::new();
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let path = root.join(name);
        if !std::fs::canonicalize(&path)
            .ok()
            .zip(canonical_root.as_ref())
            .is_some_and(|(file, root)| file.starts_with(root))
        {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            rules.insert(
                name.to_string(),
                json!(content.chars().take(12000).collect::<String>()),
            );
        }
    }
    penelope_observe::redact::redact_json(&Value::Object(rules))
}

pub(super) fn verifier_prompt(
    objective: &str,
    criteria: &[String],
    checks: &Value,
    contract: &Value,
    rules: &Value,
    workdir: &std::path::Path,
) -> String {
    format!(
        "Objectif initial : {}\n\nCritères à vérifier :\n{}\n\nRésultats des contrôles :\n{}\n\nContrat et références du build :\n{}\n\nRègles du dépôt :\n{}\n\nRépertoire : `{}`. \
         Consulte les preuves réelles et vérifie leur SHA ; les déclarations du build ne \
         valent pas validation. Respecte les clauses conditionnelles. Une \
         version et des notes exigées par le dépôt ne sont pas une release anticipée.\n\
         Réponds uniquement par un JSON : {{\"criteres\": [{{\"index\": 0, \"statut\": \
         \"passed\"|\"failed\", \"note\": \"...\"}}], \"verdict\": \"passed\"|\"failed\", \
         \"failure_kind\": \"evidence_missing\"|\"test_failed\"|\"criterion_failed\"|null}}",
        objective.chars().take(8000).collect::<String>(),
        if criteria.is_empty() {
            "(aucun critère écrit)".to_string()
        } else {
            criteria.join("\n")
        },
        serde_json::to_string_pretty(checks).unwrap_or_default(),
        serde_json::to_string_pretty(contract).unwrap_or_default(),
        serde_json::to_string_pretty(rules).unwrap_or_default(),
        workdir.display()
    )
}

/// Le pas enfant d'un contrôle : un shell, un outil, ou les tests du projet.
fn check_step(
    step: &Step,
    i: usize,
    check: &Value,
    kind: &str,
    project_dir: &str,
    project_command: &str,
) -> Step {
    let mut child = Step {
        id: format!("{}-check-{}", step.id, i + 1),
        kind: kind.to_string(),
        command: check.get("command").cloned().unwrap_or(Value::Null),
        cwd: check
            .get("cwd")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        tool: check
            .get("tool")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string(),
        args: check.get("args").cloned().unwrap_or(Value::Null),
        ..Default::default()
    };
    // Les tests du projet, pas ceux de Pénélope (issue #137) : le répertoire et la
    // commande déclarés par le plan (`project.dir`, `project.test_command`), sinon la
    // commande déduite du dépôt (Makefile, Cargo.toml, package.json, go.mod).
    if kind == "project_tests" {
        // Même politique, approbation et sandbox qu'un shell_exec du builder.
        child.kind = "tool".into();
        child.tool = "shell_exec".into();
        child.args = json!({"command": project_command, "cwd": project_dir});
    }
    child
}

/// Le commit présent dans le dépôt du projet, s'il y en a un.
async fn repository_head(project_dir: &str) -> Option<String> {
    if project_dir.is_empty() {
        None
    } else {
        tokio::process::Command::new("git")
            .args(["-C", project_dir, "rev-parse", "HEAD"])
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
    }
}

/// Les preuves déclarées par le contrat (PR, CI), bornées en nombre et en taille.
fn evidence_references(metadata: &Value) -> Vec<Value> {
    metadata["verification"]["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .take(16)
        .filter(|e| e["kind"].is_string() && e["ref"].is_string())
        .map(|e| {
            json!({
                "kind": limited_text(&e["kind"], 32),
                "ref": limited_text(&e["ref"], 512),
                "sha": e["sha"].as_str().map(|_| limited_text(&e["sha"], 64)),
            })
        })
        .collect()
}

/// Reporte sur les critères le statut et la note que le vérificateur leur donne.
fn mark_criteria(v: &Value, criteria: &mut [Value]) {
    for item in v["criteres"].as_array().cloned().unwrap_or_default() {
        let Some(i) = item["index"].as_u64().map(|i| i as usize) else {
            continue;
        };
        if let Some(c) = criteria.get_mut(i).and_then(|c| c.as_object_mut()) {
            let status = if item["statut"].as_str() == Some("passed") {
                "passed"
            } else {
                "failed"
            };
            c.insert("status".into(), json!(status));
            if let Some(note) = item["note"].as_str() {
                c.insert("note".into(), json!(note));
            }
        }
    }
}

/// `verify` : contrôles puis vérificateur ; met à jour les critères.
pub(super) async fn verify_step(ctx: &StepCtx<'_>) -> anyhow::Result<StepOutcome> {
    let s = ctx.s();
    let step = ctx.step;
    let metadata = session_metadata(s, &ctx.run.session_id).await;
    let (project_dir, project_command) = project_test_spec(&metadata);
    let mut checks_out = serde_json::Map::new();
    let mut checks_ok = true;
    let mut failure_kind = None;
    for (i, check) in step.checks.iter().enumerate() {
        let kind = check
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("shell")
            .to_string();
        let child = check_step(step, i, check, &kind, &project_dir, &project_command);
        // Une vérification qui expire n'emporte pas les suivantes (issue #56).
        let child_cancel = ctx.cancel.child();
        let child_ctx = StepCtx {
            d: ctx.d,
            run: ctx.run,
            wf: ctx.wf,
            step: &child,
            attempt: ctx.attempt,
            cancel: &child_cancel,
        };
        match Box::pin(execute_step(&child_ctx)).await? {
            StepOutcome::Waiting(why) => return Ok(StepOutcome::Waiting(why)),
            StepOutcome::Done { result, output } => {
                let mut entry = output;
                if let Some(o) = entry.as_object_mut() {
                    o.insert("result".into(), json!(result.as_str()));
                }
                let check_ok = if kind == "project_tests" {
                    result.is_ok() && classify_project_test(&entry) == "passed"
                } else {
                    result.is_ok()
                };
                checks_ok &= check_ok;
                if kind == "project_tests" && !check_ok {
                    failure_kind = Some(classify_project_test(&entry));
                }
                checks_out.insert(child.id.clone(), entry);
            }
        }
    }

    // Une preuve de CI n'a de sens que pour le commit présent dans le dépôt. Lire le
    // SHA ici, après les contrôles, interdit qu'un ancien lien vert valide une révision
    // différente. Les références restent des pistes à examiner, jamais un verdict.
    let head = repository_head(&project_dir).await;
    let references = evidence_references(&metadata);
    let missing_sha = references
        .iter()
        .any(|e| requires_current_sha(e) && !e["sha"].is_string());
    let stale: Vec<Value> = references
        .iter()
        .filter(|e| {
            requires_current_sha(e)
                && head
                    .as_deref()
                    .is_none_or(|sha| !evidence_matches_head(e, sha))
        })
        .cloned()
        .collect();
    if ((metadata["verification"].is_object() && references.is_empty()) || missing_sha)
        && failure_kind.is_none()
    {
        failure_kind = Some("evidence_missing");
    } else if !stale.is_empty() && failure_kind.is_none() {
        failure_kind = Some("stale_evidence");
    }

    if matches!(
        failure_kind,
        Some("prerequisite_missing" | "stale_evidence" | "evidence_missing")
    ) {
        let error = match failure_kind {
            Some("stale_evidence") => {
                "preuve liée à un autre SHA : actualiser les références ou le dépôt"
            }
            Some("evidence_missing") => "preuve de PR ou CI sans SHA : compléter le contrat",
            _ => {
                "contrôle non exécutable : déclarer et valider ses prérequis avant de relancer verify"
            }
        };
        return Ok(done(
            StepResult::Failed,
            json!({"checks": checks_out, "failure_kind": failure_kind, "error": error,
                   "head_sha": head, "stale_evidence": stale}),
        ));
    }

    let key = if step.criteria_key.is_empty() {
        "criteria"
    } else {
        &step.criteria_key
    };
    let mut criteria: Vec<Value> = metadata
        .get(key)
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let mut verdict_ok = true;
    let mut verdict = Value::Null;
    if !step.verifier.is_empty() {
        let list: Vec<String> = criteria
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{i}. {}",
                    c.get("text").and_then(|t| t.as_str()).unwrap_or("?")
                )
            })
            .collect();
        let contract = verification_handoff(
            &metadata,
            ctx.run.step_outputs.get("build"),
            head.as_deref(),
        );
        let rules = repository_rules(&project_dir);
        let prompt = verifier_prompt(
            ctx.run.params["objectif"].as_str().unwrap_or_default(),
            &list,
            &Value::Object(checks_out.clone()),
            &contract,
            &rules,
            &ctx.workdir(),
        );
        let model_id = match ctx.model("code") {
            Ok(m) => m,
            Err(e) => return Ok(done(StepResult::Error, json!({"error": e}))),
        };
        let model_id =
            penelope_app::codex_scope::background(&ctx.d.services, &model_id, "workflow").await;
        let mut workspaces = vec![ctx.workdir()];
        for root in penelope_executor::executor::default_workspaces(s) {
            if !workspaces.contains(&root) {
                workspaces.push(root);
            }
        }
        match run_sub_agent(
            ctx.d,
            SubAgentTask {
                session_id: &ctx.run.session_id,
                run_id: Some(&ctx.run.id),
                kind: &step.verifier,
                prompt: &prompt,
                model_id: &model_id,
                tools: &step.tools,
                workspaces,
            },
            ctx.cancel,
        )
        .await
        {
            Ok(text) => {
                let v = extract_json(&text).unwrap_or(Value::Null);
                verdict_ok = v["verdict"].as_str() == Some("passed");
                if !verdict_ok && failure_kind.is_none() {
                    failure_kind = match v["failure_kind"].as_str() {
                        Some("evidence_missing") => Some("evidence_missing"),
                        Some("test_failed") => Some("test_failed"),
                        _ => Some("criterion_failed"),
                    };
                }
                mark_criteria(&v, &mut criteria);
                verdict = v;
            }
            Err(e) => {
                verdict_ok = false;
                failure_kind = Some("prerequisite_missing");
                verdict = json!({"error": e});
            }
        }
        if !criteria.is_empty() {
            let _ = s
                .sessions
                .metadata(
                    &ctx.run.session_id,
                    MetadataOp::Set,
                    key,
                    Value::Array(criteria.clone()),
                )
                .await;
        }
    }
    let passed = checks_ok && verdict_ok;
    if !checks_ok && failure_kind.is_none() {
        failure_kind = Some("test_failed");
    }
    Ok(done(
        if passed {
            StepResult::Passed
        } else {
            StepResult::Failed
        },
        json!({"checks": checks_out, "verdict": verdict, "criteria": criteria,
               "failure_kind": failure_kind,
               "error": failure_kind.map(|kind| format!("vérification refusée ({kind}) : voir les contrôles et les notes des critères"))}),
    ))
}
