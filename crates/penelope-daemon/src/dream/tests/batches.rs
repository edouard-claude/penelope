use super::*;

/// #59 : beaucoup de candidats passent en plusieurs appels, chacun dimensionné, et
/// aucun n'est reporté faute de place dans la réponse.
#[tokio::test]
async fn many_candidates_are_consolidated_in_bounded_batches() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.memory.dream_batch = 10;
        Ok(vec!["memory.dream_batch".into()])
    })
    .unwrap();
    // Trente candidats bien distincts : la déduplication ne doit pas les regrouper.
    const SUJETS: [&str; 30] = [
        "facturation",
        "déploiement",
        "sauvegarde",
        "revue de code",
        "veille",
        "réunions",
        "voyages",
        "cuisine",
        "sport",
        "musique",
        "lecture",
        "jardin",
        "photo",
        "vélo",
        "piano",
        "café",
        "thé",
        "courses",
        "impôts",
        "banque",
        "assurance",
        "voiture",
        "maison",
        "chauffage",
        "internet",
        "téléphone",
        "vacances",
        "cinéma",
        "théâtre",
        "randonnée",
    ];
    for sujet in SUJETS {
        note(
            &d,
            CandidateType::Preference,
            &format!("Pour la {sujet}, le propriétaire décide seul et sans réunion"),
            Origin::Owner,
            "s1",
            6,
        )
        .await;
    }
    // Un verdict « gardé » par candidat du lot, sans opération : rien à écrire, mais
    // un verdict pour chacun.
    for _ in 0..3 {
        let tri: Vec<String> = (1..=10)
            .map(|n| {
                format!(
                    r#"{{"candidat": {n}, "durable": true, "utile": true, "precis": true,
                          "introuvable": true, "endosse": true, "justification": "règle dite"}}"#
                )
            })
            .collect();
        p.reply(&format!(
            r#"{{"tri": [{}], "operations": []}}"#,
            tri.join(",")
        ));
    }

    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(p.call_count(), 3, "trois lots de dix : {:?}", o.report);
    assert_eq!(o.report.deferred, 0, "{:?}", o.report);
    let sizes: Vec<u32> = p.requests().iter().filter_map(|r| r.max_tokens).collect();
    assert!(
        sizes.iter().all(|t| *t >= 2_000),
        "sortie dimensionnée au lot : {sizes:?}"
    );
}

/// #59 : une réponse coupée fait rejouer avec un lot réduit, et le dit.
#[tokio::test]
async fn a_truncated_consolidation_retries_with_a_smaller_batch() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.memory.dream_batch = 4;
        Ok(vec!["memory.dream_batch".into()])
    })
    .unwrap();
    for sujet in ["facturation", "déploiement", "sauvegarde", "veille"] {
        note(
            &d,
            CandidateType::Preference,
            &format!("Pour la {sujet}, le propriétaire décide seul et sans réunion"),
            Origin::Owner,
            "s1",
            6,
        )
        .await;
    }
    // Première réponse : JSON coupé, illisible. Puis deux lots de deux, corrects.
    p.reply(r#"{"tri": [{"candidat": 1, "durable": true, "uti"#);
    for _ in 0..2 {
        p.reply(
            r#"{"tri": [{"candidat": 1, "durable": false, "utile": false, "precis": true,
                    "introuvable": true, "endosse": true, "justification": "passager"},
                   {"candidat": 2, "durable": false, "utile": false, "precis": true,
                    "introuvable": true, "endosse": true, "justification": "passager"}],
                  "operations": []}"#,
        );
    }
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(
        o.report
            .warnings
            .iter()
            .any(|w| w.contains("coupée") && w.contains("reprise par 2")),
        "la troncature doit être nommée : {:?}",
        o.report.warnings
    );
    assert_eq!(p.call_count(), 3, "un lot coupé, puis deux lots de deux");
}

