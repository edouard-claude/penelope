use super::*;
use penelope_llm::types::ToolCall;
use serde_json::json;

/// #107 : sur une fenêtre courte, le seuil laisse la place d'un résultat d'outil
/// admis entier et de la réponse, et la queue verbatim laisse quelque chose à résumer ;
/// sur 128 k, rien ne change ; un seuil propre au modèle l'emporte.
#[test]
fn short_windows_get_a_lower_threshold_and_a_smaller_tail() {
    let cfg = penelope_kernel::config::Config::default();
    let at = |w: u64| CompactionParams::from_config(&cfg, w, "openrouter:exemple/modele");
    let p32 = at(32_768);
    assert!((p32.threshold - 0.65).abs() < 0.01, "{}", p32.threshold);
    let limit = p32.window - reserved_output(p32.window);
    assert!(p32.threshold_tokens() + p32.tool_group_budget() <= limit);
    assert!(p32.tail_budget() < p32.background_threshold_tokens(p32.background_margin) / 3);
    let p8 = at(8_192);
    assert!(p8.tail_budget() < 8_192 / 4, "{}", p8.tail_budget());
    assert!(p8.threshold < 0.65);
    let p128 = at(131_072);
    assert_eq!(p128.threshold, 0.70);
    assert_eq!(p128.tail_budget(), 10_000);

    let mut explicit = cfg.clone();
    explicit
        .context
        .model_thresholds
        .insert("exemple/modele".into(), 0.8);
    let p = CompactionParams::from_config(&explicit, 32_768, "openrouter:exemple/modele");
    assert_eq!(p.threshold, 0.8, "le seuil du modèle l'emporte");
}

fn params(window: u64) -> CompactionParams {
    CompactionParams {
        window,
        threshold: 0.70,
        tail_ratio: 0.025,
        tail_min_tokens: 10_000,
        tail_max_tokens: 25_000,
        min_tail_user_messages: 2,
        max_tool_result_share: 0.25,
        large_payload_tokens: 25_000,
        background_margin: 0.10,
        max_prompt_tokens: 0,
    }
}

fn est(m: &ChatMessage) -> u64 {
    (m.text().chars().count() as u64 / 4).max(1)
}
fn est_all(ms: &[ChatMessage]) -> u64 {
    ms.iter().map(est).sum()
}

#[test]
fn tail_budget_is_clamped() {
    assert_eq!(params(100_000).tail_budget(), 10_000, "plancher à 10K");
    assert_eq!(params(1_000_000).tail_budget(), 25_000, "plafond à 25K");
    assert_eq!(params(600_000).tail_budget(), 15_000, "2,5 % au milieu");
}

#[test]
fn tool_group_budget_is_a_capped_share_of_the_window() {
    assert_eq!(
        params(40_000).tool_group_budget(),
        10_000,
        "petite fenêtre : la part"
    );
    assert_eq!(
        params(200_000).tool_group_budget(),
        25_000,
        "plafond absolu"
    );
    assert_eq!(params(1_300_000).tool_group_budget(), 25_000);
}

/// Issue #8 : sur un modèle à grande fenêtre, un résultat de 150 k tokens part en
/// artefact au lieu de rester entier dans le contexte.
#[test]
fn a_huge_result_is_externalised_even_on_a_huge_window() {
    let body = "documentation ".repeat(150_000 * 4 / 14);
    let decisions = level1_admission(&[(0, body.clone(), 150_000)], &params(1_300_000));
    match &decisions[0].1 {
        Admission::Externalise {
            head,
            tail,
            original_tokens,
        } => {
            assert_eq!(*original_tokens, 150_000);
            let kept = (head.chars().count() + tail.chars().count()) as f64 / 3.6;
            assert!(kept <= 25_000.0 * 1.05, "{kept} tokens gardés");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn level0_only_touches_eager_tool_results() {
    let entries = vec![
        Entry::new(1, ChatMessage::user("a"), 5),
        Entry::new(
            2,
            ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }]),
            5,
        ),
        Entry::new(
            3,
            ChatMessage::tool_result("c1", "fs_read", "gros contenu"),
            900,
        )
        .eager(true),
        Entry::new(4, ChatMessage::assistant("fini"), 5),
    ];
    let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
    let n = level0_micro(&entries, &mut proj, 4);
    assert_eq!(n, 1);
    assert!(proj[2].text().contains("history_expand"));
    assert_eq!(proj[0].text(), "a", "les autres messages sont intacts");
    // Le canonique n'a pas bougé.
    assert_eq!(entries[2].message.text(), "gros contenu");
}

