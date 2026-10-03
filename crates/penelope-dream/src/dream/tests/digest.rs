use super::*;
use crate::testing::RecordingMessenger;

/// répète pas le même message ; le digest dit la nuit ratée ; la nuit suivante promeut
/// chaque candidat une seule fois.
#[tokio::test]
async fn a_failed_night_keeps_what_it_wrote_and_is_said_once() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let rec = RecordingMessenger::new();
    *d.hooks.messenger.write().unwrap() = Some(rec.clone() as Arc<dyn crate::ports::Messenger>);
    d.services
        .publish_config("test", |c| {
            c.memory.dream_batch = 1;
            c.memory.dream_retry_wait = "1ms".into();
            Ok(vec!["memory.dream_batch".into()])
        })
        .unwrap();
    for text in [
        "Toujours répondre en français",
        "Toujours tutoyer le propriétaire",
    ] {
        note(&d, CandidateType::Preference, text, Origin::Owner, "s1", 6).await;
    }
    let order = submission_order(s).await.unwrap();
    assert_eq!(order.len(), 2);
    let states = |c: Vec<Candidate>| {
        let mut v: Vec<(String, String)> = c
            .into_iter()
            .map(|c| (c.id, format!("{:?}", c.state)))
            .collect();
        v.sort();
        v
    };
    let before = states(s.candidates.pending(None).await.unwrap());
    // #135 : une passe qui s'arrête avant ses verdicts ne consomme aucun report.
    let deferrals = || async {
        s.store
            .read(|c| {
                let mut st = c.prepare("SELECT id, deferrals FROM mem_candidates ORDER BY id")?;
                let rows =
                    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap()
    };
    let deferrals_before = deferrals().await;

    p.reply(&keep(&order[0]));
    for _ in 0..3 {
        p.push(passing_error());
    }
    nightly(&d, &d.hooks.messenger).await;
    assert_eq!(p.call_count(), 4, "un lot, puis trois essais du second");
    let vault = crate::helpers::vault_dir(s);
    // #152 : le lot qui a abouti est écrit avant que le suivant soit tenté. Jusqu'ici
    // la passe accumulait tout jusqu'à la fin, et une erreur au deuxième lot jetait le
    // premier — le 21/09, sept lots et 126 candidats perdus de cette façon.
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(profil.contains(order[0].as_str()), "{profil}");
    let left = s.candidates.pending(None).await.unwrap();
    assert_eq!(
        left.len(),
        1,
        "le candidat écrit est marqué, l'autre attend"
    );
    assert_eq!(
        left[0].text, order[1],
        "c'est bien le lot non jugé qui reste"
    );
    assert_ne!(states(left), before);
    // #135 : la passe s'est arrêtée avant les verdicts du second lot, qui ne consomme
    // donc aucun report.
    assert_eq!(deferrals().await, deferrals_before, "aucun report consommé");
    assert_eq!(
        history(s, None, Some("profil.md"))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1,
        "l'écriture du premier lot est dans l'historique"
    );
    let dreams = std::fs::read_to_string(vault.join("DREAMS.md")).unwrap();
    assert!(dreams.contains(": échec"), "{dreams}");
    assert!(dreams.contains("Upstream idle timeout"), "{dreams}");
    assert!(
        dreams.contains("1 entrée(s) écrite(s) avant l'arrêt"),
        "l'échec dit ce qui a été gardé : {dreams}"
    );
    let failed = s.events.range(0, 1_000).await.unwrap();
    assert!(
        failed.iter().any(|e| e.kind == "memory.dream_failed"),
        "{failed:?}"
    );
    let sent = rec.texts();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("La consolidation de cette nuit a échoué"),
        "{sent:?}"
    );
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(
        digest.contains("Pas de consolidation cette nuit"),
        "{digest}"
    );
    assert!(!digest.contains("Appris cette nuit"), "{digest}");

    // Même panne deux nuits de plus : aucun nouveau message ; une autre raison : un.
    let reason = failed
        .iter()
        .find(|e| e.kind == "memory.dream_failed")
        .unwrap()
        .payload["error"]
        .as_str()
        .unwrap()
        .to_string();
    night_failed(&d, &d.hooks.messenger, &reason).await;
    night_failed(&d, &d.hooks.messenger, &reason).await;
    assert_eq!(rec.texts().len(), 1);
    night_failed(&d, &d.hooks.messenger, "Payment required").await;
    let sent = rec.texts();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].contains("4 nuits de suite"), "{sent:?}");

    // La nuit suivante réussit : chaque candidat est promu une fois, sans message.
    let order = submission_order(s).await.unwrap();
    for text in &order {
        p.reply(&keep(text));
    }
    nightly(&d, &d.hooks.messenger).await;
    assert!(s.candidates.pending(None).await.unwrap().is_empty());
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    for text in &order {
        assert_eq!(profil.matches(text.as_str()).count(), 1, "{profil}");
    }
    assert_eq!(rec.texts().len(), 2, "un succès ne dit rien");
    assert!(
        d.services
            .kv_get(FAILED_NIGHTS_KEY)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !digest_text(&d, d.hooks.mcp_supervisor())
            .await
            .unwrap()
            .contains("Pas de consolidation")
    );
}

