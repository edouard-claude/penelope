use super::*;

/// Harnais à horloge réglable : la seconde nuit doit être un autre jour que la première,
/// sans quoi ce qui vient d'être écrit est protégé de la descente.
async fn clocked() -> (
    tempfile::TempDir,
    Arc<Harness>,
    Arc<MockProvider>,
    TestClock,
) {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_789_516_800_000);
    let shared: penelope_kernel::clock::SharedClock = Arc::new(clock.clone());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), shared)
            .await
            .unwrap(),
    );
    let p = Arc::new(MockProvider::new());
    let d = harness(s, p.clone());
    (dir, d, p, clock)
}

/// Verdict « gardé » et `add_entry` dans `memoire.md`, pour chaque `(texte, importance)`,
/// dans l'ordre donné : la nuit traite les opérations dans cet ordre.
async fn kept_in_core(s: &Services, entries: &[(&str, u8)]) -> String {
    let mut tri = Vec::new();
    let mut ops = Vec::new();
    for (text, importance) in entries {
        let n = number(s, text).await;
        tri.push(format!(
            r#"{{"candidat": {n}, "durable": true, "utile": true, "precis": true,
                 "introuvable": true, "endosse": true, "justification": "fait stable"}}"#
        ));
        ops.push(format!(
            r#"{{"op": "add_entry", "candidat": {n}, "file": "memoire.md",
                 "section": "Faits", "text": "{text}", "importance": {importance}}}"#
        ));
    }
    format!(
        r#"{{"tri": [{}], "operations": [{}]}}"#,
        tri.join(","),
        ops.join(",")
    )
}

fn cost(text: &str) -> u64 {
    (text.chars().count() as u64 / 4).max(1)
}

fn uid_of(file: &str, text: &str) -> String {
    let line = file
        .lines()
        .find(|l| l.contains(text))
        .unwrap_or_else(|| panic!("« {text} » absent :\n{file}"));
    Annotations::parse(line).uid.expect("uid")
}

async fn core_block(s: &Services) -> String {
    penelope_vault::snapshot::fresh_snapshot(s, &crate::session_project::Scope::All).await[1]
        .clone()
}

/// #298 : le Cœur plein, la nuit range en notes ce que le bloc ne servirait pas, écrit au
/// Cœur ce qui s'y classe et fait descendre, une fois par nuit, la moins utile des entrées
/// hors du bloc ; jamais ce que le propriétaire a écrit ni ce qui reprend sa phrase. Le
/// digest dit ce qui a été fait, sans commande à taper, et le bloc T2 ne change que par la
/// nuit.
#[tokio::test]
async fn a_full_core_files_newcomers_in_notes_and_demotes_one_entry_per_night() {
    let (_dir, d, p, clock) = clocked().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    // Écrite par le propriétaire lui-même : provenance `owner`, jamais rétrogradée.
    let hand = "La société du propriétaire tient sa comptabilité en euros, sans exception.";
    std::fs::write(
        vault.join("memoire.md"),
        format!("# Mémoire de fond\n\n## Faits\n- {hand} <!-- importance: 2 --> ^01HAND\n"),
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();

    // Première nuit : quatre faits au Cœur, dont un qui reprend la phrase du propriétaire.
    let debian = "Le serveur de production Atlas tourne sous Debian 12 chez OVH à Roubaix.";
    let martin = "Le client Martin règle ses commandes à trente jours fin de mois.";
    let durand = "Le dépôt principal du client Durand est hébergé sur une forge GitLab privée.";
    let quoted = "L'agence ferme ses bureaux le vendredi après-midi toute l'année.";
    for text in [debian, martin, durand] {
        note(&d, CandidateType::Fait, text, Origin::Owner, "s1", 6).await;
    }
    s.candidates
        .record(
            vec![
                Candidate::new(
                    CandidateType::Fait,
                    quoted,
                    Origin::Agent,
                    "interactive",
                    &s.clock.now_rfc3339(),
                )
                .said_by_owner(
                    "Retiens que l'agence ferme ses bureaux le vendredi après-midi toute l'année",
                )
                .in_session("s1"),
            ],
            5,
        )
        .await
        .unwrap();
    p.reply(&kept_in_core(s, &[(debian, 4), (martin, 6), (durand, 6), (quoted, 3)]).await);
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 4, "{:?}", o.report);
    assert!(
        o.report.demoted.is_empty() && o.report.core_full == 0,
        "{:?}",
        o.report
    );
    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    let debian_uid = uid_of(&memoire, "Debian 12");
    for _ in 0..3 {
        s.memory
            .record_recall(&uid_of(&memoire, "Martin"), "délais Martin ?", true)
            .await
            .unwrap();
    }
    s.memory
        .record_recall(&debian_uid, "quel OS sur Atlas ?", false)
        .await
        .unwrap();
    let block_after_first_night = core_block(s).await;
    assert_eq!(
        block_after_first_night,
        core_block(s).await,
        "mêmes entrées, même bloc"
    );

    // Le lendemain, le budget ne tient plus que trois entrées : les deux nouveautés
    // importantes et Martin ; Debian (importance 4, un rappel sans suite), la phrase du
    // propriétaire et sa ligne manuscrite sont hors du bloc.
    clock.advance_secs(86_400);
    let recette = "Le serveur de recette Atlas écoute sur le port 8443, réservé au réseau interne.";
    let dupont = "Le client Dupont préfère être appelé le matin, avant dix heures.";
    let backups = "Les sauvegardes du serveur Atlas partent chaque nuit vers un stockage objet.";
    let budget = cost(recette) + cost(backups) + cost(martin);
    d.services
        .publish_config("test", |c| {
            c.memory.core_budget_tokens = budget as usize;
            Ok(vec!["memory.core_budget_tokens".into()])
        })
        .unwrap();
    for text in [recette, dupont, backups] {
        note(&d, CandidateType::Fait, text, Origin::Owner, "s2", 6).await;
    }
    p.reply(&kept_in_core(s, &[(recette, 9), (dupont, 1), (backups, 9)]).await);
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 3, "{:?}", o.report);
    assert_eq!(
        o.report.core_full, 1,
        "Dupont rangé en notes : {:?}",
        o.report
    );
    assert_eq!(
        o.report.demoted.len(),
        1,
        "une descente par nuit, pas deux : {:?}",
        o.report.demoted
    );
    assert!(
        o.report.demoted[0].contains("Debian 12") && o.report.demoted[0].contains("importance 4"),
        "{:?}",
        o.report.demoted
    );

    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    let notes = std::fs::read_to_string(vault.join("notes.md")).unwrap();
    for text in [recette, backups, martin, durand, quoted, hand] {
        assert!(memoire.contains(text), "{text} :\n{memoire}");
    }
    assert!(!memoire.contains("Debian 12"), "{memoire}");
    assert!(!memoire.contains("Dupont"), "{memoire}");
    assert!(notes.contains("## Descendues du Cœur"), "{notes}");
    assert_eq!(
        uid_of(&notes, "Debian 12"),
        debian_uid,
        "même ligne, même uid : {notes}"
    );
    assert!(notes.contains(dupont), "{notes}");
    // L'index suit, et les signaux d'usage restent attachés à l'uid.
    let moved = s.memory.get(&debian_uid).await.unwrap().expect("indexée");
    assert_eq!(
        (moved.file.as_str(), moved.level, moved.pinned),
        ("notes.md", Level::Cure, false)
    );
    assert!(
        !s.memory
            .by_level(Level::Coeur)
            .await
            .unwrap()
            .iter()
            .any(|e| e.uid == debian_uid)
    );
    assert_eq!(s.memory.signals_of(&debian_uid).await.unwrap().recalls, 1);
    let hist = history(s, Some(&debian_uid), None).await.unwrap();
    let demotions: Vec<&str> = hist
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["op"] == "demote_entry")
        .map(|h| h["file"].as_str().unwrap())
        .collect();
    assert_eq!(demotions, vec!["notes.md", "memoire.md"], "{hist}");

    // Le bloc T2 : les nouveautés y sont, la descendue et Dupont non ; il a changé par la
    // nuit, et ne bouge plus ensuite.
    let block = core_block(s).await;
    assert_ne!(block, block_after_first_night);
    assert!(
        block.contains(recette) && block.contains(backups),
        "{block}"
    );
    assert!(
        !block.contains("Debian 12") && !block.contains("Dupont"),
        "{block}"
    );
    assert_eq!(block, core_block(s).await);

    // Le digest et `DREAMS.md` disent ce qui a été fait, sans injonction ni commande.
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(digest.contains("Rétrogradée du Cœur en notes"), "{digest}");
    assert!(digest.contains("Cœur plein : 1 nouveauté(s)"), "{digest}");
    assert!(
        !digest.contains("config set") && !digest.contains("alléger"),
        "{digest}"
    );
    let dreams = std::fs::read_to_string(vault.join("DREAMS.md")).unwrap();
    assert!(
        dreams.contains("↓ descendue en notes « Le serveur de production Atlas"),
        "{dreams}"
    );
    assert!(
        dreams.contains("↓ rangée en notes « Le client Dupont"),
        "{dreams}"
    );
}