#[test]
fn level0_protects_recent_entries() {
    let entries = vec![Entry::new(1, ChatMessage::tool_result("c", "t", "x"), 100).eager(true)];
    let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
    assert_eq!(level0_micro(&entries, &mut proj, 0), 0);
}

#[test]
fn level1_keeps_small_groups_untouched() {
    let p = params(100_000);
    let r = vec![(0, "petit".to_string(), 100), (1, "aussi".to_string(), 200)];
    let out = level1_admission(&r, &p);
    assert!(out.iter().all(|(_, a)| *a == Admission::Keep));
}

#[test]
fn level1_externalises_only_the_oversized_result() {
    let p = params(100_000); // budget de groupe : 25 000
    let big: String = "x".repeat(400_000);
    let r = vec![(0, "petit".to_string(), 50), (1, big.clone(), 100_000)];
    let out = level1_admission(&r, &p);
    assert_eq!(out[0].1, Admission::Keep, "le petit passe entier");
    match &out[1].1 {
        Admission::Externalise {
            head,
            tail,
            original_tokens,
        } => {
            assert_eq!(*original_tokens, 100_000);
            assert!(!head.is_empty() && !tail.is_empty());
            assert!(head.chars().count() < big.chars().count());
        }
        other => panic!("attendu une externalisation, obtenu {other:?}"),
    }
}

#[test]
fn externalised_body_carries_the_pointer() {
    let b = externalised_body("art_1", "début", "fin", 50_000, "log");
    assert!(b.contains("artifact_read(\"art_1\")"));
    assert!(b.contains("50000 tokens"));
    assert!(b.contains("début") && b.contains("fin"));
}

#[test]
fn level2_degrades_until_it_fits() {
    let long: String = "a".repeat(4000);
    let entries: Vec<Entry> = (0..6)
        .map(|i| {
            Entry::new(
                i,
                ChatMessage::tool_result(format!("c{i}"), "t", long.clone()),
                1000,
            )
        })
        .collect();
    let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
    let before = est_all(&proj);
    let (after, touched) = level2_degrade(&entries, &mut proj, 5, before / 2, before, &est);
    assert!(after < before, "{after} doit être < {before}");
    assert!(touched > 0);
    assert_eq!(
        proj[5].text().chars().count(),
        4000,
        "le groupe protégé reste intact"
    );
}

#[test]
fn split_keeps_at_least_two_user_messages_in_the_tail() {
    let p = params(100_000); // queue : 10 000 tokens
    let mut entries = Vec::new();
    for i in 0..20 {
        entries.push(Entry::new(i * 2, ChatMessage::user(format!("q{i}")), 3000));
        entries.push(Entry::new(i * 2 + 1, ChatMessage::assistant("r"), 3000));
    }
    let (boundary, tail) = split_for_summary(&entries, &p);
    let users_in_tail = entries[boundary..]
        .iter()
        .filter(|e| e.role() == Role::User)
        .count();
    assert!(
        users_in_tail >= 2,
        "queue : {users_in_tail} messages utilisateur"
    );
    assert!(boundary > 0, "il doit rester quelque chose à résumer");
    assert!(tail > 0);
}

#[test]
fn split_never_cuts_inside_a_tool_group() {
    let p = params(100_000);
    let entries = vec![
        Entry::new(1, ChatMessage::user("a"), 6000),
        Entry::new(2, ChatMessage::user("b"), 6000),
        Entry::new(
            3,
            ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "t".into(),
                arguments: json!({}),
            }]),
            100,
        ),
        Entry::new(4, ChatMessage::tool_result("c1", "t", "r"), 4000),
        Entry::new(5, ChatMessage::user("c"), 100),
    ];
    let (boundary, _) = split_for_summary(&entries, &p);
    assert_ne!(
        boundary, 4,
        "la frontière ne tombe jamais entre l'appel et son résultat"
    );
}

