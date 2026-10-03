//! L'agenda dans l'« Aujourd'hui » du digest (#295).

use super::*;

/// #295 : l'« Aujourd'hui » du digest mêle les rendez-vous de l'agenda, lus par l'outil
/// MCP de `digest.agenda` dans le fuseau du propriétaire, aux planifications du jour,
/// rangés par heure ; éteint par défaut ; un outil inconnu, en écriture ou en erreur est
/// dit dans le digest sans le faire tomber.
#[tokio::test]
async fn the_digest_mixes_agenda_events_with_the_schedules_of_the_day() {
    use penelope_mcp_host::testing::{FakeConnector, declare};
    let (d, clock, _rec) = harness().await;
    let s = d.services.clone();
    s.channel.delivery.set(Some(Arc::new(Places)));
    let calls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "agenda",
        Arc::new(move |m, p| match m {
            "initialize" => Ok(json!({"protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}}, "serverInfo": {"name": "agenda"}})),
            "tools/list" => Ok(json!({"tools": [
                {"name": "events_today", "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true}},
                {"name": "event_create", "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": false}}
            ]})),
            "tools/call" => {
                seen.lock().unwrap().push(p.clone());
                if p["arguments"]["timezone"] == "Mars/Olympus" {
                    return Ok(json!({"content": [{"type": "text",
                        "text": "identifiants refusés par le serveur CalDAV (HTTP 401)"}],
                        "isError": true}));
                }
                Ok(json!({
                    "content": [{"type": "text", "text": "2 événement(s)"}],
                    "structuredContent": {"events": [
                        {"summary": "Dentiste", "all_day": false, "calendar": "Perso",
                         "start": "2026-01-01T07:00:00Z", "end": "2026-01-01T07:45:00Z"},
                        {"summary": "Fête", "all_day": true, "calendar": "Famille",
                         "start": "2026-01-01", "end": "2026-01-01"},
                        {"summary": "Apéro", "all_day": false,
                         "start": "2026-01-01T18:30:00+04:00", "end": "2026-01-01T20:00:00+04:00"}
                    ]}
                }))
            }
            "server/discover" => Err(penelope_mcp::McpError::Rpc {
                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                message: "Method not found".into(),
                data: None,
            }),
            _ => Ok(json!({})),
        }),
    );
    let sup = penelope_mcp_host::testing::supervisor(s.clone(), fake);
    declare(&sup, "agenda", "");
    sup.reload().await;
    d.set_mcp(sup.clone());

    // 2026-01-01 à La Réunion : une veille à 9 h.
    s.schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            json!({"type": "notify", "template": "Veille du matin", "label": "Veille du matin"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.set_ms(1_767_236_400_000); // 2026-01-01T03:00:00Z = 07:00 à La Réunion
    tick(&d, &d.scheduler()).await.unwrap();

    // Éteint par défaut : pas d'appel, pas d'erreur.
    let feed = DigestFeed {
        services: s.clone(),
        mcp: d.scheduler().mcp,
    };
    let inputs = penelope_dream::DigestSource::digest_inputs(&feed).await;
    assert_eq!(
        inputs.due_today,
        vec!["- 09:00 Veille du matin → conv 42 (par défaut)"]
    );
    assert!(inputs.agenda_error.is_none());
    assert!(calls.lock().unwrap().is_empty());

    s.publish_config("test", |c| {
        c.digest.agenda = "mcp__agenda__events_today".into();
        Ok(vec!["digest.agenda".into()])
    })
    .unwrap();
    let inputs = penelope_dream::DigestSource::digest_inputs(&feed).await;
    assert_eq!(
        inputs.due_today,
        vec![
            "- journée Fête (Famille)",
            "- 09:00 Veille du matin → conv 42 (par défaut)",
            "- 11:00–11:45 Dentiste (Perso)",
            "- 18:30–20:00 Apéro",
        ]
    );
    assert!(inputs.agenda_error.is_none());
    let call = calls.lock().unwrap().last().cloned().unwrap();
    assert_eq!(call["name"], "events_today");
    assert_eq!(call["arguments"]["timezone"], "Indian/Reunion");
    let digest = penelope_dream::digest_text(&d.dream(), inputs, Some(sup.clone()))
        .await
        .unwrap();
    assert!(
        digest.contains("🗓 Aujourd'hui :\n- journée Fête (Famille)\n- 09:00 Veille"),
        "{digest}"
    );
    assert!(!digest.contains("Agenda non lu"), "{digest}");

    // Sans superviseur branché, l'agenda est dit non lu, les planifications restent.
    let inputs = digest_inputs(&s).await;
    assert_eq!(inputs.due_today.len(), 1);
    assert!(
        inputs
            .agenda_error
            .as_deref()
            .unwrap()
            .contains("superviseur MCP"),
        "{inputs:?}"
    );

    // Un outil en erreur : sa réponse, telle quelle.
    s.publish_config("test", |c| {
        c.owner.timezone = "Mars/Olympus".into();
        Ok(vec!["owner.timezone".into()])
    })
    .ok();
    let inputs = digest_inputs_with(&s, Some(sup.clone())).await;
    if let Some(why) = &inputs.agenda_error {
        assert!(why.contains("identifiants refusés"), "{why}");
    }
    s.publish_config("test", |c| {
        c.owner.timezone = "Indian/Reunion".into();
        Ok(vec!["owner.timezone".into()])
    })
    .unwrap();

    // Un outil en écriture ou inconnu est refusé par le digest, pas appelé.
    let before = calls.lock().unwrap().len();
    for (tool, expected) in [
        ("mcp__agenda__event_create", "en lecture"),
        ("mcp__agenda__events_tomorrow", "inconnu"),
    ] {
        s.publish_config("test", |c| {
            c.digest.agenda = tool.into();
            Ok(vec!["digest.agenda".into()])
        })
        .unwrap();
        let inputs = digest_inputs_with(&s, Some(sup.clone())).await;
        assert!(
            inputs.agenda_error.as_deref().unwrap().contains(expected),
            "{tool} : {inputs:?}"
        );
        assert_eq!(inputs.due_today.len(), 1);
    }
    assert_eq!(calls.lock().unwrap().len(), before, "rien n'a été appelé");
    let digest = penelope_dream::digest_text(&d.dream(), inputs_with_error(), Some(sup))
        .await
        .unwrap();
    assert!(
        digest.contains("🗓 Agenda non lu : outil `x` inconnu"),
        "{digest}"
    );
}

fn inputs_with_error() -> penelope_dream::DigestInputs {
    penelope_dream::DigestInputs {
        agenda_error: Some("outil `x` inconnu".into()),
        ..Default::default()
    }
}
