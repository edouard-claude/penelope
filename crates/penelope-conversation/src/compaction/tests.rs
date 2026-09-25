use super::*;
use crate::SessionConversation;
use penelope_app::bus::{Bus, Origin};
use penelope_app::conversation::Conversation;
use penelope_app::testing::MockProviders;
use penelope_kernel::clock::TestClock;
use penelope_kernel::session::SessionKind;
use penelope_llm::catalog::ModelInfo;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ChatMessage;
use std::sync::Arc;

const SUMMARY: &str = r#"{"objectif": "préparer la migration PROJ-7",
        "contraintes_et_preferences": "réponses courtes", "fait": "schéma exporté",
        "en_cours": "vérification", "bloque": "", "decisions_cles": "PostgreSQL 17",
        "fichiers_et_ressources": "db/schema.sql", "prochaines_etapes": "migrer ce soir",
        "contexte_critique": ""}"#;

/// #179 : l'observation voit une action explicite et un identifiant perdus,
/// sans modifier le résumé accepté.
#[test]
fn fidelity_observation_reports_missing_evidence_without_rewriting_summary() {
    let job = SummaryJob {
        session_id: "s1".into(),
        from_seq: 1,
        to_seq: 1,
        chunk_from_seq: 1,
        source_text: "[UTILISATEUR #1] TODO: envoyer le rapport de PROJ-42\n".into(),
        anchors: vec![],
        verbatim_users: vec![],
        tokens_src: 20,
        batches: vec![(1, 1)],
        chunk_messages: 1,
        ..Default::default()
    };
    let summary = json!({"objectif": "préparer le projet"});
    let before = summary.clone();
    let observed = observe_fidelity(&job, &summary);
    assert_eq!(observed.actions_total, 1);
    assert_eq!(observed.actions_missing, 1);
    assert_eq!(observed.identifiers_total, 1);
    assert_eq!(observed.identifiers_missing, 1);
    assert_eq!(summary, before);
}

/// #179 : le contrôle porte sur le contexte final, y compris les ancres et
/// les messages utilisateur conservés, pas seulement sur le texte du modèle.
#[test]
fn fidelity_observation_counts_automatically_preserved_evidence() {
    let job = SummaryJob {
        session_id: "s1".into(),
        from_seq: 1,
        to_seq: 1,
        chunk_from_seq: 1,
        source_text: "[UTILISATEUR #1] TODO: envoyer le rapport de PROJ-42\n".into(),
        anchors: penelope_context::anchors::extract("PROJ-42"),
        verbatim_users: vec!["TODO: envoyer le rapport de PROJ-42".into()],
        tokens_src: 20,
        batches: vec![(1, 1)],
        chunk_messages: 1,
        ..Default::default()
    };
    let observed = observe_fidelity(&job, &json!({"objectif": "préparer le projet"}));
    assert_eq!(observed.actions_missing, 0);
    assert_eq!(observed.identifiers_missing, 0);
}

/// #179 : le diagnostic reste limité aux messages utilisateur et borne le
/// texte enregistré dans l'événement.
#[test]
fn fidelity_observation_bounds_samples_and_ignores_other_roles() {
    let job = SummaryJob {
        session_id: "s1".into(),
        from_seq: 1,
        to_seq: 2,
        chunk_from_seq: 1,
        source_text: format!(
            "[ASSISTANT #1] TODO: ignorer PROJ-999\n[UTILISATEUR #2] TODO: {} PROJ-42\n- [ ] vérifier PROJ-43\n",
            "envoyer le rapport ".repeat(10)
        ),
        anchors: vec![],
        verbatim_users: vec![],
        tokens_src: 100,
        batches: vec![(1, 2)],
        chunk_messages: 2,
        ..Default::default()
    };
    let observed = observe_fidelity(&job, &json!({"objectif": "travail en cours"}));
    assert_eq!(observed.actions_total, 2);
    assert_eq!(observed.identifiers_total, 2);
    assert_eq!(observed.identifier_samples, vec!["PROJ-42", "PROJ-43"]);
    assert!(
        observed
            .action_samples
            .iter()
            .all(|s| s.chars().count() <= 80)
    );
}

