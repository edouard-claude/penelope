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

/// Réponse « gardé » d'un candidat, avec une réécriture de l'entrée `uid`.
fn rewrite(op: &str, uid: &str, text: &str) -> String {
    format!(
        r#"{{"tri": [{{"candidat": 1, "durable": true, "utile": true, "precis": true,
                "introuvable": true, "endosse": true, "justification": "règle dite"}}],
              "operations": [{{"op": "{op}", "candidat": 1, "uid": "{uid}",
                "text": "{text}", "reason": "le propriétaire a changé d'avis"}}]}}"#
    )
}

/// Vault avec le tutoiement au profil, et un fait de fond ; renvoie le chemin.
async fn tutoiement(s: &Services) -> PathBuf {
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("profil.md"),
        "# Profil\n\n## Préférences\n- Toujours tutoyer le propriétaire <!-- uid: TU1 -->\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire\n\n## Faits\n- Le serveur de build tourne sur le port 8080 <!-- uid: PORT1 -->\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    vault
}

/// #224 : la nuit du jour 10, « je préfère qu'on se vouvoie » a remplacé le tutoiement
/// du profil par un `supersede_entry`, sans question. Une préférence du propriétaire ne
/// se réécrit plus sans lui : carte de contradiction, ligne du profil intacte.
#[tokio::test]
async fn a_preference_does_not_supersede_the_owner_profile_without_asking() {
    for op in ["supersede_entry", "replace_entry"] {
        let (_dir, d, p) = daemon().await;
        let s = &d.services;
        let vault = tutoiement(s).await;
        note(
            &d,
            CandidateType::Preference,
            "Je préfère qu'on se vouvoie.",
            Origin::Owner,
            "s10",
            7,
        )
        .await;
        p.reply(&rewrite(op, "TU1", "Le propriétaire préfère être vouvoyé"));

        let o = run(&d, &d.hooks.messenger, false).await.unwrap();
        assert_eq!(o.report.promoted, 0, "{op} : {:?}", o.report);
        let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
        assert!(
            profil.contains("Toujours tutoyer le propriétaire"),
            "{op} : {profil}"
        );
        assert!(!profil.contains("vouvoy"), "{op} : {profil}");
        let card = s
            .approvals
            .pending(10)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.payload["contradiction"] == true)
            .unwrap_or_else(|| panic!("{op} : carte de contradiction"));
        assert_eq!(card.payload["existing_uid"], "TU1");
        assert_eq!(card.payload["file"], "profil.md");
        assert_eq!(
            card.payload["proposed"],
            "Le propriétaire préfère être vouvoyé"
        );
        assert!(
            o.report.questions[0].contains("j'avais « Toujours tutoyer le propriétaire »"),
            "{op} : {:?}",
            o.report.questions
        );
        assert!(
            s.candidates.pending(None).await.unwrap().is_empty(),
            "{op} : le candidat attend la réponse"
        );
    }
}

/// Ce qui passe toujours sans question (#224) : la correction explicite du propriétaire
/// (« non, … »), la précision qui garde le texte d'avant entier, et la mise à jour d'un
/// fait qui n'est ni au profil ni écrit par le propriétaire.
#[tokio::test]
async fn corrections_precisions_and_facts_still_rewrite_without_a_card() {
    let cases = [
        (
            CandidateType::Correction,
            "Non, vouvoie-moi.",
            "supersede_entry",
            "TU1",
            "Vouvoyer le propriétaire",
            "profil.md",
        ),
        (
            CandidateType::Preference,
            "Tutoie-moi aussi par écrit.",
            "replace_entry",
            "TU1",
            "Toujours tutoyer le propriétaire, y compris par écrit",
            "profil.md",
        ),
        (
            CandidateType::Fait,
            "Le serveur de build est passé sur le port 9090.",
            "supersede_entry",
            "PORT1",
            "Le serveur de build tourne sur le port 9090",
            "memoire.md",
        ),
    ];
    for (ctype, said, op, uid, text, file) in cases {
        let (_dir, d, p) = daemon().await;
        let s = &d.services;
        let vault = tutoiement(s).await;
        note(&d, ctype, said, Origin::Owner, "s1", 7).await;
        p.reply(&rewrite(op, uid, text));
        let o = run(&d, &d.hooks.messenger, false).await.unwrap();
        assert_eq!(o.report.promoted, 1, "{said} : {:?}", o.report);
        let raw = std::fs::read_to_string(vault.join(file)).unwrap();
        assert!(raw.contains(text), "{said} : {raw}");
        assert!(
            !s.approvals
                .pending(10)
                .await
                .unwrap()
                .iter()
                .any(|a| a.payload["contradiction"] == true),
            "{said} : aucune carte"
        );
    }
}

/// Une entrée que le propriétaire a écrite lui-même (`mem_remember`, provenance
/// `owner`) est protégée comme le profil, où qu'elle soit rangée (#224).
#[tokio::test]
async fn an_entry_written_by_the_owner_is_protected_outside_the_profile() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    let uid = crate::vault_ops::remember(
        s,
        &vault,
        Level::Coeur,
        "Les factures ACME partent en dollars américains",
        "s1",
    )
    .await
    .unwrap();
    note(
        &d,
        CandidateType::Decision,
        "Facturer ACME en euros désormais.",
        Origin::Agent,
        "s2",
        6,
    )
    .await;
    p.reply(&rewrite(
        "supersede_entry",
        &uid,
        "Les factures ACME partent en euros",
    ));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 0, "{:?}", o.report);
    let raw = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    assert!(raw.contains("dollars américains"), "{raw}");
    assert_eq!(o.report.questions.len(), 1, "{:?}", o.report);
}

/// Le même vouvoiement proposé en `add_entry` : la substitution tu/vous est vue par le
/// contrôle de contradiction, là où la négation seule laissait deux règles opposées
/// cohabiter au profil (#224). Sans embeddings, le voisin se trouve par un mot commun.
#[tokio::test]
async fn an_added_substitution_of_the_owner_preference_asks() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let vault = tutoiement(s).await;
    note(
        &d,
        CandidateType::Preference,
        "Le propriétaire préfère qu'on le vouvoie.",
        Origin::Owner,
        "s10",
        7,
    )
    .await;
    p.reply(&keep("Le propriétaire préfère être vouvoyé"));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 0, "{:?}", o.report);
    assert_eq!(o.report.questions.len(), 1, "{:?}", o.report);
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(!profil.contains("vouvoy"), "{profil}");
}