/// #140 : la passe de l'essai à blanc du 19 septembre, rejouée avec un modèle qui
/// écrit 250 tokens par candidat facile et 2 400 pour un épineux : trois lots
/// faciles tirent l'estimation vers le bas, puis un épineux sur dix. Avant, le
/// quatrième lot redescendait jusqu'à 1 sur sa tête épineuse, puis toute la passe
/// partait par lots d'un candidat (121 lots, 45 minutes). Elle finit maintenant en
/// moins de 20 appels, chaque candidat jugé, aucune réponse tronquée gardée.
#[tokio::test]
async fn a_verbose_candidate_does_not_leave_the_pass_one_by_one() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 160, |k| k >= 115 && k % 10 == 5).await;
    p.set_responder(Some(verbose_model(250, 2_400, usize::MAX)));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(o.report.calls < 20, "{:?}", o.report);
    assert_eq!(o.report.calls as usize, p.call_count());
    let batches = dream_batches(&d.services.events.range(0, 10_000).await.unwrap());
    let judged: u64 = batches.iter().filter(|(_, cut)| !cut).map(|(n, _)| n).sum();
    assert_eq!(judged, 160, "{batches:?}");
    let lone = batches.iter().filter(|(n, _)| *n == 1).count();
    assert!(lone <= 2, "{batches:?}");
    assert!(
        !o.report
            .warnings
            .iter()
            .any(|w| w.contains("tronquée gardée")),
        "{:?}",
        o.report.warnings
    );
}

/// #152 : un modèle qui dépense son budget de sortie en raisonnement ne fait **pas**
/// réduire les lots — réfléchir ne dépend pas du nombre de candidats. La nuit du
/// 20/09, 8 000 tokens sur un seul candidat, zéro opération, et l'échelle de #135
/// descendait jusqu'à l'échec. La passe relève maintenant le budget de réflexion et
/// rejoue le même lot.
#[tokio::test]
async fn a_starved_batch_raises_the_reasoning_budget_instead_of_shrinking() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 6, |_| false).await;
    // Le modèle ne rend rien tant qu'il n'a pas 16 000 jetons pour réfléchir.
    let seen: Arc<std::sync::Mutex<Vec<(u32, u32)>>> = Arc::new(std::sync::Mutex::new(vec![]));
    let log = seen.clone();
    p.set_responder(Some(Arc::new(move |req: &ChatRequest| {
        let budget = req.reasoning_max_tokens.unwrap_or(0);
        log.lock()
            .unwrap()
            .push((budget, req.max_tokens.unwrap_or(0)));
        if budget < 16_000 {
            return penelope_llm::mock::Scripted::ReasonedOnly {
                completion: budget as u64,
                reasoning: budget as u64,
            };
        }
        penelope_llm::mock::Scripted::Written {
            text: verdicts(6),
            completion: 1_500,
            cut: false,
        }
    })));

    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    let w = o.report.warnings.join(" | ");
    assert!(
        w.contains("raisonnement plein"),
        "l'avertissement nomme la vraie cause : {w}"
    );
    assert!(
        w.contains("budget de raisonnement relevé"),
        "le budget monte, le lot ne rétrécit pas : {w}"
    );
    assert!(
        !w.contains("consolidation coupée"),
        "un raisonnement plein n'est pas une sortie trop longue : {w}"
    );
    // Le lot n'a jamais été réduit, et `max_tokens` porte bien les deux budgets.
    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "un rejeu, pas une descente d'échelle");
    assert_eq!(calls[0].0, 8_000, "budget de départ");
    assert_eq!(calls[1].0, 16_000, "budget doublé");
    for (r, max) in &calls {
        assert!(max > r, "max_tokens = raisonnement + sortie : {r} / {max}");
    }
    let batches = dream_batches(&d.services.events.range(0, 10_000).await.unwrap());
    let judged: u64 = batches
        .iter()
        .filter(|(_, lost)| !lost)
        .map(|(n, _)| n)
        .sum();
    assert_eq!(judged, 6, "{batches:?}");
}

