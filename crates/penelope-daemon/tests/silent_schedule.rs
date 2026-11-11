//! Une exécution planifiée qui n'a rien à dire ne dit rien (#467), de bout en bout sur
//! l'API publique du daemon : la planification tire, le tour passe par le runner, et l'on
//! regarde ce que le canal a reçu.
//!
//! Le défaut : un prompt planifié qui demande de se taire sans nouveauté obtenait une
//! réponse vide du modèle, que la boucle prenait pour un raté. Elle rejouait le prompt
//! avec « réponds maintenant, en texte », et le « rien de neuf » forcé partait chez le
//! propriétaire, plusieurs fois par jour.

use penelope_agent::TurnOutcome;
use penelope_app::bus::{ChannelDelivery, Origin};
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;
use penelope_workflow::schedules::TriggerKind;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Le canal : garde ce qui lui est livré.
#[derive(Default)]
struct Channel(Mutex<Vec<TurnOutcome>>);

#[async_trait::async_trait]
impl ChannelDelivery for Channel {
    async fn deliver(&self, _turn: &str, _session: &str, _origin: &Origin, outcome: &TurnOutcome) {
        self.0.lock().unwrap().push(outcome.clone());
    }
}

impl Channel {
    fn texts(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|o| match o {
                TurnOutcome::Answered { text, .. } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }
}

struct World {
    _dir: tempfile::TempDir,
    d: Arc<Daemon>,
    p: Arc<MockProvider>,
    clock: Arc<TestClock>,
    channel: Arc<Channel>,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    // L'horloge de test démarre à 04:00 chez le propriétaire, dans ses heures calmes.
    clock.advance_hours(8);
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    d.publish_config("test", |c| {
        c.models.routing.classifier = false;
        Ok(vec!["models.routing.classifier".into()])
    })
    .unwrap();
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let channel = Arc::new(Channel::default());
    d.slots.delivery.set(Some(channel.clone()));
    let workspace = d.services.platform.dirs.data().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("etat.json"), "{\"vus\": [1, 2]}\n").unwrap();
    World {
        _dir: dir,
        d,
        p,
        clock,
        channel,
    }
}

/// Le travail d'une veille : lire son état, puis décider.
fn look() -> Scripted {
    Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "fs_read".into(),
            arguments: json!({"path": "etat.json"}),
        }],
    )
}

/// Crée la planification, la laisse tirer, exécute son tour comme un runner du pool.
async fn fire(w: &World, target: Value) -> (String, TurnOutcome) {
    let origin = Origin::Telegram {
        chat_id: 42,
        topic_id: None,
        message_id: None,
    };
    let mut target = target;
    target["type"] = json!("prompt");
    target["prompt"] = json!("Regarde l'état. N'envoie rien s'il n'y a rien de neuf.");
    target["origin"] = origin.to_value();
    let s = &w.d.services;
    let sched = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            target,
            json!({}),
        )
        .await
        .unwrap();
    w.clock.advance_ms(3_600_500);
    let cx = penelope_daemon::workflow::context_of(&w.d.core);
    penelope_orchestrator::scheduler::tick(&cx, &w.d.slots.scheduler())
        .await
        .unwrap();
    let turn = s
        .turns
        .claim("test")
        .await
        .unwrap()
        .expect("un tir en file");
    let out = penelope_daemon::runner::process(&w.d, turn, Duration::from_secs(30)).await;
    (sched.id, out)
}

/// Le test de l'issue, tel que les journaux l'ont montré : le modèle regarde, n'a rien à
/// dire, rend une réponse vide. Un seul appel de plus aurait été la relance : il n'y en a
/// pas, rien n'arrive chez le propriétaire, et l'exécution compte.
#[tokio::test]
async fn a_scheduled_run_with_nothing_to_say_sends_nothing() {
    let w = world().await;
    w.p.push(look());
    w.p.reply("");
    let (id, out) = fire(&w, json!({})).await;
    assert!(
        matches!(out, TurnOutcome::Answered { ref text, .. } if text.is_empty()),
        "{out:?}"
    );
    assert_eq!(w.p.call_count(), 2, "le prompt n'est pas rejoué");
    assert!(w.channel.texts().is_empty(), "{:?}", w.channel.texts());
    let sched = w.d.services.schedules.get(&id).await.unwrap().unwrap();
    assert_eq!(sched.runs, 1);
    assert_eq!(sched.last_error, None);
}

/// Ce qui marchait et doit continuer : quand il y a du neuf, la réponse part, une fois.
#[tokio::test]
async fn a_scheduled_run_with_news_still_delivers_it() {
    let w = world().await;
    w.p.push(look());
    w.p.reply("Un élément nouveau : le numéro 3.");
    let (_id, out) = fire(&w, json!({})).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(
        w.channel.texts(),
        vec!["Un élément nouveau : le numéro 3.".to_string()]
    );
}

/// Une planification qui promet un message (`"livrable": "message"`) n'a pas le droit de
/// se taire : la réponse vide est relancée comme avant, et la réponse obtenue est livrée.
#[tokio::test]
async fn a_schedule_that_owes_a_message_keeps_its_retry() {
    let w = world().await;
    w.p.push(look());
    w.p.reply("");
    w.p.reply("Point du matin : rien de neuf.");
    let (id, out) = fire(&w, json!({"livrable": "message"})).await;
    assert!(matches!(out, TurnOutcome::Answered { .. }), "{out:?}");
    assert_eq!(w.p.call_count(), 3, "une relance");
    assert_eq!(
        w.channel.texts(),
        vec!["Point du matin : rien de neuf.".to_string()]
    );
    let sched = w.d.services.schedules.get(&id).await.unwrap().unwrap();
    assert_eq!((sched.runs, sched.last_error), (1, None));
}
