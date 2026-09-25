use super::*;
use crate::workflow::harness::Harness;
use penelope_app::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use std::sync::Mutex;

async fn harness() -> (Harness, Arc<TestClock>, Arc<RecordingMessenger>) {
    let clock = Arc::new(TestClock::default());
    let d = Harness::new(clock.clone()).await;
    d.services
        .publish_config("test", |c| {
            c.owner.telegram_user_id = 42;
            Ok(vec!["owner.telegram_user_id".into()])
        })
        .unwrap();
    let rec = RecordingMessenger::new();
    d.set_messenger(rec.clone());
    (d, clock, rec)
}

#[tokio::test]
async fn a_dated_reminder_fires_once_then_is_done() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    // 2026-01-01 04:00 à La Réunion ; rappel le 2 janvier à 9 h.
    let sched = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0 9 2 1 *", "once": true}),
            json!({"type": "notify", "template": "⏰ Appeler Paul"}),
            json!({}),
        )
        .await
        .unwrap();
    assert!(
        tick(&d, &d.scheduler()).await.unwrap().fired.is_empty(),
        "pas encore l'heure"
    );

    clock.set_ms(1_767_330_030_000); // 2026-01-02T05:00:30Z = 09:00:30 à La Réunion
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()]);
    assert!(
        s.events
            .range(0, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "schedule.fired" && event.payload["schedule"] == sched.id),
        "tout déclenchement réussi apparaît dans le journal runtime"
    );
    let sent = rec.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, "⏰ Appeler Paul");
    assert!(matches!(sent[0].0, Origin::Telegram { chat_id: 42, .. }));
    assert_eq!(
        s.schedules.get(&sched.id).await.unwrap().unwrap().state,
        "done"
    );

    clock.advance_days(365);
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(rec.texts().len(), 1, "un rappel unique ne revient pas");
}

/// #120 : un prompt qui déclare un message pour livrable et répond sans rien envoyer
/// est un échec prévenu, et son état « déjà vu » est remis ; le même tour avec message
/// compte et valide l'état ; sans livrable déclaré, rien ne change.
#[tokio::test]
async fn a_silent_scheduled_run_is_a_failure_and_its_state_is_restored() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let origin = Origin::Telegram {
        chat_id: 42,
        topic_id: Some(7),
        message_id: None,
    };
    let ws = penelope_executor::executor::default_workspaces(s)[0].clone();
    std::fs::create_dir_all(ws.join("veille")).unwrap();
    std::fs::write(ws.join("veille/seen.json"), r#"["a","b"]"#).unwrap();
    let make = |livrable: Value| {
        let origin = origin.to_value();
        async move {
            s.schedules
                .create(
                    TriggerKind::Interval,
                    json!({"every_ms": 3_600_000}),
                    json!({"type": "prompt", "prompt": "Prépare la veille",
                           "origin": origin, "livrable": livrable,
                           "etat": "veille/seen.json"}),
                    json!({}),
                )
                .await
                .unwrap()
        }
    };
    let ports = d.scheduler();
    let run = |text: &'static str| {
        let (d, ports) = (d.cx.clone(), ports.clone());
        async move {
            let turn = d.services.turns.claim("t").await.unwrap().expect("tour");
            // Le travail consomme l'état.
            let ws = penelope_executor::executor::default_workspaces(&d.services)[0].clone();
            std::fs::write(ws.join("veille/seen.json"), r#"["a","b","c","d"]"#).unwrap();
            let outcome = penelope_agent::TurnOutcome::Answered {
                text: text.into(),
                iterations: 1,
                cost_usd: 0.0,
            };
            d.services.turns.complete(&turn).await.unwrap();
            let schedule = turn.payload["schedule"].as_str().unwrap().to_string();
            trigger_outcome_of(&d, &ports, &schedule, &outcome, &turn).await;
            schedule
        }
    };

    let muet = make(json!("message")).await;
    clock.advance_ms(3_600_500);
    tick(&d, &d.scheduler()).await.unwrap();
    run("").await;
    let after = s.schedules.get(&muet.id).await.unwrap().unwrap();
    assert!(
        after
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("sans livrable"),
        "{after:?}"
    );
    assert!(
        rec.texts()
            .iter()
            .any(|t| t.contains("aucun message envoyé")),
        "{:?}",
        rec.texts()
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("veille/seen.json")).unwrap(),
        r#"["a","b"]"#,
        "état remis : la suivante reprend les mêmes éléments"
    );
    let inputs = digest_inputs(&d.services).await;
    let digest = penelope_dream::digest_text(&d.dream(), inputs, d.ports.mcp_supervisor.get())
        .await
        .unwrap();
    assert!(digest.contains("planification(s) en échec"), "{digest}");
    s.schedules.set_state(&muet.id, "paused").await.unwrap();

    let parle = make(json!("message")).await;
    clock.advance_ms(3_600_500);
    tick(&d, &d.scheduler()).await.unwrap();
    let alerts = rec.texts().len();
    run("Six items cette semaine.").await;
    let after = s.schedules.get(&parle.id).await.unwrap().unwrap();
    assert!(after.last_error.is_none() && after.runs == 1, "{after:?}");
    assert_eq!(rec.texts().len(), alerts, "pas d'alerte");
    assert_eq!(
        std::fs::read_to_string(ws.join("veille/seen.json")).unwrap(),
        r#"["a","b","c","d"]"#,
        "état validé"
    );
    s.schedules.set_state(&parle.id, "paused").await.unwrap();

    let libre = make(Value::Null).await;
    clock.advance_ms(3_600_500);
    tick(&d, &d.scheduler()).await.unwrap();
    run("").await;
    let after = s.schedules.get(&libre.id).await.unwrap().unwrap();
    assert!(
        after.last_error.is_none() && after.runs == 1,
        "comme avant : {after:?}"
    );
}