/// #152 : une passe coupée en vol garde ce qu'elle a écrit. Le 21/09, sept lots
/// réussis (126 candidats) ont été jetés parce que le huitième n'a jamais répondu :
/// rien n'était écrit avant la fin. Chaque lot est maintenant une unité complète.
#[tokio::test]
async fn batches_are_written_one_by_one_and_survive_a_failure() {
    let (_dir, d, p) = daemon().await;
    // Douze lots d'un candidat : le huitième échoue sans relâche.
    let n = 12usize;
    for k in 0..n {
        note(
            &d,
            CandidateType::Fait,
            &format!("Le serveur de production du projet{k:03} écoute sur le port 8{k:03}"),
            Origin::Owner,
            "s1",
            8,
        )
        .await;
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    p.set_responder(Some(Arc::new(move |req: &ChatRequest| {
        let k = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // Les sept premiers lots passent, le huitième ne répond jamais.
        if k >= 7 {
            return penelope_llm::mock::Scripted::Error(
                LlmErrorKind::BadRequest,
                "le modèle refuse".into(),
            );
        }
        let user = req.messages.last().map(|m| m.text()).unwrap_or_default();
        penelope_llm::mock::Scripted::Written {
            text: promotions(&user),
            completion: 400,
            cut: false,
        }
    })));
    // Un lot par candidat : la taille de lot descend à 1.
    d.publish_config("test", |c| {
        c.memory.dream_batch = 1;
        Ok(vec!["memory.dream_batch".into()])
    })
    .unwrap();

    let err = run(&d, &d.hooks.messenger, false).await.unwrap_err();
    assert!(format!("{err}").contains("refuse"), "{err}");

    // Ce que les sept premiers lots ont écrit est gardé, et leurs candidats marqués.
    let entries = d.services.memory.by_level(Level::Projet).await.unwrap();
    assert_eq!(entries.len(), 7, "sept lots écrits avant l'échec");
    let left = d.services.candidates.pending(None).await.unwrap();
    assert_eq!(left.len(), n - 7, "seuls les candidats non jugés restent");

    // La passe échouée porte le compte de ses lots, et l'échec est annoncé.
    let (_, stats) = last_run(&d.services).await.unwrap();
    assert_eq!(stats.lots, 7, "les lots écrits sont comptés");
    let events = d.services.events.range(0, 10_000).await.unwrap();
    assert!(
        events.iter().any(|e| e.kind == "memory.dream_failed"),
        "une passe lancée à la main qui échoue le dit aussi"
    );

    // Relance : les sept premiers ne sont pas rejoués, et rien ne se dédouble.
    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let p2 = p.clone();
    p2.set_responder(Some(Arc::new(move |req: &ChatRequest| {
        let user = req.messages.last().map(|m| m.text()).unwrap_or_default();
        penelope_llm::mock::Scripted::Written {
            text: promotions(&user),
            completion: 400,
            cut: false,
        }
    })));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(
        o.report.candidates_seen as usize,
        n - 7,
        "reprise sur le reste"
    );
    let entries = d.services.memory.by_level(Level::Projet).await.unwrap();
    assert_eq!(entries.len(), n, "aucune entrée en double");
    assert!(
        d.services
            .candidates
            .pending(None)
            .await
            .unwrap()
            .is_empty(),
        "tous les candidats sont jugés"
    );
}

/// #152 : `memory.consolidation_reasoning = "off"` éteint vraiment le raisonnement —
/// `reasoning: {enabled: false}`, pas un effort — et rend tout le budget à la sortie.
#[tokio::test]
async fn switching_reasoning_off_sends_the_kill_switch() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 3, |_| false).await;
    d.publish_config("test", |c| {
        c.memory.consolidation_reasoning = "off".into();
        Ok(vec!["memory.consolidation_reasoning".into()])
    })
    .unwrap();
    // Ce qui est demandé au modèle à chaque appel : l'effort, et le budget de
    // raisonnement.
    type Asked = Vec<(Option<String>, Option<u32>)>;
    let seen: Arc<std::sync::Mutex<Asked>> = Arc::new(std::sync::Mutex::new(vec![]));
    let log = seen.clone();
    p.set_responder(Some(Arc::new(move |req: &ChatRequest| {
        log.lock()
            .unwrap()
            .push((req.reasoning_effort.clone(), req.reasoning_max_tokens));
        penelope_llm::mock::Scripted::Written {
            text: verdicts(3),
            completion: 900,
            cut: false,
        }
    })));
    run(&d, &d.hooks.messenger, false).await.unwrap();
    let calls = seen.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0.as_deref(),
        Some("none"),
        "éteint, pas réglé au plus bas"
    );
    assert_eq!(calls[0].1, None, "aucun budget réservé à la réflexion");
}

