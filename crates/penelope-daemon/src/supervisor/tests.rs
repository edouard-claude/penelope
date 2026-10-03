//! Tests de la supervision : relevé des workspaces, relances d'approbation (#97, #296),
//! skills déposées (#63).

use super::*;
use penelope_app::testing::RecordingMessenger;
use penelope_kernel::clock::TestClock;

/// #177 : les liens symboliques ne gonflent pas le relevé et ne font pas
/// sortir le parcours du workspace d'un run.
#[cfg(unix)]
#[test]
fn workspace_measurement_stays_inside_its_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("first"), b"abc").unwrap();
    std::fs::write(root.join("nested/second"), b"12345").unwrap();
    let outside = dir.path().join("outside");
    std::fs::write(&outside, vec![b'x'; 100]).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
    assert_eq!(workspace_size_bytes(&root).unwrap(), 8);
    assert!(workspace_size_bytes(&root.join("link")).is_err());
}

/// #177 : le diagnostic nomme le run en pause et ne touche pas ses fichiers.
#[test]
fn paused_large_workspace_is_reported_without_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("runs");
    let path = root.join("r_test");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("build.bin"), b"12345").unwrap();
    let owners = vec![(
        "r_test".into(),
        RunState::Paused,
        path.to_string_lossy().into_owned(),
    )];
    let reported = large_workspaces(&root, owners.clone(), 5);
    assert_eq!(reported.len(), 1);
    assert_eq!(reported[0].state, RunState::Paused);
    assert_eq!(reported[0].path, path);
    assert_eq!(reported[0].bytes, 5);
    assert!(path.join("build.bin").exists());
    assert!(large_workspaces(&root, owners, 6).is_empty());
}

/// #97 : une demande sans réponse reçoit un rappel à T+1 h et un à T+6 h, pas plus ;
/// une demande tranchée n'en reçoit aucun.
#[tokio::test]
async fn pending_approvals_are_reminded_at_one_and_six_hours() {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::default();
    let s = Arc::new(
        penelope_app::services::Services::for_tests(
            dir.path().to_path_buf(),
            Arc::new(clock.clone()),
        )
        .await
        .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let rec = RecordingMessenger::new();
    *d.hooks.messenger.write().unwrap() =
        Some(rec.clone() as Arc<dyn penelope_executor::executor::Messenger>);
    let ask = |subject: &'static str| {
        let s = s.clone();
        async move {
            s.approvals
                .create(
                    penelope_hitl::ApprovalKind::ToolCall,
                    subject,
                    penelope_kernel::risk::RiskClass::Write,
                    serde_json::json!({"arguments": {}}),
                    vec![],
                    None,
                    None,
                    false,
                )
                .await
                .unwrap()
        }
    };
    let oubliee = ask("shell_exec").await;
    let tranchee = ask("fs_write").await;
    let cards = |r: &RecordingMessenger| r.approvals();

    maintenance_pass(&d).await.unwrap();
    assert!(cards(&rec).is_empty(), "rien avant une heure");

    // 5 h chez le propriétaire, heures calmes du réglage par défaut (#296) : la relance
    // échue n'est ni envoyée ni marquée ; elle partira à la sortie de la plage.
    clock.advance_ms(3_600_000 + 1);
    maintenance_pass(&d).await.unwrap();
    assert!(cards(&rec).is_empty() && rec.texts().is_empty(), "retenue");
    d.publish_config("test", |c| {
        c.telegram.quiet_hours.clear();
        Ok(vec!["telegram.quiet_hours".into()])
    })
    .unwrap();
    s.approvals
        .decide(
            tranchee.id.as_str(),
            &penelope_hitl::Decision::approve_once("cli"),
        )
        .await
        .unwrap();
    maintenance_pass(&d).await.unwrap();
    assert_eq!(cards(&rec), vec![oubliee.id.0.clone()], "premier rappel");
    assert!(rec.texts()[0].contains("Rappel 1/2"));

    clock.advance_ms(30 * 60_000);
    maintenance_pass(&d).await.unwrap();
    assert_eq!(cards(&rec).len(), 1, "rien à T+1 h 30");

    clock.advance_ms(5 * 3_600_000);
    maintenance_pass(&d).await.unwrap();
    assert_eq!(cards(&rec).len(), 2, "second rappel à T+6 h");

    clock.advance_ms(3_600_000);
    maintenance_pass(&d).await.unwrap();
    assert_eq!(cards(&rec).len(), 2, "rien à T+7 h");
    let reminded: Option<String> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT reminded_at FROM approval_requests WHERE id = ?1",
                [oubliee.id.0.clone()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert!(reminded.is_some());
}

/// #63 : une skill déposée après le démarrage est visible après une passe
/// d'entretien, sans redémarrage ; un fichier invalide n'efface pas les autres.
#[tokio::test]
async fn a_skill_dropped_by_scp_is_picked_up_by_the_maintenance_pass() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    penelope_app::services::reload_skills(&s).await.unwrap();
    let before = s.skills.all().len();
    assert!(s.skills.get("revue-express").is_none());

    // Dépôt en SSH, daemon en marche.
    let root = s.platform.dirs.skills().join("revue-express");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("SKILL.md"),
        "---\nname: revue-express\ndescription: Relire un diff en cinq points\n---\n\n\
         # Revue express\n\nLis le diff, donne cinq points.\n",
    )
    .unwrap();

    maintenance_pass(&d).await.unwrap();
    assert!(
        s.skills.get("revue-express").is_some(),
        "la skill doit être visible sans redémarrage"
    );

    // Fichier invalide : les autres restent.
    let bad = s.platform.dirs.skills().join("cassee");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("SKILL.md"), "pas de frontmatter du tout\n").unwrap();
    maintenance_pass(&d).await.unwrap();
    assert!(s.skills.get("revue-express").is_some(), "toujours là");
    assert!(s.skills.all().len() > before);
}