#[test]
fn verbatim_users_are_most_recent_first_under_budget() {
    let entries: Vec<Entry> = (0..10)
        .map(|i| Entry::new(i, ChatMessage::user(format!("message {i}")), 100))
        .collect();
    let kept = select_verbatim_users(&entries, 350);
    assert_eq!(kept.len(), 3);
    assert_eq!(
        kept.last().unwrap(),
        "message 9",
        "les plus récents d'abord"
    );
}

#[test]
fn summary_schema_and_render() {
    let s = summary_schema();
    let good = json!({
        "objectif": "corriger le bug de TVA",
        "fait": "localisé dans facture.rs",
        "en_cours": "écriture du test",
        "prochaines_etapes": "ouvrir la PR"
    });
    assert!(penelope_kernel::schema::validate(&s, &good).is_empty());
    let bad = json!({"objectif": "x"});
    assert!(!penelope_kernel::schema::validate(&s, &bad).is_empty());

    let txt = render_summary(
        &good,
        "Ancres :\n- chemin : facture.rs\n",
        &["où en est-on ?".into()],
        None,
    );
    assert!(txt.contains("### Objectif"));
    assert!(txt.contains("### Prochaines étapes"));
    assert!(txt.contains("verbatim"));
    assert!(txt.contains("facture.rs"));
}

/// CA 5 : niveau 4, la requête est prouvée conforme **avant** envoi.
#[test]
fn ca_5_1_level4_proves_it_fits() {
    let long = "x".repeat(40_000);
    let mut msgs = vec![ChatMessage::system("règles")];
    for i in 0..8 {
        msgs.push(ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
            id: format!("c{i}"),
            name: "t".into(),
            arguments: json!({}),
        }]));
        msgs.push(ChatMessage::tool_result(format!("c{i}"), "t", long.clone()));
    }
    msgs.push(ChatMessage::user("et maintenant ?"));

    let limit = 5_000;
    let (out, fits) = level4_emergency(msgs, limit, &est_all);
    assert!(fits, "le niveau 4 doit converger");
    let tokens = proof_of_fit(&out, limit, &est_all).expect("preuve locale");
    assert!(tokens <= limit);
    assert!(
        out.iter().any(|m| m.text() == "et maintenant ?"),
        "le dernier message utilisateur est conservé"
    );
    assert!(out.iter().any(|m| m.role == Role::System));
}

#[test]
fn proof_rejects_broken_pairs() {
    let msgs = vec![ChatMessage::tool_result("orphelin", "t", "x")];
    assert!(proof_of_fit(&msgs, 1_000_000, &est_all).is_err());
}

#[test]
fn cooldown_escalates_then_clears() {
    let steps = [60_000u64, 300_000, 900_000];
    let mut c = Cooldown::default();
    assert!(!c.is_active(0));
    c.record_failure(0, &steps);
    assert!(c.is_active(59_000));
    assert!(!c.is_active(61_000));
    c.record_failure(61_000, &steps);
    assert_eq!(c.until_ms, 61_000 + 300_000);
    c.record_failure(400_000, &steps);
    assert_eq!(c.until_ms, 400_000 + 900_000);
    c.record_failure(2_000_000, &steps);
    assert_eq!(
        c.until_ms,
        2_000_000 + 900_000,
        "plafonné au dernier palier"
    );
    c.clear();
    assert!(!c.is_active(0));
    assert_eq!(c.failures, 0);
}

#[test]
fn plan_levels_follows_the_prd_triggers() {
    let p = params(100_000);
    assert_eq!(plan_levels(10_000, &p, true), Vec::<u8>::new());
    assert_eq!(plan_levels(10_000, &p, false), vec![0, 2]);
    assert_eq!(plan_levels(80_000, &p, true), vec![3]);
    assert_eq!(plan_levels(80_000, &p, false), vec![0, 2, 3]);
}