/// #298 : tant que la place reste, rien ne change ; et quand seules des entrées protégées
/// sont hors du bloc, la nouveauté entre sans que rien ne descende.
#[tokio::test]
async fn protected_entries_never_leave_the_core() {
    let (_dir, d, p, clock) = clocked().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    let hand = "Le propriétaire travaille depuis son bureau de Nantes trois jours par semaine.";
    std::fs::write(
        vault.join("memoire.md"),
        format!("# Mémoire de fond\n\n## Faits\n- {hand} <!-- importance: 1 --> ^01HAND\n"),
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    let quoted = "Les réunions d'équipe de l'agence ont lieu le mardi matin à neuf heures.";
    s.candidates
        .record(
            vec![
                Candidate::new(
                    CandidateType::Fait,
                    quoted,
                    Origin::Agent,
                    "interactive",
                    &s.clock.now_rfc3339(),
                )
                .said_by_owner(
                    "Retiens que nos réunions d'équipe ont lieu le mardi matin à neuf heures",
                )
                .in_session("s1"),
            ],
            5,
        )
        .await
        .unwrap();
    p.reply(&kept_in_core(s, &[(quoted, 1)]).await);
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);

    clock.advance_secs(86_400);
    let fact = "Le client Martin héberge son site chez un prestataire de Lille.";
    d.services
        .publish_config("test", |c| {
            c.memory.core_budget_tokens = cost(fact) as usize;
            Ok(vec!["memory.core_budget_tokens".into()])
        })
        .unwrap();
    note(&d, CandidateType::Fait, fact, Origin::Owner, "s2", 6).await;
    p.reply(&kept_in_core(s, &[(fact, 8)]).await);
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    assert!(o.report.demoted.is_empty(), "{:?}", o.report.demoted);
    assert_eq!(o.report.core_full, 0);
    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    for text in [hand, quoted, fact] {
        assert!(memoire.contains(text), "{text} :\n{memoire}");
    }
    assert!(!vault.join("notes.md").exists(), "rien n'est descendu");
    assert!(
        o.report.warnings.iter().any(|w| w.contains("Cœur")),
        "le constat reste : {:?}",
        o.report.warnings
    );
}