/// Issue #39 : un prompt planifié s'exécute dans une session neuve, titrée d'après la
/// planification, même si la conversation où il est né a été fermée ; il ne compte
/// qu'une fois son tour terminé.
#[tokio::test]
async fn a_recurring_prompt_survives_the_closing_of_its_conversation() {
    let (d, clock, _rec) = harness().await;
    let s = &d.services;
    let sid = s
        .sessions
        .create(SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    s.sessions
        .set_title(&sid, "Veille agents IA", false)
        .await
        .unwrap();
    let origin = Origin::Telegram {
        chat_id: 42,
        topic_id: Some(7),
        message_id: None,
    };
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "prompt", "prompt": "Fais le point sur mes tickets",
                   "origin_session": sid, "origin": origin.to_value()}),
            json!({}),
        )
        .await
        .unwrap();
    // La conversation d'origine est fermée (`/new`, `/close`).
    s.sessions.set_state(&sid, "closed").await.unwrap();

    clock.advance_ms(3_600_500);
    tick(&d, &d.scheduler()).await.unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(
        s.turns.pending_count().await.unwrap(),
        1,
        "une occurrence, un tour"
    );
    let turn = s.turns.claim("t").await.unwrap().expect("tour non annulé");
    assert_eq!(turn.kind, TurnKind::Trigger);
    assert_ne!(turn.session_id, sid, "session neuve");
    let session = s.sessions.get(&turn.session_id).await.unwrap().unwrap();
    assert_eq!(session.kind, SessionKind::Scheduled);
    assert!(
        session
            .title
            .as_deref()
            .unwrap()
            .starts_with("Veille agents IA · "),
        "{:?}",
        session.title
    );
    assert_eq!(turn.payload["text"], "Fais le point sur mes tickets");
    assert_eq!(Origin::from_payload(&turn.payload), origin);
    let pending = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(pending.runs, 0, "rien n'est compté avant la fin du tour");
    assert!(pending.next_run.is_some());

    let answered = penelope_agent::TurnOutcome::Answered {
        text: "Trois tickets ouverts.".into(),
        iterations: 1,
        cost_usd: 0.0,
    };
    s.turns.complete(&turn).await.unwrap();
    trigger_outcome(&d, &d.scheduler(), &sched.id, &answered).await;
    let done = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!(done.runs, 1);
    assert!(done.last_run.is_some() && done.last_error.is_none());

    run_now(&d, &d.scheduler(), &sched.id).await.unwrap();
    assert_eq!(
        s.turns.pending_count().await.unwrap(),
        1,
        "exécution immédiate"
    );
}

