//! Filet du lot B (épopée #208, tâche T8 de `design/v1/gel-et-outillage.md`) : un message
//! Telegram simulé du propriétaire donne une réponse, un effet dans le ledger et les
//! événements du tour ; le daemon redémarre sur le même répertoire et retrouve tout, sans
//! rien rejouer.
//!
//! ```text
//!  update (mock) ─► process_update ─► turn_queue ─► runner ─► agent ─► time_now ─► effects
//!                                                                  └─► deliver ─► tg_outbox
//!                                                                                   └─► flush ─► sendMessage (mock)
//! ```
//!
//! Tout passe par l'API publique du daemon : le test survit aux déplacements internes de
//! la refonte. La boucle de scrutation (`poll_loop`) n'est pas lancée : `MockTransport`
//! répond à `getUpdates` sans attendre, elle tournerait à vide ; `process_update` est ce
//! qu'elle appelle pour chaque update reçu.

use penelope_app::services::Services;
use penelope_daemon::Daemon;
use penelope_gateway_telegram::TelegramGateway;
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ToolCall;
use penelope_telegram::mock::{MockTransport, updates};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const OWNER: i64 = 42;
const QUESTION: &str = "quelle heure est-il, et fais le point sur la journée";
const FINAL_TEXT: &str = "Il est midi pile d'après l'horloge du daemon.";

/// Une vie du daemon : services sur `home`, passerelle sur le transport simulé, pool de
/// runners. Tuée par [`kill`], reconstruite par [`boot`] comme après un `kill -9`.
struct Life {
    d: Arc<Daemon>,
    g: Arc<TelegramGateway>,
    runners: tokio::task::JoinHandle<()>,
}

async fn boot(
    home: &Path,
    clock: &TestClock,
    p: &Arc<MockProvider>,
    t: &Arc<MockTransport>,
) -> (Life, penelope_daemon::runtime::RecoveryReport) {
    let shared: SharedClock = Arc::new(clock.clone());
    // La vie précédente libère la base de façon asynchrone (fil écrivain) : quelques essais.
    let mut attempt = 0;
    let s = loop {
        match Services::for_tests(home.to_path_buf(), shared.clone()).await {
            Ok(s) => break Arc::new(s),
            Err(e) if attempt < 100 => {
                attempt += 1;
                let _ = e;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => panic!("redémarrage impossible : {e}"),
        }
    };
    let d = Arc::new(Daemon::from_services(s));
    // Le limiteur réel dort une seconde par message : inutile en test.
    d.publish_config("test", |c| {
        c.telegram.rate_per_chat_per_s = 1_000.0;
        Ok(vec!["telegram.rate_per_chat_per_s".into()])
    })
    .unwrap();
    d.set_provider_override(p.clone());
    let g = TelegramGateway::with_transport(d.clone(), t.clone());
    g.register();
    let report = d.recover().await.unwrap();
    let runners = tokio::spawn(penelope_daemon::runner::run_pool(d.clone()));
    (Life { d, g, runners }, report)
}

/// Fin brutale d'une vie : les runners s'arrêtent, plus rien ne tient le daemon.
async fn kill(life: Life) {
    *life.d.hooks.messenger.write().unwrap() = None;
    *life.d.hooks.delivery.write().unwrap() = None;
    life.d.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), life.runners).await;
    drop(life.g);
    drop(life.d);
}

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

/// Textes des `sendMessage` partis sur le transport, dans l'ordre.
async fn sent_texts(t: &MockTransport) -> Vec<String> {
    t.calls_to(penelope_telegram::api::method::SEND_MESSAGE)
        .await
        .iter()
        .filter_map(|b| b["text"].as_str().map(String::from))
        .collect()
}

