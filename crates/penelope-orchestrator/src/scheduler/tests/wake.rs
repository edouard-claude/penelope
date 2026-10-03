//! Sortie de veille (#228) : la veille constatée une fois, la santé vérifiée, chaque
//! planification rattrapée une fois et en retard dit.

use super::*;

/// Un canal dont la sonde échoue `failures` fois avant de répondre : le réseau qui
/// revient quelques instants après le réveil.
struct Waking {
    failures: Mutex<u32>,
    probes: Mutex<u32>,
}

#[async_trait::async_trait]
impl ChannelDelivery for Waking {
    async fn deliver(&self, _: &str, _: &str, _: &Origin, _: &penelope_agent::TurnOutcome) {}

    async fn probe(&self) -> Result<(), String> {
        *self.probes.lock().unwrap() += 1;
        let mut left = self.failures.lock().unwrap();
        if *left == 0 {
            return Ok(());
        }
        *left -= 1;
        Err("réseau pas encore revenu".into())
    }
}

/// #228 : trois heures de veille (l'horloge murale saute, la monotone non). Un seul
/// `host.woke` journalisé avec sa durée ; la passe de santé attend le canal et relance le
/// serveur MCP tombé pendant la veille ; puis un seul run de rattrapage par planification,
/// qui dit l'heure prévue, la veille et les créneaux fusionnés.
#[tokio::test]
async fn a_three_hour_sleep_is_noticed_and_each_schedule_catches_up_once_late() {
    use penelope_mcp_host::testing::{FakeConnector, declare, server};
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    d.delivery.set(Some(Arc::new(Waking {
        failures: Mutex::new(1),
        probes: Mutex::new(0),
    })));
    let fake = Arc::new(FakeConnector::default());
    fake.serve("agenda", server(Arc::new(Mutex::new(Vec::new()))));
    fake.fail_open
        .lock()
        .unwrap()
        .insert("agenda".into(), "réseau coupé".into());
    let sup = penelope_mcp_host::testing::supervisor(s.clone(), fake.clone());
    declare(&sup, "agenda", "");
    sup.reload().await;
    d.set_mcp(sup.clone());

    // 04:00 à La Réunion : veille toutes les demi-heures, eau toutes les heures.
    let veille = s
        .schedules
        .create(
            TriggerKind::Cron,
            json!({"expr": "0,30 * * * *"}),
            json!({"type": "notify", "template": "📰 Veille du jour"}),
            json!({}),
        )
        .await
        .unwrap();
    let eau = s
        .schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "notify", "template": "💧 Boire un verre d'eau"}),
            json!({}),
        )
        .await
        .unwrap();

    let ports = d.scheduler();
    let mut watch = WakeWatch::new(s.clock.now_ms());
    assert_eq!(
        wake_check(&d, &ports, &mut watch).await,
        None,
        "pas de saut"
    );

    // La veille coupe le réseau ; il revient au réveil.
    fake.fail_open.lock().unwrap().clear();
    clock.advance_ms(3 * 3_600_000 + 2 * 60_000);
    let wake = wake_check(&d, &ports, &mut watch)
        .await
        .expect("veille constatée");
    assert!(
        (3 * 3_600_000..3 * 3_600_000 + 3 * 60_000).contains(&wake.slept_ms),
        "{wake:?}"
    );
    assert_eq!(wake_check(&d, &ports, &mut watch).await, None, "une fois");

    let events = s.events.range(0, 1000).await.unwrap();
    let woke: Vec<_> = events.iter().filter(|e| e.kind == "host.woke").collect();
    assert_eq!(woke.len(), 1);
    assert_eq!(woke[0].payload["slept"], "3 h 02");
    let health = &events
        .iter()
        .find(|e| e.kind == "host.health")
        .expect("passe de santé")
        .payload;
    assert_eq!(health["channel"], "ok", "{health}");
    assert_eq!(
        health["mcp_restarted"][0]["server"], "agenda",
        "le serveur tombé pendant la veille est relancé : {health}"
    );
    assert_eq!(health["mcp_restarted"][0]["state"], "ready", "{health}");

    let report = tick_after(&d, &ports, watch.last()).await.unwrap();
    assert_eq!(
        report.fired.len(),
        2,
        "un run par planification : {report:?}"
    );
    let texts = rec.texts();
    assert_eq!(texts.len(), 2, "{texts:?}");
    let of = |needle: &str| texts.iter().find(|t| t.contains(needle)).unwrap().clone();
    assert_eq!(
        of("Veille du jour"),
        "⏰ Exécution en retard : prévue à 4h30, lancée à 7h02 après une veille de 3 h 02. \
         Les 6 créneaux manqués partent en une seule exécution.\n\n📰 Veille du jour"
    );
    assert!(
        of("verre d'eau").starts_with(
            "⏰ Exécution en retard : prévue à 5h00, lancée à 7h02 après une veille de 3 h 02. \
             Les 3 créneaux manqués"
        ),
        "{texts:?}"
    );
    let fired: Vec<_> = s
        .events
        .range(0, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "schedule.fired")
        .collect();
    assert!(fired.iter().all(|e| e.payload["late"].is_string()));

    // Le suivant se compte depuis le réveil : plus rien de dû.
    assert!(
        tick_after(&d, &ports, watch.last())
            .await
            .unwrap()
            .fired
            .is_empty()
    );
    let next = |id: String| async move { s.schedules.get(&id).await.unwrap().unwrap().next_run };
    assert!(next(veille.id).await.unwrap().ends_with("03:30:00.000Z"));
    assert!(next(eau.id).await.is_some());
}

