//! Filet du lot B (épopée #208, tâche T9 de `design/v1/gel-et-outillage.md`) :
//! l'approbation de bout en bout, sur l'API publique seulement.
//!
//! Un outil à risque (`fs_write`, classe `write`, sans règle) arrête le tour sur une
//! demande. Trois suites : approuver puis reprendre (l'outil s'exécute une fois, le ledger
//! le montre), refuser (le modèle reçoit « Non exécuté » et répond sans l'outil),
//! approuver « pour cette session » (un second appel identique passe sans nouvelle
//! demande, une autre session redemande).
//!
//! Les assertions portent sur le monde : fichiers du workspace, lignes de `effects`,
//! demandes et règles en base, ce que le harnais envoie au modèle. Jamais sur sa prose.

use penelope_agent::TurnOutcome;
use penelope_app::bus::Origin;
use penelope_app::engine::TurnIntake;
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_hitl::{ApprovalKind, ApprovalState, Decision};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::risk::PolicyWindow;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const FILE: &str = "notes/rapport.txt";
const CONTENT: &str = "Rapport du jour : rien à signaler.\n";

async fn setup() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    // Sans classifieur : chaque réponse scriptée va à la boucle d'agent, y compris sur
    // un tour de reprise, dont le message est vide.
    d.publish_config("test", |c| {
        c.models.routing.classifier = false;
        Ok(vec!["models.routing.classifier".into()])
    })
    .unwrap();
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    (dir, d, p)
}

fn write_call(id: &str, path: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "fs_write".into(),
        arguments: json!({"path": path, "content": CONTENT}),
    }
}

/// Réclame le prochain tour en file et l'exécute, comme un runner du pool.
async fn run_next(d: &Arc<Daemon>) -> TurnOutcome {
    let turn = d
        .services
        .turns
        .claim("test")
        .await
        .unwrap()
        .expect("un tour en file");
    penelope_daemon::runner::process(d, turn, Duration::from_secs(30)).await
}

/// Envoie un message dans la session et exécute son tour.
async fn say(d: &Arc<Daemon>, sid: &str, text: &str) -> TurnOutcome {
    d.enqueue_message(sid, text, &Origin::Cli, None)
        .await
        .unwrap()
        .expect("tour créé");
    run_next(d).await
}

/// Remet la suite du tour en file après la décision, et l'exécute.
async fn resume(d: &Arc<Daemon>, sid: &str, approval_id: &str) -> TurnOutcome {
    d.enqueue_resume(sid, approval_id, &Origin::Cli)
        .await
        .unwrap()
        .expect("tour de reprise créé");
    run_next(d).await
}

/// Premier workspace autorisé : `{data}/workspace` sans configuration.
fn workspace(d: &Daemon) -> PathBuf {
    d.services.platform.dirs.data().join("workspace")
}

/// Lignes de `effects` pour un outil : (état) dans l'ordre de création.
async fn effects_of(d: &Daemon, tool: &str) -> Vec<String> {
    let tool = tool.to_string();
    d.services
        .store
        .read(move |c| {
            let mut st =
                c.prepare("SELECT state FROM effects WHERE tool = ?1 ORDER BY created_at")?;
            let rows = st
                .query_map([&tool], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap()
}

/// Un premier message dont l'outil est arrêté sur une demande : rend la session et la
/// demande, après avoir vérifié qu'aucune écriture n'a eu lieu.
async fn ask_to_write(d: &Arc<Daemon>, p: &MockProvider) -> (String, String) {
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![write_call("c1", FILE)],
    ));
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    let approval_id = match say(d, &sid, "écris le rapport du jour").await {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("attendu une demande d'approbation : {other:?}"),
    };
    let pending = d.services.approvals.pending(10).await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].id.as_str(), approval_id);
    assert_eq!(pending[0].kind, ApprovalKind::ToolCall);
    assert_eq!(pending[0].subject, "fs_write");
    assert_eq!(pending[0].session_id.as_deref(), Some(sid.as_str()));
    assert_eq!(pending[0].payload["arguments"]["path"], FILE);
    assert!(
        !workspace(d).join(FILE).exists(),
        "rien n'est écrit avant la décision"
    );
    assert!(effects_of(d, "fs_write").await.is_empty());
    assert_eq!(p.call_count(), 1);
    (sid, approval_id)
}

/// CA 9 : approuver une fois, puis reprendre le tour : l'outil s'exécute une seule fois,
/// le ledger le montre, la réponse arrive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ca_9_4_an_approved_tool_runs_once_after_resume_and_the_ledger_shows_it() {
    let (_dir, d, p) = setup().await;
    let (sid, id) = ask_to_write(&d, &p).await;

    assert!(
        penelope_daemon::agent::decide_approval(&d.services, &id, &Decision::approve_once("cli"))
            .await
            .unwrap()
    );
    p.reply("Le rapport est écrit.");
    let outcome = resume(&d, &sid, &id).await;
    assert!(
        matches!(&outcome, TurnOutcome::Answered { text, .. } if text == "Le rapport est écrit."),
        "{outcome:?}"
    );

    assert_eq!(
        std::fs::read_to_string(workspace(&d).join(FILE)).unwrap(),
        CONTENT
    );
    assert_eq!(
        effects_of(&d, "fs_write").await,
        vec!["completed".to_string()]
    );
    let a = d.services.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(a.state, ApprovalState::Approved);
    assert_eq!(a.decided_via.as_deref(), Some("cli"));
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
    assert!(d.services.policies.active_rules().await.unwrap().is_empty());
    // Une seule reprise : aucun autre tour n'attend.
    assert!(d.services.turns.claim("test").await.unwrap().is_none());
    assert_eq!(p.call_count(), 2);
}

