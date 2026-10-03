//! `mcp_subscribe` (#293) : abonnement posé puis reposé, notification groupée sur une
//! fenêtre, dédoublonnage, plafond horaire, repli en sondage, abonnement perdu dans le
//! digest, carte du catalogue par le port du canal.

use super::*;
use penelope_app::channel::{CardTemplate, Cards};
use penelope_mcp_host::testing::{FakeConnector, Handler, declare, supervisor};
use std::sync::atomic::{AtomicBool, Ordering};

/// Un pont de messagerie simulé : une ressource JSON, un abonnement compté, et un
/// interrupteur qui le fait mourir.
struct Bridge {
    content: Arc<Mutex<Value>>,
    subscribed: Arc<Mutex<Vec<String>>>,
    dead: Arc<AtomicBool>,
    fake: Arc<FakeConnector>,
    sup: Arc<penelope_mcp_host::McpSupervisor>,
}

impl Bridge {
    fn push(&self, item: Value) {
        self.content
            .lock()
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(item);
    }
    fn notify(&self) {
        self.fake
            .last_transport("pont")
            .push_notification("notifications/resources/updated", json!({"uri": URI}));
    }
    fn subscriptions(&self) -> usize {
        self.subscribed.lock().unwrap().len()
    }
}

const URI: &str = "mail://inbox";

fn handler(bridge: &Bridge, subscribe: bool) -> Handler {
    let (content, subscribed, dead) = (
        bridge.content.clone(),
        bridge.subscribed.clone(),
        bridge.dead.clone(),
    );
    Arc::new(move |m, p| {
        if dead.load(Ordering::SeqCst) {
            return Err(penelope_mcp::McpError::Transport(
                "le serveur a fermé la connexion".into(),
            ));
        }
        match m {
            "server/discover" => Err(penelope_mcp::McpError::Rpc {
                code: penelope_mcp::protocol::METHOD_NOT_FOUND,
                message: "Method not found".into(),
                data: None,
            }),
            "initialize" => Ok(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}, "resources": {"subscribe": subscribe}},
                "serverInfo": {"name": "pont"}
            })),
            "tools/list" => Ok(json!({"tools": []})),
            "resources/subscribe" => {
                subscribed
                    .lock()
                    .unwrap()
                    .push(p["uri"].as_str().unwrap_or_default().to_string());
                Ok(json!({}))
            }
            "resources/read" => Ok(json!({"contents": [{
                "uri": p["uri"], "mimeType": "application/json",
                "text": content.lock().unwrap().to_string()
            }]})),
            _ => Ok(json!({})),
        }
    })
}

async fn bridge(d: &Harness, subscribe: bool, first: Value) -> Bridge {
    let fake = Arc::new(FakeConnector::default());
    let sup = supervisor(d.services.clone(), fake.clone());
    let b = Bridge {
        content: Arc::new(Mutex::new(json!([first]))),
        subscribed: Arc::new(Mutex::new(Vec::new())),
        dead: Arc::new(AtomicBool::new(false)),
        fake,
        sup,
    };
    b.fake.serve("pont", handler(&b, subscribe));
    declare(&b.sup, "pont", "");
    b.sup.reload().await;
    d.set_mcp(b.sup.clone());
    b
}

fn spec(extra: Value) -> Value {
    let mut v = json!({"server": "pont", "uri": URI, "id_path": "id"});
    for (k, x) in extra.as_object().cloned().unwrap_or_default() {
        v[k] = x;
    }
    v
}

/// Attend que `n` événements de ce genre soient au journal (la pompe du superviseur les
/// écrit en tâche de fond).
async fn wait_events(s: &Services, kind: &str, n: usize) {
    for _ in 0..100 {
        let count = s
            .events
            .range(0, 1000)
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .count();
        if count >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{n} événement(s) `{kind}` attendus");
}

fn count(events: &[penelope_kernel::event::Event], kind: &str) -> usize {
    events.iter().filter(|e| e.kind == kind).count()
}

#[tokio::test]
async fn a_subscribed_resource_fires_on_notification_once_the_window_closes() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let b = bridge(&d, true, json!({"id": 1, "subject": "Ancien"})).await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpSubscribe,
            spec(json!({})),
            json!({"type": "notify", "template": "🆕 {{count}} message(s)\n{{items}}"}),
            json!({}),
        )
        .await
        .unwrap();

    // Premier passage : abonnement posé, existant mémorisé sans tir.
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(b.subscriptions(), 1);
    assert!(rec.texts().is_empty(), "l'amorçage ne notifie pas");
    let st = subscription_state(s, &sched.id).await;
    assert_eq!((st.mode.as_str(), st.seeded), ("subscribe", true));

    // Le serveur prévient : la fenêtre s'ouvre, rien ne part avant qu'elle se ferme.
    b.push(json!({"id": 2, "subject": "Nouveau"}));
    b.notify();
    wait_events(s, "mcp.resource_updated", 1).await;
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(
        subscription_state(s, &sched.id)
            .await
            .window_since_ms
            .is_some()
    );
    clock.advance_ms(10_000);
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(rec.texts().is_empty(), "fenêtre de 30 s encore ouverte");
    clock.advance_ms(21_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()], "{report:?}");
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].contains("1 message(s)") && texts[0].contains("2 : Nouveau"),
        "{texts:?}"
    );
    assert!(!texts[0].contains("Ancien"));
    assert_eq!(
        b.subscriptions(),
        1,
        "l'abonnement n'est pas reposé à chaque passage"
    );

    // Même contenu, nouvelle notification : relu, rien de neuf, rien d'envoyé.
    b.notify();
    wait_events(s, "mcp.resource_updated", 2).await;
    tick(&d, &d.scheduler()).await.unwrap();
    clock.advance_ms(31_000);
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(rec.texts().len(), 1, "dédoublonné par empreinte");
    assert!(
        subscription_state(s, &sched.id)
            .await
            .window_since_ms
            .is_none()
    );
}

