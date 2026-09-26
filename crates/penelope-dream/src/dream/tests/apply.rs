use super::*;

#[tokio::test]
async fn a_stated_rule_is_promoted_once_with_history_and_review() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    note(
        &d,
        CandidateType::Preference,
        "Toujours répondre en français",
        Origin::Owner,
        "s1",
        6,
    )
    .await;
    let keep = r#"{"tri": [{"candidat": 1, "durable": true, "utile": true, "precis": true,
                "introuvable": true, "endosse": true, "justification": "règle dite par le propriétaire"}],
            "operations": [{"op": "add_entry", "candidat": 1, "file": "profil.md", "section": "Préférences",
                "text": "Toujours répondre en français", "importance": 7,
                "declencheurs": ["langue"]}]}"#;
    p.reply(keep);

    // À blanc : le rapport dit ce qui serait fait, rien n'est écrit.
    let dry = run(&d, &d.hooks.messenger, true).await.unwrap();
    assert_eq!(dry.report.promoted, 1);
    let vault = crate::helpers::vault_dir(s);
    assert!(!vault.join("profil.md").exists());
    assert_eq!(s.candidates.pending(None).await.unwrap().len(), 1);

    p.reply(keep);
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    assert_eq!(o.report.files_touched, vec!["profil.md"]);

    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(profil.contains("## Préférences"));
    let line = profil
        .lines()
        .find(|l| l.contains("Toujours répondre en français"))
        .expect("entrée écrite");
    let uid = Annotations::parse(line).uid.expect("uid");
    let indexed = s.memory.get(&uid).await.unwrap().expect("indexée");
    assert_eq!(indexed.level, Level::Profil);
    assert_eq!(s.memory.origin_of(&uid).await.unwrap(), Some(Origin::Agent));
    assert!(s.candidates.pending(None).await.unwrap().is_empty());
    let dreams = std::fs::read_to_string(vault.join("DREAMS.md")).unwrap();
    assert!(dreams.contains(&o.run_id));
    assert!(
        dreams.contains("### Tri")
            && dreams.contains("✅ gardé « Toujours répondre en français »")
            && dreams.contains("règle dite par le propriétaire"),
        "{dreams}"
    );

    let hist = history(s, Some(&uid), None).await.unwrap();
    assert_eq!(hist.as_array().unwrap().len(), 1);
    let learned = learned(s, 7).await.unwrap();
    assert_eq!(learned[0]["text"], "Toujours répondre en français");
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(digest.contains("Appris cette nuit : 1"), "{digest}");

    // Une seconde passe sans nouvelle donnée ne change rien et n'appelle pas le modèle.
    let calls = p.requests().len();
    let again = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(again.report.is_noop());
    assert_eq!(p.requests().len(), calls);

    // La pré-image rend le fichier tel qu'il était.
    let id = hist[0]["id"].as_i64().unwrap();
    let restored = restore(s, id).await.unwrap();
    assert_eq!(restored["file"], "profil.md");
    assert!(
        !std::fs::read_to_string(vault.join("profil.md"))
            .unwrap()
            .contains("français")
    );
}

/// Issue #27 : un vault neuf avec autocommit actif devient un dépôt, un rêve y laisse un
/// commit qui touche `memoire.md`, et `mem diff --since dream` le montre.
#[tokio::test]
async fn a_dream_is_committed_in_the_vault_history() {
    if penelope_platform::which("git").is_none() {
        return;
    }
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    assert!(
        crate::vault_git::ensure_repo(s).await.unwrap(),
        "dépôt créé"
    );
    assert!(vault.join(".git").exists() && vault.join(".gitignore").exists());
    assert!(crate::vault_git::warning(s).is_none());
    assert!(
        !crate::vault_git::ensure_repo(s).await.unwrap(),
        "idempotent"
    );

    note(
        &d,
        CandidateType::Fait,
        "Le propriétaire héberge ses projets sur un serveur à Roubaix",
        Origin::Owner,
        "s1",
        9,
    )
    .await;
    p.reply(
        r#"{"tri": [{"candidat": 1, "durable": true, "utile": true, "precis": true,
                "introuvable": true, "endosse": true}],
              "operations": [{"op": "add_entry", "candidat": 1, "file": "memoire.md",
                "text": "Le propriétaire héberge ses projets sur un serveur à Roubaix", "importance": 6}]}"#,
    );
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);

    let (_, log, _) =
        penelope_tools::git::run(&vault, &["log", "--format=%s", "--name-only", "-n", "1"])
            .await
            .unwrap();
    let expected = format!("rêve du {} ({}) : 1 promues", today(s), o.run_id);
    assert!(log.starts_with(&expected), "{log}");
    assert!(log.contains("memoire.md"), "{log}");

    let diff = crate::vault_git::diff(s, true).await.unwrap();
    assert!(
        diff["text"].as_str().unwrap().contains("serveur à Roubaix"),
        "{diff}"
    );
    let clean = crate::vault_git::diff(s, false).await.unwrap();
    assert!(
        clean["text"]
            .as_str()
            .unwrap()
            .starts_with("Aucun changement"),
        "{clean}"
    );
}

