//! Fin du tour d'un prompt planifié : état consommé, livrable, issue (#39, #120, #133).

use super::*;

/// Clé de l'état d'une planification gardé avant son tour.
pub(super) fn state_key(session_id: &str) -> String {
    format!("schedule.state.{session_id}")
}

/// Chemin d'un état ou d'un fichier livrable : absolu, ou relatif au premier workspace ;
/// jamais hors des workspaces.
pub(super) fn resolve_in_workspace(d: &Context, path: &str) -> Option<std::path::PathBuf> {
    let workspaces = crate::executor::default_workspaces(&d.services);
    let p = std::path::Path::new(path);
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspaces.first()?.join(p)
    };
    let full = penelope_platform::sandbox::normalise(&full);
    workspaces
        .iter()
        .any(|w| full.starts_with(w))
        .then_some(full)
}

/// Garde l'état tel qu'avant le tour (2 Mio au plus, texte).
pub(super) async fn save_state(d: &Context, session_id: &str, path: &str) {
    let Some(full) = resolve_in_workspace(d, path) else {
        tracing::warn!(
            path,
            "état de planification hors des workspaces : non gardé"
        );
        return;
    };
    let saved = match std::fs::read(&full) {
        Ok(bytes) if bytes.len() <= 2 * 1024 * 1024 => match String::from_utf8(bytes) {
            Ok(text) => json!({"path": full, "content": text}),
            Err(_) => return,
        },
        Ok(_) => return,
        Err(_) => json!({"path": full, "content": null}),
    };
    let _ = d
        .services
        .kv_set(&state_key(session_id), &saved.to_string())
        .await;
}

/// Remet l'état d'avant le tour (`restore`), ou l'oublie : il est validé.
pub(super) async fn settle_state(d: &Context, session_id: &str, restore: bool) {
    let key = state_key(session_id);
    let Ok(Some(raw)) = d.services.kv_get(&key).await else {
        return;
    };
    if restore && let Ok(v) = serde_json::from_str::<Value>(&raw) {
        let path = std::path::PathBuf::from(v["path"].as_str().unwrap_or_default());
        let done: Result<(), String> = match v["content"].as_str() {
            Some(text) => penelope_kernel::config::atomic_write(&path, text.as_bytes())
                .map_err(|e| e.to_string()),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            },
        };
        match done {
            Ok(()) => tracing::info!(path = %path.display(), "état de planification remis"),
            Err(e) => tracing::warn!(error = %e, "état de planification non remis"),
        }
    }
    let _ = d.services.kv_delete(&key).await;
}

/// Ce que le tour devait livrer et n'a pas livré, s'il en déclarait un (issue #120).
pub(super) async fn missing_deliverable(
    d: &Context,
    turn: &penelope_kernel::turn::Turn,
    outcome: &crate::agent::TurnOutcome,
) -> Option<String> {
    let s = &d.services;
    let wanted = turn.payload["livrable"].as_str()?.trim().to_string();
    let since = s
        .sessions
        .get(&turn.session_id)
        .await
        .ok()
        .flatten()
        .map(|x| x.created_at)
        .unwrap_or_default();
    if wanted == "message" {
        let answered = matches!(
            outcome,
            crate::agent::TurnOutcome::Answered { text, .. } if !text.trim().is_empty()
        );
        let channel = matches!(
            crate::bus::Origin::from_payload(&turn.payload),
            crate::bus::Origin::Telegram { .. }
        );
        // Un message envoyé par l'agent lui-même compte aussi.
        let sent = s
            .context
            .history
            .load(&turn.session_id, 0)
            .await
            .unwrap_or_default()
            .iter()
            .any(|e| {
                matches!(
                    e.message.name.as_deref(),
                    Some("send_message" | "send_file")
                ) && !e.message.text().starts_with("Erreur")
            });
        return (!(answered && channel || sent))
            .then(|| "aucun message envoyé au propriétaire".to_string());
    }
    if let Some(path) = wanted.strip_prefix("fichier:") {
        let Some(full) = resolve_in_workspace(d, path.trim()) else {
            return Some(format!("fichier `{}` hors des workspaces", path.trim()));
        };
        let since = chrono::DateTime::parse_from_rfc3339(&since)
            .map(std::time::SystemTime::from)
            .unwrap_or(std::time::UNIX_EPOCH);
        let written = std::fs::metadata(&full)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t >= since);
        return (!written).then(|| format!("fichier `{}` non écrit", path.trim()));
    }
    if wanted == "run" {
        let started = s
            .runs
            .list(None, 50)
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.started_at >= since);
        return (!started).then(|| "aucun run lancé".to_string());
    }
    tracing::warn!(livrable = %wanted, "livrable de planification inconnu : ignoré");
    None
}

/// Fin du tour d'un prompt planifié, livrable compris (issue #120) : une exécution qui
/// répond sans livrer ce qu'elle a promis est un échec, prévenu comme ceux de #39, et
/// l'état qu'elle a consommé est remis pour que la suivante reprenne les mêmes éléments.
pub async fn trigger_outcome_of(
    d: &Context,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
    turn: &penelope_kernel::turn::Turn,
) {
    use crate::agent::TurnOutcome;
    if let TurnOutcome::AwaitingApproval { .. } = outcome {
        return trigger_outcome(d, ports, schedule_id, outcome).await;
    }
    let missing = match outcome {
        TurnOutcome::Answered { .. } => missing_deliverable(d, turn, outcome).await,
        _ => None,
    };
    let delivered = matches!(outcome, TurnOutcome::Answered { .. }) && missing.is_none();
    settle_state(d, &turn.session_id, !delivered).await;
    match missing {
        None => trigger_outcome(d, ports, schedule_id, outcome).await,
        Some(what) => {
            let s = &d.services;
            let reason = format!("exécutée sans livrable : {what}");
            if let Err(e) = s.schedules.record_outcome(schedule_id, Some(&reason)).await {
                tracing::warn!(schedule = %schedule_id, error = %e, "issue de planification non enregistrée");
            }
            if let Ok(Some(sched)) = s.schedules.get(schedule_id).await {
                alert(d, ports, &sched, &reason).await;
            }
        }
    }
}

