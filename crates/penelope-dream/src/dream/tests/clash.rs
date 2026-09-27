//! #145 : une contradiction devient une carte à trois boutons ; sans réponse, la
//! question est rangée dans `DREAMS.md` et ne se repose pas.

use super::*;

#[tokio::test]
async fn a_contradiction_becomes_a_card_then_a_filed_question() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("profil.md"),
        "# Profil\n\n## Préférences\n- Toujours répondre en anglais aux clients <!-- uid: ANG1 -->\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    note(
        &d,
        CandidateType::Preference,
        "Jamais de réponse en anglais",
        Origin::Owner,
        "s1",
        8,
    )
    .await;
    p.reply(&keep("Jamais de réponse en anglais"));

    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 0, "{:?}", o.report);
    assert_eq!(o.report.questions.len(), 1, "{:?}", o.report);
    assert!(
        o.report.questions[0].contains("je remplace, j'ajoute une exception, ou j'ignore"),
        "{:?}",
        o.report.questions
    );
    assert!(
        s.candidates.pending(None).await.unwrap().is_empty(),
        "le candidat attend la réponse hors de la passe suivante"
    );
    let card = s
        .approvals
        .pending(10)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.payload["contradiction"] == true)
        .expect("carte de contradiction");
    assert_eq!(card.payload["existing_uid"], "ANG1");
    assert_eq!(card.payload["file"], "profil.md");
    assert_eq!(card.payload["source"], "profil.md");
    assert_eq!(card.choices, vec!["Remplacer", "Exception", "Ignorer"]);
    assert_eq!(card.payload["candidates"].as_array().unwrap().len(), 1);

    // Expirée sans réponse : rangée sous sa section, avant ce qui suit.
    let dreams = vault.join("DREAMS.md");
    std::fs::write(
        &dreams,
        "# Revue\n\n## Questions sans réponse\n\n- ancienne\n\n## Nuits\n\nrien\n",
    )
    .unwrap();
    file_unanswered_clash(s, &card).await;
    let raw = std::fs::read_to_string(&dreams).unwrap();
    let question = raw
        .lines()
        .position(|l| l.contains("sans réponse · nouveau « Jamais de réponse en anglais »"))
        .expect("question rangée");
    let nights = raw.lines().position(|l| l == "## Nuits").unwrap();
    assert!(question < nights, "{raw}");
    assert!(raw.contains("[[memoire#^ANG1]]"), "{raw}");
    assert!(
        s.events
            .range(0, 1000)
            .await
            .unwrap()
            .iter()
            .any(|e| e.kind == "memory.clash_unanswered" && e.payload["uid"] == "ANG1")
    );
}

/// Sans section, elle est créée en fin de fichier ; une carte qui n'est pas une
/// contradiction ne laisse aucune trace.
#[tokio::test]
async fn an_unanswered_question_opens_its_section_once() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(vault.join("DREAMS.md"), "# Revue\n\n## Nuits").unwrap();
    let clash = Clash {
        question: String::new(),
        existing_uid: "X1".into(),
        existing: "Café le matin".into(),
        proposed: "Thé le matin".into(),
        quand: Some("lieu=bureau".into()),
    };
    ask_about_clash(s, &clash, &["c1".to_string()])
        .await
        .unwrap();
    let card = s.approvals.pending(10).await.unwrap().remove(0);
    assert_eq!(card.payload["file"], "", "entrée inconnue de l'index");
    assert_eq!(card.payload["source"], "mémoire");
    assert_eq!(card.payload["quand"], "lieu=bureau");

    file_unanswered_clash(s, &card).await;
    file_unanswered_clash(s, &card).await;
    let raw = std::fs::read_to_string(vault.join("DREAMS.md")).unwrap();
    assert_eq!(raw.matches("## Questions sans réponse").count(), 1, "{raw}");
    assert_eq!(raw.matches("« Thé le matin »").count(), 2, "{raw}");
    assert!(
        raw.find("## Nuits").unwrap() < raw.find("## Questions sans réponse").unwrap(),
        "section ouverte en fin de fichier : {raw}"
    );

    let before = raw.clone();
    let mut other = card.clone();
    other.payload["contradiction"] = json!(false);
    file_unanswered_clash(s, &other).await;
    assert_eq!(
        std::fs::read_to_string(vault.join("DREAMS.md")).unwrap(),
        before
    );
}