#[test]
fn natural_split_leaves_a_short_history_alone() {
    let p = params(100_000); // queue : 10 000 tokens
    let entries: Vec<Entry> = (1..=6)
        .map(|i| Entry::new(i, ChatMessage::user(format!("m{i}")), 500))
        .collect();
    assert_eq!(natural_split(&entries, &p).0, 0, "tout tient dans la queue");
    assert!(
        split_for_summary(&entries, &p).0 > 0,
        "la découpe forcée trouve toujours quelque chose à résumer"
    );
}

#[test]
fn summary_validation_normalises_the_model_output() {
    let raw = "Voici le résumé :\n```json\n{\"objectif\": \" livrer la 0.3 \", \
                   \"fait\": [\"tests verts\", \"CI verte\"], \"en_cours\": null, \
                   \"bruit\": 1}\n```";
    let v = validate_summary(raw).expect("résumé valide");
    assert_eq!(v["objectif"], "livrer la 0.3");
    assert_eq!(v["fait"], "- tests verts\n- CI verte");
    assert_eq!(v["en_cours"], "");
    assert_eq!(v["bloque"], "", "toutes les sections sont présentes");
    assert!(v.get("bruit").is_none(), "les clés inconnues sont écartées");
    assert!(penelope_kernel::schema::validate(&summary_schema(), &v).is_empty());

    let long = format!("{{\"objectif\": \"{}\"}}", "é".repeat(9_000));
    let v = validate_summary(&long).unwrap();
    assert_eq!(
        v["objectif"].as_str().unwrap().chars().count(),
        SECTION_MAX_CHARS
    );
}

#[test]
fn summary_validation_rejects_empty_or_foreign_output() {
    assert!(validate_summary("je ne sais pas").is_err());
    assert!(validate_summary("[1, 2]").is_err());
    assert!(validate_summary("{\"autre\": \"x\"}").is_err());
    assert!(
        validate_summary("{\"objectif\": \"\", \"fait\": \" \"}").is_err(),
        "un résumé sans contenu ne remplace pas l'historique"
    );
}

#[test]
fn response_format_requires_every_section() {
    let f = summary_response_format();
    let schema = &f["json_schema"]["schema"];
    assert_eq!(f["json_schema"]["strict"], true);
    assert_eq!(
        schema["required"].as_array().unwrap().len(),
        SUMMARY_SECTIONS.len()
    );
    assert!(schema["properties"]["contraintes_et_preferences"].is_object());
}

#[test]
fn sections_only_strips_mechanical_blocks() {
    let rendered = render_summary(
        &json!({"objectif": "migrer la base", "fait": "export"}),
        "Ancres :\n- chemin : db/schema.sql\n",
        &["on migre ce soir".into()],
        None,
    );
    let body = summary_sections_only(&rendered);
    assert!(body.starts_with("### Objectif"));
    assert!(body.contains("export"));
    assert!(!body.contains("verbatim"));
    assert!(!body.contains("db/schema.sql"));
    assert!(!body.contains(SUMMARY_HEADER));
}

#[test]
fn background_threshold_is_ten_points_lower() {
    let p = params(100_000);
    assert_eq!(p.threshold_tokens(), 70_000);
    assert_eq!(p.background_threshold_tokens(0.10), 60_000);
}

/// Issue #18 : sur une fenêtre de 1,3 M, le plafond de 120 k borne les seuils et le
/// budget des résultats d'outils ; une petite fenêtre garde les siens.
#[test]
fn max_prompt_tokens_caps_the_thresholds() {
    let p = CompactionParams {
        max_prompt_tokens: 120_000,
        ..params(1_300_000)
    };
    assert_eq!(p.threshold_tokens(), 120_000);
    assert_eq!(p.background_threshold_tokens(0.10), 102_856);
    assert_eq!(p.tool_group_budget(), 25_000);
    assert_eq!(
        p.tail_budget(),
        25_000,
        "la queue verbatim suit la vraie fenêtre"
    );
    let small = CompactionParams {
        max_prompt_tokens: 120_000,
        ..params(100_000)
    };
    assert_eq!(small.threshold_tokens(), 70_000);
    let tight = CompactionParams {
        max_prompt_tokens: 40_000,
        ..params(1_300_000)
    };
    assert_eq!(tight.tool_group_budget(), 14_285);
    assert_eq!(
        tight.tail_budget(),
        8_571,
        "queue sous le quart du seuil de fond"
    );
}

