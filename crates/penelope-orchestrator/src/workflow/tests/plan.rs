//! Runs de plans approuvés (#191) : un seul run par plan, phases à contexte neuf, cartes
//! d'OK, « laisse filer », revue qui renvoie au code, reprise après redémarrage.

use super::*;
use penelope_kernel::session::SessionKind;
use penelope_workflow::plan::execution::{CONTINUE, LET_RUN};
use penelope_workflow::plan::{Phase, Plan, PlanDraft, PlanStep, PlanStore};

const SECRET: &str = "le code du coffre est 4242";

fn steps() -> Vec<PlanStep> {
    vec![
        PlanStep::new(Phase::Specification, "Définir le contrat"),
        PlanStep::new(Phase::Tests, "Écrire les tests"),
        PlanStep::new(Phase::Implementation, "Coder"),
        PlanStep::new(Phase::Review, "Relire"),
    ]
}

/// Une conversation qui a préparé un plan, encore en revue : ce que `workflow_plan` pose.
async fn proposed(e: &Env) -> (String, PlanDraft) {
    let s = &e.d.services;
    let chat = s.sessions.create(SessionKind::Chat, None).await.unwrap();
    s.context
        .history
        .append(
            chat.id.as_str(),
            &penelope_llm::types::ChatMessage::user(SECRET),
            10,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    let draft = PlanDraft {
        workflow_id: "build-verify".into(),
        params: json!({"objectif": "stop"}),
        brief: Some("Rendre /stop définitif.".into()),
        plan: Plan::new("Corriger /stop", steps()).unwrap(),
    };
    PlanStore::new(s.store.clone())
        .create(chat.id.as_str(), &draft)
        .await
        .unwrap();
    (chat.id.to_string(), draft)
}

fn tool(id: &str, name: &str, args: Value) -> penelope_llm::types::ToolCall {
    penelope_llm::types::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

/// Le script d'une phase : elle rend `result` et `content`, déclare la fin, conclut.
fn phase(e: &Env, n: &str, result: &str, content: &str) {
    e.p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            tool(
                &format!("{n}a"),
                "return_value",
                json!({"result": result, "content": content}),
            ),
            tool(&format!("{n}b"), "step_done", json!({})),
        ],
    ));
    e.p.reply("Fait.");
}

async fn current(e: &Env, run: &str) -> Run {
    e.d.services.runs.get(run).await.unwrap().unwrap()
}

async fn answer_current(e: &Env, run: &str, choice: &str) {
    let r = current(e, run).await;
    let visit = format!("{}.{}", r.current_step.unwrap(), r.iterations);
    answer(&e.d, run, &visit, choice, None).await.unwrap();
}

/// Le daemon redémarre : même base, nouvel état des runs (aucun pilote ne tient le run).
fn restarted(e: &Env) -> Context {
    Context {
        workflows: Arc::new(State::with_ports(e.d.ports.clone())),
        ..e.d.cx.clone()
    }
}

async fn user_messages(s: &Services, session: &str) -> Vec<String> {
    s.context
        .history
        .load(session, 0)
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.message.role == penelope_llm::types::Role::User)
        .map(|m| serde_json::to_string(&m.message).unwrap())
        .collect()
}

#[tokio::test]
async fn a_stale_button_is_refused_and_an_approved_plan_creates_one_run() {
    let e = env().await;
    let s = &e.d.services;
    let (chat, v1) = proposed(&e).await;
    let plans = PlanStore::new(s.store.clone());
    let mut v2 = v1.clone();
    v2.revise(1, "Corriger /stop et /stop tout", steps())
        .unwrap();
    plans.replace(&chat, &v1, &v2).await.unwrap();

    let stale = go_plan(&e.d, &chat, 1, &v1.fingerprint(), &owner())
        .await
        .unwrap_err();
    assert!(stale.contains("clic périmé"), "{stale}");
    assert!(s.runs.list(None, 10).await.unwrap().is_empty());
    assert!(!plans.get(&chat).await.unwrap().unwrap().plan.can_execute());

    let first = go_plan(&e.d, &chat, 2, &v2.fingerprint(), &owner())
        .await
        .unwrap();
    assert!(first.created);
    assert!(first.draft.plan.can_execute());
    assert!(plans.get(&chat).await.unwrap().unwrap().plan.can_execute());
    let again = go_plan(&e.d, &chat, 2, &v2.fingerprint(), &owner())
        .await
        .unwrap();
    assert!(!again.created, "double clic : même run");
    assert_eq!(again.run.id, first.run.id);
    assert_eq!(s.runs.list(None, 10).await.unwrap().len(), 1);
    let wf = workflow_of(s, &first.run)
        .await
        .expect("définition compilée");
    assert_eq!(wf.metadata.id, first.run.workflow_id);
    assert_eq!(
        plan_runs_of(s, &chat).await.unwrap()[0].id,
        first.run.id,
        "le run est relié à la conversation du plan"
    );

    // Un nouveau plan dans la conversation : l'ancien bouton ne l'approuve pas, il
    // retrouve le run déjà lancé.
    let mut next = PlanDraft {
        plan: Plan::new("Autre chose", steps()).unwrap(),
        ..v2.clone()
    };
    next.params = json!({"objectif": "autre"});
    let approved = plans.get(&chat).await.unwrap().unwrap();
    plans.start_next(&chat, &approved, &next).await.unwrap();
    let old = go_plan(&e.d, &chat, 2, &v2.fingerprint(), &owner())
        .await
        .unwrap();
    assert_eq!(old.run.id, first.run.id);
    let wrong = go_plan(&e.d, &chat, 1, &v1.fingerprint(), &owner())
        .await
        .unwrap_err();
    assert!(wrong.contains("clic périmé"), "{wrong}");
    assert!(!plans.get(&chat).await.unwrap().unwrap().plan.can_execute());
}

