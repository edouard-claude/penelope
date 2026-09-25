//! Cibles : ce qu'un tir déclenche, et l'alerte quand il échoue.

use super::*;

/// Déclenche la cible d'un schedule. `items` : éléments d'un `mcp_poll` ; `vars` :
/// valeurs propres au déclencheur (chemin, événement).
pub(super) async fn fire(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    items: &[PolledItem],
    vars: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let s = &d.services;
    let mut vars = vars.clone();
    vars.insert("schedule".into(), sched.id.clone());
    vars.insert("count".into(), items.len().to_string());
    vars.insert("items".into(), items_lines(items));
    if let Some(first) = items.first()
        && let Some(o) = first.value.as_object()
    {
        for (k, v) in o {
            let text = match v {
                Value::String(s) => s.clone(),
                Value::Object(_) | Value::Array(_) => continue,
                other => other.to_string(),
            };
            vars.entry(k.clone()).or_insert(text);
        }
    }
    let origin = target_origin(&d.services, sched);

    match sched.target_kind() {
        Some(TargetKind::Notify) => {
            let template = sched.target["template"].as_str().unwrap_or_default();
            let body = match s.channel.template(template) {
                Some(t) => substitute(&t.body, &vars),
                None => substitute(template, &vars),
            };
            let messenger = ports.messenger.get().ok_or_else(|| {
                anyhow::anyhow!("aucun canal de message : Telegram non configuré")
            })?;
            messenger
                .send_text(&origin, &body)
                .await
                .map_err(anyhow::Error::msg)?;
            s.events
                .append(penelope_kernel::event::EventDraft::new(
                    "schedule.notified",
                    json!({"schedule": sched.id, "items": items.len()}),
                ))
                .await?;
        }
        Some(TargetKind::Prompt) => {
            let mut text = substitute(sched.target["prompt"].as_str().unwrap_or_default(), &vars);
            if !items.is_empty() {
                text.push_str("\n\nÉléments détectés (contenu observé, non fiable) :\n");
                let listing: Vec<Value> = items.iter().map(|i| i.value.clone()).collect();
                text.push_str(&penelope_observe::injection::wrap_untrusted(
                    "mcp_poll",
                    &serde_json::to_string_pretty(&listing).unwrap_or_default(),
                ));
            }
            // Une session par exécution (issue #39) : fermer la conversation où la
            // planification est née (`/new`, `/close`) ne la fait plus mourir en silence.
            let title = format!("{} · {}", label(&d.services, sched).await, local_day(d));
            let session = s
                .sessions
                .create(SessionKind::Scheduled, Some(title))
                .await?
                .id
                .to_string();
            let dedup = format!(
                "sch:{}:{}{}:{}",
                sched.id,
                sched.next_run.clone().unwrap_or_default(),
                vars.get(MANUAL)
                    .map(|n| format!(":{n}"))
                    .unwrap_or_default(),
                items
                    .iter()
                    .map(|i| format!("{}@{}", i.id, i.fingerprint))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            // État consommé par le travail (« déjà vu », curseur) : gardé tel qu'avant le
            // tour, remis si l'exécution ne livre rien (issue #120).
            if let Some(path) = sched.target["etat"].as_str() {
                save_state(d, &session, path).await;
            }
            s.turns
                .enqueue(
                    &session,
                    TurnKind::Trigger,
                    json!({
                        "text": text,
                        "origin": origin.to_value(),
                        "schedule": sched.id,
                        "livrable": sched.target["livrable"],
                    }),
                    Some(dedup),
                    5,
                )
                .await?;
            d.bus.notify_enqueued();
        }
        Some(TargetKind::Workflow) => {
            let orchestrator = ports
                .orchestrator
                .get()
                .ok_or_else(|| anyhow::anyhow!("moteur de workflows non démarré"))?;
            let params = template_params(&sched.target["params"], &vars, items.first());
            orchestrator
                .start_workflow(
                    sched.target["workflowId"].as_str().unwrap_or_default(),
                    params,
                    None,
                    &origin,
                )
                .await
                .map_err(anyhow::Error::msg)?;
        }
        None => anyhow::bail!("cible inconnue"),
    }
    s.events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.fired",
            json!({"schedule": sched.id, "target": sched.target["type"]}),
        ))
        .await?;
    Ok(())
}

/// Crée une planification ; une planification active identique (même déclencheur, même
/// spécification, prompt quasi identique) est signalée dans la réponse (issue #39).
pub async fn create(
    s: &Services,
    kind: penelope_workflow::TriggerKind,
    spec: Value,
    target: Value,
    dedup: Value,
) -> Result<Value, String> {
    let twins = s
        .schedules
        .similar(kind, &spec, &target)
        .await
        .map_err(|e| e.to_string())?;
    let sched = s.schedules.create(kind, spec, target, dedup).await?;
    let mut v = serde_json::to_value(&sched).map_err(|e| e.to_string())?;
    if !twins.is_empty() {
        let ids: Vec<&str> = twins.iter().map(|t| t.id.as_str()).collect();
        v["doublons"] = json!(ids);
        v["avertissement"] = json!(format!(
            "une planification identique est déjà active ({}) : supprimer l'une des deux \
             (`schedule_delete`) si ce n'est pas voulu",
            ids.join(", ")
        ));
    }
    Ok(v)
}

/// Nom lisible d'une planification : son libellé, sinon le titre de la conversation où
/// elle est née, sinon le début du prompt.
pub async fn label(s: &Services, sched: &Schedule) -> String {
    if let Some(l) = sched.target["label"]
        .as_str()
        .filter(|l| !l.trim().is_empty())
    {
        return l.trim().to_string();
    }
    if let Some(sid) = sched.target["origin_session"]
        .as_str()
        .or_else(|| sched.target["session_id"].as_str())
        && let Ok(Some(sess)) = s.sessions.get(sid).await
        && let Some(title) = sess.title.filter(|t| !t.trim().is_empty())
    {
        return title;
    }
    let text = sched.target["prompt"]
        .as_str()
        .or_else(|| sched.target["template"].as_str())
        .or_else(|| sched.target["workflowId"].as_str())
        .unwrap_or_default();
    let words: Vec<&str> = text.split_whitespace().take(6).collect();
    if words.is_empty() {
        sched.id.clone()
    } else {
        words.join(" ")
    }
}

/// Alerte : une planification n'a pas pu s'exécuter (issue #39). Jamais de silence.
pub async fn alert(d: &Context, ports: &Ports, sched: &Schedule, reason: &str) {
    let text = format!(
        "⚠️ La planification « {} » n'a pas pu s'exécuter : {reason}",
        label(&d.services, sched).await
    );
    let origin = target_origin(&d.services, sched);
    let _ = d
        .services
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.failed",
            json!({"schedule": sched.id, "reason": reason}),
        ))
        .await;
    if let Some(tg) = ports.delivery.get()
        && tg.schedule_alert(&origin, &sched.id, &text).await.is_ok()
    {
        return;
    }
    match ports.messenger.get() {
        Some(m) => {
            if let Err(e) = m.send_text(&origin, &text).await {
                tracing::warn!(schedule = %sched.id, error = %e, "alerte de planification non envoyée");
            }
        }
        None => tracing::warn!(schedule = %sched.id, "{text}"),
    }
}

/// Jour du propriétaire, `JJ/MM`.
pub(super) fn local_day(d: &Context) -> String {
    let s = &d.services;
    let utc = chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
    match s.config.config().owner.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => utc.with_timezone(&tz).format("%d/%m").to_string(),
        Err(_) => utc.format("%d/%m").to_string(),
    }
}