/// Issue #37 : la grille trie, le rêve met à jour plutôt qu'empiler, date les faits,
/// range les états passagers au journal jusqu'à leur expiration, écarte ce qui se
/// retrouve ailleurs, retente ce qui n'a pas de verdict, et propose au retrait ce qui ne
/// sert jamais.
#[tokio::test]
async fn the_grid_updates_journals_and_ages_the_memory() {
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
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("memoire.md"),
        "# Mémoire de fond\n\n## Infrastructure\n\
             - La base de données Atlas dev écoute sur 127.0.0.1 <!-- depuis: 2026-06-01 --> ^01OLDBASE\n\
             - Le client Martin est basé à Lyon <!-- depuis: 2026-06-01 --> ^01MARTIN\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();

    use CandidateType::{Fait, Preference};
    for (ctype, text, origin) in [
        (
            Preference,
            "Sur Atlas on pousse toujours sur dev d'abord, jamais de merge vers qa",
            Origin::Owner,
        ),
        (
            Fait,
            "La base de données Atlas dev écoute sur 127.0.0.1:40000, base atlas-dev",
            Origin::Owner,
        ),
        (
            Fait,
            "Le ticket 4821 des factures Stripe est corrigé en dev par le commit a0af5de",
            Origin::Agent,
        ),
        (
            Fait,
            "La proposition commerciale Dupont n'est pas encore lue",
            Origin::Owner,
        ),
        (
            Fait,
            "Le propriétaire travaille sur le ticket 4821",
            Origin::Agent,
        ),
        (Fait, "Le client Martin est basé à Lyon", Origin::Owner),
        (
            Fait,
            "Le serveur de prod Atlas tourne sous Debian 12",
            Origin::Owner,
        ),
    ] {
        note(&d, ctype, text, origin, "s1", 6).await;
    }
    let raw = r#"{"candidats": [{"type": "fait", "texte": "La clé Stripe de test du projet Atlas est sk_test_FauxCle0123456", "importance": 6}]}"#;
    crate::review::record_candidates(s, raw, "s2", "turn:t1", false, 5)
        .await
        .unwrap();

    let n = |needle: &'static str| number(s, needle);
    let (rule, base, ticket, propale, works, martin, stripe) = (
        n("pousse toujours").await,
        n("127.0.0.1:40000").await,
        n("corrigé en dev").await,
        n("Dupont").await,
        n("travaille sur").await,
        n("Martin").await,
        n("clé Stripe").await,
    );
    let stripe_text = submission_order(s).await.unwrap()[stripe - 1].clone();
    let all = |c: usize, why: &str| {
        json!({"candidat": c, "durable": true, "utile": true, "precis": true,
               "introuvable": true, "endosse": true, "justification": why})
    };
    let mut passing = all(ticket, "corrigé aujourd'hui, inutile dans un mois");
    passing["durable"] = json!(false);
    let mut unread = all(propale, "document pas encore lu");
    unread["durable"] = json!(false);
    unread["expire"] = json!("2026-09-20");
    let mut elsewhere = all(works, "dans le tracker et git");
    elsewhere["introuvable"] = json!(false);
    let reply = json!({
        "tri": [all(rule, "règle dite par le propriétaire"), all(base, "corrige l'adresse"),
                passing, unread, elsewhere, all(martin, "déjà connu"),
                all(stripe, "clé de test rangée")],
        "operations": [
            {"op": "add_entry", "candidat": rule, "file": "profil.md", "section": "Git",
             "text": "Sur Atlas, toujours pousser sur dev d'abord, jamais de merge vers qa"},
            {"op": "add_entry", "candidat": rule, "file": "profil.md", "section": "Git",
             "text": "Sur Atlas, toujours pousser sur dev d'abord, jamais de merge vers qa"},
            {"op": "supersede_entry", "candidat": base, "uid": "01OLDBASE",
             "text": "La base de données Atlas dev écoute sur 127.0.0.1:40000, base atlas-dev",
             "reason": "port et base précisés"},
            {"op": "add_entry", "candidat": ticket, "file": "projets.md",
             "text": "Ticket 4821 (factures Stripe) corrigé en dev, commit a0af5de"},
            {"op": "add_entry", "candidat": works, "file": "memoire.md",
             "text": "Le propriétaire travaille sur le ticket 4821"},
            {"op": "noop", "candidat": martin, "reason": "déjà en mémoire (01MARTIN)"},
            {"op": "add_entry", "candidat": stripe, "file": "memoire.md", "section": "Accès",
             "text": stripe_text}
        ]
    });
    p.reply(&reply.to_string());
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();

    first_night_is_sorted_dated_and_journaled(s, &vault, &o).await;

    // Vingt jours plus tard : les états passagers expirés quittent le journal.
    clock.advance_secs(20 * 86_400);
    p.reply(
        &json!({"tri": [all(1, "version du système de prod")],
                "operations": [{"op": "add_entry", "candidat": 1, "file": "memoire.md",
                                "section": "Infrastructure",
                                "text": "Le serveur de prod Atlas tourne sous Debian 12"}]})
        .to_string(),
    );
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.journal_expired, 2, "{:?}", o.report);
    let projets = std::fs::read_to_string(vault.join("projets.md")).unwrap();
    assert!(
        !projets.contains("4821") && !projets.contains("Dupont"),
        "{projets}"
    );
    assert!(o.report.unused.is_empty(), "moins de 60 jours de mesure");

    // Soixante-dix jours après la première passe : ce qui n'a jamais servi est proposé
    // au retrait, pas ce qui a été rappelé.
    clock.advance_secs(50 * 86_400);
    s.memory
        .record_recall("01MARTIN", "où est Martin ?", true)
        .await
        .unwrap();
    // Vue dix fois dans les résultats sans être retenue : elle a eu sa chance (#86).
    let base_uid: String = s
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT uid FROM mem_entries WHERE text LIKE '%127.0.0.1:40000%'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    for _ in 0..penelope_memory::grid::SEEN_BEFORE_RETIRE {
        s.memory
            .record_seen(std::slice::from_ref(&base_uid))
            .await
            .unwrap();
    }
    // Le signal ne porte que sur ce qui n'est pas servi d'office : le budget de
    // l'instantané est ramené à rien pour ce tour (issue #62).
    d.services
        .publish_config("test", |c| {
            c.memory.core_budget_tokens = 0;
            c.memory.project_budget_tokens = 0;
            Ok(vec!["memory.core_budget_tokens".into()])
        })
        .unwrap();
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(
        o.report
            .unused
            .iter()
            .any(|u| u.contains("127.0.0.1:40000")),
        "{:?}",
        o.report.unused
    );
    assert!(!o.report.unused.iter().any(|u| u.contains("Martin")));
    assert!(
        !o.report.unused.iter().any(|u| u.contains("Debian")),
        "trop récente"
    );
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(
        digest.contains("Jamais rappelées depuis 60 jours"),
        "{digest}"
    );
}