/// CA 9 : refuser : rien n'est écrit, le modèle apprend le refus et répond sans l'outil.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ca_9_5_a_denied_tool_is_reported_to_the_model_and_never_runs() {
    let (_dir, d, p) = setup().await;
    let (sid, id) = ask_to_write(&d, &p).await;

    assert!(
        !penelope_daemon::agent::decide_approval(
            &d.services,
            &id,
            &Decision::deny("cli", Some("pas aujourd'hui".into()))
        )
        .await
        .unwrap()
    );
    p.reply("Entendu, je n'écris rien.");
    let outcome = resume(&d, &sid, &id).await;
    assert!(
        matches!(&outcome, TurnOutcome::Answered { text, .. } if text == "Entendu, je n'écris rien."),
        "{outcome:?}"
    );

    // Ce que le harnais a mis sous les yeux du modèle à la reprise : le résultat de
    // l'appel refusé, avec la raison du propriétaire.
    let seen: Vec<String> = p
        .requests()
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.text())
        .collect();
    assert!(
        seen.iter()
            .any(|t| t.contains("Non exécuté") && t.contains("pas aujourd'hui")),
        "{seen:?}"
    );
    assert!(!workspace(&d).join(FILE).exists());
    assert!(effects_of(&d, "fs_write").await.is_empty());
    let a = d.services.approvals.get(&id).await.unwrap().unwrap();
    assert_eq!(a.state, ApprovalState::Denied);
    assert_eq!(a.reason.as_deref(), Some("pas aujourd'hui"));
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
    assert!(d.services.policies.active_rules().await.unwrap().is_empty());
    assert_eq!(p.call_count(), 2);
}

/// CA 9 : « pour cette session » : une règle bornée à la session naît de la décision ; un
/// second appel du même répertoire passe sans demande ; une autre session redemande.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ca_9_6_a_session_window_lets_the_same_call_pass_without_a_new_request() {
    let (_dir, d, p) = setup().await;
    let (sid, id) = ask_to_write(&d, &p).await;

    let decision = Decision {
        window: PolicyWindow::Session,
        choice: "Pour cette session".into(),
        ..Decision::approve_once("cli")
    };
    assert!(
        penelope_daemon::agent::decide_approval(&d.services, &id, &decision)
            .await
            .unwrap()
    );
    p.reply("Le rapport est écrit.");
    assert!(matches!(
        resume(&d, &sid, &id).await,
        TurnOutcome::Answered { .. }
    ));
    let rules = d.services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(rules[0].tool.as_deref(), Some("fs_write"));
    assert_eq!(rules[0].window, PolicyWindow::Session);
    assert_eq!(rules[0].window_ref.as_deref(), Some(sid.as_str()));
    assert_eq!(
        rules[0].arg_match,
        Some(json!({"path": {penelope_hitl::policy::PATH_PREFIX_OP: "notes/"}}))
    );
    // La demande garde la trace qu'une règle en est née (marqueur, pas l'identifiant).
    assert!(
        d.services
            .approvals
            .get(&id)
            .await
            .unwrap()
            .unwrap()
            .rule_created
            .is_some()
    );

    // Second appel, même répertoire, même session : exécuté d'office.
    let second = "notes/suite.txt";
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![write_call("c2", second)],
    ));
    p.reply("La suite est écrite.");
    let outcome = say(&d, &sid, "écris la suite").await;
    assert!(
        matches!(&outcome, TurnOutcome::Answered { text, .. } if text == "La suite est écrite."),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace(&d).join(second)).unwrap(),
        CONTENT
    );
    assert_eq!(
        effects_of(&d, "fs_write").await,
        vec!["completed".to_string(), "completed".to_string()]
    );
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
    assert_eq!(
        d.services.approvals.count_pending().await.unwrap(),
        0,
        "aucune nouvelle demande"
    );
    let rules = d.services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].hits, 1, "la règle a servi une fois : {rules:?}");

    // Une autre session n'hérite pas de la fenêtre : la même écriture redemande.
    let other = d
        .services
        .sessions
        .create(penelope_kernel::session::SessionKind::Chat, None)
        .await
        .unwrap();
    let third = "notes/ailleurs.txt";
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![write_call("c3", third)],
    ));
    let outcome = say(&d, other.id.as_str(), "écris ailleurs").await;
    assert!(
        matches!(outcome, TurnOutcome::AwaitingApproval { .. }),
        "{outcome:?}"
    );
    assert!(!workspace(&d).join(third).exists());
    assert_eq!(d.services.approvals.count_pending().await.unwrap(), 1);
    assert_eq!(effects_of(&d, "fs_write").await.len(), 2);
}