/// #152 : un lot coupé par le réseau ou par une machine qui s'endort est rejoué au
/// retour, pas compté dans les deux reprises d'une erreur passagère. Le 21/09, un trou
/// de journal de douze minutes (Mac sur batterie) a conclu « sans réponse complète en
/// 240 s » et la passe a tout abandonné.
#[tokio::test(start_paused = true)]
async fn a_network_stall_replays_the_batch_instead_of_giving_up() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 3, |_| false).await;
    let tries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = tries.clone();
    p.set_responder(Some(Arc::new(move |_: &ChatRequest| {
        // Trois coupures réseau d'affilée : plus que les deux reprises d'une erreur
        // passagère ordinaire.
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3 {
            // Une vraie coupure locale. Surtout pas notre propre délai : le
            // confondre avec une coupure faisait attendre 300 s par tentative,
            // indéfiniment (issue #152).
            return penelope_llm::mock::Scripted::Error(
                LlmErrorKind::Transient,
                "error sending request for url (openrouter.ai)".into(),
            );
        }
        penelope_llm::mock::Scripted::Written {
            text: verdicts(3),
            completion: 900,
            cut: false,
        }
    })));

    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(tries.load(std::sync::atomic::Ordering::SeqCst), 4);
    let w = o.report.warnings.join(" | ");
    assert!(
        w.contains("réseau coupé ou machine endormie"),
        "la coupure est nommée pour elle-même : {w}"
    );
    assert!(
        !w.contains("reprise 1/"),
        "ce n'est pas une erreur passagère ordinaire : {w}"
    );
    // Les trois candidats ont fini par être jugés.
    let batches = dream_batches(&d.services.events.range(0, 10_000).await.unwrap());
    let judged: u64 = batches
        .iter()
        .filter(|(_, lost)| !lost)
        .map(|(n, _)| n)
        .sum();
    assert_eq!(judged, 3, "{batches:?}");
}

/// #152 : la sortie utile seule dimensionne les lots suivants. Un appel où le
/// raisonnement a écrit 10 000 jetons et le JSON 600 ne doit pas faire croire que
/// chaque candidat coûte des milliers de jetons.
#[tokio::test]
async fn the_output_budget_ignores_what_was_spent_thinking() {
    let mut b = OutputBudget::new(16_000);
    let before = b.max_tokens(10);
    // 600 jetons utiles pour 10 candidats, le reste en réflexion.
    b.observe(10, 600);
    let after = b.max_tokens(10);
    assert!(
        after < before + 500,
        "la sortie utile tire l'estimation vers le bas : {before} → {after}"
    );
    // La même complétion comptée en entier (10 600) la ferait exploser.
    let mut naive = OutputBudget::new(16_000);
    naive.observe(10, 10_600);
    assert!(
        naive.max_tokens(10) > after * 2,
        "c'est bien la mesure qui change, pas le hasard"
    );
}