/// Un canal qui nomme ses conversations par leurs identifiants et refuse `-100999`.
struct Places;

fn chat_of(origin: &Origin) -> Option<(i64, Option<i64>)> {
    match origin {
        Origin::Telegram {
            chat_id, topic_id, ..
        } => Some((*chat_id, *topic_id)),
        _ => None,
    }
}

#[async_trait::async_trait]
impl ChannelDelivery for Places {
    async fn deliver(&self, _: &str, _: &str, _: &Origin, _: &penelope_agent::TurnOutcome) {}

    async fn describe_origin(&self, origin: &Origin) -> Option<String> {
        Some(match chat_of(origin)? {
            (chat, None) => format!("conv {chat}"),
            (chat, Some(topic)) => format!("conv {chat} sujet {topic}"),
        })
    }

    async fn destination_for(&self, origin: &Origin) -> Result<Origin, String> {
        match chat_of(origin) {
            Some((-100_999, _)) | None => Err("conversation non autorisée".into()),
            Some((chat_id, topic_id)) => Ok(Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            }),
        }
    }
}

/// #124 : chaque planification dit où elle livre, avec les mots du canal ; on la déplace
/// sans la recréer (identifiant, exécutions et libellé gardés), là où le canal l'accepte
/// seulement, et l'exécution suivante part au nouvel endroit ; le digest rappelle ce qui
/// part aujourd'hui, et où. Les noms Telegram sont vérifiés par la passerelle.
#[tokio::test]
async fn a_schedule_says_where_it_delivers_and_can_be_moved() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let sched = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            json!({"type": "notify", "template": "🧭 Veille", "label": "Veille du matin",
                   "origin": {"channel": "telegram", "chat_id": 42, "message_id": 99}}),
            json!({}),
        )
        .await
        .unwrap();
    let other = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0 9 1 6 *"}),
            json!({"type": "notify", "template": "☀️ Été"}),
            json!({}),
        )
        .await
        .unwrap();
    let to_of = |list: &[Value], id: &str| {
        list.iter().find(|v| v["id"] == id).unwrap()["destination"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let list = listing(s).await.unwrap();
    assert_eq!(
        to_of(&list, &sched.id),
        "aucune conversation (canal non configuré)"
    );
    // Sans canal branché, seule la conversation du propriétaire est une destination sûre.
    let home = retarget(s, &sched.id, &owner_origin_of(s)).await.unwrap();
    assert_eq!(home, "aucune conversation (canal non configuré)");
    let group = Origin::Telegram {
        chat_id: -100_777,
        topic_id: None,
        message_id: None,
    };
    let refused = retarget(s, &sched.id, &group).await;
    assert!(refused.unwrap_err().contains("aucun canal"));

    s.channel.delivery.set(Some(Arc::new(Places)));
    let list = listing(s).await.unwrap();
    assert_eq!(to_of(&list, &sched.id), "conv 42");
    assert_eq!(to_of(&list, &other.id), "conv 42 (par défaut)");

    let group = |chat_id, topic_id| Origin::Telegram {
        chat_id,
        topic_id,
        message_id: Some(7),
    };
    let refused = retarget(s, &sched.id, &group(-100_999, Some(1)))
        .await
        .unwrap_err();
    assert!(refused.contains("non autorisée"), "{refused}");
    assert!(retarget(s, "inconnu", &group(42, None)).await.is_err());

    clock.set_ms(1_767_243_630_000); // 2026-01-01T05:00:30Z = 09:00:30 à La Réunion
    assert_eq!(
        tick(&d, &d.scheduler()).await.unwrap().fired,
        vec![sched.id.clone()]
    );
    let to = retarget(s, &sched.id, &group(-100_777, Some(12)))
        .await
        .unwrap();
    assert_eq!(to, "conv -100777 sujet 12");
    let moved = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert_eq!((moved.runs, moved.state.as_str()), (1, "active"));
    assert!(moved.last_run.is_some());
    assert_eq!(moved.target["label"], "Veille du matin");
    assert_eq!(moved.target["origin"]["message_id"], Value::Null);
    assert_eq!(to_of(&listing(s).await.unwrap(), &sched.id), to);

    clock.set_ms(1_767_330_030_000); // le lendemain, 09:00:30
    assert_eq!(
        tick(&d, &d.scheduler()).await.unwrap().fired,
        vec![sched.id.clone()]
    );
    let sent = rec.sent();
    assert!(matches!(sent[0].0, Origin::Telegram { chat_id: 42, .. }));
    assert!(
        matches!(
            sent.last().unwrap().0,
            Origin::Telegram {
                chat_id: -100_777,
                topic_id: Some(12),
                ..
            }
        ),
        "{sent:?}"
    );

    clock.set_ms(1_767_405_600_000); // surlendemain, 06:00 à La Réunion
    let today = due_today(&d.services).await;
    assert_eq!(
        today,
        vec!["- 09:00 Veille du matin → conv -100777 sujet 12".to_string()]
    );
    let inputs = digest_inputs(&d.services).await;
    let digest = penelope_dream::digest_text(&d.dream(), inputs, d.ports.mcp_supervisor.get())
        .await
        .unwrap();
    assert!(digest.contains("Aujourd'hui"), "{digest}");
    assert!(digest.contains("conv -100777 sujet 12"), "{digest}");
}

/// #124 : l'outil `schedule_move` déplace une planification vers la conversation de
/// l'appel (sujet compris) ou vers la conversation privée ; `schedule_list` dit où livre
/// chacune ; hors canal (CLI), `here` n'a pas de sens.
#[tokio::test]
async fn schedule_move_sends_a_schedule_here_or_home() {
    use penelope_app::tool_executor::ToolExecutor;
    use penelope_executor::executor::{NativeToolExecutor, ToolEnv};
    let (d, _clock, _rec) = harness().await;
    let s = d.services.clone();
    s.channel.delivery.set(Some(Arc::new(Places)));
    let env = ToolEnv {
        session_id: "s1".into(),
        run_id: None,
        origin: Origin::Cli,
        workspaces: vec![],
        in_workflow: false,
        turn_model: None,
    };
    let mut x = NativeToolExecutor::new(s.clone(), env);
    x.orchestrator = d.ports.orchestrator.get();
    let sched = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0 9 * * *"}),
            json!({"type": "notify", "template": "🧭 Veille"}),
            json!({}),
        )
        .await
        .unwrap();
    let err = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "here"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("private"), "{err}");

    x.env.origin = Origin::Telegram {
        chat_id: -100_777,
        topic_id: Some(12),
        message_id: Some(5),
    };
    let moved = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "here"}))
        .await
        .unwrap();
    assert_eq!(moved.value["destination"], "conv -100777 sujet 12");
    let listed = x.execute("schedule_list", &json!({})).await.unwrap();
    assert_eq!(listed.value[0]["destination"], "conv -100777 sujet 12");
    assert_eq!(
        listed.value[0]["target"]["origin"]["message_id"],
        Value::Null
    );

    let home = x
        .execute("schedule_move", &json!({"id": sched.id, "to": "private"}))
        .await
        .unwrap();
    assert_eq!(home.value["destination"], "conv 42");
}

