use super::*;
use crate::agent::Conversation;
use crate::bus::Origin;
use crate::conversation::SessionConversation;
use crate::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use penelope_llm::catalog::ModelInfo;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ChatMessage;

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

async fn daemon() -> (tempfile::TempDir, Arc<Daemon>, Arc<MockProvider>) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    (dir, d, p)
}

/// Une longue conversation : 40 échanges d'environ 600 tokens chacun.
async fn long_session(d: &Arc<Daemon>) -> String {
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
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
    let (_dir, d, p) = daemon().await;
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
    let (_dir, d, p) = daemon().await;
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

/// #131 : un résumeur qui échoue trois fois de suite ne fait pas attendre un
/// quatrième refroidissement : la compaction se fait sans modèle, franche (un nœud),
/// avec les messages du propriétaire gardés, et le propriétaire le sait ; `/status` et
/// le digest disent la session en échec.
#[tokio::test]
async fn three_failures_compact_without_a_model_and_say_so() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), Arc::new(clock.clone()))
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let rec = RecordingMessenger::new();
    *d.hooks.messenger.write().unwrap() = Some(rec.clone() as Arc<dyn crate::executor::Messenger>);
    let sid = long_session(&d).await;
    for _ in 0..3 {
        p.reply("Désolé, je ne peux pas résumer.");
    }
    let conv = SessionConversation::new(
        s.clone(),
        &sid,
        "openrouter:deepseek/deepseek-v4-pro",
        penelope_context::TiersBuilder::new()
            .soul("Pénélope.")
            .build(),
        0,
    );
    let before = conv.request_messages().await.unwrap();
    for round in 1..=2 {
        let err = compact(&d, &sid, Trigger::Background, None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("résumé a échoué"),
            "{round} : {err}"
        );
        assert!(s.context.lcm.active_nodes(&sid).await.unwrap().is_empty());
        clock.advance_ms(3_600_000);
    }
    assert_eq!(
        conv.request_messages().await.unwrap(),
        before,
        "le préfixe ne bouge pas pendant les échecs"
    );
    let view = context_view(&s, &sid, None).await.unwrap();
    assert_eq!(view["compaction_failures"], 2);
    assert!(
        crate::dream::digest_text(&d, d.hooks.mcp_supervisor())
            .await
            .unwrap()
            .contains("Résumé de session en échec"),
        "le digest signale la session"
    );

    let r = compact(&d, &sid, Trigger::Background, None).await.unwrap();
    assert_eq!(r.published, 1, "{r:?}");
    assert_eq!(r.model.as_deref(), Some(MECHANICAL_MODEL));
    assert!(
        r.recovered.iter().any(|x| x.contains("sans modèle")),
        "{r:?}"
    );
    assert_eq!(
        summarizer_requests(&p).len(),
        3,
        "une demande par tentative"
    );
    let nodes = s.context.lcm.active_nodes(&sid).await.unwrap();
    assert_eq!(nodes.len(), 1, "une compaction franche");
    assert!(
        nodes[0].summary.contains("Compaction sans modèle"),
        "{}",
        nodes[0].summary
    );
    assert!(
        nodes[0].summary.contains("question "),
        "derniers messages du propriétaire gardés"
    );
    assert_eq!(load_cooldown(&d.services, &sid).await.failures, 0);
    let sent = rec.texts();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("ne se résumait plus"), "{sent:?}");
    let events = s.events.session_events(&sid, 0).await.unwrap();
    assert!(events.iter().any(|e| {
        e.kind == "context.compacted" && e.payload["evidence"]["actions_total"].is_number()
    }));
    assert!(
        events
            .iter()
            .any(|e| e.kind == "context.compaction_mechanical")
    );
}