/// #140 : un candidat seul coupé au plancher est repris une fois avec une sortie
/// doublée ; s'il est coupé encore, la réponse tronquée est gardée et l'appel compté
/// jeté.
#[tokio::test]
async fn a_lone_cut_is_retried_with_twice_the_output() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 1, |_| true).await;
    p.set_responder(Some(verbose_model(250, 2_400, usize::MAX)));
    let o = run(&d, &d.hooks.messenger, true).await.unwrap();
    assert_eq!(
        (o.report.calls, o.report.wasted_calls),
        (2, 1),
        "{:?}",
        o.report
    );
    // Sortie utile demandée, hors budget de raisonnement (issue #152).
    let limits: Vec<u32> = p
        .requests()
        .iter()
        .map(|r| {
            r.max_tokens
                .unwrap_or(0)
                .saturating_sub(r.reasoning_max_tokens.unwrap_or(0))
        })
        .collect();
    assert_eq!(limits, vec![2_000, 4_000]);
    assert!(
        o.report
            .warnings
            .iter()
            .any(|w| w.contains("coupée sur un seul candidat à 2000 tokens : reprise à 4000")),
        "{:?}",
        o.report.warnings
    );

    let (_dir, d, p) = daemon().await;
    projects(&d, 1, |_| true).await;
    p.set_responder(Some(verbose_model(250, 9_000, usize::MAX)));
    let o = run(&d, &d.hooks.messenger, true).await.unwrap();
    assert_eq!(
        (o.report.calls, o.report.wasted_calls),
        (2, 2),
        "{:?}",
        o.report
    );
    assert!(
        o.report
            .warnings
            .iter()
            .any(|w| w.contains("même à 4000 tokens : réponse tronquée gardée")),
        "{:?}",
        o.report.warnings
    );
}