/// #133 : répéter un message déjà parti, c'est le même texte à la mise en forme près,
/// l'un dans l'autre, ou le même corps sous un autre en-tête ; un autre message n'en
/// est pas un.
#[test]
fn a_repeated_final_answer_is_recognised() {
    let digest = "🧭 **Veille** — 19/09\n\n6 retenus sur 41.";
    assert!(repeats(digest, "🧭 Veille — 19/09\n6 retenus sur 41."));
    assert!(
        repeats("Veille du 19/09\n6 retenus sur 41.", digest),
        "autre en-tête"
    );
    assert!(repeats("6 retenus sur 41.", digest), "contenu dans l'autre");
    assert!(!repeats(digest, "Limite GitHub atteinte, je continue."));
    assert!(!repeats(digest, ""));
}

/// Issue #39 : un tour planifié annulé dans la file ou en échec n'est jamais silencieux.
#[tokio::test]
async fn a_cancelled_or_failed_scheduled_prompt_warns_the_owner() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "prompt", "label": "Veille du matin", "prompt": "Veille"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    // Un ancien tour, dans une session fermée : la file l'annule sans le jouer.
    let closed = s
        .sessions
        .create(SessionKind::Chat, None)
        .await
        .unwrap()
        .id
        .to_string();
    s.sessions.set_state(&closed, "closed").await.unwrap();
    clock.advance_ms(1_000);
    s.turns
        .enqueue(
            &closed,
            TurnKind::Trigger,
            json!({"text": "Veille", "schedule": sched.id}),
            None,
            5,
        )
        .await
        .unwrap();
    assert!(s.turns.claim("t").await.unwrap().is_none());
    clock.advance_ms(1_000);
    tick(&d, &d.scheduler()).await.unwrap();
    let texts = rec.texts();
    assert!(
        texts.iter().any(|t| t
            .contains("⚠️ La planification « Veille du matin » n'a pas pu s'exécuter")
            && t.contains("session fermée")),
        "{texts:?}"
    );
    let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert!(
        after
            .last_error
            .as_deref()
            .unwrap()
            .contains("session fermée")
    );
    assert_eq!(after.runs, 0);
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(rec.texts().len(), texts.len(), "une seule alerte");

    trigger_outcome(
        &d,
        &d.scheduler(),
        &sched.id,
        &penelope_agent::TurnOutcome::Failed {
            error: "fournisseur indisponible".into(),
        },
    )
    .await;
    let texts = rec.texts();
    assert!(
        texts
            .last()
            .unwrap()
            .contains("erreur du modèle : fournisseur indisponible"),
        "{texts:?}"
    );
    let after = s.schedules.get(&sched.id).await.unwrap().unwrap();
    assert!(after.last_error.as_deref().unwrap().contains("fournisseur"));
    let doctor = penelope_ops::doctor::schedules_check(s).await;
    assert!(
        !doctor.ok && doctor.detail.contains(&sched.id),
        "{doctor:?}"
    );
}