/// Le contexte de compaction d'un daemon de test, sans le daemon : services, provider
/// factice pour tous les modèles, bus des tours et état des compactions.
async fn context() -> (tempfile::TempDir, Context, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let p = Arc::new(MockProvider::new());
    let d = Context {
        services: s,
        providers: MockProviders::new(p.clone()),
        bus: Arc::new(Bus::new()),
        compaction: Arc::default(),
    };
    (dir, d, p)
}

/// Une longue conversation : 40 échanges d'environ 600 tokens chacun, dans une session
/// comme celle de la CLI.
async fn long_session(d: &Context) -> String {
    let sid = d
        .services
        .sessions
        .create(SessionKind::Chat, Some("CLI".into()))
        .await
        .unwrap()
        .id
        .to_string();
    let h = &d.services.context.history;
    for i in 0..20 {
        let q = format!("question {i} sur PROJ-7 : {}", "détail ".repeat(340));
        let a = format!("réponse {i} : {}", "analyse ".repeat(300));
        h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
            .await
            .unwrap();
        h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
            .await
            .unwrap();
    }
    sid
}

fn summarizer_requests(p: &MockProvider) -> Vec<penelope_llm::types::ChatRequest> {
    p.requests()
        .into_iter()
        .filter(|r| {
            r.messages
                .first()
                .is_some_and(|m| m.text().contains("module de compaction"))
        })
        .collect()
}

#[tokio::test]
async fn manual_compaction_replaces_old_turns_with_a_summary() {
    let (_dir, d, p) = context().await;
    let sid = long_session(&d).await;
    p.reply(SUMMARY);

    let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
    assert_eq!(r.published, 1, "{r:?}");
    assert!(r.messages > 10 && r.tokens_src > 0 && r.tokens_summary > 0);
    assert!(r.skipped.is_none());
    assert!(report_text(&r).contains("messages résumés"));

    // Le résumeur est le modèle du rôle `compaction`.
    let cfg = d.services.config.config();
    let asked = summarizer_requests(&p);
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].model,
        cfg.alias_model(&cfg.role_alias("compaction")).unwrap()
    );

    // Canonique marqué, un seul nœud, projection = résumé + queue verbatim.
    let s = &d.services;
    let nodes = s.context.lcm.active_nodes(&sid).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].summary.contains("PostgreSQL 17"));
    assert!(nodes[0].anchors.iter().any(|a| a.value == "PROJ-7"));
    let history = s.context.history.load(&sid, 0).await.unwrap();
    assert!(history.iter().any(|e| e.compacted));
    assert!(
        !history.last().unwrap().compacted,
        "la queue reste verbatim"
    );

    let conv = SessionConversation::new(
        s.clone(),
        &sid,
        "openrouter:deepseek/deepseek-v4-pro",
        penelope_context::TiersBuilder::new()
            .soul("Pénélope.")
            .build(),
        0,
    );
    let texts: Vec<String> = conv
        .request_messages()
        .await
        .unwrap()
        .iter()
        .map(|m| m.text())
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Résumé de la conversation antérieure"))
    );
    assert!(
        !texts.iter().any(|t| t.starts_with("question 0 ")),
        "les tours résumés ne sont plus envoyés"
    );
    assert!(texts.iter().any(|t| t.starts_with("question 19 ")));

    // Coût et trace.
    let roles = s.budget.report("role", Some(&sid), None, 10).await.unwrap();
    assert!(roles.iter().any(|r| r.key == "compaction"));
    let events = s.events.session_events(&sid, 0).await.unwrap();
    let ev = events
        .iter()
        .find(|e| e.kind == "context.compacted")
        .expect("événement de compaction");
    assert_eq!(ev.payload["trigger"], "manual");
    assert!(ev.payload["evidence"]["actions_total"].is_number());
    assert!(ev.payload["evidence"]["identifiers_missing"].is_number());

    // Rien de neuf : pas de second appel au résumeur, même forcé.
    let again = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert_eq!(again.published, 0);
    assert!(again.skipped.is_some());
    assert_eq!(summarizer_requests(&p).len(), 1);
}

