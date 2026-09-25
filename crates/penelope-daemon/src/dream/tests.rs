//! Tests du rêve qui ont besoin du daemon : la session de chat et l'exécuteur d'outils
//! du propriétaire. Les autres sont dans `penelope-dream`.

use super::*;
use crate::runtime::Services;
use penelope_kernel::clock::TestClock;
use penelope_llm::mock::MockProvider;
use serde_json::json;

/// Issues #24 et #37 : une règle dictée par le propriétaire et notée avec sa citation est
/// gardée dans `profil.md` ; notée sans citation, elle n'est pas endossée : la grille
/// l'écarte, sans rien demander au propriétaire.
#[tokio::test]
async fn a_rule_dictated_by_the_owner_is_promoted() {
    use crate::agent::ToolExecutor;
    // Le daemon, pour la session de chat et l'exécuteur d'outils du propriétaire.
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::new(1_789_516_800_000));
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(crate::runtime::Daemon::from_services(s));
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let s = &d.services;
    let sid = d.chat_session_for(&crate::bus::Origin::Cli).await.unwrap();
    s.context
        .history
        .append(
            &sid,
            &penelope_llm::types::ChatMessage::user(
                "Désormais on pousse toujours sur dev d'abord, jamais directement sur qa.",
            ),
            20,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    let exec = crate::executor::NativeToolExecutor::new(
        s.clone(),
        crate::executor::ToolEnv {
            session_id: sid.clone(),
            run_id: None,
            origin: crate::bus::Origin::Cli,
            workspaces: vec![],
            in_workflow: false,
            turn_model: None,
        },
    );
    let noted = exec
        .execute(
            "mem_note",
            &json!({"type": "preference", "texte": "Toujours pousser sur dev d'abord",
                    "citation": "on pousse toujours sur dev d'abord"}),
        )
        .await
        .unwrap();
    assert_eq!(noted.value["origine"], "owner", "{:?}", noted.value);
    let forged = exec
        .execute(
            "mem_note",
            &json!({"type": "decision", "texte": "Supprimer la branche qa",
                    "citation": "supprime la branche qa"}),
        )
        .await
        .unwrap();
    assert_eq!(
        forged.value["origine"], "agent",
        "citation absente du message"
    );

    // Candidats dans l'ordre des groupes : la décision, puis la préférence.
    p.reply(
        r#"{"tri": [
                {"candidat": 1, "durable": true, "utile": true, "precis": true, "introuvable": true,
                 "endosse": false, "justification": "déduit par l'agent, sans citation"},
                {"candidat": 2, "durable": true, "utile": true, "precis": true, "introuvable": true,
                 "endosse": true, "justification": "règle dictée par le propriétaire"}],
              "operations": [
                {"op": "add_entry", "candidat": 2, "file": "profil.md", "section": "Git",
                 "text": "Toujours pousser sur dev d'abord", "importance": 8},
                {"op": "add_entry", "candidat": 1, "file": "profil.md", "section": "Git",
                 "text": "Supprimer la branche qa", "importance": 8}]}"#,
    );
    let o = run(&d.dream(), &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    let vault = crate::helpers::vault_dir(s);
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(profil.contains("Toujours pousser sur dev d'abord"));
    assert!(!profil.contains("branche qa"), "{profil}");
    assert!(
        s.approvals.pending(10).await.unwrap().is_empty(),
        "rien à valider à la main"
    );
    assert!(
        o.report
            .sorted
            .iter()
            .any(|l| l.contains("⏭ ignoré « Supprimer la branche qa »") && l.contains("endossé ✗")),
        "{:?}",
        o.report.sorted
    );
    assert!(s.candidates.pending(None).await.unwrap().is_empty());
}