/// Vide la file d'envoi et attend que la réponse finale soit partie sur le transport.
async fn wait_for_answer(life: &Life, t: &MockTransport) {
    for _ in 0..500 {
        life.g.flush_outbox().await.unwrap();
        if sent_texts(t).await.iter().any(|m| m.contains(FINAL_TEXT)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "pas de réponse finale sur le transport : {:?}",
        t.calls().await
    );
}

/// Ce que la base dit du monde : lignes de `tg_outbox`, effets, événements du tour.
struct World {
    outbox: Vec<(i64, String, String, Value)>,
    effects: Vec<(String, String, Option<String>)>,
    event_kinds: Vec<String>,
    updates_processed: i64,
}

async fn world(s: &Services, session_id: &str) -> World {
    let sid = session_id.to_string();
    s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT chat_id, method, state, payload FROM tg_outbox ORDER BY created_at, rowid",
            )?;
            let outbox = st
                .query_map([], |r| {
                    let raw: String = r.get(3)?;
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        serde_json::from_str(&raw).unwrap_or(Value::Null),
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st =
                c.prepare("SELECT tool, state, session_id FROM effects ORDER BY created_at")?;
            let effects = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut st = c.prepare("SELECT kind FROM events WHERE session_id = ?1 ORDER BY id")?;
            let event_kinds = st
                .query_map([&sid], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let updates_processed = c.query_row(
                "SELECT count(*) FROM tg_updates WHERE processed = 1",
                [],
                |r| r.get(0),
            )?;
            Ok(World {
                outbox,
                effects,
                event_kinds,
                updates_processed,
            })
        })
        .await
        .unwrap()
}

fn assert_world(w: &World, session_id: &str) {
    assert_eq!(
        w.outbox.len(),
        1,
        "une seule ligne de tg_outbox : {:?}",
        w.outbox
    );
    let (chat_id, method, state, payload) = &w.outbox[0];
    assert_eq!(*chat_id, OWNER);
    assert_eq!(method, "sendMessage");
    assert_eq!(state, "sent");
    assert!(
        payload["text"]
            .as_str()
            .unwrap_or_default()
            .contains(FINAL_TEXT),
        "{payload}"
    );

    assert_eq!(w.effects.len(), 1, "un seul effet : {:?}", w.effects);
    let (tool, state, sid) = &w.effects[0];
    assert_eq!(tool, "time_now");
    assert_eq!(state, "completed");
    assert_eq!(sid.as_deref(), Some(session_id));

    let count = |kind: &str| w.event_kinds.iter().filter(|k| *k == kind).count();
    assert_eq!(count("turn.started"), 1, "{:?}", w.event_kinds);
    assert_eq!(count("turn.finished"), 1, "{:?}", w.event_kinds);
    assert_eq!(count("tool.result"), 1, "{:?}", w.event_kinds);
    assert_eq!(w.updates_processed, 1);
}

/// CA 14 : un message du propriétaire est traité de bout en bout ; le redémarrage du daemon
/// retrouve le même état et ne rejoue rien (ni l'outil, ni l'envoi, ni l'update).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ca_14_2_a_telegram_message_gets_an_answer_and_a_ledger_entry() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let clock = TestClock::new(1_789_516_800_000);
    let p = Arc::new(MockProvider::new());
    let t = MockTransport::new();

    // Le modèle : classement du message, un appel d'outil sans risque, puis le texte final.
    p.reply(r#"{"complexity":"low"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![call("c1", "time_now", json!({}))],
    ));
    p.reply(FINAL_TEXT);

    // ---------------------------------------------------------------- première vie
    let (life, report) = boot(&home, &clock, &p, &t).await;
    assert!(report.is_clean());
    // Le profil est vide : sans ce marqueur, le premier message déclenche aussi la
    // proposition d'accueil, une carte de plus dans la file. Ici, elle a déjà été faite.
    life.d
        .services
        .kv_set("tg.onboard.proposed", "test")
        .await
        .unwrap();

    let update = updates::text_message(1, OWNER, OWNER, QUESTION);
    life.g.process_update(&update).await.unwrap();
    wait_for_answer(&life, &t).await;

    let s = &life.d.services;
    let session = s
        .sessions
        .find_by_topic(OWNER, None)
        .await
        .unwrap()
        .expect("une session liée au chat du propriétaire");
    assert_eq!(session.tg_chat_id, Some(OWNER));
    let session_id = session.id.to_string();

    assert_eq!(
        p.call_count(),
        3,
        "classifieur, appel d'outil, réponse finale"
    );
    let first = world(s, &session_id).await;
    assert_world(&first, &session_id);
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    assert!(
        s.events.verify().await.unwrap().ok,
        "chaîne d'événements intacte"
    );
    assert_eq!(sent_texts(&t).await.len(), 1);
    kill(life).await;

    // ---------------------------------------------------------------- seconde vie
    let (life, report) = boot(&home, &clock, &p, &t).await;
    assert_eq!(report.turns_requeued, 0, "{report:?}");
    assert_eq!(report.effects_unknown, 0, "{report:?}");
    let s = &life.d.services;
    let again = world(s, &session_id).await;
    assert_world(&again, &session_id);
    assert_eq!(again.event_kinds, first.event_kinds);
    assert!(s.events.verify().await.unwrap().ok);
    assert_eq!(
        s.sessions
            .find_by_topic(OWNER, None)
            .await
            .unwrap()
            .map(|x| x.id.to_string()),
        Some(session_id.clone()),
        "la session survit au redémarrage"
    );

    // Rien à rejouer : la file d'envoi est vide, l'update déjà traité est ignoré, le
    // modèle et le transport ne sont plus sollicités.
    assert_eq!(life.g.flush_outbox().await.unwrap(), 0);
    life.g.process_update(&update).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.turns.pending_count().await.unwrap(), 0);
    assert_eq!(life.g.flush_outbox().await.unwrap(), 0);
    assert_eq!(p.call_count(), 3, "aucun nouvel appel au modèle");
    assert_eq!(sent_texts(&t).await.len(), 1, "aucun nouvel envoi");
    let after = world(s, &session_id).await;
    assert_world(&after, &session_id);
    kill(life).await;
}