/// #140 : un modèle qui ne tient qu'un candidat à la fois ne fait pas une passe d'un
/// appel par candidat : elle s'arrête, le dit, et les candidats non jugés restent en
/// attente sans consommer de report.
#[tokio::test]
async fn a_pass_of_lone_lots_stops_and_says_so() {
    let (_dir, d, p) = daemon().await;
    projects(&d, 40, |_| false).await;
    let s = &d.services;
    let deferrals = || async {
        s.store
            .read(|c| {
                let mut st =
                    c.prepare("SELECT id, deferrals, state FROM mem_candidates ORDER BY id")?;
                let rows = st.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap()
    };
    let before = deferrals().await;
    p.set_responder(Some(verbose_model(250, 2_400, 1)));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(o.report.calls <= 25, "{:?}", o.report);
    let stop = o
        .report
        .warnings
        .iter()
        .find(|w| w.starts_with("passe arrêtée"))
        .unwrap_or_else(|| panic!("{:?}", o.report.warnings));
    assert!(stop.contains("sans report consommé"), "{stop}");
    let batches = dream_batches(&s.events.range(0, 10_000).await.unwrap());
    let judged = batches.iter().filter(|(_, cut)| !cut).count();
    assert_eq!(judged, LONE_WATCH, "{batches:?}");
    let after = deferrals().await;
    let untouched = before.iter().filter(|b| after.contains(b)).count();
    assert_eq!(untouched, 40 - LONE_WATCH, "{after:?}");
}

/// #135 : un modèle qui coupe au-delà de 10 candidats juge 188 groupes en au plus
/// 25 appels ; après une descente à 5, le lot suivant part entre 5 et 10.
#[test]
fn batch_sizes_remember_what_held() {
    let mut sizer = BatchSizer::new(40);
    let (mut done, mut calls) = (0usize, 0usize);
    while done < 188 {
        let size = sizer.size(188 - done);
        calls += 1;
        if size > 10 {
            sizer.cut(size);
        } else {
            sizer.ok(size);
            done += size;
        }
    }
    assert!(calls <= 25, "{calls} appels");

    let mut sizer = BatchSizer::new(40);
    sizer.cut(40);
    sizer.cut(20);
    sizer.cut(10);
    assert_eq!(sizer.size(100), 5);
    sizer.ok(5);
    let next = sizer.size(100);
    assert!((5..=10).contains(&next), "{next}");
    let budget = OutputBudget::new(16_000);
    assert!(
        budget.max_tokens(40) > 40 * 120,
        "plus que 120 tokens par candidat"
    );
    assert!(budget.fit(40) * 455 <= 16_000 + 455);
}

/// #140 : après des coupures à 40, 20, 10, 5 et 2 sur la même tête de lot, qui ne
/// tient qu'à un candidat, le lot suivant ne fait pas 1 : seule la première coupure
/// accuse la taille. Deux fois de suite, c'est la taille. Une coupure à 2 ne pose
/// jamais un plafond à 1.
#[test]
fn a_heavy_head_does_not_shrink_every_later_batch() {
    let mut sizer = BatchSizer::new(40);
    sizer.ok(40);
    for size in [40, 20, 10, 5, 2] {
        assert_eq!(sizer.size(200), size);
        sizer.cut(size);
    }
    assert_eq!(sizer.size(200), 1);
    sizer.ok(1);
    assert_eq!(sizer.size(200), 20);
    sizer.ok(20);
    assert_eq!(sizer.size(200), 30);

    // Deux rejeux de suite jusqu'à 1 : la taille est en cause, la plus petite coupure
    // au-dessus de 2 (3) devient le plafond, et le lot suivant fait 2, pas 1.
    for size in [30, 15, 7, 3] {
        sizer.cut(size);
    }
    sizer.ok(1);
    for size in [20, 10, 5, 3] {
        sizer.cut(size);
    }
    sizer.ok(1);
    assert_eq!(sizer.size(200), 2);
    sizer.cut(2);
    sizer.ok(1);
    assert_eq!(
        sizer.size(200),
        2,
        "une coupure à 2 ne pose pas de plafond à 1"
    );
}

/// #140 : les lots faciles de l'essai à blanc tiraient l'estimation à ~191 tokens par
/// candidat ; la coupure du lot de 40 à 9 958 tokens la remonte au-dessus de la
/// preuve, et les lots faciles qui suivent ne la font plus redescendre dessous. Le
/// plancher double quand c'est lui qui a coupé.
#[test]
fn a_cut_teaches_the_output_estimate() {
    let mut budget = OutputBudget::new(32_000);
    budget.observe(40, 17_927);
    budget.observe(40, 3_069);
    budget.observe(40, 5_339);
    let before = budget.max_tokens(40);
    assert!((9_800..10_000).contains(&before), "{before}");
    budget.cut(40, before, 9_958);
    assert!(budget.max_tokens(20) as f64 >= 20.0 * 9_958.0 / 40.0 * 1.3 - 1.0);
    budget.observe(40, 3_069);
    assert!(budget.max_tokens(40) as f64 >= 9_958.0 * 1.3 - 1.0);

    let mut budget = OutputBudget::new(16_000);
    assert_eq!(budget.max_tokens(1), 2_000);
    budget.cut(2, 2_000, 2_000);
    assert_eq!(budget.max_tokens(1), 4_000, "plancher doublé");
    assert!(
        budget.fit(40) >= 30,
        "l'estimation par candidat ne bouge pas"
    );
    budget.cut(1, 4_000, 4_000);
    assert_eq!(budget.max_tokens(1), 8_000);
    budget.cut(1, 8_000, 8_000);
    budget.cut(1, 16_000, 16_000);
    assert_eq!(
        budget.max_tokens(1),
        16_000,
        "jamais au-delà de la limite du modèle"
    );
}

/// #135 : la passe entière, avec un modèle qui coupe au-delà de 10 candidats : peu
/// d'appels jetés, aucun lot jugé au-delà de 10, un événement par lot, et le rapport
/// dit appels, appels jetés et durée.
#[tokio::test]
async fn a_pass_does_not_restart_from_the_full_batch_after_a_cut() {
    let (_dir, d, p) = daemon().await;
    const SUJETS: [&str; 30] = [
        "facturation",
        "déploiement",
        "sauvegarde",
        "revue de code",
        "veille",
        "réunions",
        "voyages",
        "cuisine",
        "sport",
        "musique",
        "lecture",
        "jardin",
        "photo",
        "vélo",
        "piano",
        "café",
        "thé",
        "courses",
        "impôts",
        "banque",
        "assurance",
        "voiture",
        "maison",
        "chauffage",
        "internet",
        "téléphone",
        "vacances",
        "cinéma",
        "théâtre",
        "randonnée",
    ];
    for sujet in SUJETS {
        note(
            &d,
            CandidateType::Preference,
            &format!("Pour la {sujet}, le propriétaire décide seul et sans réunion"),
            Origin::Owner,
            "s1",
            6,
        )
        .await;
    }
    p.set_responder(Some(Arc::new(|req: &ChatRequest| {
        let user = req.messages.last().map(|m| m.text()).unwrap_or_default();
        let n = user
            .lines()
            .filter(|l| {
                l.split_once(". [")
                    .is_some_and(|(k, _)| k.chars().all(|c| c.is_ascii_digit()))
            })
            .count();
        if n > 10 {
            return penelope_llm::mock::Scripted::Text(r#"{"tri": [{"candidat": 1, "dur"#.into());
        }
        let tri: Vec<String> = (1..=n)
            .map(|k| {
                format!(
                    r#"{{"candidat": {k}, "durable": false, "utile": false, "precis": true,
                          "introuvable": true, "endosse": true, "justification": "passager"}}"#
                )
            })
            .collect();
        penelope_llm::mock::Scripted::Text(format!(
            r#"{{"tri": [{}], "operations": []}}"#,
            tri.join(",")
        ))
    })));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert!(o.report.calls <= 10, "{:?}", o.report);
    assert!(o.report.wasted_calls <= 3, "{:?}", o.report);
    assert_eq!(o.report.calls as usize, p.call_count());
    let events = d.services.events.range(0, 1_000).await.unwrap();
    let batches: Vec<&penelope_kernel::event::Event> = events
        .iter()
        .filter(|e| e.kind == "memory.dream_batch")
        .collect();
    assert_eq!(batches.len(), o.report.calls as usize);
    for b in &batches {
        if b.payload["truncated"] == false {
            assert!(b.payload["size"].as_u64().unwrap() <= 10, "{}", b.payload);
        }
    }
    assert!(
        o.report.render_brief().contains("appel(s) au modèle"),
        "{}",
        o.report.render_brief()
    );
}

/// #127 : un flux muet sur un lot est repris sur ce lot, après l'attente réglée, et le
/// rapport le dit ; une erreur qui n'est pas passagère (requête refusée) ne l'est pas.
#[tokio::test]
async fn a_passing_error_is_retried_on_its_batch() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.memory.dream_retry_wait = "1ms".into();
        Ok(vec!["memory.dream_retry_wait".into()])
    })
    .unwrap();
    note(
        &d,
        CandidateType::Preference,
        "Toujours répondre en français",
        Origin::Owner,
        "s1",
        6,
    )
    .await;
    p.push(passing_error());
    p.reply(&keep("Toujours répondre en français"));
    let o = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(o.report.promoted, 1, "{:?}", o.report);
    assert_eq!(p.call_count(), 2);
    assert!(
        o.report
            .warnings
            .iter()
            // L'avertissement nomme l'erreur du fournisseur telle quelle : c'est
            // elle qu'on lit au matin, pas une catégorie (issue #152).
            .any(|w| w.contains("Upstream idle timeout") && w.contains("reprise 1/2")),
        "{:?}",
        o.report.warnings
    );

    note(
        &d,
        CandidateType::Preference,
        "Toujours tutoyer le propriétaire",
        Origin::Owner,
        "s1",
        6,
    )
    .await;
    p.push(penelope_llm::mock::Scripted::Error(
        LlmErrorKind::BadRequest,
        "requête refusée".into(),
    ));
    assert!(run(&d, &d.hooks.messenger, false).await.is_err());
    assert_eq!(p.call_count(), 3, "pas de reprise d'un refus");
}