#[tokio::test]
async fn phases_run_in_fresh_contexts_wait_for_ok_and_a_review_sends_back_to_code() {
    let e = env().await;
    let s = &e.d.services;
    let (chat, draft) = proposed(&e).await;
    let run = go_plan(&e.d, &chat, 1, &draft.fingerprint(), &owner())
        .await
        .unwrap()
        .run;

    phase(&e, "s", "completed", "SPEC : /stop met le run en pause");
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    assert_eq!(
        current(&e, &run.id).await.current_step.as_deref(),
        Some("ok-e1")
    );
    let questions = e.r.questions();
    assert_eq!(questions.len(), 1);
    assert!(
        questions[0].1.contains("SPEC"),
        "la sortie est lisible : {questions:?}"
    );
    assert_eq!(questions[0].2, ["Continuer", "Laisse filer", "Arrêter"]);
    drive(&e.d, &run.id).await.unwrap();
    assert_eq!(e.p.call_count(), 2, "rien ne part avant l'OK");

    answer_current(&e, &run.id, CONTINUE).await;
    phase(&e, "t", "completed", "TESTS écrits");
    drive(&e.d, &run.id).await.unwrap();
    let r = current(&e, &run.id).await;
    assert_eq!(r.current_step.as_deref(), Some("ok-e2"), "{r:?}");
    answer_current(&e, &run.id, LET_RUN).await;
    phase(&e, "c", "completed", "CODE v1");
    phase(&e, "r", "failed", "REVUE : le cas /stop tout manque");
    phase(&e, "c2", "completed", "CODE v2");
    phase(&e, "r2", "passed", "REVUE : bon");
    // La revue accepte : le plan écrit du code, il entre en livraison (#192). Aucune phase
    // n'a déclaré de dépôt : la carte le demande, rien n'est supposé.
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Running);
    assert_eq!(
        current(&e, &run.id).await.current_step.as_deref(),
        Some("livraison-pr-bloquee")
    );
    let asked = e.r.questions().last().unwrap().clone();
    assert!(
        asked.1.contains("je ne sais pas quel dépôt livrer"),
        "{asked:?}"
    );
    assert_eq!(asked.2, ["Réessayer", "Arrêter"]);

    let trace: Vec<String> = s
        .runs
        .trace(&run.id)
        .await
        .unwrap()
        .iter()
        .map(|t| t["step"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        trace,
        [
            "e1-spec",
            "ok-e1",
            "e2-tests",
            "ok-e2",
            "e3-code-libre",
            "e4-revue-libre",
            "e3-code-r1-libre",
            "e4-revue-r1-libre",
            "livraison-pr"
        ]
    );
    assert_eq!(
        e.r.questions().len(),
        3,
        "laisse filer : plus de carte d'OK, seulement celle de la livraison"
    );

    // Aucun agent ne voit la conversation du propriétaire ; chacun a sa session.
    let requests = e.p.requests();
    for req in &requests {
        let raw = serde_json::to_string(&req.messages).unwrap();
        assert!(!raw.contains(SECRET), "historique du propriétaire transmis");
    }
    let rework = requests
        .iter()
        .map(|r| serde_json::to_string(&r.messages).unwrap())
        .find(|raw| raw.contains("étape 3 (Code)") && raw.contains("REVUE : le cas"))
        .expect("la reprise du code reçoit le refus de la revue");
    assert!(!rework.contains("CODE v2"));
    let sessions = s.sessions.list(None, 50).await.unwrap();
    let fresh: Vec<_> = sessions
        .iter()
        .filter(|x| x.parent_id.as_deref() == Some(run.session_id.as_str()))
        .collect();
    assert_eq!(fresh.len(), 6, "une session neuve par visite d'étape");
    for f in &fresh {
        assert_eq!(user_messages(s, f.id.as_str()).await.len(), 1);
    }
    assert!(user_messages(s, &run.session_id).await.is_empty());
}