/// #297 : un candidat écarté par la grille compte une fois, quoi que le modèle propose
/// dessus ; son numéro n'est pas un motif. Le digest du 02/10 disait 20 promus et 185
/// écartés pour 189 examinés, avec « candidat 16 écarté par la grille : 4 » en motif.
#[tokio::test]
async fn a_candidate_rejected_by_the_grid_is_counted_once_whatever_the_model_proposes() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let kept = "Le serveur de production Atlas tourne sous Debian 12 chez OVH.";
    let elsewhere = [
        "Le ticket 4821 du tracker concerne la facture du client Martin.",
        "La branche feature-paiement du dépôt Atlas attend sa relecture.",
    ];
    for text in [kept, elsewhere[0], elsewhere[1]] {
        note(&d, CandidateType::Fait, text, Origin::Owner, "s1", 6).await;
    }
    let (k, e1, e2) = (
        number(s, kept).await,
        number(s, elsewhere[0]).await,
        number(s, elsewhere[1]).await,
    );
    let verdict = |n: usize, introuvable: bool| {
        format!(
            r#"{{"candidat": {n}, "durable": true, "utile": true, "precis": true,
                 "introuvable": {introuvable}, "endosse": true, "justification": "j"}}"#
        )
    };
    let add = |n: usize, text: &str| {
        format!(
            r#"{{"op": "add_entry", "candidat": {n}, "file": "memoire.md", "text": "{text}",
                 "importance": 6}}"#
        )
    };
    // Le modèle écarte deux candidats et propose quand même deux opérations sur chacun.
    p.reply(&format!(
        r#"{{"tri": [{}, {}, {}], "operations": [{}, {}, {}, {}, {}]}}"#,
        verdict(k, true),
        verdict(e1, false),
        verdict(e2, false),
        add(k, kept),
        add(e1, elsewhere[0]),
        add(e1, "Le client Martin a une facture ouverte au ticket 4821."),
        add(e2, elsewhere[1]),
        add(
            e2,
            "La relecture de la branche feature-paiement est attendue."
        ),
    ));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    let r = &o.report;
    assert_eq!(r.candidates_seen, 3);
    assert_eq!(r.promoted, 1, "{r:?}");
    assert_eq!(r.rejected.len(), 2, "{:?}", r.rejected);
    assert_eq!(
        r.promoted as usize + r.rejected.len(),
        r.candidates_seen as usize
    );
    assert!(s.candidates.pending(None).await.unwrap().is_empty());
    let families = rejection_families(&r.rejected);
    assert_eq!(
        families,
        vec![("retrouvable ailleurs".to_string(), 2)],
        "{families:?}"
    );
    assert_eq!(
        r.sorted
            .iter()
            .filter(|l| l.contains("non appliquée : candidat"))
            .count(),
        4,
        "les opérations sur un écarté vont au journal du tri : {:?}",
        r.sorted
    );
    let t = night_summary(&o.run_id, r, &[]);
    assert!(
        t.contains("3 candidat(s) examiné(s) : 1 promu(s), 2 écarté(s)"),
        "{t}"
    );
    assert!(t.contains("- retrouvable ailleurs : 2"), "{t}");
    assert!(
        !t.contains("candidat ") && !t.contains("autres motifs"),
        "{t}"
    );
}