/// #131 : un résumeur en délai est d'abord relancé sur une demande trois fois plus
/// courte ; un résumé invalide ne l'est pas (ce n'est pas une affaire de taille).
#[tokio::test]
async fn a_passing_failure_is_retried_on_a_shorter_request() {
    let (_dir, d, p) = daemon().await;
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
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
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
    let (_dir, d, p) = daemon().await;
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

#[tokio::test]
async fn a_turn_over_the_threshold_compacts_in_the_background() {
    let (_dir, d, p) = daemon().await;
    let sid = long_session(&d).await;
    // Fenêtre réduite du modèle principal : la conversation dépasse le seuil de fond.
    let cfg = d.services.config.config();
    let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
    d.services
        .catalog
        .upsert(vec![ModelInfo::minimal(&main, "deepseek", 40_000)]);

    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Je reprends où nous en étions.");
    p.reply(SUMMARY);
    d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, crate::agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();

    let s = &d.services;
    let mut nodes = Vec::new();
    for _ in 0..100 {
        nodes = s.context.lcm.active_nodes(&sid).await.unwrap();
        if !nodes.is_empty() && !d.compaction.is_running(&sid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(nodes.len(), 1, "la compaction de fond a publié son résumé");
    let by_turn = s.budget.report("turn", Some(&sid), None, 10).await.unwrap();
    assert_eq!(
        by_turn.len(),
        1,
        "le résumé est attribué au tour qui l'a déclenché"
    );
    assert_eq!(by_turn[0].calls, 3, "classifieur, réponse, résumé");
}

/// Issue #18 : sur une fenêtre de 1,3 M, le plafond `max_prompt_tokens` suffit à
/// déclencher la compaction de fond (ici 20 k pour une conversation de 24 k ; en
/// production 120 k).
#[tokio::test]
async fn max_prompt_tokens_compacts_a_huge_window_in_the_background() {
    let (_dir, d, p) = daemon().await;
    let sid = long_session(&d).await;
    let cfg = d.services.config.config();
    let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
    d.services
        .catalog
        .upsert(vec![ModelInfo::minimal(&main, "z-ai", 1_300_000)]);
    d.publish_config("test", |c| {
        c.context.max_prompt_tokens = 20_000;
        Ok(vec!["context.max_prompt_tokens".into()])
    })
    .unwrap();

    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Je reprends où nous en étions.");
    p.reply(SUMMARY);
    d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, crate::agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();

    let mut nodes = Vec::new();
    for _ in 0..100 {
        nodes = d.services.context.lcm.active_nodes(&sid).await.unwrap();
        if !nodes.is_empty() && !d.compaction.is_running(&sid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        nodes.len(),
        1,
        "compaction de fond malgré la grande fenêtre"
    );

    let view = context_view(&d.services, &sid, None).await.unwrap();
    assert_eq!(view["window"], 1_300_000);
    assert_eq!(view["compaction_at"], 20_000);
    assert!(view["last_prompt_tokens"].as_i64().is_some(), "{view}");
}

#[tokio::test]
async fn a_proven_overflow_compacts_then_retries_once() {
    let (_dir, d, p) = daemon().await;
    let sid = long_session(&d).await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ContextOverflow);
    p.reply(SUMMARY);
    p.reply("C'est reparti.");
    d.enqueue_message(&sid, "et maintenant ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    match d.run_turn(&turn).await {
        crate::agent::TurnOutcome::Answered { text, .. } => {
            assert_eq!(text, "C'est reparti.")
        }
        other => panic!("{other:?}"),
    }
    d.services.turns.complete(&turn).await.unwrap();

    assert_eq!(
        d.services
            .context
            .lcm
            .active_nodes(&sid)
            .await
            .unwrap()
            .len(),
        1
    );
    let last = p.requests().last().unwrap().clone();
    assert!(
        last.messages
            .iter()
            .any(|m| m.text().contains("Résumé de la conversation antérieure")),
        "la nouvelle requête part avec le résumé"
    );

    // Un second dépassement dans le même tour n'entraîne pas de boucle : une seule
    // compaction, puis l'échec est dit. La compaction est une frontière : le message
    // suivant repasse par le classifieur (#82).
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ContextOverflow);
    p.reply(SUMMARY);
    p.push(Scripted::ContextOverflow);
    d.enqueue_message(&sid, "encore ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    match d.run_turn(&turn).await {
        crate::agent::TurnOutcome::Failed { error } => {
            assert!(error.contains("/compact"), "{error}")
        }
        other => panic!("{other:?}"),
    }
    d.services.turns.complete(&turn).await.unwrap();
    assert_eq!(summarizer_requests(&p).len(), 2, "une compaction par tour");
}

fn kinds(events: &[penelope_kernel::event::Event]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

/// Issue #40 : l'estimation locale reste sous le seuil, mais le prompt réellement
/// facturé le dépasse : la compaction de fond est demandée, dite, puis publiée.
#[tokio::test]
async fn the_billed_prompt_size_requests_a_background_compaction() {
    let (_dir, d, p) = daemon().await;
    let sid = long_session(&d).await;
    let cfg = d.services.config.config();
    let main = strip_provider(cfg.alias_model("main").unwrap()).to_string();
    d.services
        .catalog
        .upsert(vec![ModelInfo::minimal(&main, "z-ai", 1_300_000)]);
    d.publish_config("test", |c| {
        c.context.max_prompt_tokens = 50_000;
        Ok(vec!["context.max_prompt_tokens".into()])
    })
    .unwrap();
    let threshold = background_threshold(&d.services, cfg.alias_model("main").unwrap());
    assert!(threshold > 30_000, "{threshold}");
    p.set_usage(penelope_llm::types::Usage {
        prompt: threshold + 2_000,
        ..Default::default()
    });

    p.reply(r#"{"complexity":"medium"}"#);
    p.reply("Je reprends où nous en étions.");
    p.reply(SUMMARY);
    d.enqueue_message(&sid, "on continue ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, crate::agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();

    let mut nodes = Vec::new();
    for _ in 0..100 {
        nodes = d.services.context.lcm.active_nodes(&sid).await.unwrap();
        if !nodes.is_empty() && !d.compaction.is_running(&sid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(nodes.len(), 1, "résumé publié");
    let events = d.services.events.session_events(&sid, 0).await.unwrap();
    let requested = events
        .iter()
        .position(|e| e.kind == "context.compaction_requested")
        .unwrap_or_else(|| panic!("{:?}", kinds(&events)));
    assert_eq!(events[requested].payload["reason"], "taille réelle");
    let compacted = events
        .iter()
        .position(|e| e.kind == "context.compacted")
        .expect("compacté");
    assert!(requested < compacted);
    let view = context_view(&d.services, &sid, None).await.unwrap();
    assert!(view["last_compaction"].is_string(), "{view}");
}

/// Issue #40 : un budget de session dépassé n'empêche pas le résumé ; seul le plafond
/// du jour l'arrête, une fois la réserve des résumés dépensée, et le saut est dit.
#[tokio::test]
async fn budgets_do_not_block_compaction_until_the_summary_reserve_is_spent() {
    let (_dir, d, p) = daemon().await;
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

/// Issue #40 : une session reprise après une pause, au-delà du seuil, est résumée avant
/// l'appel au modèle, et le prompt envoyé passe sous le plafond.
#[tokio::test]
async fn a_cold_session_is_compacted_before_the_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let sid = long_session(&d).await;
    let cfg = d.services.config.config();
    let main_id = cfg.alias_model("main").unwrap().to_string();
    d.services.catalog.upsert(vec![ModelInfo::minimal(
        strip_provider(&main_id),
        "z-ai",
        1_300_000,
    )]);
    d.publish_config("test", |c| {
        c.context.max_prompt_tokens = 20_000;
        Ok(vec!["context.max_prompt_tokens".into()])
    })
    .unwrap();
    // Dernier appel : 300 k tokens, puis une longue pause.
    d.services
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(sid.clone()),
            model: main_id.clone(),
            provider: "openrouter".into(),
            role: Some("chat".into()),
            prompt: 300_000,
            ..Default::default()
        })
        .await
        .unwrap();
    clock.advance_ms(3 * 3_600_000);

    p.reply(r#"{"complexity":"medium"}"#);
    p.reply(SUMMARY);
    p.reply("Me revoilà.");
    d.enqueue_message(&sid, "on reprend ?", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = d.services.turns.claim("test").await.unwrap().unwrap();
    let out = d.run_turn(&turn).await;
    assert!(
        matches!(out, crate::agent::TurnOutcome::Answered { .. }),
        "{out:?}"
    );
    d.services.turns.complete(&turn).await.unwrap();

    let requests = p.requests();
    let summary_at = requests
        .iter()
        .position(|r| {
            r.messages
                .first()
                .is_some_and(|m| m.text().contains("module de compaction"))
        })
        .expect("résumé demandé");
    let chat_at = requests
        .iter()
        .position(|r| {
            r.messages
                .first()
                .is_some_and(|m| m.text().starts_with("Tu es Pénélope"))
        })
        .expect("appel de conversation");
    assert!(summary_at < chat_at, "le résumé précède l'appel");
    let chat = &requests[chat_at];
    assert!(
        chat.messages
            .iter()
            .any(|m| m.text().contains("Résumé de la conversation antérieure")),
        "l'appel part avec le résumé"
    );
    let tokens: u64 = chat
        .messages
        .iter()
        .map(|m| d.services.context.estimator.message_tokens(&main_id, m))
        .sum();
    assert!(tokens < 20_000, "prompt sous le plafond : {tokens}");
    let events = d.services.events.session_events(&sid, 0).await.unwrap();
    assert!(
        events
            .iter()
            .any(|e| e.kind == "context.compaction_requested" && e.payload["trigger"] == "resume"),
        "{:?}",
        kinds(&events)
    );
}