#[tokio::test]
async fn a_restart_between_phases_or_during_an_agent_repeats_nothing() {
    let e = env().await;
    let s = &e.d.services;
    let (chat, draft) = proposed(&e).await;
    let run = go_plan(&e.d, &chat, 1, &draft.fingerprint(), &owner())
        .await
        .unwrap()
        .run;

    // Le processus meurt pendant l'appel au modèle de la première phase.
    phase(&e, "s", "completed", "SPEC");
    e.p.slow(std::time::Duration::from_secs(30));
    let cx = e.d.cx.clone();
    let id = run.id.clone();
    let task = tokio::spawn(async move { drive(&cx, &id).await });
    for _ in 0..200 {
        if e.p.call_count() > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(e.p.call_count(), 1);
    task.abort();
    let _ = task.await;
    e.p.slow(std::time::Duration::ZERO);

    let after = restarted(&e);
    drive(&after, &run.id).await.unwrap();
    assert_eq!(
        current(&e, &run.id).await.current_step.as_deref(),
        Some("ok-e1")
    );
    let fresh: Vec<_> = s
        .sessions
        .list(None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|x| x.parent_id.as_deref() == Some(run.session_id.as_str()))
        .collect();
    assert_eq!(fresh.len(), 1, "la visite retrouve sa session");
    assert_eq!(
        user_messages(s, fresh[0].id.as_str()).await.len(),
        1,
        "la consigne n'est pas rejouée"
    );
    assert_eq!(e.r.questions().len(), 1);
    // L'appel coupé par la mort du processus est refait, puis la conclusion : rien d'autre.
    assert_eq!(e.p.call_count(), 3);

    // Redémarrage entre deux phases : la carte d'OK n'est pas renvoyée, rien ne repart.
    let again = restarted(&e);
    drive(&again, &run.id).await.unwrap();
    assert_eq!(e.r.questions().len(), 1);
    assert_eq!(e.p.call_count(), 3);
    assert_eq!(s.runs.trace(&run.id).await.unwrap().len(), 1);

    // `go` rejoué après le redémarrage : le même run, pas un second.
    let replay = go_plan(&again, &chat, 1, &draft.fingerprint(), &owner())
        .await
        .unwrap();
    assert!(!replay.created);
    assert_eq!(s.runs.list(None, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_refusal_stops_the_run_and_nothing_resumes_it() {
    let e = env().await;
    let s = &e.d.services;
    let (chat, draft) = proposed(&e).await;
    let run = go_plan(&e.d, &chat, 1, &draft.fingerprint(), &owner())
        .await
        .unwrap()
        .run;
    phase(&e, "s", "completed", "SPEC");
    drive(&e.d, &run.id).await.unwrap();
    answer_current(&e, &run.id, "Arrêter").await;
    assert_eq!(drive(&e.d, &run.id).await.unwrap(), RunState::Blocked);
    let stopped = current(&e, &run.id).await;
    assert!(
        stopped
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("arrêt demandé par le propriétaire"),
        "{stopped:?}"
    );
    assert_eq!(drive_all(&restarted(&e)).await.unwrap(), 0);
    assert_eq!(e.p.call_count(), 2);
    assert_eq!(s.runs.trace(&run.id).await.unwrap().len(), 2);
}

/// #302 : « vas-y » écrit dans la **conversation d'origine** l'événement du lancement et
/// une note pour son prochain tour, qui nomme le run et `workflow_status` ; un clic rejoué
/// n'écrit rien de plus, et la session du run n'en reçoit pas.
#[tokio::test]
async fn go_tells_the_origin_conversation_once() {
    let e = env().await;
    let s = &e.d.services;
    let (chat, v1) = proposed(&e).await;
    let first = go_plan(&e.d, &chat, 1, &v1.fingerprint(), &owner())
        .await
        .unwrap();
    assert!(first.created);
    let kind = super::super::plan_run::KIND_PLAN_LAUNCHED;
    let launched = s.events.session_events_of_kind(&chat, kind).await.unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].payload["run"], first.run.id);
    assert_eq!(launched[0].payload["version"], 1);
    assert_eq!(launched[0].payload["step"], "e1-spec");
    let notes = penelope_app::notices::peek(s, &chat).await.unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains(&format!("run `{}`", first.run.id))
            && notes[0].contains("`workflow_status`")
            && notes[0].contains("Corriger /stop")
            && notes[0].contains("`e1-spec`"),
        "{}",
        notes[0]
    );

    go_plan(&e.d, &chat, 1, &v1.fingerprint(), &owner())
        .await
        .unwrap();
    assert_eq!(
        s.events
            .session_events_of_kind(&chat, kind)
            .await
            .unwrap()
            .len(),
        1,
        "clic rejoué : rien de plus"
    );
    assert_eq!(
        penelope_app::notices::peek(s, &chat).await.unwrap().len(),
        1
    );
    assert!(
        penelope_app::notices::peek(s, &first.run.session_id)
            .await
            .unwrap()
            .is_empty()
    );
}
