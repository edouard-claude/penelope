//! Commandes d'exploitation tapées : `/upgrade`, `/stop tout`, `/schedules`, `/mcp`,
//! `/secret`.

use super::*;
use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};

/// Tape une commande et rend les messages envoyés depuis.
async fn say(g: &Arc<TelegramGateway>, t: &MockTransport, text: &str) -> Vec<String> {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(60_000);
    let before = t.calls_to(tg::SEND_MESSAGE).await.len();
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    g.process_update(&updates::text_message(id, OWNER, OWNER, text))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    texts(&t.calls_to(tg::SEND_MESSAGE).await[before..])
}

fn owner() -> Origin {
    Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    }
}

/// Installer une version précise ou revenir en arrière passe par une confirmation.
#[tokio::test]
async fn upgrade_arguments_open_their_confirmation() {
    let (_d, g, t, _p) = gateway().await;
    let out = say(&g, &t, "/upgrade rollback").await;
    assert!(out[0].contains("Revenir au binaire précédent"), "{out:?}");
    let out = say(&g, &t, "/upgrade v9.9.9").await;
    assert!(
        out[0].contains("Installer v9.9.9 puis redémarrer ?"),
        "{out:?}"
    );
}

/// #155 : `/stop tout` met en pause les runs qui tournent, nomme ceux qui attendent et
/// ouvre l'écran des runs pour décider ; les sous-agents du chat sont compris.
#[tokio::test]
async fn stop_everything_pauses_runs_and_names_the_waiting_ones() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let cx = penelope_daemon::workflow::context_of(&g.daemon);
    let running = penelope_orchestrator::workflow::start_run(
        &cx,
        "build-verify",
        json!({"objectif": "a"}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap();
    let blocked = penelope_orchestrator::workflow::start_run(
        &cx,
        "build-verify",
        json!({"objectif": "b"}),
        &owner(),
        None,
        0,
    )
    .await
    .unwrap();
    s.runs
        .set_state(
            &blocked.id,
            penelope_workflow::RunState::Blocked,
            Some("budget"),
        )
        .await
        .unwrap();
    let parent = g.daemon.chat_session_for(&owner()).await.unwrap();
    let child = s
        .sessions
        .create_with(
            penelope_kernel::session::SessionKind::Chat,
            Some("sous-agent".into()),
            Some(parent.clone()),
            None,
        )
        .await
        .unwrap();
    g.daemon
        .enqueue_message(child.id.as_str(), "travail du sous-agent", &owner(), None)
        .await
        .unwrap();

    let out = say(&g, &t, "/stop tout").await;
    assert!(out.len() >= 2, "{out:?}");
    assert!(
        out[0].contains(&blocked.id),
        "run en attente nommé : {out:?}"
    );
    assert!(out.iter().any(|x| x.contains("Runs")), "{out:?}");
    assert_eq!(
        s.runs.get(&running.id).await.unwrap().unwrap().state,
        penelope_workflow::RunState::Paused
    );
    assert_eq!(
        s.runs.get(&blocked.id).await.unwrap().unwrap().state,
        penelope_workflow::RunState::Blocked,
        "le propriétaire décide"
    );
    assert_eq!(s.turns.pending_count().await.unwrap(), 0, "file vidée");
}

/// `/schedules` pilote un déclencheur par son identifiant ; livrer ici un identifiant
/// inconnu est refusé.
#[tokio::test]
async fn schedules_are_driven_by_id() {
    let (_d, g, t, _p) = gateway().await;
    let sched = g
        .daemon
        .services
        .schedules
        .create(
            penelope_workflow::TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "notify", "template": "Bonjour"}),
            json!({}),
        )
        .await
        .unwrap();
    let id = sched.id.clone();
    for (cmd, expected) in [
        (format!("/schedules pause {id}"), "en pause."),
        (format!("/schedules resume {id}"), "repris."),
        (format!("/schedules ici {id}"), "livrera désormais ici"),
        (format!("/schedules run {id}"), "déclenché."),
        (format!("/schedules rm {id}"), "Supprimer le déclencheur"),
        ("/schedules ici sch_absent".to_string(), "❌"),
    ] {
        let out = say(&g, &t, &cmd).await;
        assert!(out.iter().any(|x| x.contains(expected)), "{cmd} : {out:?}");
    }
}

/// #221 : pause, reprise et suppression d'un identifiant inconnu sont refusées (RPC et
/// `/schedules`) ; mettre en pause une planification déjà en pause reste sans erreur.
#[tokio::test]
async fn unknown_schedules_are_refused() {
    use penelope_kernel::api::method as m;
    let (_d, g, t, _p) = gateway().await;
    let rpc = penelope_daemon::rpc::Rpc::new(g.daemon.clone());
    for method in [m::SCHEDULE_PAUSE, m::SCHEDULE_RESUME, m::SCHEDULE_RM] {
        let err = rpc
            .call(method, json!({"id": "sch_absent"}))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(err, "planification inconnue : sch_absent", "{method}");
    }
    for cmd in [
        "/schedules pause sch_absent",
        "/schedules resume sch_absent",
    ] {
        let out = say(&g, &t, cmd).await;
        assert!(
            out.iter()
                .any(|x| x.contains("❌ planification inconnue : sch_absent")),
            "{cmd} : {out:?}"
        );
    }
    let sched = g
        .daemon
        .services
        .schedules
        .create(
            penelope_workflow::TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "notify", "template": "Bonjour"}),
            json!({}),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        rpc.call(m::SCHEDULE_PAUSE, json!({"id": sched.id}))
            .await
            .unwrap();
    }
    rpc.call(m::SCHEDULE_RM, json!({"id": sched.id}))
        .await
        .unwrap();
}

/// `/mcp` : test, journal, désactivation et activation d'un serveur ; un serveur inconnu
/// est dit à chaque sous-commande.
#[tokio::test]
async fn mcp_subcommands_act_on_a_server() {
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "list_issues",
            json!({"readOnlyHint": true}),
        )]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(g.daemon.services.clone(), fake);
    declare(&sup, "redmine", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());
    for (cmd, expected) in [
        ("/mcp test redmine", "✅ `redmine` répond"),
        ("/mcp disable redmine", "désactivé."),
        ("/mcp enable redmine", "activé."),
        ("/mcp redmine", "list_issues"),
        ("/mcp un deux trois", "redmine"),
        ("/mcp test absent", "❌"),
        ("/mcp logs absent", "❌"),
        ("/mcp restart absent", "❌"),
        ("/mcp enable absent", "❌"),
        ("/mcp auth absent", "inconnu"),
    ] {
        let out = say(&g, &t, cmd).await;
        let joined = out
            .join("\n")
            .replace("<code>", "`")
            .replace("</code>", "`");
        assert!(joined.contains(expected), "{cmd} : {out:?}");
    }
}

/// Un secret ne se saisit jamais dans la conversation ; sa suppression se confirme.
#[tokio::test]
async fn secrets_are_never_typed_in_the_chat() {
    let (_d, g, t, _p) = gateway().await;
    let out = say(&g, &t, "/secret set jeton").await;
    assert!(out[0].contains("ne se saisit"), "{out:?}");
    let out = say(&g, &t, "/secret rm jeton").await;
    assert!(out[0].contains("Supprimer le secret"), "{out:?}");
}