/// Niveau 4, dernier recours : quand même les groupes récents ne tiennent pas, il ne
/// reste que le système et le dernier message utilisateur ; la preuve dit si cela tient.
#[test]
fn level4_falls_back_to_system_and_last_user() {
    let msgs = vec![
        ChatMessage::system("sys"),
        ChatMessage::user("ancienne question"),
        ChatMessage::assistant("x".repeat(10_000)),
        ChatMessage::user("dernière question"),
        ChatMessage::assistant("y".repeat(10_000)),
    ];
    let estimate = |m: &[ChatMessage]| m.iter().map(|x| x.text().len() as u64).sum::<u64>();
    let (out, fits) = level4_emergency(msgs.clone(), 100, &estimate);
    assert!(fits);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].role, Role::System);
    assert_eq!(out[1].text(), "dernière question");

    // Même le minimum dépasse : la réponse le dit au lieu de prétendre tenir.
    let (out, fits) = level4_emergency(msgs, 5, &estimate);
    assert!(!fits);
    assert_eq!(out.len(), 2);

    assert_eq!(proof_of_fit(&out, 1_000, &estimate), Ok(21));
    let e = proof_of_fit(&out, 5, &estimate).unwrap_err();
    assert!(
        e.contains("pèse encore 21 tokens pour une limite de 5"),
        "{e}"
    );
}

/// Une réponse d'outil sans son appel n'est pas une requête valide : la preuve refuse.
#[test]
fn proof_of_fit_refuses_orphan_tool_results() {
    let msgs = vec![
        ChatMessage::user("q"),
        ChatMessage::tool_result("c1", "fs_read", "contenu"),
    ];
    let e = proof_of_fit(&msgs, 1_000, &|_| 1).unwrap_err();
    assert!(e.contains("paires appel/résultat invalides"), "{e}");
}

/// Un corps externalisé sans queue se termine sans séparateur d'élision.
#[test]
fn an_externalised_body_without_tail_has_no_elision() {
    let b = externalised_body("art_2", "tout", "", 900, "json");
    assert!(b.ends_with("tout\n--- fin ---"), "{b}");
    assert!(!b.contains("--- … ---"), "{b}");
}

/// Quand l'aperçu ne suffit pas, la seconde passe remplace les anciens résultats par un
/// stub qui dit comment les relire ; un résultat court n'est pas « aperçu », et un
/// stub n'est pas restubbé.
#[test]
fn level2_second_pass_stubs_old_results() {
    let short = "court".to_string();
    let long = "b".repeat(4000);
    let entries: Vec<Entry> = [short.clone(), long.clone(), long]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            Entry::new(
                i as i64 * 10,
                ChatMessage::tool_result(format!("c{i}"), "t", text),
                1000,
            )
        })
        .chain(std::iter::once(Entry::new(
            30,
            ChatMessage::user("la suite"),
            10,
        )))
        .collect();
    let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
    let before = est_all(&proj);
    let (after, touched) = level2_degrade(&entries, &mut proj, 3, 1, before, &est);
    assert!(after < before);
    assert!(touched >= 3, "{touched}");
    for (i, m) in proj.iter().take(3).enumerate() {
        assert_eq!(
            m.text(),
            format!(
                "[résultat élidé — récupérable via history_expand(seq:{})]",
                i * 10
            )
        );
    }
    assert_eq!(proj[3].text(), "la suite");
}

/// Un appel d'outil et son résultat, aux adresses `seq` et `seq + 1`.
fn tool_pair(seq: i64, tokens: u64) -> Vec<Entry> {
    let id = format!("c{seq}");
    vec![
        Entry::new(
            seq,
            ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: id.clone(),
                name: "shell_exec".into(),
                arguments: json!({}),
            }]),
            100,
        ),
        Entry::new(
            seq + 1,
            ChatMessage::tool_result(id, "shell_exec", "sortie"),
            tokens - 100,
        ),
    ]
}