#[tokio::test]
async fn a_summary_ready_during_a_turn_waits_for_its_end() {
    let (_dir, d, p) = context().await;
    let sid = long_session(&d).await;
    let _active = d.bus.begin("t_en_cours", &sid, &Origin::Cli);
    p.reply(SUMMARY);

    let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
    assert!(r.deferred && r.published == 0, "{r:?}");
    assert!(report_text(&r).contains("fin du tour"));
    let s = &d.services;
    assert!(s.context.lcm.active_nodes(&sid).await.unwrap().is_empty());

    // Pendant l'attente, aucune autre compaction ne prépare un lot concurrent.
    let other = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert!(other.deferred && other.published == 0);
    assert_eq!(summarizer_requests(&p).len(), 1);

    d.bus.end(&sid, "t_en_cours");
    let published = publish_pending(&d, &sid)
        .await
        .unwrap()
        .expect("publication");
    assert_eq!(published.published, 1);
    assert_eq!(s.context.lcm.active_nodes(&sid).await.unwrap().len(), 1);
    assert!(
        publish_pending(&d, &sid).await.unwrap().is_none(),
        "publié une fois"
    );
}

/// #131 : un résumeur en délai est d'abord relancé sur une demande trois fois plus
/// courte ; un résumé invalide ne l'est pas (ce n'est pas une affaire de taille).
#[tokio::test]
async fn a_passing_failure_is_retried_on_a_shorter_request() {
    let (_dir, d, p) = context().await;
    let sid = long_session(&d).await;
    p.push(Scripted::Error(
        penelope_llm::types::LlmErrorKind::Transient,
        "Upstream idle timeout exceeded".into(),
    ));
    for _ in 0..4 {
        p.reply(SUMMARY);
    }
    let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
    assert!(r.published >= 1, "{r:?}");
    assert!(
        r.recovered
            .iter()
            .any(|x| x.contains("demande plus courte")),
        "{r:?}"
    );
    let asked = summarizer_requests(&p);
    let size = |i: usize| {
        asked[i]
            .messages
            .iter()
            .map(|m| m.text().len())
            .sum::<usize>()
    };
    assert!(size(1) < size(0), "{} puis {}", size(0), size(1));
}

/// #131 : l'alias de repli déclaré pour le résumeur n'est essayé que si la réserve des
/// résumés couvre son coût estimé ; un modèle au prix inconnu n'est pas essayé.
#[tokio::test]
async fn the_fallback_summarizer_stays_within_the_reserve() {
    let (_dir, d, p) = context().await;
    d.services
        .publish_config("test", |c| {
            c.models
                .routing
                .fallback
                .insert("summarizer".into(), vec!["main".into()]);
            Ok(vec!["models.routing.fallback".into()])
        })
        .unwrap();
    let sid = long_session(&d).await;
    for _ in 0..2 {
        p.push(Scripted::Error(
            penelope_llm::types::LlmErrorKind::Transient,
            "timeout".into(),
        ));
    }
    let err = compact(&d, &sid, Trigger::Manual, None).await.unwrap_err();
    assert!(err.to_string().contains("résumé a échoué"), "{err}");
    let cfg = d.services.config.config();
    let main = cfg.alias_model("main").unwrap().to_string();
    assert!(
        !summarizer_requests(&p).iter().any(|r| r.model == main),
        "prix inconnu : pas de repli"
    );

    let mut info = ModelInfo::minimal(strip_provider(&main), "deepseek", 128_000);
    info.price_prompt = 0.000_000_5;
    info.price_completion = 0.000_002;
    d.services.catalog.upsert(vec![info]);
    for _ in 0..2 {
        p.push(Scripted::Error(
            penelope_llm::types::LlmErrorKind::Transient,
            "timeout".into(),
        ));
    }
    for _ in 0..3 {
        p.reply(SUMMARY);
    }
    let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
    assert!(r.published >= 1, "{r:?}");
    assert!(r.recovered.iter().any(|x| x.contains("repli sur")), "{r:?}");
    assert!(summarizer_requests(&p).iter().any(|r| r.model == main));
}