/// Issue #39 : une planification identique déjà active est signalée à la création.
#[tokio::test]
async fn a_duplicate_schedule_is_reported_at_creation() {
    let (d, _clock, _rec) = harness().await;
    let s = &d.services;
    let spec = json!({"expr": "30 8 * * *"});
    let first = create(
        s,
        TriggerKind::Cron,
        spec.clone(),
        json!({"type": "prompt", "prompt": "Charge la skill veille et exécute le protocole de veille"}),
        json!({}),
    )
    .await
    .unwrap();
    assert!(first.get("doublons").is_none());
    let twin = create(
        s,
        TriggerKind::Cron,
        spec.clone(),
        json!({"type": "prompt", "prompt": "Charge la skill veille et exécute le protocole de veille."}),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(twin["doublons"], json!([first["id"]]));
    assert!(
        twin["avertissement"]
            .as_str()
            .unwrap()
            .contains("identique")
    );
    let other = create(
        s,
        TriggerKind::Cron,
        spec,
        json!({"type": "prompt", "prompt": "Résume mes mails de la nuit"}),
        json!({}),
    )
    .await
    .unwrap();
    assert!(other.get("doublons").is_none());
}

#[tokio::test]
async fn mcp_poll_seeds_then_notifies_new_items_only() {
    use penelope_mcp_host::testing::{FakeConnector, declare};
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let issues = Arc::new(Mutex::new(vec![json!({"id": 1, "subject": "Ancien"})]));
    let list = issues.clone();
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        Arc::new(move |m, _| match m {
            "initialize" => Ok(json!({"protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}}, "serverInfo": {"name": "r"}})),
            "tools/list" => Ok(json!({"tools": [
                {"name": "list_issues", "inputSchema": {"type": "object"},
                 "annotations": {"readOnlyHint": true}},
                {"name": "delete_issue", "inputSchema": {"type": "object"},
                 "annotations": {"destructiveHint": true}}
            ]})),
            "tools/call" => Ok(json!({
                "content": [{"type": "text", "text": "ok"}],
                "structuredContent": {"issues": list.lock().unwrap().clone()}
            })),
            "server/discover" => Err(penelope_mcp::McpError::Rpc {
                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                message: "Method not found".into(),
                data: None,
            }),
            _ => Ok(json!({})),
        }),
    );
    let sup = penelope_mcp_host::testing::supervisor(s.clone(), fake);
    declare(&sup, "redmine", "");
    sup.reload().await;
    d.set_mcp(sup);

    let poll_spec = |tool: &str| {
        json!({"server": "redmine", "tool": tool, "args": {}, "every_ms": 60_000,
               "item_path": "$.issues", "id_path": "id"})
    };
    let target = json!({"type": "notify", "template": "🆕 {{count}} ticket(s)\n{{items}}"});
    s.schedules
        .create(
            TriggerKind::McpPoll,
            poll_spec("list_issues"),
            target.clone(),
            json!({}),
        )
        .await
        .unwrap();

    clock.advance_ms(61_000);
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(
        rec.texts().is_empty(),
        "le premier passage amorce sans notifier"
    );

    issues
        .lock()
        .unwrap()
        .push(json!({"id": 2, "subject": "Nouveau"}));
    clock.advance_ms(61_000);
    tick(&d, &d.scheduler()).await.unwrap();
    let texts = rec.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].contains("1 ticket(s)") && texts[0].contains("2 : Nouveau"),
        "{texts:?}"
    );
    assert!(!texts[0].contains("Ancien"));

    clock.advance_ms(61_000);
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(rec.texts().len(), 1, "rien de nouveau, rien d'envoyé");

    s.schedules
        .create(
            TriggerKind::McpPoll,
            poll_spec("delete_issue"),
            target,
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(61_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert!(
        report.errors.iter().any(|(_, e)| e.contains("lecture")),
        "{report:?}"
    );
}

#[tokio::test]
async fn a_watched_file_fires_when_it_changes() {
    let (d, _clock, rec) = harness().await;
    let path = d.dir.path().join("rapport.txt");
    std::fs::write(&path, "v1").unwrap();
    d.services
        .schedules
        .create(
            TriggerKind::WatchFile,
            json!({"path": path.display().to_string()}),
            json!({"type": "notify", "template": "📄 {{path}} {{state}}"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(rec.texts().is_empty(), "première observation : on mémorise");
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(rec.texts().is_empty());

    std::fs::write(&path, "version 2, plus longue").unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    let texts = rec.texts();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("rapport.txt modifié"), "{texts:?}");
}

#[tokio::test]
async fn internal_events_fire_their_schedules_once() {
    let (d, _clock, rec) = harness().await;
    let s = &d.services;
    s.events
        .append(penelope_kernel::event::EventDraft::new(
            "run.done",
            json!({}),
        ))
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    s.schedules
        .create(
            TriggerKind::Event,
            json!({"event": "run.done"}),
            json!({"type": "notify", "template": "✅ run terminé ({{session}})"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(rec.texts().is_empty(), "l'historique ne déclenche rien");

    s.events
        .append(
            penelope_kernel::event::EventDraft::new("run.done", json!({"run": "r1"}))
                .session("s_42"),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(rec.texts(), vec!["✅ run terminé (s_42)"]);
}