/// Un message du propriétaire suivi de `pairs` appels d'outils de 1 500 tokens.
fn tool_turn(from_seq: i64, pairs: i64) -> Vec<Entry> {
    let mut entries = vec![Entry::new(
        from_seq,
        ChatMessage::user("audite le dépôt"),
        200,
    )];
    for i in 0..pairs {
        entries.extend(tool_pair(from_seq + 1 + i * 2, 1_500));
    }
    entries
}

/// La coupe d'avant #460 : la queue garde deux messages du propriétaire, quoi qu'ils
/// coûtent.
fn cut_before_460(entries: &[Entry], p: &CompactionParams) -> (usize, u64) {
    let (mut tail, mut users, mut boundary) = (0u64, 0usize, entries.len());
    for g in group(entries).iter().rev() {
        let tokens: u64 = entries[g.range.clone()].iter().map(|e| e.tokens).sum();
        if tail + tokens > p.tail_budget() && users >= p.min_tail_user_messages {
            break;
        }
        tail += tokens;
        users += entries[g.range.clone()]
            .iter()
            .filter(|e| e.role() == Role::User)
            .count();
        boundary = g.range.start;
    }
    (boundary, tail)
}

/// #460 : un message du propriétaire puis 40 appels d'outils. Il n'y a pas de second
/// message à garder : la queue revient à son budget, coupée après un résultat complet, et
/// la tête part au résumé avec la consigne.
#[test]
fn a_long_tool_turn_is_cut_at_a_tool_boundary() {
    let p = params(100_000); // queue : 10 000 tokens
    let entries = tool_turn(1, 40);
    assert_eq!(cut_before_460(&entries, &p).0, 0, "avant : rien à résumer");

    let (boundary, tail) = natural_split(&entries, &p);
    assert_eq!(tail, 9_000, "six appels entiers sous le budget");
    assert_eq!(boundary, entries.len() - 12);
    assert!(
        !entries[boundary].message.tool_calls.is_empty(),
        "la queue s'ouvre sur un appel, pas sur un résultat"
    );
    let tail_messages: Vec<ChatMessage> = entries[boundary..]
        .iter()
        .map(|e| e.message.clone())
        .collect();
    assert!(pairs_are_valid(&tail_messages));
    let head: Vec<ChatMessage> = entries[..boundary]
        .iter()
        .map(|e| e.message.clone())
        .collect();
    assert!(pairs_are_valid(&head));
    assert_eq!(split_for_summary(&entries, &p), (boundary, tail));
}

/// #460 : une conversation ordinaire garde la coupe qu'elle avait, tours d'outils
/// compris, tant que cette coupe laisse de quoi faire un résumé.
#[test]
fn an_ordinary_conversation_keeps_its_cut() {
    let p = params(100_000);
    let chat: Vec<Entry> = (0..40)
        .map(|i| match i % 2 {
            0 => Entry::new(i, ChatMessage::user(format!("q{i}")), 3_000),
            _ => Entry::new(i, ChatMessage::assistant("r"), 3_000),
        })
        .collect();
    let short: Vec<Entry> = (1..=6)
        .map(|i| Entry::new(i, ChatMessage::user(format!("m{i}")), 500))
        .collect();
    // Trois tours avec des outils : les deux derniers pèsent 30 000 tokens, trois fois le
    // budget, et restent entiers dans la queue.
    let mut tools = Vec::new();
    for turn in 0..3 {
        tools.extend(tool_turn(turn * 100 + 1, 10));
        tools.push(Entry::new(
            turn * 100 + 50,
            ChatMessage::assistant("fait"),
            100,
        ));
    }
    for (name, entries) in [("échanges", chat), ("courte", short), ("outils", tools)] {
        assert_eq!(
            natural_split(&entries, &p),
            cut_before_460(&entries, &p),
            "{name}"
        );
    }
}