/// #228 : un créneau à l'heure, ou parti avec le retard d'un passage, ne se dit pas en
/// retard ; un créneau manqué sans veille connue (daemon arrêté) le dit sans parler de
/// veille.
#[test]
fn only_a_real_delay_is_announced() {
    let sched = Schedule {
        id: "sch_1".into(),
        kind: TriggerKind::Cron,
        spec: json!({"expr": "30 8 * * *"}),
        target: json!({"type": "notify", "template": "x"}),
        dedup: json!({}),
        state: "active".into(),
        last_run: None,
        next_run: Some("2026-01-01T04:30:00.000Z".into()),
        runs: 0,
        last_error: None,
        failures_in_a_row: 0,
        alerted_reason: None,
    };
    let tz = "Indian/Reunion";
    let planned = 1_767_241_800_000; // 2026-01-01T04:30:00Z = 8h30 à La Réunion
    assert_eq!(late_of(&sched, planned + 30_000, tz, None, None), None);
    let late = late_of(&sched, planned + 26 * 3_600_000, tz, None, None).unwrap();
    assert_eq!(late.missed, 2, "le créneau du lendemain est fusionné");
    assert_eq!(
        late_text(&late, planned + 26 * 3_600_000, tz),
        "⏰ Exécution en retard : prévue le 01/01 à 8h30, lancée à 10h30. \
         Les 2 créneaux manqués partent en une seule exécution."
    );
}

/// #228 : un prompt planifié en retard l'annonce tout de suite, avant la réponse du
/// modèle, et le modèle le sait ; sans veille constatée (daemon arrêté), pas de veille.
#[tokio::test]
async fn a_late_prompt_is_announced_before_its_answer() {
    let (d, clock, rec) = harness().await;
    let s = &d.services;
    s.schedules
        .create(
            TriggerKind::Interval,
            json!({"every_ms": 3_600_000}),
            json!({"type": "prompt", "prompt": "Prépare la revue de presse",
                   "label": "Revue de presse"}),
            json!({}),
        )
        .await
        .unwrap();
    clock.advance_ms(2 * 3_600_000 + 60_000);
    let report = tick(&d, &d.scheduler()).await.unwrap();
    assert_eq!(report.fired.len(), 1);
    assert_eq!(
        rec.texts(),
        vec![
            "⏰ Exécution en retard : prévue à 5h00, lancée à 6h01. Les 2 créneaux manqués \
             partent en une seule exécution. (Revue de presse)"
                .to_string()
        ]
    );
    let turn = s.turns.claim("t").await.unwrap().expect("tour");
    let text = turn.payload["text"].as_str().unwrap();
    assert!(
        text.starts_with("⏰ Exécution en retard") && text.ends_with("Prépare la revue de presse"),
        "{text}"
    );
}