/// Textes envoyés avec succès par `send_message` pendant le tour d'une session planifiée
/// (chaque exécution a sa session : tout son historique est ce tour).
pub(super) async fn sent_during_turn(s: &Services, session_id: &str) -> Vec<String> {
    let history = s
        .context
        .history
        .load(session_id, 0)
        .await
        .unwrap_or_default();
    let failed: std::collections::BTreeSet<String> = history
        .iter()
        .filter(|e| e.message.name.as_deref() == Some("send_message"))
        .filter(|e| e.message.text().starts_with("Erreur"))
        .filter_map(|e| e.message.tool_call_id.clone())
        .collect();
    history
        .iter()
        .flat_map(|e| e.message.tool_calls.iter())
        .filter(|c| c.name == "send_message" && !failed.contains(&c.id))
        .filter_map(|c| {
            c.arguments
                .get("text")
                .and_then(|t| t.as_str())
                .map(String::from)
        })
        .collect()
}

/// Texte ramené à ses mots : casse, ponctuation, emoji et mise en forme retirés.
pub(super) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Vrai si `final_text` répète un message déjà parti (issue #133) : même texte à la mise
/// en forme près, l'un contenu dans l'autre, ou le même corps sous une autre première
/// ligne (un en-tête).
pub fn repeats(final_text: &str, sent: &str) -> bool {
    let (a, b) = (words(final_text), words(sent));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let (ja, jb) = (a.join(" "), b.join(" "));
    if ja == jb || ja.contains(&jb) || jb.contains(&ja) {
        return true;
    }
    let body = |t: &str| words(t.split_once('\n').map(|(_, rest)| rest).unwrap_or("")).join(" ");
    let (ba, bb) = (body(final_text), body(sent));
    !ba.is_empty() && ba == bb
}

/// Pour un tour planifié, la réponse finale est le livrable (issue #133) : elle part,
/// sauf si l'agent a déjà envoyé le même contenu à la même cible pendant le tour
/// (`send_message` part toujours vers l'origine du tour). Un message intermédiaire
/// différent, ou un `send_message` en échec, n'empêche jamais la réponse finale.
pub async fn final_already_sent(s: &Services, session_id: &str, final_text: &str) -> bool {
    sent_during_turn(s, session_id)
        .await
        .iter()
        .any(|m| repeats(final_text, m))
}

/// Fin du tour d'un prompt planifié : l'exécution compte si le modèle a répondu, sinon la
/// raison est gardée et le propriétaire prévenu (issue #39).
pub async fn trigger_outcome(
    d: &Context,
    ports: &Ports,
    schedule_id: &str,
    outcome: &crate::agent::TurnOutcome,
) {
    use crate::agent::TurnOutcome;
    let s = &d.services;
    let Ok(Some(sched)) = s.schedules.get(schedule_id).await else {
        return;
    };
    let (error, warn) = match outcome {
        TurnOutcome::Answered { .. } => (None, false),
        // La carte d'approbation est partie : l'exécution reprendra après la décision.
        TurnOutcome::AwaitingApproval { .. } => return,
        TurnOutcome::LoopAborted { .. } => (
            Some("boucle d'outils arrêtée (réponse envoyée)".to_string()),
            false,
        ),
        TurnOutcome::Failed { error } => (Some(format!("erreur du modèle : {error}")), true),
        TurnOutcome::BudgetExceeded { scope, .. } => {
            (Some(format!("budget {scope} atteint")), true)
        }
        TurnOutcome::Cancelled => (Some("exécution interrompue".to_string()), true),
    };
    if let Err(e) = s
        .schedules
        .record_outcome(schedule_id, error.as_deref())
        .await
    {
        tracing::warn!(schedule = %schedule_id, error = %e, "issue de planification non enregistrée");
    }
    if let (Some(reason), true) = (error, warn) {
        alert(d, ports, &sched, &reason).await;
    }
}

/// Tours de prompts planifiés annulés dans la file sans jamais tourner (session fermée,
/// `/stop`) : erreur gardée, propriétaire prévenu. Le premier passage prend l'instant sans
/// rien signaler.
pub(super) async fn cancelled_triggers(d: &Context, ports: &Ports) -> anyhow::Result<()> {
    const CURSOR: &str = "scheduler.cancelled_cursor";
    let s = &d.services;
    let now = s.clock.now_rfc3339();
    let Some(cursor) = s.kv_get(CURSOR).await? else {
        s.kv_set(CURSOR, &now).await?;
        return Ok(());
    };
    let since = cursor.clone();
    let rows: Vec<(String, String, Option<String>)> = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT payload, finished_at, last_error FROM turn_queue
                 WHERE kind = 'trigger' AND state = 'cancelled' AND finished_at > ?1
                 ORDER BY finished_at",
            )?;
            let rows = st.query_map([&since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    let mut last = cursor;
    for (payload, finished, error) in rows {
        last = last.max(finished);
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let Some(id) = payload["schedule"].as_str() else {
            continue;
        };
        let Some(sched) = s.schedules.get(id).await? else {
            continue;
        };
        let reason = format!(
            "tour annulé ({})",
            error.unwrap_or_else(|| "sans raison".into())
        );
        s.schedules.record_outcome(id, Some(&reason)).await?;
        alert(d, ports, &sched, &reason).await;
    }
    s.kv_set(CURSOR, &last).await?;
    Ok(())
}