/// #118 : deux passes sans changement ne relisent les skills qu'une fois ; réécrire
/// une skill livrée à l'identique ne change pas l'empreinte ; modifier le contenu d'une
/// skill déposée relance exactement un rechargement ; un rechargement sans changement de
/// contenu laisse le préfixe du prompt intact.
#[tokio::test]
async fn skills_reload_only_when_their_content_changes() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let root = s.platform.dirs.skills().join("revue-express");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("SKILL.md"),
        "---\nname: revue-express\ndescription: Relire un diff\n---\n\nCinq points.\n",
    )
    .unwrap();

    assert!(
        skills_tick(&d).await.unwrap(),
        "premier passage : rechargement"
    );
    let prefix = penelope_conversation::build_tiers(&s, "bonjour", &[], None)
        .await
        .prefix_hash();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(!skills_tick(&d).await.unwrap(), "rien n'a changé");
    assert!(!skills_tick(&d).await.unwrap(), "toujours rien");

    let before = skills_fingerprint(&s);
    let bundled = s
        .platform
        .dirs
        .bundled_skills()
        .join("wiki-markdown")
        .join("SKILL.md");
    let modified = std::fs::metadata(&bundled).unwrap().modified().unwrap();
    penelope_app::services::reload_skills(&s).await.unwrap();
    assert_eq!(
        std::fs::metadata(&bundled).unwrap().modified().unwrap(),
        modified,
        "une skill livrée identique n'est pas réécrite"
    );
    std::fs::write(&bundled, std::fs::read(&bundled).unwrap()).unwrap();
    assert_eq!(
        skills_fingerprint(&s),
        before,
        "même contenu, même empreinte"
    );
    assert!(!skills_tick(&d).await.unwrap());
    assert_eq!(
        penelope_conversation::build_tiers(&s, "bonjour", &[], None)
            .await
            .prefix_hash(),
        prefix,
        "préfixe intact"
    );

    std::fs::write(
        root.join("SKILL.md"),
        "---\nname: revue-express\ndescription: Relire un diff en sept points\n---\n\nSept.\n",
    )
    .unwrap();
    assert!(
        skills_tick(&d).await.unwrap(),
        "contenu modifié : rechargement"
    );
    assert!(!skills_tick(&d).await.unwrap(), "un seul");
    assert!(
        s.skills
            .get("revue-express")
            .is_some_and(|k| k.description.contains("sept")),
    );
}

#[tokio::test]
async fn an_expired_approval_resumes_its_turn() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(TestClock::default());
    let s = Arc::new(
        penelope_app::services::Services::for_tests(dir.path().to_path_buf(), clock.clone())
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s.clone()));
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    s.approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            serde_json::json!({"call_id": "c1"}),
            vec![],
            Some(&sid),
            None,
            false,
        )
        .await
        .unwrap();
    clock.advance_ms(25 * 3_600_000);
    maintenance_pass(&d).await.unwrap();
    let turn = s.turns.claim("t").await.unwrap().expect("reprise en file");
    assert_eq!(turn.kind, penelope_kernel::turn::TurnKind::Resume);
}
