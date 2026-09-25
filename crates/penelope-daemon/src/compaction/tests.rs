//! Tests de la compaction qui ont besoin du daemon : un tour réel (`Daemon::run_turn`),
//! le digest du rêve ou le fork. Les autres vivent dans `penelope-conversation` (T23).

use super::*;
use crate::agent::Conversation;
use crate::bus::Origin;
use crate::conversation::SessionConversation;
use crate::runtime::{Daemon, Services};
use crate::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;
use penelope_llm::catalog::ModelInfo;
use penelope_llm::catalog::strip_provider;
use penelope_llm::mock::{MockProvider, Scripted};
use penelope_llm::types::ChatMessage;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const SUMMARY: &str = r#"{"objectif": "préparer la migration PROJ-7",
        "contraintes_et_preferences": "réponses courtes", "fait": "schéma exporté",
        "en_cours": "vérification", "bloque": "", "decisions_cles": "PostgreSQL 17",
        "fichiers_et_ressources": "db/schema.sql", "prochaines_etapes": "migrer ce soir",
        "contexte_critique": ""}"#;

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
    *d.hooks.messenger.write().unwrap() =
        Some(rec.clone() as Arc<dyn penelope_app::ports::Messenger>);
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
        let err = compact(&context_of(&d), &sid, Trigger::Background, None)
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

    let r = compact(&context_of(&d), &sid, Trigger::Background, None)
        .await
        .unwrap();
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

/// Issue #32 : après une compaction, le prompt contient toujours les notes à jour ; un
/// fork en reçoit sa propre copie ; le rêve relève les décisions une seule fois. Venu de
/// `session_notes` (T22) : il compacte et forke, deux étages au-dessus du vault.
#[tokio::test]
async fn notes_survive_compaction_are_copied_by_fork_and_harvested_once() {
    use crate::session_notes::{file_of, harvest, mark_harvested, parse, prompt_block, read, tool};
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
    let s = &d.services;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    s.sessions
        .set_title(&sid, "Refonte facturation", false)
        .await
        .unwrap();
    let h = &s.context.history;
    for i in 0..20 {
        let q = format!("question {i} : {}", "détail ".repeat(340));
        let a = format!("réponse {i} : {}", "analyse ".repeat(300));
        h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
            .await
            .unwrap();
        h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
            .await
            .unwrap();
    }

    // Session résumée sans notes : le harnais le rappelle.
    p.reply(r#"{"objectif": "migration", "contraintes_et_preferences": "", "fait": "schéma", "en_cours": "migration", "bloque": "", "decisions_cles": "PostgreSQL 17", "fichiers_et_ressources": "", "prochaines_etapes": "migrer", "contexte_critique": ""}"#);
    compact(&context_of(&d), &sid, Trigger::Manual, None)
        .await
        .unwrap();
    let reminder = prompt_block(s, &sid).await.expect("rappel");
    assert!(reminder.contains("session_notes"), "{reminder}");

    tool(s, &sid, &json!({"action": "update_section", "section": "Objectif", "content": "Migrer la facturation vers PostgreSQL 17"})).await.unwrap();
    tool(s, &sid, &json!({"action": "update_section", "section": "Décisions", "content": "- Garder les montants en centimes entiers"})).await.unwrap();
    tool(s, &sid, &json!({"action": "update_section", "section": "Décisions", "content": "- Migrer table par table", "mode": "append"})).await.unwrap();

    let tiers =
        crate::conversation::build_tiers_in(s, "on continue", &[], None, Some((&sid, 0)), None)
            .await;
    assert!(
        tiers
            .volatile
            .contains("Migrer la facturation vers PostgreSQL 17"),
        "{}",
        tiers.volatile
    );
    assert!(
        tiers.volatile.contains("centimes entiers") && tiers.volatile.contains("table par table")
    );

    let rel = file_of(s, &sid).await.unwrap().expect("fichier de notes");
    assert!(rel.starts_with("notes/refonte-facturation-"), "{rel}");
    let raw = std::fs::read_to_string(crate::helpers::vault_dir(s).join(&rel)).unwrap();
    assert!(
        raw.contains("type: session") && raw.contains(&format!("session: {sid}")),
        "{raw}"
    );

    // Nouvelle compaction : les notes, lues à part, restent dans le prompt.
    for i in 0..10 {
        h.append(
            &sid,
            &ChatMessage::user(format!("encore {i} {}", "x ".repeat(600))),
            600,
            0,
            false,
            None,
        )
        .await
        .unwrap();
        h.append(
            &sid,
            &ChatMessage::assistant(format!("ok {i} {}", "y ".repeat(600))),
            600,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    }
    p.reply(r#"{"objectif": "migration", "contraintes_et_preferences": "", "fait": "schéma", "en_cours": "migration", "bloque": "", "decisions_cles": "PostgreSQL 17", "fichiers_et_ressources": "", "prochaines_etapes": "migrer", "contexte_critique": ""}"#);
    compact(&context_of(&d), &sid, Trigger::Manual, None)
        .await
        .unwrap();
    tool(s, &sid, &json!({"action": "update_section", "section": "Prochaine étape", "content": "Écrire la migration des avoirs"})).await.unwrap();
    let tiers =
        crate::conversation::build_tiers_in(s, "et maintenant ?", &[], None, Some((&sid, 0)), None)
            .await;
    assert!(
        tiers.volatile.contains("Écrire la migration des avoirs"),
        "{}",
        tiers.volatile
    );
    assert!(tiers.volatile.contains("centimes entiers"));

    // Fork : copie propre, modifiable sans toucher l'original.
    let fork = crate::session_ops::fork(&d.services, &sid, None)
        .await
        .unwrap();
    let fork = fork["session"].as_str().unwrap().to_string();
    let fork_file = file_of(s, &fork).await.unwrap().expect("notes du fork");
    assert_ne!(fork_file, rel);
    tool(s, &fork, &json!({"action": "update_section", "section": "Objectif", "content": "Variante sans avoirs"})).await.unwrap();
    let original = parse(&read(s, &sid).await.unwrap().unwrap());
    assert_eq!(
        original["Objectif"],
        "Migrer la facturation vers PostgreSQL 17"
    );

    // Rêve : décisions relevées une seule fois.
    let first = harvest(s).await.unwrap();
    assert!(
        first
            .iter()
            .any(|(session, t)| session == &sid && t == "Garder les montants en centimes entiers"),
        "{first:?}"
    );
    // Le marqueur « récoltée » est posé par l'appelant, une fois le candidat
    // enregistré (issue #61) : sans lui, la décision reste récoltable.
    let still = harvest(s).await.unwrap();
    assert_eq!(
        still.len(),
        first.len(),
        "rien n'est consommé sans marqueur"
    );
    for (session, text) in &first {
        mark_harvested(s, session, text).await.unwrap();
    }
    let again = harvest(s).await.unwrap();
    assert!(again.is_empty(), "{again:?}");

    let err = tool(
        s,
        &sid,
        &json!({"action": "update_section", "section": "Divers", "content": "x"}),
    )
    .await
    .unwrap_err();
    assert!(err.contains("section inconnue"));
}

/// T23 : la conversation lit la durée du cache dans `penelope_app::helpers`, la boucle
/// dans `penelope-agent` ; les deux doivent rester égales.
#[test]
fn the_cache_ttl_is_the_same_for_the_loop_and_the_conversation() {
    assert_eq!(crate::helpers::CACHE_TTL_MS, penelope_agent::CACHE_TTL_MS);
}