#[tokio::test]
async fn a_lost_subscription_is_reported_after_an_hour_then_reposted_and_caught_up() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let b = bridge(&d, true, json!({"id": 1})).await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpSubscribe,
            spec(json!({})),
            json!({"type": "notify", "template": "🆕 {{items}}", "label": "Boîte mail"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(b.subscriptions(), 1);

    // Le serveur meurt ; la sonde de santé du superviseur s'en aperçoit.
    b.dead.store(true, Ordering::SeqCst);
    clock.advance_ms(61_000);
    b.sup.maintenance().await;
    tick(&d, &d.scheduler()).await.unwrap();
    let st = subscription_state(s, &sched.id).await;
    assert_eq!(st.mode, "lost");
    assert!(st.lost_since_ms.is_some(), "{st:?}");
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(count(&events, "schedule.subscription_lost"), 1);
    assert!(
        digest_inputs(s).await.failing_schedules.is_empty(),
        "moins d'une heure : le digest ne dit rien"
    );

    // Une heure plus tard, toujours mort : le digest le dit, une fois par abonnement.
    clock.advance_ms(3_600_000);
    tick(&d, &d.scheduler()).await.unwrap();
    let lines = digest_inputs(s).await.failing_schedules;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("Boîte mail") && lines[0].contains("abonnement MCP perdu depuis 1 h"),
        "{lines:?}"
    );
    assert_eq!(
        count(
            &s.events.range(0, 1000).await.unwrap(),
            "schedule.subscription_lost"
        ),
        1,
        "la perte n'est journalisée qu'une fois"
    );
    assert!(rec.texts().is_empty(), "pas de tir pendant la coupure");

    // Le serveur revient avec un message arrivé pendant la coupure : abonnement reposé,
    // ressource relue, message notifié.
    b.dead.store(false, Ordering::SeqCst);
    b.push(json!({"id": 2, "subject": "Pendant la coupure"}));
    clock.advance_ms(10_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()], "{report:?}");
    assert_eq!(b.subscriptions(), 2, "reposé sur la nouvelle connexion");
    let st = subscription_state(s, &sched.id).await;
    assert_eq!((st.mode.as_str(), st.lost_since_ms), ("subscribe", None));
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].contains("2 : Pendant la coupure"), "{texts:?}");
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(count(&events, "schedule.subscription_restored"), 1);
    assert!(digest_inputs(s).await.failing_schedules.is_empty());
}

#[tokio::test]
async fn a_server_that_cannot_subscribe_is_polled_instead() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let b = bridge(&d, false, json!({"id": 1, "subject": "Ancien"})).await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpSubscribe,
            spec(json!({"every_ms": 60_000})),
            json!({"type": "notify", "template": "🆕 {{items}}"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();
    let st = subscription_state(s, &sched.id).await;
    assert_eq!((st.mode.as_str(), st.seeded), ("poll", true));
    assert_eq!(b.subscriptions(), 0, "aucun resources/subscribe envoyé");

    b.push(json!({"id": 2, "subject": "Nouveau"}));
    clock.advance_ms(30_000);
    tick(&d, &d.scheduler()).await.unwrap();
    assert!(rec.texts().is_empty(), "sondage pas encore dû");
    clock.advance_ms(31_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()]);
    assert!(rec.texts()[0].contains("2 : Nouveau"), "{:?}", rec.texts());
}

