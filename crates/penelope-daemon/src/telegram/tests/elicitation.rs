use super::*;

/// Issue #12 : une demande `elicitation/create` devient une carte Telegram ; le clic du
/// propriétaire part au serveur (confirmation, formulaire, refus, lien), un délai
/// dépassé annule et le dit.
#[tokio::test]
async fn mcp_elicitation_is_answered_from_telegram() {
    use crate::mcp::testing::{FakeConnector, declare, server, tool};
    use penelope_mcp::transport::LoopbackTransport;
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    for name in ["redmine", "lent"] {
        fake.serve(
            name,
            server(Arc::new(std::sync::Mutex::new(vec![tool(
                "update_issue",
                json!({}),
            )]))),
        );
    }
    let sup = crate::mcp::testing::supervisor(g.daemon.services.clone(), fake.clone());
    declare(&sup, "redmine", "");
    declare(&sup, "lent", "elicitation_timeout = \"300ms\"\n");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    let broker = g.daemon.services.elicitations.clone();

    // Un propriétaire est joignable : l'élicitation est annoncée, pas le sampling.
    let tr = fake.last_transport("redmine");
    let log = tr.call_log().await;
    let (_, init) = log.iter().find(|(m, _)| m == "initialize").unwrap();
    assert!(init["capabilities"]["elicitation"].is_object(), "{init}");
    assert!(init["capabilities"].get("sampling").is_none(), "{init}");

    let answer = |tr: Arc<LoopbackTransport>, id: u64| async move {
        for _ in 0..300 {
            if let Some((_, r)) = tr
                .responses
                .lock()
                .await
                .iter()
                .find(|(i, _)| *i == json!(id))
            {
                return r.clone().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("aucune réponse à la requête {id}");
    };
    // La carte part par la file, comme les cartes d'approbation (issue #143) : le
    // test la pousse, puis la lit. La file ne rend pas d'identifiant de message ;
    // celui du clic suffit, et c'est lui qui sera modifié en place.
    let card = |needle: &'static str| {
        let (t, broker, g) = (t.clone(), broker.clone(), g.clone());
        async move {
            for i in 0..300 {
                let _ = g.flush_outbox().await;
                let open = broker.open();
                if let Some((req, _)) = open.iter().find(|(r, _)| r.message.contains(needle))
                    && let Some(sent) = t
                        .calls_to(tg::SEND_MESSAGE)
                        .await
                        .into_iter()
                        .rev()
                        .find(|c| c["text"].as_str().is_some_and(|x| x.contains(needle)))
                {
                    return (req.clone(), 9_000 + i, sent);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("aucune carte « {needle} »");
        }
    };

    // 1. Confirmation : Accepter.
    tr.push_server_request(
        8,
        "elicitation/create",
        json!({"message": "Modifier le ticket 42 ?",
               "requestedSchema": {"type": "object", "properties": {}}}),
    );
    let (_, card_id, sent) = card("Modifier le ticket 42").await;
    let text = sent["text"].as_str().unwrap();
    assert!(text.contains("<code>redmine</code>"), "{text}");
    assert!(text.contains("Sans réponse d'ici 10 min"), "{text}");
    let b = inline_buttons(&sent);
    let labels: Vec<&str> = b.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, ["✅ Accepter", "🚫 Refuser", "✖️ Annuler"]);
    g.process_update(&updates::callback(200, OWNER, &b[0].1, card_id))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        answer(tr.clone(), 8).await,
        json!({"action": "accept", "content": {}})
    );
    let edits = t.calls_to(tg::EDIT_MESSAGE_TEXT).await;
    assert!(
        edits.last().unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Accepté"),
        "{edits:?}"
    );

    // 2. Formulaire : Remplir, un enum titré au bouton, un texte en message, Envoyer.
    tr.push_server_request(
        9,
        "elicitation/create",
        json!({"message": "Précise la priorité",
               "requestedSchema": {"type": "object", "properties": {
                   "priorite": {"type": "string", "title": "Priorité", "oneOf": [
                       {"const": "high", "title": "Haute"},
                       {"const": "low", "title": "Basse"}]},
                   "note": {"type": "string", "title": "Note"}},
               "required": ["priorite"]}}),
    );
    let (_, card_id, sent) = card("Précise la priorité").await;
    assert!(sent["text"].as_str().unwrap().contains("Priorité *, Note"));
    let fill = inline_buttons(&sent)[0].clone();
    assert_eq!(fill.0, "📝 Remplir");
    g.process_update(&updates::callback(201, OWNER, &fill.1, card_id))
        .await
        .unwrap();
    settle_click(&g).await;
    let step = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    let high = inline_buttons(&step)
        .into_iter()
        .find(|(l, _)| l == "Haute")
        .unwrap();
    g.process_update(&updates::callback(202, OWNER, &high.1, 3000))
        .await
        .unwrap();
    settle_click(&g).await;
    g.process_update(&updates::text_message(
        203,
        OWNER,
        OWNER,
        "client en attente",
    ))
    .await
    .unwrap();
    let summary = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    let buttons = inline_buttons(&summary);
    assert!(
        buttons.iter().any(|(l, _)| l == "🚫 Refuser"),
        "{buttons:?}"
    );
    let send = buttons.iter().find(|(l, _)| l == "✅ Envoyer").unwrap();
    g.process_update(&updates::callback(204, OWNER, &send.1, 3001))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(
        answer(tr.clone(), 9).await,
        json!({"action": "accept",
               "content": {"priorite": "high", "note": "client en attente"}})
    );
    assert!(
        g.daemon
            .services
            .turns
            .claim("test")
            .await
            .unwrap()
            .is_none(),
        "la saisie du formulaire n'ouvre pas de tour"
    );

    // 3. Refus.
    tr.push_server_request(
        10,
        "elicitation/create",
        json!({"message": "Supprimer le ticket 7 ?"}),
    );
    let (_, card_id, sent) = card("Supprimer le ticket 7").await;
    let decline = inline_buttons(&sent)[1].clone();
    g.process_update(&updates::callback(205, OWNER, &decline.1, card_id))
        .await
        .unwrap();
    settle_click(&g).await;
    assert_eq!(answer(tr.clone(), 10).await, json!({"action": "decline"}));

    // 4. Sans réponse : annulation, carte mise à jour.
    let slow = fake.last_transport("lent");
    slow.push_server_request(
        11,
        "elicitation/create",
        json!({"message": "Toujours là ?"}),
    );
    assert_eq!(answer(slow.clone(), 11).await, json!({"action": "cancel"}));
    // Sans clic, la file n'a pas d'identifiant de message à modifier : l'issue arrive
    // en message, dans la conversation où la carte a été posée (issue #143), avec un
    // bouton « Relancer » quand la demande a une session.
    let _ = g.flush_outbox().await;
    let seen = [
        texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await),
        texts(&t.calls_to(tg::SEND_MESSAGE).await),
    ]
    .concat();
    assert!(
        seen.iter()
            .any(|e| e.contains("Toujours là") && e.contains("Sans réponse")),
        "{seen:?}"
    );
}