/// #109 : une nuit de 24 candidats sans rien promouvoir se lit en trois chiffres et
/// en motifs groupés par famille, jamais en liste intégrale.
#[test]
fn a_night_is_told_in_three_numbers_and_grouped_reasons() {
    let mut report = DreamReport {
        candidates_seen: 24,
        ..Default::default()
    };
    for i in 0..18 {
        report.rejected.push(format!(
            "« Le client {i} veut ça » : imprécis : sujet ou phrase incomplets"
        ));
    }
    for i in 0..4 {
        report.rejected.push(format!(
            "« Ticket {i} corrigé » : retrouvable ailleurs (code, docs, tracker, git)"
        ));
    }
    report
        .rejected
        .push("add_entry : contenu interdit : numéro de carte".into());
    report
        .rejected
        .push("« Source web » : origine non promouvable (contenu non fiable ou système)".into());
    let t = night_summary("d_1", &report, &[report.clone()]);
    assert!(
        t.contains("24 candidat(s) examiné(s) : 0 promu(s), 24 écarté(s)"),
        "{t}"
    );
    for line in [
        "- imprécis : 18",
        "- retrouvable ailleurs : 4",
        "- contenu interdit : 1",
        "- origine non promouvable : 1",
    ] {
        assert!(t.contains(line), "{line} :\n{t}");
    }
    assert!(!t.contains("Le client 3"), "pas de liste intégrale");
    assert!(t.contains("`d_1`"));
    assert!(!t.contains("nuits de suite"), "une seule nuit");
}

/// #109 : cinquante rejets tiennent dans un message Telegram ; deux nuits de suite sans
/// rien promouvoir le disent, une nuit normale non.
#[tokio::test]
async fn quiet_nights_are_said_and_the_digest_stays_short() {
    let (_dir, d, _p) = daemon().await;
    let s = &d.services;
    let night = |n: u32, promoted: u32, rejected: usize| {
        let mut r = DreamReport {
            candidates_seen: n,
            promoted,
            ..Default::default()
        };
        for i in 0..rejected {
            r.rejected.push(format!(
                "« {} » : motif numéro {} : {}",
                "une longue phrase de candidat écarté ".repeat(4),
                i % 12,
                "précision ".repeat(10)
            ));
        }
        r
    };
    let insert = |id: &'static str, started: &'static str, r: DreamReport| async move {
        record_run(s, id, started, "light", None).await.unwrap();
        finish_run(s, id, "done", &r, None).await.unwrap();
    };

    insert("d_1", "2026-09-15T01:30:00Z", night(12, 3, 2)).await;
    insert("d_2", "2026-09-16T01:30:00Z", night(50, 0, 50)).await;
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(digest.chars().count() < 4_096, "{}", digest.chars().count());
    assert!(digest.contains("50 candidat(s) examiné(s) : 0 promu(s), 50 écarté(s)"));
    assert!(digest.contains("- autres motifs :"), "{digest}");
    assert!(
        !digest.contains("nuits de suite"),
        "une seule nuit vide : {digest}"
    );

    insert("d_3", "2026-09-17T01:30:00Z", night(2, 0, 2)).await;
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(
        digest.contains("⚠️ 2 nuits de suite sans rien retenir"),
        "{digest}"
    );
    assert!(digest.contains("52 candidat(s) examiné(s)"), "{digest}");
    assert!(
        digest.contains("motif dominant « motif numéro 0 »"),
        "{digest}"
    );

    insert("d_4", "2026-09-18T01:30:00Z", night(5, 2, 1)).await;
    let digest = digest_text(&d, d.hooks.mcp_supervisor()).await.unwrap();
    assert!(!digest.contains("nuits de suite"), "{digest}");
}

