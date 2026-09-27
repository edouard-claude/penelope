//! La parole du propriétaire, du tour au rêve (issue #245), sur l'API publique du daemon :
//! un fait dicté par « Retiens que… » et noté par `mem_note` sans citation mot pour mot
//! est gardé à la consolidation, même quand le modèle de tri le juge « non endossé ».
//!
//! Le modèle est scripté : les verdicts sont écrits ici, seul le code décide.

use penelope_agent::ToolExecutor;
use penelope_app::bus::Origin;
use penelope_app::engine::TurnIntake;
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_executor::executor::{NativeToolExecutor, ToolEnv};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_llm::mock::MockProvider;
use penelope_llm::types::ChatMessage;
use serde_json::json;
use std::sync::Arc;

const SAID: &str = "Retiens que nos réunions d'équipe ont lieu le mardi matin à 9 h.";

#[tokio::test]
async fn an_owner_fact_noted_without_citation_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(TestClock::new(1_789_516_800_000));
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
    s.context
        .history
        .append(&sid, &ChatMessage::user(SAID), 20, 0, false, None)
        .await
        .unwrap();
    let exec = NativeToolExecutor::new(
        s.clone(),
        ToolEnv {
            session_id: sid.clone(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![],
            in_workflow: false,
            turn_model: None,
        },
    );

    // Le cas de la passe réelle : reformulé, fuseau en plus, sans `citation`.
    let noted = exec
        .execute(
            "mem_note",
            &json!({"type": "fait", "importance": 8,
                    "texte": "Les réunions d'équipe ont lieu le mardi matin à 9 h (fuseau Indian/Reunion)."}),
        )
        .await
        .unwrap();
    assert_eq!(noted.value["origine"], "owner", "{:?}", noted.value);
    // Une déduction de l'agent, absente du message : elle reste à l'agent.
    let guessed = exec
        .execute(
            "mem_note",
            &json!({"type": "fait",
                    "texte": "Le propriétaire est indisponible pour des rendez-vous clients avant midi."}),
        )
        .await
        .unwrap();
    assert_eq!(guessed.value["origine"], "agent", "{:?}", guessed.value);

    let order = penelope_dream::dream::submission_order(s).await.unwrap();
    let meeting = 1 + order.iter().position(|t| t.contains("mardi")).unwrap();
    let guess = 1 + order.iter().position(|t| t.contains("midi")).unwrap();
    // Le modèle de tri se trompe sur les deux et n'écrit rien pour les réunions.
    p.reply(&format!(
        r#"{{"tri": [
                {{"candidat": {meeting}, "durable": true, "utile": true, "precis": true,
                 "introuvable": true, "endosse": false, "justification": "origine agent"}},
                {{"candidat": {guess}, "durable": true, "utile": true, "precis": true,
                 "introuvable": true, "endosse": false, "justification": "déduction"}}],
              "operations": [
                {{"op": "add_entry", "candidat": {guess}, "file": "memoire.md",
                 "text": "Le propriétaire est indisponible avant midi", "importance": 5}}]}}"#
    ));
    let o = penelope_dream::dream::run(&d.dream(), &d.hooks.messenger, false)
        .await
        .unwrap();

    // Le tri a vu la phrase, pas seulement l'origine.
    let prompt = p.requests().last().unwrap().messages[1].text();
    let line = format!(
        "dit par le propriétaire : « {} »",
        SAID.trim_end_matches('.')
    );
    assert!(prompt.contains(&line), "{prompt}");
    // Gardé par construction, écrit tel quel ; la déduction est écartée.
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    let vault = penelope_app::helpers::vault_dir(s);
    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    assert!(memoire.contains("mardi matin à 9 h"), "{memoire}");
    assert!(!memoire.contains("indisponible"), "{memoire}");
    assert!(
        o.report
            .rejected
            .iter()
            .any(|l| l.contains("indisponible") && l.contains("ni dit ni confirmé")),
        "{:?}",
        o.report.rejected
    );
    assert!(s.candidates.pending(None).await.unwrap().is_empty());
}
