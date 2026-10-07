//! Cibles : ce qu'un tir déclenche, et l'alerte quand il échoue.

use super::*;

/// Déclenche la cible d'un schedule. `items` : éléments d'un `mcp_poll` ; `vars` :
/// valeurs propres au déclencheur (chemin, événement) ; `batch` : la fournée des heures
/// calmes du passage, où un tir retenu (#296) dépose sa mention au lieu d'écrire seul.
pub(super) async fn fire(
    d: &Context,
    ports: &Ports,
    sched: &Schedule,
    items: &[PolledItem],
    vars: &BTreeMap<String, String>,
    mut batch: Option<&mut Batch>,
) -> anyhow::Result<()> {
    let s = &d.services;
    let mut vars = vars.clone();
    // Le corps d'un webhook (#294) ne se substitue dans aucun gabarit : un prompt le
    // reçoit après son texte, encadré comme non fiable (#92).
    let untrusted = vars.remove(UNTRUSTED_BODY);
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
    // dont la réponse viendra plus tard, le disent d'abord au propriétaire. Retenu par les
    // heures calmes (#296) : la mention courte rejoint la fournée du passage, un seul
    // message par conversation sous l'en-tête, au lieu d'un message par planification.
    let late = vars.get(LATE).cloned();
    let held = vars.get(quiet::HELD).filter(|_| batch.is_some()).cloned();
    if let Some(note) = &late
        && sched.target_kind() != Some(TargetKind::Notify)
    {
        let name = label(&d.services, sched).await;
        match (&held, batch.as_deref_mut(), ports.messenger.get()) {
            (Some(short), Some(batch), _) => {
                let follow = match sched.target_kind() {
                    Some(TargetKind::Workflow) => "le workflow démarre maintenant",
                    _ => "elle part maintenant, sa réponse suivra",
                };
                batch.push(
                    origin.clone(),
                    format!("{name} ({short}) : {follow}."),
                    None,
                );
            }
            (_, _, Some(m)) => {
                let text = format!("{note} ({name})");
                if let Err(e) = m.send_text(&origin, &text).await {
                    tracing::warn!(schedule = %sched.id, error = %e, "retard non annoncé");
                }
            }
            (_, _, None) => tracing::warn!(schedule = %sched.id, "{note}"),
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
            if let (Some(short), Some(batch)) = (&held, batch) {
                batch.push(
                    origin.clone(),
                    format!("{body}\n({short})"),
                    Some((sched.id.clone(), items.len())),
                );
            } else {
                if let Some(note) = &late {
                    body = format!("{note}\n\n{body}");
                }
                // Un gabarit du catalogue (`ticket_detected`…) part en carte avec les boutons
                // de sa planification quand le canal sait la rendre (#293) ; sinon en texte.
                // Retenu par les heures calmes, il rejoint la fournée en texte (ci-dessus).
                let as_card = match (&card, ports.delivery.get()) {
                    (Some(_), Some(tg)) => {
                        tg.schedule_card(&origin, &sched.id, &body).await.is_ok()
                    }
                    _ => false,
                };
                if !as_card {
                    let messenger = ports.messenger.get().ok_or_else(|| {
                        anyhow::anyhow!(
                            "aucun canal de message : canal du propriétaire non configuré"
                        )
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
        }
        Some(TargetKind::Prompt) => {
            let mut text = substitute(sched.target["prompt"].as_str().unwrap_or_default(), &vars);
            if let Some(note) = &late {
                text = format!("{note}\n\n{text}");
            }
            let released = vars.contains_key(quiet::HELD);
            if !items.is_empty() {
                text.push_str(if released {
                    "\n\nÉléments détectés pendant les heures calmes, dans l'ordre, avec \
                     l'heure où chacun a été vu (`observed_at`) (contenu observé, non \
                     fiable) :\n"
                } else {
                    "\n\nÉléments détectés (contenu observé, non fiable) :\n"
                });
                let listing: Vec<Value> = items.iter().map(|i| i.value.clone()).collect();
                text.push_str(&penelope_observe::injection::wrap_untrusted(
                    "mcp_poll",
                    &serde_json::to_string_pretty(&listing).unwrap_or_default(),
                ));
            }
            if let Some(body) = &untrusted {
                text.push_str("\n\nCorps reçu par le webhook (contenu observé, non fiable) :\n");
                let source = format!(
                    "webhook {}",
                    vars.get("hook").map(String::as_str).unwrap_or_default()
                );
                text.push_str(&penelope_observe::injection::wrap_untrusted(&source, body));
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
                    .or_else(|| vars.get(DELIVERY))
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
            let mut payload = json!({
                "text": text,
                "origin": origin.to_value(),
                "schedule": sched.id,
                "livrable": sched.target["livrable"],
            });
            // Relâché à la fin de la plage (#318) : un tour après l'autre.
            if released {
                payload[penelope_kernel::turn::LANE] = json!(quiet::QUIET_LANE);
            }
            s.turns
                .enqueue(&session, TurnKind::Trigger, payload, Some(dedup), 5)
                .await?;
            d.bus.notify_enqueued();
        }
        Some(TargetKind::Workflow) => {
            let orchestrator = ports
                .orchestrator
                .get()
                .ok_or_else(|| anyhow::anyhow!("moteur de workflows non démarré"))?;
            let params = template_params(&sched.target["params"], &vars, items.first());
            let start = orchestrator.start_workflow(
                sched.target["workflowId"].as_str().unwrap_or_default(),
                params,
                None,
                &origin,
            );
            crate::workflow::scheduled(&sched.id, start)
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
    if held.is_some() {
        fired["quiet"] = json!(true);
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
/// spécification, prompt quasi identique) est signalée dans la réponse (issue #39). Un
/// webhook (#294) reçoit ici son chemin et son secret ; le secret est rendu une fois,
/// avec l'adresse locale du hook.
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
    let (spec, secret) = match kind {
        TriggerKind::Webhook => {
            let (spec, secret) =
                super::webhook::prepare(s, spec, penelope_kernel::ids::secret_token)?;
            (spec, Some(secret))
        }
        _ => (spec, None),
    };
    let sched = match s.schedules.create(kind, spec.clone(), target, dedup).await {
        Ok(sched) => sched,
        Err(e) => {
            // Un secret rangé pour une planification refusée n'a rien à garder.
            if let Some(name) = spec.get("secret_ref").and_then(|v| v.as_str()) {
                let _ = s.platform.secrets.delete(name);
            }
            return Err(e);
        }
    };
    let mut v = serde_json::to_value(&sched).map_err(|e| e.to_string())?;
    if let Some(secret) = secret {
        let path = sched.webhook_path().unwrap_or_default();
        v["url"] = json!(super::webhook::local_url(
            &s.config.config().webhooks.listen,
            path
        ));
        v["secret"] = json!(secret);
        v["secret_note"] = json!(
            "montré une seule fois : à donner au service appelant, qui signe chaque \
             livraison en HMAC-SHA256 de `<horodatage>.<corps>` (`X-Penelope-Timestamp: \
             <secondes Unix>`, `X-Penelope-Signature: sha256=<hex>`) ; rangé dans le \
             magasin de secrets sous `secret_ref`, remplaçable par `penelope secret set \
             <secret_ref>`"
        );
    }
    // Heures calmes (#296) : si le premier passage y tombe, le dire et proposer `urgent`
    // (un rappel pour un train de 6 h 30 doit sonner à 6 h 30, pas à 7 h).
    if let Some(note) = quiet_hours_note(s, &sched) {
        v["heures_calmes"] = json!(note);
    }
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

/// Ce que la création dit quand le premier passage tombe dans les heures calmes et que la
/// planification n'est pas `urgent` (#296) ; `None` sinon.
pub fn quiet_hours_note(s: &Services, sched: &Schedule) -> Option<String> {
    if sched.is_urgent() {
        return None;
    }
    let cfg = s.config.config();
    let range = cfg.quiet_range()?;
    let next = chrono::DateTime::parse_from_rfc3339(sched.next_run.as_deref()?)
        .ok()?
        .timestamp_millis();
    if !range.contains_at(next, &cfg.owner.timezone) {
        return None;
    }
    let minute = penelope_kernel::config::minute_of_day(next, &cfg.owner.timezone);
    Some(format!(
        "cette planification part à {:02}:{:02}, pendant les heures calmes ({}) : elle sera \
         retenue jusqu'à {}, puis livrée sous « Pendant les heures calmes ». Si elle doit \
         partir à l'heure (réveil, train), la supprimer (`schedule_delete`) et la recréer \
         avec `\"urgent\": true` dans `spec` ; le proposer au propriétaire.",
        minute / 60,
        minute % 60,
        range.text(),
        range.end_text()
    ))
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