/// Issue #12, suite : en 2026-07-28 (MRTR) l'appel est relancé avec les réponses et
/// l'état du serveur, le modèle lit qui a répondu ; un lien (2025-11-25) montre son
/// domaine, s'ouvre après accord et sa fin signalée met la carte à jour.
#[tokio::test]
#[allow(clippy::too_many_lines)] // gel 0.17 : scénario de test bout en bout
async fn mcp_links_and_mrtr_elicitations_from_telegram() {
    use crate::executor::McpGateway;
    use crate::mcp::testing::{FakeConnector, declare, server, tool};
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "tracker",
        Arc::new(|m, p| match m {
            "server/discover" => Ok(json!({
                "protocolVersion": "2026-07-28",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tracker"}
            })),
            "tools/list" => Ok(json!({"tools": [tool("close_ticket", json!({}))]})),
            "tools/call" => Ok(
                match (p.get("inputResponses"), p["arguments"]["lien"].as_bool()) {
                    (Some(r), _) => {
                        json!({"content": [{"type": "text", "text": format!("reçu {r}")}]})
                    }
                    (None, Some(true)) => json!({
                        "resultType": "input_required",
                        "inputRequests": {"compte": {"method": "elicitation/create", "params": {
                            "mode": "url", "url": "https://auth.example.com/connect?t=1",
                            "message": "Relie ton compte."}}},
                        "requestState": "etat-lien"
                    }),
                    (None, _) => json!({
                        "resultType": "input_required",
                        "inputRequests": {"confirm": {"method": "elicitation/create",
                            "params": {"message": "Fermer le ticket 9 ?"}}},
                        "requestState": "etat-1"
                    }),
                },
            ),
            _ => Ok(json!({})),
        }),
    );
    let legacy = server(Arc::new(std::sync::Mutex::new(vec![tool(
        "sync",
        json!({}),
    )])));
    fake.serve(
        "drive",
        Arc::new(move |m, p| match m {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "drive"}
            })),
            _ => legacy(m, p),
        }),
    );
    let sup = crate::mcp::testing::supervisor(g.daemon.services.clone(), fake.clone());
    declare(&sup, "tracker", "");
    declare(&sup, "drive", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    let broker = g.daemon.services.elicitations.clone();
    let card = |needle: &'static str| {
        let (t, broker, g) = (t.clone(), broker.clone(), g.clone());
        async move {
            for i in 0..300 {
                let _ = g.flush_outbox().await;
                if let Some((req, _)) = broker
                    .open()
                    .into_iter()
                    .find(|(r, _)| r.message.contains(needle))
                    && let Some(sent) = t
                        .calls_to(tg::SEND_MESSAGE)
                        .await
                        .into_iter()
                        .rev()
                        .find(|c| c["text"].as_str().is_some_and(|x| x.contains(needle)))
                {
                    return (req, 9_500 + i, sent);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("aucune carte « {needle} »");
        }
    };

    // MRTR, formulaire : refus, relance avec la réponse et l'état, note pour le modèle.
    let call = {
        let sup = sup.clone();
        tokio::spawn(async move {
            sup.call_tool("mcp__tracker__close_ticket", &json!({}), Default::default())
                .await
        })
    };
    let (_, card_id, sent) = card("Fermer le ticket 9").await;
    g.process_update(&updates::callback(
        300,
        OWNER,
        &inline_buttons(&sent)[1].1,
        card_id,
    ))
    .await
    .unwrap();
    settle_click(&g).await;
    let result = call.await.unwrap().unwrap();
    let text = result.to_string();
    assert!(
        text.contains(r#"reçu {\"confirm\":{\"action\":\"decline\"}}"#),
        "{text}"
    );
    assert!(
        text.contains("le propriétaire a refusé sur Telegram (decline)"),
        "{text}"
    );
    let tr = fake.last_transport("tracker");
    let log = tr.call_log().await;
    let retry = &log
        .iter()
        .filter(|(m, _)| m == "tools/call")
        .nth(1)
        .unwrap()
        .1;
    assert_eq!(retry["requestState"], "etat-1");
    assert_eq!(
        retry["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"],
        json!({"form": {}, "url": {}})
    );

    // MRTR, lien : domaine et URL entière, accord, puis « J'ai terminé ».
    let call = {
        let sup = sup.clone();
        tokio::spawn(async move {
            sup.call_tool(
                "mcp__tracker__close_ticket",
                &json!({"lien": true}),
                Default::default(),
            )
            .await
        })
    };
    let (_, card_id, sent) = card("Relie ton compte").await;
    let html = sent["text"].as_str().unwrap();
    assert!(html.contains("Domaine : <b>auth.example.com</b>"), "{html}");
    assert!(
        html.contains("<code>https://auth.example.com/connect?t=1</code>"),
        "{html}"
    );
    g.process_update(&updates::callback(
        301,
        OWNER,
        &inline_buttons(&sent)[0].1,
        card_id,
    ))
    .await
    .unwrap();
    settle_click(&g).await;
    let edited = t
        .calls_to(tg::EDIT_MESSAGE_TEXT)
        .await
        .last()
        .unwrap()
        .clone();
    let buttons = inline_buttons(&edited);
    assert_eq!(
        buttons[0],
        (
            "🌐 Ouvrir auth.example.com".to_string(),
            "https://auth.example.com/connect?t=1".to_string()
        )
    );
    let done = buttons
        .iter()
        .find(|(l, _)| l == "✅ J'ai terminé")
        .unwrap();
    g.process_update(&updates::callback(302, OWNER, &done.1, card_id))
        .await
        .unwrap();
    settle_click(&g).await;
    let text = call.await.unwrap().unwrap().to_string();
    assert!(
        text.contains(r#"{\"compte\":{\"action\":\"accept\"}}"#),
        "{text}"
    );

    // 2025-11-25, requête du serveur en mode URL : accord, puis fin signalée.
    let drive = fake.last_transport("drive");
    drive.push_server_request(
        21,
        "elicitation/create",
        json!({"mode": "url", "elicitationId": "e-9",
               "url": "https://drive.example.org/oauth", "message": "Autorise Drive."}),
    );
    let (_, card_id, sent) = card("Autorise Drive").await;
    g.process_update(&updates::callback(
        303,
        OWNER,
        &inline_buttons(&sent)[0].1,
        card_id,
    ))
    .await
    .unwrap();
    settle_click(&g).await;
    for _ in 0..300 {
        if !drive.responses.lock().await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        drive.responses.lock().await[0].1.clone().unwrap(),
        json!({"action": "accept"})
    );
    drive.push_notification(
        "notifications/elicitation/complete",
        json!({"elicitationId": "e-9"}),
    );
    let mut finished = false;
    for _ in 0..300 {
        let _ = g.flush_outbox().await;
        // La carte posée par la file n'a pas d'identifiant : l'issue arrive en
        // message dans la même conversation (issue #143).
        let seen = [
            texts(&t.calls_to(tg::EDIT_MESSAGE_TEXT).await),
            texts(&t.calls_to(tg::SEND_MESSAGE).await),
        ]
        .concat();
        if seen
            .iter()
            .any(|e| e.contains("Autorise Drive") && e.contains("terminée"))
        {
            finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(finished, "issue du lien dite dans la conversation");
}