/// #145 : le digest du matin tient en une bulle — pas de wikilink brut, pas
/// d'avertissement interne, pas de journal ni de secrets rangés ; le détail reste
/// dans `DREAMS.md`.
#[test]
fn the_morning_digest_is_short_and_readable() {
    let mut report = DreamReport {
        promoted: 113,
        proposals: 0,
        ..Default::default()
    };
    report.files_touched = vec!["memoire.md".into(), "projets.md".into()];
    report.promoted_refs = (0..113)
        .map(|i| format!("[[memoire#^01M2Y1QK{i}]]"))
        .collect();
    report.questions = vec!["Tu as dit « a », j'avais « b » : ?".into(); 2];
    report.warnings = vec![
        "consolidation coupée sur 40 candidats : reprise par 20".into(),
        "erreur passagère (Transient) reprise 1/2 après 120 s".into(),
        "niveau Cœur à ~3021 jetons pour un budget de 1200".into(),
    ];
    report.journal = 4;
    report.secrets = vec!["cle_api".into()];
    report.lint = vec!["43 liens non résolus".into()];
    // Ce que la nuit a appris, en clair, et le nettoyage proposé.
    report.promoted_examples = (0..5)
        .map(|i| format!("Yobbu ouvre son catalogue le {i} octobre (memoire.md)"))
        .collect();
    report.cleanup = (0..6)
        .map(|i| format!("PROJET N°{i} … (3 188 caractères) — `penelope mem split 01M2Y1QK{i}`"))
        .collect();
    // Ce que la nuit a fait du Cœur plein (#298) : dit, sans commande à taper.
    report.core_full = 2;
    report.demoted =
        vec!["« Le client Martin règle à trente jours » (importance 3, 0 rappel(s))".into()];

    let digest = report.render_digest();
    assert!(
        digest.chars().count() <= 1_500,
        "{} caractères",
        digest.chars().count()
    );
    assert!(
        !digest.contains("[[memoire#^"),
        "aucun wikilink brut : {digest}"
    );
    assert!(
        !digest.contains("Transient"),
        "pas de journal interne : {digest}"
    );
    assert!(!digest.contains("Secrets rangés"), "{digest}");
    assert!(!digest.contains("liens non résolus"), "{digest}");
    assert!(digest.contains("113 entrées promues"), "{digest}");
    assert!(
        digest.contains("2 question(s)"),
        "le compte, pas les questions"
    );
    // Le constat sur le Cœur reste, suivi de ce que la nuit a fait, jamais d'une
    // injonction ni d'une commande (#298).
    assert!(digest.contains("niveau Cœur à ~3021 jetons"), "{digest}");
    assert!(digest.contains("Cœur plein : 2 nouveauté(s)"), "{digest}");
    assert!(
        digest.contains("Rétrogradée du Cœur en notes : « Le client Martin"),
        "{digest}"
    );
    assert!(!digest.contains("config set"), "{digest}");
    // Ce qui a été appris se lit, et le nettoyage est proposé, jamais lancé.
    assert!(
        digest.contains("Yobbu ouvre son catalogue le 0"),
        "{digest}"
    );
    assert!(digest.contains("penelope mem split"), "{digest}");
    assert!(
        digest.contains("… et 3 autres"),
        "six entrées, trois montrées : {digest}"
    );

    // Le rapport complet, lui, garde tout : c'est ce qui va dans `DREAMS.md`.
    let full = report.render();
    assert!(full.contains("[[memoire#^01M2Y1QK0]]"));
    assert!(full.contains("Transient"));
}

/// #295 : quand `digest.agenda` est posé mais que l'agenda n'a pas pu être lu, le digest
/// le dit en une ligne, après l'« Aujourd'hui », et tronque une raison trop longue.
#[tokio::test]
async fn an_unread_agenda_is_said_in_one_line() {
    let (_dir, d, _p) = daemon().await;
    let inputs = DigestInputs {
        due_today: vec!["- 09:00 Veille → conv 42".into()],
        agenda_error: Some(format!("identifiants refusés {}", "x".repeat(300))),
        ..Default::default()
    };
    let digest = super::super::digest_text(&d, inputs, None).await.unwrap();
    let today = digest
        .find("🗓 Aujourd'hui :\n- 09:00 Veille")
        .expect(&digest);
    let unread = digest
        .find("🗓 Agenda non lu : identifiants refusés")
        .expect(&digest);
    assert!(today < unread, "{digest}");
    let line = digest
        .lines()
        .find(|l| l.starts_with("🗓 Agenda non lu"))
        .unwrap();
    assert!(line.chars().count() < 230, "{line}");
    let silent = super::super::digest_text(&d, DigestInputs::default(), None)
        .await
        .unwrap();
    assert!(!silent.contains("Agenda"), "{silent}");
}
