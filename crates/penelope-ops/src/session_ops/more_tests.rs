//! Sessions : fork avec métadonnées et budget, désignation ambiguë, refus du retour en
//! arrière, exports hors session.

use super::*;
use penelope_kernel::clock::TestClock;
use penelope_llm::types::ChatMessage;

async fn services() -> (tempfile::TempDir, Arc<Services>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (dir, s)
}

async fn chat(s: &Services, title: &str) -> String {
    s.sessions
        .create(SessionKind::Chat, Some(title.into()))
        .await
        .unwrap()
        .id
        .to_string()
}

#[tokio::test]
async fn a_fork_keeps_metadata_and_budget() {
    let (_dir, s) = services().await;
    let sid = chat(&s, "Devis").await;
    s.sessions
        .metadata(
            &sid,
            penelope_kernel::session::MetadataOp::Set,
            "projet",
            json!("acme"),
        )
        .await
        .unwrap();
    s.sessions.set_budget(&sid, Some(2.5)).await.unwrap();
    let v = fork(&s, &sid, Some("Variante".into())).await.unwrap();
    let forked = s
        .sessions
        .get(v["session"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(forked.title.as_deref(), Some("Variante"));
    assert_eq!(forked.metadata["projet"], "acme");
    assert_eq!(forked.budget_usd, Some(2.5));
    assert!(fork(&s, "s_inconnue", None).await.is_err());
}

/// Une session se désigne par identifiant, préfixe unique ou titre exact ; une
/// désignation ambiguë demande l'identifiant.
#[tokio::test]
async fn an_ambiguous_session_asks_for_its_id() {
    let (_dir, s) = services().await;
    let a = chat(&s, "Devis").await;
    chat(&s, "devis").await;
    let c = chat(&s, "Facture").await;
    assert_eq!(resolve(&s, "  ").await.unwrap_err(), "session non précisée");
    assert_eq!(resolve(&s, &a).await.unwrap().id.to_string(), a);
    assert_eq!(resolve(&s, "facture").await.unwrap().id.to_string(), c);
    let err = resolve(&s, "Devis").await.unwrap_err();
    assert_eq!(
        err,
        "2 sessions s'appellent « Devis » : préciser l'identifiant"
    );
    let err = resolve(&s, "s_").await.unwrap_err();
    assert!(err.contains("désigne 3 sessions"), "{err}");
    let err = resolve(&s, "Inconnue").await.unwrap_err();
    assert!(err.starts_with("aucune session ne correspond"), "{err}");
}

#[tokio::test]
async fn a_rewind_is_refused_when_it_cannot_be_done() {
    let (_dir, s) = services().await;
    let bus = Bus::new();
    let sid = chat(&s, "CLI").await;
    let err = rewind(&s, &bus, &sid, 0).await.unwrap_err();
    assert!(err.to_string().contains("au moins 1"), "{err}");
    let err = rewind(&s, &bus, &sid, 1).await.unwrap_err();
    assert!(err.to_string().contains("la session est vide"), "{err}");
    s.context
        .history
        .append(&sid, &ChatMessage::user("bonjour"), 5, 0, false, None)
        .await
        .unwrap();
    let origin = crate::bus::Origin::Internal {
        source: "test".into(),
    };
    let _turn = bus.begin("t1", &sid, &origin);
    let err = rewind(&s, &bus, &sid, 1).await.unwrap_err();
    assert!(err.to_string().contains("`/stop` d'abord"), "{err}");
}

/// `all` exporte chaque session, précédée de sa fiche ; un run ou un type inconnus sont
/// refusés sans fichier.
#[tokio::test]
async fn exports_cover_every_session_and_refuse_the_unknown() {
    let (_dir, s) = services().await;
    let a = chat(&s, "Un").await;
    chat(&s, "Deux").await;
    s.context
        .history
        .append(&a, &ChatMessage::user("bonjour"), 5, 0, false, None)
        .await
        .unwrap();
    let v = export(&s, "all", None).await.unwrap();
    let raw = std::fs::read_to_string(v["path"].as_str().unwrap()).unwrap();
    let lines: Vec<Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.iter().filter(|l| l["type"] == "session").count(), 2);
    assert!(lines.iter().any(|l| l["type"] == "message"));

    for (what, id, why) in [
        ("run", None, "identifiant de run attendu"),
        ("run", Some("r_absent"), "run r_absent introuvable"),
        ("session", None, "identifiant de session attendu"),
        ("tout", None, "export inconnu `tout`"),
    ] {
        let err = export(&s, what, id).await.unwrap_err();
        assert!(err.to_string().contains(why), "{what} : {err}");
    }
}