/// La première nuit de la grille (issue #37) : mise à jour plutôt qu'accumulation,
/// états passagers au journal, secret rangé, ce qui est ailleurs ignoré, ce qui n'a pas
/// de verdict retenté, et `DREAMS.md` qui dit chaque tri.
async fn first_night_is_sorted_dated_and_journaled(
    s: &Services,
    vault: &std::path::Path,
    o: &DreamOutcome,
) {
    let memoire = std::fs::read_to_string(vault.join("memoire.md")).unwrap();
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    let projets = std::fs::read_to_string(vault.join("projets.md")).unwrap();
    // Mise à jour plutôt qu'accumulation : l'ancienne adresse est remplacée, liée, datée.
    let base_line = memoire
        .lines()
        .find(|l| l.contains("40000"))
        .expect("adresse");
    assert!(
        base_line.contains("remplace: 01OLDBASE") && base_line.contains("depuis: 2026-09-16"),
        "{base_line}"
    );
    assert!(!memoire.contains("^01OLDBASE"), "{memoire}");
    assert!(
        !s.memory
            .by_level(Level::Coeur)
            .await
            .unwrap()
            .iter()
            .any(|e| e.uid == "01OLDBASE")
    );
    assert_eq!(
        profil.matches("pousser sur dev d'abord").count(),
        1,
        "jamais doublée"
    );
    assert_eq!(memoire.matches("Martin").count(), 1, "noop");
    // Journal : états passagers, expirations bornées.
    assert!(projets.contains("## États en cours"), "{projets}");
    let ticket_line = projets
        .lines()
        .find(|l| l.contains("4821"))
        .expect("journal");
    assert!(ticket_line.contains("expire: 2026-09-30"), "{ticket_line}");
    let propale_line = projets
        .lines()
        .find(|l| l.contains("Dupont"))
        .expect("journal");
    assert!(
        propale_line.contains("expire: 2026-09-20"),
        "{propale_line}"
    );
    assert_eq!(o.report.journal, 2, "{:?}", o.report);
    // Retrouvable ailleurs : ignoré, même si le modèle proposait de l'écrire.
    assert!(!memoire.contains("travaille sur"));
    // Secret : la référence, jamais la valeur.
    assert!(memoire.contains("${SECRET:cle-stripe-"), "{memoire}");
    assert!(!memoire.contains("sk_test_"));
    assert_eq!(o.report.secrets.len(), 1);
    // Sans verdict : retenté la nuit suivante.
    assert!(!memoire.contains("Debian"));
    let pending = s.candidates.pending(None).await.unwrap();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert!(pending[0].text.contains("Debian"));
    let dreams = std::fs::read_to_string(vault.join("DREAMS.md")).unwrap();
    for expected in [
        "✅ gardé « Sur Atlas on pousse",
        "🗓 journal « La proposition commerciale Dupont",
        "jusqu'au 2026-09-20",
        "⏭ ignoré « Le propriétaire travaille sur le ticket 4821 »",
        "introuvable ailleurs ✗",
        "＝ déjà en mémoire « Le client Martin est basé à Lyon »",
        "＝ déjà en mémoire « Sur Atlas, toujours pousser",
        "⏳ en attente « Le serveur de prod Atlas tourne sous Debian 12 »",
    ] {
        assert!(dreams.contains(expected), "{expected} :\n{dreams}");
    }
}

