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
    // Créneau en retard (#228) : la notification le dit en tête ; un prompt ou un workflow,
    // dont la réponse viendra plus tard, le disent d'abord au propriétaire.
    let late = vars.get(LATE).cloned();
    if let Some(note) = &late
        && sched.target_kind() != Some(TargetKind::Notify)
    {
        match ports.messenger.get() {
            Some(m) => {
                let text = format!("{note} ({})", label(&d.services, sched).await);
                if let Err(e) = m.send_text(&origin, &text).await {
                    tracing::warn!(schedule = %sched.id, error = %e, "retard non annoncé");
                }
            }
            None => tracing::warn!(schedule = %sched.id, "{note}"),
        }
    }

    match sched.target_kind() {
        Some(TargetKind::Notify) => {
            let template = sched.target["template"].as_str().unwrap_or_default();
            let card = s.channel.template(template);
            let mut body = match &card {
                Some(t) => substitute(&t.body, &vars),
                None => substitute(template, &vars),
            };
            if let Some(note) = &late {
                body = format!("{note}\n\n{body}");
            }
            // Un gabarit du catalogue (`ticket_detected`…) part en carte avec les boutons
            // de sa planification quand le canal sait la rendre (#293) ; sinon en texte.
            let as_card = match (&card, ports.delivery.get()) {
                (Some(_), Some(tg)) => tg.schedule_card(&origin, &sched.id, &body).await.is_ok(),
                _ => false,
            };
            if !as_card {
                let messenger = ports.messenger.get().ok_or_else(|| {
                    anyhow::anyhow!("aucun canal de message : canal du propriétaire non configuré")
                })?;
                messenger
                    .send_text(&origin, &body)
                    .await
                    .map_err(anyhow::Error::msg)?;
            }
            s.events
                .append(penelope_kernel::event::EventDraft::new(
                    "schedule.notified",
                    json!({"schedule": sched.id, "items": items.len()}),
                ))
                .await?;
        }
        Some(TargetKind::Prompt) => {
            let mut text = substitute(sched.target["prompt"].as_str().unwrap_or_default(), &vars);
            if let Some(note) = &late {
                text = format!("{note}\n\n{text}");
            }
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
    let mut fired = json!({"schedule": sched.id, "target": sched.target["type"]});
    if let Some(note) = late {
        fired["planned"] = json!(sched.next_run);
        fired["late"] = json!(note);
    }
    s.events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.fired",
            fired,
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

/// Alerte : une planification n'a pas pu s'exécuter (issue #39). L'échec entre toujours
/// au journal ; le message part au premier échec de la série, quand le motif change, et
/// aux paliers (#229) : une planification cassée n'en envoie pas un à chaque exécution,
/// que le propriétaire finirait par ne plus lire. Appelée après l'enregistrement de
/// l'échec, qui a allongé la série.
pub async fn alert(d: &Context, ports: &Ports, sched: &Schedule, reason: &str) {
    let s = &d.services;
    let due = match s.schedules.alert_due(&sched.id, reason).await {
        Ok(due) => due,
        Err(e) => {
            tracing::warn!(schedule = %sched.id, error = %e, "série d'échecs illisible : alerte envoyée");
            Some(1)
        }
    };
    let _ = s
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.failed",
            json!({"schedule": sched.id, "reason": reason, "alerted": due.is_some()}),
        ))
        .await;
    let Some(failures) = due else {
        tracing::info!(schedule = %sched.id, "échec de planification au même motif : alerte tue");
        return;
    };
    let streak = if failures > 1 {
        format!(" ({failures} échecs de suite)")
    } else {
        String::new()
    };
    let text = format!(
        "⚠️ La planification « {} » n'a pas pu s'exécuter{streak} : {reason}",
        label(s, sched).await
    );
    let origin = target_origin(s, sched);
    if let Some(tg) = ports.delivery.get()
        && tg.schedule_alert(&origin, &sched.id, &text).await.is_ok()
    {
        return;
    }
    send(ports, sched, &origin, &text).await;
}

/// Retour au succès d'une planification dont la panne avait été signalée (#229).
pub async fn recovered(d: &Context, ports: &Ports, sched: &Schedule, failures: u32) {
    let s = &d.services;
    let _ = s
        .events
        .append(penelope_kernel::event::EventDraft::new(
            "schedule.recovered",
            json!({"schedule": sched.id, "failures": failures}),
        ))
        .await;
    let text = format!(
        "✅ La planification « {} » est rétablie après {failures} échec{}.",
        label(s, sched).await,
        if failures > 1 { "s" } else { "" }
    );
    send(ports, sched, &target_origin(s, sched), &text).await;
}

async fn send(ports: &Ports, sched: &Schedule, origin: &Origin, text: &str) {
    match ports.messenger.get() {
        Some(m) => {
            if let Err(e) = m.send_text(origin, text).await {
                tracing::warn!(schedule = %sched.id, error = %e, "message de planification non envoyé");
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
