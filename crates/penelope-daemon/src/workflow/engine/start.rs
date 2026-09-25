//! Démarrage d'un run : verdict de la politique, brief, paramètres.

use super::*;

/// Démarre un run. `Coalesce` rend le run déjà actif, `Hold` le met en file.
pub async fn start_run(
    d: &Arc<Daemon>,
    workflow_id: &str,
    params: Value,
    origin: &Origin,
    parent: Option<&str>,
    depth: u32,
) -> Result<Run, String> {
    start_run_briefed(d, workflow_id, params, origin, parent, depth, None).await
}

/// [`start_run`] avec le résumé de la conversation qui décide du lancement (issue #35).
/// Attend le premier verdict d'un run : un état qui n'est plus `running`, ou la fin de sa
/// première étape. Rend `None` si rien n'a bougé dans le délai (issue #154).
///
/// Cinq secondes au plus : l'outil rend la main même si l'étape est longue, mais il aura
/// vu l'échec d'un `git clone` qui casse en une seconde — le cas du 21/09.
pub(super) async fn first_verdict(
    d: &Arc<Daemon>,
    run_id: &str,
    within: Duration,
) -> Option<penelope_workflow::runs::Run> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if let Ok(Some(r)) = d.services.runs.get(run_id).await
            && (r.state != RunState::Running || r.step_outputs.get("__last").is_some())
        {
            return Some(r);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

pub async fn start_run_briefed(
    d: &Arc<Daemon>,
    workflow_id: &str,
    params: Value,
    origin: &Origin,
    parent: Option<&str>,
    depth: u32,
    brief: Option<&str>,
) -> Result<Run, String> {
    let s = &d.services;
    let wf = s
        .workflows
        .get(workflow_id)
        .ok_or_else(|| format!("workflow `{workflow_id}` introuvable (`/wf` les liste)"))?;
    let os = s.platform.os_name();
    if !wf.runs_on(os) {
        return Err(format!("`{workflow_id}` ne tourne pas sur {os}"));
    }
    if depth > MAX_DEPTH {
        return Err(format!("profondeur de sous-workflow limitée à {MAX_DEPTH}"));
    }
    let params = resolve_params(&wf, params)?;

    let concurrency = &wf.settings.concurrency;
    let admission = if parent.is_some() {
        Admission::Start
    } else {
        s.runs
            .admit(
                workflow_id,
                concurrency.max_concurrent,
                &concurrency.admission,
            )
            .await
            .map_err(|e| e.to_string())?
    };
    if admission == Admission::Drop {
        return Err(format!(
            "`{workflow_id}` tourne déjà (admission `drop`) : demande ignorée"
        ));
    }
    if admission == Admission::Coalesce {
        let active = s
            .runs
            .list(None, 200)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.workflow_id == workflow_id && !r.state.is_terminal());
        if let Some(r) = active {
            return Ok(r);
        }
    }

    let session = s
        .sessions
        .create(SessionKind::WorkflowRun, Some(wf.metadata.name.clone()))
        .await
        .map_err(|e| e.to_string())?;
    let run = s
        .runs
        .create(&wf, session.id.as_str(), params, None, parent, depth)
        .await
        .map_err(|e| e.to_string())?;
    // Un sous-workflow travaille dans l'espace de son parent (le dépôt cloné, par exemple),
    // sauf s'il demande son propre espace persistant.
    let parent_workdir = match parent {
        Some(p) => s.runs.get(p).await.ok().flatten().and_then(|r| r.workdir),
        None => None,
    };
    let workdir = match (
        wf.settings.workspace.strip_prefix("persistent:"),
        parent_workdir,
    ) {
        (Some(name), _) => s
            .platform
            .dirs
            .data()
            .join("workspaces")
            .join(penelope_platform::slugify(name)),
        (None, Some(dir)) => std::path::PathBuf::from(dir),
        (None, None) => s.platform.dirs.state().join("runs").join(&run.id),
    };
    std::fs::create_dir_all(&workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
    s.runs
        .set_workdir(&run.id, &workdir.to_string_lossy())
        .await
        .map_err(|e| e.to_string())?;
    let _ = s
        .kv_set(&origin_key(&run.id), &origin.to_value().to_string())
        .await;
    if let Some(brief) = brief.map(str::trim).filter(|b| !b.is_empty()) {
        let brief: String = brief.chars().take(BRIEF_CHARS).collect();
        let _ = s.kv_set(&brief_key(&run.id), &brief).await;
    }
    if admission == Admission::Hold {
        s.runs
            .set_state(&run.id, RunState::Paused, Some("en attente d'admission"))
            .await
            .map_err(|e| e.to_string())?;
        let _ = s.kv_set(&format!("wf.held.{}", run.id), "1").await;
    }
    let _ = s
        .events
        .append(
            EventDraft::new(
                "workflow.started",
                json!({"run": run.id, "workflow": workflow_id, "parent": parent, "held": admission == Admission::Hold}),
            )
            .session(session.id.as_str()),
        )
        .await;
    let run = s
        .runs
        .get(&run.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("run introuvable")?;
    progress(d, &run, &wf, None).await;
    d.workflows.wake();
    Ok(run)
}

/// Paramètres effectifs : valeurs par défaut, obligatoires vérifiés, inconnus refusés.
pub fn resolve_params(wf: &Workflow, given: Value) -> Result<Value, String> {
    let mut given = match given {
        Value::Null => serde_json::Map::new(),
        Value::Object(m) => m,
        other => return Err(format!("paramètres attendus en objet, reçu {other}")),
    };
    let mut out = serde_json::Map::new();
    for p in &wf.metadata.parameters {
        match given.remove(&p.id).or_else(|| p.default.clone()) {
            Some(v) => {
                out.insert(p.id.clone(), v);
            }
            None if p.required => {
                return Err(format!(
                    "paramètre obligatoire manquant : `{}` ({})",
                    p.id, p.label
                ));
            }
            None => {}
        }
    }
    if let Some(extra) = given.keys().next() {
        return Err(format!(
            "paramètre inconnu `{extra}` pour `{}` (attendus : {})",
            wf.metadata.id,
            wf.metadata
                .parameters
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(Value::Object(out))
}