/// #127 : une nuit dont un lot échoue trois fois n'écrit rien, laisse les candidats tels
/// quels, le dit dans `DREAMS.md`, un événement et un message ; une panne qui dure ne
/// #152 : rejouer un lot déjà écrit ne doit rien dédoubler. La garde « un texte déjà
/// en mémoire ne s'ajoute pas » part de l'instantané du vault, relu à chaque lot :
/// elle tient donc aussi **entre deux passes**, y compris après une passe tuée en vol.
#[tokio::test]
async fn replaying_a_written_batch_adds_nothing_twice() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    let texte = "Toujours répondre en français";
    note(&d, CandidateType::Preference, texte, Origin::Owner, "s1", 6).await;

    // Première passe : le candidat est promu, le texte entre dans le vault.
    p.reply(&keep(texte));
    let first = run(&d, &d.hooks.messenger, false).await.unwrap();
    assert_eq!(first.report.promoted, 1, "{:?}", first.report);
    let vault = crate::helpers::vault_dir(s);
    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert_eq!(profil.matches(texte).count(), 1, "{profil}");

    // Le candidat promu ne repasse pas : il n'est plus en attente.
    assert!(
        s.candidates.pending(None).await.unwrap().is_empty(),
        "un candidat promu ne revient pas dans la file"
    );

    // Seconde passe sur un candidat neuf dont le modèle propose **le même texte** :
    // c'est le cas d'un lot rejoué après une interruption au milieu de l'écriture.
    note(
        &d,
        CandidateType::Preference,
        "Parler français au bureau",
        Origin::Owner,
        "s2",
        6,
    )
    .await;
    p.reply(&keep(texte));
    let second = run(&d, &d.hooks.messenger, false).await.unwrap();

    let profil = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert_eq!(
        profil.matches(texte).count(),
        1,
        "aucune entrée en double après rejeu : {profil}"
    );
    assert!(
        second
            .report
            .sorted
            .iter()
            .any(|l| l.contains("déjà en mémoire")),
        "le rejeu est dit, pas silencieux : {:?}",
        second.report.sorted
    );
    assert_eq!(second.report.promoted, 0, "{:?}", second.report);
}