#[tokio::test]
async fn prompts_are_capped_per_hour_and_the_surplus_is_notified() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    let b = bridge(&d, true, json!({"id": 1, "subject": "Ancien"})).await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpSubscribe,
            spec(json!({"max_per_hour": 2})),
            json!({"type": "prompt", "prompt": "Traite le message {{id}}", "label": "Tri"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();

    for (id, subject) in [(2, "Deux"), (3, "Trois"), (4, "Quatre")] {
        b.push(json!({"id": id, "subject": subject}));
    }
    b.notify();
    wait_events(s, "mcp.resource_updated", 1).await;
    tick(&d, &d.scheduler()).await.unwrap();
    clock.advance_ms(31_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired, vec![sched.id.clone()], "{report:?}");
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(count(&events, "schedule.fired"), 2, "deux tours, pas trois");
    assert_eq!(count(&events, "schedule.capped"), 1);
    let texts = rec.texts();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(
        texts[0].contains("« Tri »")
            && texts[0].contains("plafond atteint")
            && texts[0].contains("4 : Quatre"),
        "{texts:?}"
    );
    assert!(!texts[0].contains("Deux"), "{texts:?}");

    // Une heure plus tard, le compteur glisse : un nouveau message repasse en tour.
    clock.advance_ms(3_600_000);
    b.push(json!({"id": 5, "subject": "Cinq"}));
    b.notify();
    wait_events(s, "mcp.resource_updated", 2).await;
    tick(&d, &d.scheduler()).await.unwrap();
    clock.advance_ms(31_000);
    tick(&d, &d.scheduler()).await.unwrap();
    let events = s.events.range(0, 1000).await.unwrap();
    assert_eq!(count(&events, "schedule.fired"), 3);
    assert_eq!(rec.texts().len(), 1, "rien de plus en notification");
}

/// Un canal qui ne connaît qu'un gabarit, et qui reçoit les cartes de planification.
struct Catalogue;

#[async_trait::async_trait]
impl Cards for Catalogue {
    fn catalog(&self) -> Vec<String> {
        vec!["ticket_detected".into()]
    }
    fn template(&self, id: &str) -> Option<CardTemplate> {
        (id == "ticket_detected").then(|| CardTemplate {
            body: "🎫 **{{titre}}**\n\nProjet : {{projet}}".into(),
            variables: vec!["titre".into(), "projet".into()],
        })
    }
}

#[derive(Default)]
struct CardSink(Mutex<Vec<(String, String)>>);

#[async_trait::async_trait]
impl ChannelDelivery for CardSink {
    async fn deliver(&self, _: &str, _: &str, _: &Origin, _: &penelope_agent::TurnOutcome) {}
    async fn schedule_card(
        &self,
        _origin: &Origin,
        schedule_id: &str,
        markdown: &str,
    ) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push((schedule_id.to_string(), markdown.to_string()));
        Ok(())
    }
}

#[tokio::test]
async fn a_catalogue_template_goes_out_as_a_card_through_the_channel_port() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    s.channel.cards.set(Some(Arc::new(Catalogue)));
    let sink = Arc::new(CardSink::default());
    d.delivery.set(Some(sink.clone()));
    let b = bridge(
        &d,
        true,
        json!({"id": 7, "titre": "TVA incorrecte", "projet": "Compta"}),
    )
    .await;
    let sched = s
        .schedules
        .create(
            TriggerKind::McpSubscribe,
            spec(json!({})),
            json!({"type": "notify", "template": "ticket_detected"}),
            json!({}),
        )
        .await
        .unwrap();
    tick(&d, &d.scheduler()).await.unwrap();

    b.push(json!({"id": 8, "titre": "Facture en double", "projet": "Compta"}));
    b.notify();
    wait_events(s, "mcp.resource_updated", 1).await;
    tick(&d, &d.scheduler()).await.unwrap();
    clock.advance_ms(31_000);
    tick(&d, &d.scheduler()).await.unwrap();
    let cards = sink.0.lock().unwrap().clone();
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].0, sched.id);
    assert!(
        cards[0].1.contains("🎫 **Facture en double**") && cards[0].1.contains("Projet : Compta"),
        "{cards:?}"
    );
    assert!(
        rec.texts().is_empty(),
        "la carte ne part pas aussi en texte"
    );
}

/// Les données d'une ressource lue : JSON décodé du premier texte, texte brut sinon, URI
/// d'un contenu binaire seul.
#[test]
fn a_resource_payload_is_the_first_text_decoded() {
    let json = json!({"contents": [{"uri": "a", "text": "[{\"id\":1}]"}]});
    assert_eq!(resource_payload(&json), json!([{"id": 1}]));
    let text = json!({"contents": [{"uri": "a", "text": "bonjour"}]});
    assert_eq!(resource_payload(&text), json!("bonjour"));
    let blob = json!({"contents": [{"uri": "a", "blob": "AA=="}]});
    assert_eq!(resource_payload(&blob), json!("a"));
    assert_eq!(resource_payload(&json!({})), Value::Null);
}