/// #460 : deux longs tours d'outils après un bonjour. Les deux messages du propriétaire
/// tiendraient la queue à 60 000 tokens pour 300 à résumer : le budget reprend la main.
/// Dès que la tête vaut un résumé, la garantie des deux messages revient.
#[test]
fn two_heavy_turns_are_cut_when_nothing_else_is_left() {
    let p = params(100_000);
    let mut entries = vec![
        Entry::new(1, ChatMessage::user("bonjour"), 100),
        Entry::new(2, ChatMessage::assistant("bonjour"), 200),
    ];
    entries.extend(tool_turn(10, 20));
    entries.extend(tool_turn(60, 20));
    assert_eq!(cut_before_460(&entries, &p), (2, 60_400));
    let (boundary, tail) = natural_split(&entries, &p);
    assert_eq!((boundary, tail), (entries.len() - 12, 9_000));

    let mut longer = vec![
        Entry::new(1, ChatMessage::user("contexte"), 1_500),
        Entry::new(2, ChatMessage::assistant("noté"), 1_500),
    ];
    longer.extend(entries.iter().skip(2).cloned());
    assert_eq!(natural_split(&longer, &p), cut_before_460(&longer, &p));
    assert_eq!(natural_split(&longer, &p).0, 2);
}

/// #460 : un seul appel d'outils plus gros que le budget n'a pas de frontière, et un
/// résumé déjà publié en tête de la projection n'est pas « de quoi résumer ».
#[test]
fn one_oversized_group_has_no_boundary_and_a_summary_is_not_a_head() {
    let p = params(100_000);
    let alone = tool_pair(1, 30_000);
    assert_eq!(natural_split(&alone, &p), (0, 30_000));

    let mut projected = vec![Entry::new(
        0,
        ChatMessage::system("Résumé de la conversation antérieure"),
        5_000,
    )];
    projected.extend(tool_turn(1, 40).into_iter().skip(1));
    let (boundary, tail) = natural_split(&projected, &p);
    assert_eq!((boundary, tail), (projected.len() - 12, 9_000));
}

/// #460 : la consigne ouvre le résumé, citée ligne à ligne ; le résumeur ne la relit pas
/// comme une section, même si elle contient un titre de section.
#[test]
fn the_task_opens_the_summary_and_is_never_resummarised() {
    let task = "Audite le dépôt.\n\n### Objectif\nTout corriger, puis ouvrir la PR.";
    let rendered = render_summary(
        &json!({"objectif": "audit du dépôt", "fait": "lecture de src/"}),
        "Ancres :\n- chemin : src/lib.rs\n",
        &["avant ça, regarde le README".into()],
        Some(task),
    );
    assert!(
        rendered.starts_with(&format!(
            "{SUMMARY_HEADER}\n\n{TASK_HEADER}\n> Audite le dépôt.\n> \n> ### Objectif\n> \
             Tout corriger, puis ouvrir la PR.\n\n### Objectif\naudit du dépôt\n"
        )),
        "{rendered}"
    );
    let body = summary_sections_only(&rendered);
    assert!(body.starts_with("### Objectif\naudit du dépôt"), "{body}");
    assert!(!body.contains("Tout corriger"), "{body}");
    assert!(!body.contains("README") && !body.contains("src/lib.rs"));

    // Sans consigne, rien ne change ; une consigne vide n'ouvre pas de bloc.
    let plain = render_summary(&json!({"objectif": "x", "fait": "y"}), "", &[], Some("  "));
    assert!(!plain.contains(TASK_HEADER));
}

/// #460 : une consigne plus longue que le budget de la queue garde son début, sa fin et
/// l'adresse qui la rend entière.
#[test]
fn an_oversized_task_keeps_its_ends_and_its_address() {
    let text = format!("DEBUT {} FIN", "x".repeat(40_000));
    let short = Entry::new(7, ChatMessage::user("corrige le test"), 5);
    assert_eq!(task_quote(&short, 1_000), "corrige le test");
    let dated = "<contexte>\nDate et heure : jeudi 10:02.\n</contexte>\n\ncorrige le test";
    let dated = Entry::new(7, ChatMessage::user(dated), 20);
    assert_eq!(
        task_quote(&dated, 1_000),
        "corrige le test",
        "sans le bloc volatil"
    );
    let long = Entry::new(7, ChatMessage::user(text), 12_000);
    let quote = task_quote(&long, 1_000);
    assert!(quote.starts_with("DEBUT ") && quote.ends_with(" FIN"));
    assert!(quote.contains("history_expand(seq:7)"), "{quote}");
    assert!(quote.chars().count() < 3_200, "{}", quote.chars().count());
}