#[tokio::test]
async fn corrections_become_exceptions_of_an_existing_practice() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(
        vault.join("pratiques/langage-backend.md"),
        "---\ntype: pratique\nid: langage-backend\nconfiance: 0.8\n---\n# Langage backend\n\n## Défaut\n- Go (stdlib) <!-- uid: DEF1 -->\n\n## Exceptions\n\n## Écarts observés\n",
    )
    .unwrap();
    crate::vault_ops::reindex(s, &vault).await.unwrap();
    note(
        &d,
        CandidateType::Correction,
        "Pour le code critique, on écrit en Rust",
        Origin::Owner,
        "s1",
        9,
    )
    .await;
    p.reply(
        r#"{"tri": [{"candidat": 1, "durable": true, "utile": true, "precis": true,
                "introuvable": true, "endosse": true}],
              "operations": [
                {"op": "add_exception", "candidat": 1, "practice": "langage-backend", "text": "Rust", "quand": "tache=code; criticite=haute"},
                {"op": "update_default", "candidat": 1, "practice": "langage-backend", "text": "Rust partout"},
                {"op": "add_entry", "candidat": 1, "file": "../../etc/passwd", "text": "x"}
            ]}"#,
    );
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    assert_eq!(o.report.proposals, 1, "le défaut reste une proposition");
    let raw = std::fs::read_to_string(vault.join("pratiques/langage-backend.md")).unwrap();
    let practice = Practice::parse(&raw, "langage-backend").unwrap();
    assert_eq!(practice.exceptions.len(), 1);
    assert_eq!(practice.exceptions[0].text, "Rust");
    assert_eq!(
        practice.default_entry.unwrap().text,
        "Go (stdlib)",
        "le défaut n'a pas bougé"
    );
    assert!(
        o.report.rejected.iter().any(|r| r.contains("add_entry")),
        "{:?}",
        o.report.rejected
    );
}

#[tokio::test]
async fn the_vault_check_flags_secrets_and_broken_practices() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let vault = crate::helpers::vault_dir(s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(
        vault.join("notes.md"),
        "# Notes\n- mot de passe ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    std::fs::write(
        vault.join("pratiques/cassee.md"),
        "---\ntype: autre\n---\n# x\n",
    )
    .unwrap();
    let report = vault_check(s).await;
    assert_eq!(report["ok"], false);
    let issues = report["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["file"] == "notes.md" && i["severity"] == "error")
    );
    assert!(issues.iter().any(|i| i["file"] == "pratiques/cassee.md"));
    crate::helpers::set_config_path(&d.services, "memory.vault_git_autocommit", json!("0s"))
        .unwrap();
    let sync = vault_sync(&d.services, "test").await.unwrap();
    assert_eq!(
        sync["git"], false,
        "autocommit désactivé : le vault reste hors git"
    );
}