/// #152 : 240 s fixes tuaient tout appel qui réfléchissait vraiment. Le 21/09, avec
/// 8 000 à 16 000 tokens de raisonnement autorisés, chaque lot demandait cinq à onze
/// minutes : tué à quatre, classé « réseau coupé », rejoué après 300 s, sans fin.
#[test]
fn the_call_deadline_follows_the_budget_it_was_given() {
    let hour = Duration::from_secs(3600);
    // Un petit budget garde le plancher : rien ne justifie d'attendre plus.
    assert_eq!(call_timeout(1_000, hour), CALL_TIMEOUT_MIN);
    // Le budget qui a fait échouer la nuit : 16 000 de raisonnement + la sortie.
    assert!(
        call_timeout(24_000, hour) > Duration::from_secs(240),
        "un lot qui réfléchit a plus de quatre minutes"
    );
    // Mais jamais sans borne : un seul appel n'immobilise pas la nuit.
    assert_eq!(call_timeout(1_000_000, hour), CALL_TIMEOUT_MAX);
    // Ni au-delà de ce qu'il reste à la passe.
    assert_eq!(
        call_timeout(1_000_000, Duration::from_secs(300)),
        Duration::from_secs(300),
        "le reste de la nuit borne le délai"
    );
    // Mais le plancher tient : un reste dérisoire ne donne pas un appel mort-né.
    assert_eq!(
        call_timeout(1_000_000, Duration::from_secs(10)),
        CALL_TIMEOUT_MIN
    );
}

/// #152 : « notre appel a dépassé son délai » n'est pas « la machine dormait ». Les
/// confondre coûtait 300 s d'attente par tentative, indéfiniment.
#[test]
fn our_own_deadline_is_not_a_network_cut() {
    let mine: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        format!("{OWN_TIMEOUT} : rien de complet en 600 s"),
    )
    .into();
    assert!(own_timeout(&mine));
    assert!(
        !network_stall(&mine),
        "sans sonde réseau, notre délai ne prouve aucune coupure"
    );

    // Une vraie coupure locale, elle, reste reconnue.
    let cut: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        "error sending request for url".to_string(),
    )
    .into();
    assert!(network_stall(&cut));
    assert!(!own_timeout(&cut));

    // Et l'erreur du fournisseur de #127 n'est ni l'un ni l'autre.
    let upstream: anyhow::Error = LlmError::new(
        LlmErrorKind::Transient,
        "Upstream idle timeout exceeded".to_string(),
    )
    .into();
    assert!(!network_stall(&upstream));
    assert!(!own_timeout(&upstream));
}