#[tokio::test]
async fn a_failed_summary_cools_down_until_compact_is_forced() {
    let (_dir, d, p) = context().await;
    let sid = long_session(&d).await;
    p.reply("Désolé, je ne peux pas résumer.");

    let err = compact(&d, &sid, Trigger::Manual, None).await.unwrap_err();
    assert!(err.to_string().contains("résumé a échoué"), "{err}");
    let cooldown = load_cooldown(&d.services, &sid).await;
    assert_eq!(cooldown.failures, 1);
    let events = d.services.events.session_events(&sid, 0).await.unwrap();
    assert!(events.iter().any(|e| e.kind == "context.compaction_failed"));

    // La tâche de fond respecte le cooldown, et le dit (issue #40)…
    let bg = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert!(bg.skipped.unwrap().contains("échec récent"));
    let events = d.services.events.session_events(&sid, 0).await.unwrap();
    let skipped = events
        .iter()
        .find(|e| e.kind == "context.compaction_skipped")
        .expect("saut journalisé");
    assert!(
        skipped.payload["reason"]
            .as_str()
            .unwrap()
            .contains("échec récent")
    );
    assert_eq!(skipped.payload["trigger"], "background");
    assert_eq!(summarizer_requests(&p).len(), 1);

    // … `/compact` le lève.
    p.reply(SUMMARY);
    let r = compact(&d, &sid, Trigger::Manual, None).await.unwrap();
    assert_eq!(r.published, 1);
    assert_eq!(load_cooldown(&d.services, &sid).await.failures, 0);
}

fn kinds(events: &[penelope_kernel::event::Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

/// Issue #40 : un budget de session dépassé n'empêche pas le résumé ; seul le plafond
/// du jour l'arrête, une fois la réserve des résumés dépensée, et le saut est dit.
#[tokio::test]
async fn budgets_do_not_block_compaction_until_the_summary_reserve_is_spent() {
    let (_dir, d, p) = context().await;
    let sid = long_session(&d).await;
    let s = &d.services;
    let spend =
        |role: &str, cost: f64, session: Option<&str>| penelope_kernel::budget::UsageRecord {
            session_id: session.map(String::from),
            model: "m".into(),
            provider: "p".into(),
            role: Some(role.into()),
            cost_usd: cost,
            ..Default::default()
        };
    s.budget
        .record(spend("chat", 6.0, Some(&sid)))
        .await
        .unwrap();
    let statuses = s
        .budget
        .status(&s.config.config().budget, Some(&sid), None)
        .await
        .unwrap();
    assert!(
        statuses.iter().any(|b| b.exceeded),
        "session au-delà de son plafond"
    );
    p.reply(SUMMARY);
    let r = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert_eq!(r.published, 1, "{r:?}");

    s.budget.record(spend("chat", 30.0, None)).await.unwrap();
    s.budget
        .record(spend("compaction", 0.6, None))
        .await
        .unwrap();
    let r = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert!(r.skipped.as_deref().unwrap().contains("réserve"), "{r:?}");
    let events = d.services.events.session_events(&sid, 0).await.unwrap();
    assert!(
        events.iter().any(|e| e.kind == "context.compaction_skipped"
            && e.payload["reason"].as_str().unwrap().contains("réserve")),
        "{:?}",
        kinds(&events)
    );
}
