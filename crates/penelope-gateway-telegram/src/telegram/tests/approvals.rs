use super::*;

/// #83 : au démarrage de la passerelle, la demande `effect_unknown` part sans que le
/// propriétaire la demande, une fois, avec la requête et trois boutons sans
/// « Toujours » ; « C'est fait » tranche le ledger et ne crée aucune règle.
#[tokio::test]
async fn an_uncertain_effect_is_pushed_then_decided_from_telegram() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let id = match s
        .effects
        .plan(penelope_kernel::effects::EffectSpec::new(
            penelope_kernel::effects::EffectKind::Git,
            "git_push",
            json!({"remote": "origin", "branch": "correctif-tva"}),
        ))
        .await
        .unwrap()
    {
        penelope_kernel::effects::Planned::Fresh(id) => id,
        o => panic!("{o:?}"),
    };
    s.effects.dispatching(&id).await.unwrap();
    g.daemon.recover().await.unwrap();

    assert_eq!(g.announce_uncertain_effects().await.unwrap(), 1);
    assert_eq!(g.announce_uncertain_effects().await.unwrap(), 0, "une fois");
    g.flush_outbox().await.unwrap();
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    let card = sent.last().expect("carte envoyée").clone();
    assert_eq!(card["chat_id"], OWNER);
    let text = card["text"].as_str().unwrap();
    assert!(
        text.contains("Effet incertain") && text.contains("correctif-tva"),
        "{text}"
    );
    let buttons = inline_buttons(&card);
    let labels: Vec<&str> = buttons.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["✅ C'est fait", "🔁 Relancer", "⏭ Ignorer"]);

    g.process_update(&updates::callback(5, OWNER, &buttons[0].1, 77))
        .await
        .unwrap();
    settle_click(&g).await;
    let effect = s.effects.get(&id).await.unwrap().unwrap();
    assert_eq!(
        effect.state,
        penelope_kernel::effects::EffectState::Completed
    );
    assert!(s.approvals.pending(10).await.unwrap().is_empty());
    assert!(s.policies.active_rules().await.unwrap().is_empty());
    let sent = t.calls_to(tg::SEND_MESSAGE).await;
    assert!(
        texts(&sent)
            .iter()
            .any(|x| x.contains("je ne le relance pas")),
        "{sent:?}"
    );
}

/// Parcours complet : carte, clic « Autoriser », reprise, réponse.
/// #116 : la carte dit d'abord ce que Pénélope cherche à faire (sa phrase, sinon le
/// message du propriétaire, jamais la politique), puis la commande telle qu'elle sera
/// exécutée, puis une ligne de qualificatifs ; « Toujours » dit sur quoi il porte.
#[tokio::test]
async fn an_approval_card_says_the_intention_first() {
    let (_d, g, t, p) = gateway().await;
    g.daemon
        .publish_config("test", |c| {
            c.models.routing.classifier = false;
            Ok(vec!["models.routing.classifier".into()])
        })
        .unwrap();
    let command =
        r#"gh pr list --repo Fidelatoo/cron-send-add-sender --state all --json "number,title""#;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({
                "command": command,
                "network": true,
                "output": "full",
                "pourquoi": "Je vérifie si la correction du nom d'expéditeur est déjà partie en revue."
            }),
        }],
    ));
    g.process_update(&updates::text_message(
        900,
        OWNER,
        OWNER,
        "où en est la PR ?",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap().to_string();
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} absent : {text}"))
    };
    assert!(
        at("Je vérifie si la correction") < at("gh pr list"),
        "{text}"
    );
    assert!(at("gh pr list") < at("classe external"), "{text}");
    assert!(
        text.contains(r#"--json "number,title""#),
        "commande sans échappement JSON : {text}"
    );
    assert!(!text.contains("\\\""), "{text}");
    assert!(
        text.contains("réseau") && text.contains("sortie complète"),
        "{text}"
    );
    assert!(text.contains("🌐 Accès au réseau demandé."), "{text}");
    assert!(
        !text.contains("pourquoi"),
        "l'intention n'est pas un argument affiché : {text}"
    );
    let labels: Vec<String> = inline_buttons(&card).into_iter().map(|(l, _)| l).collect();
    assert!(
        labels
            .iter()
            .any(|l| l == "♾️ Toujours pour « gh pr » (réseau)"),
        "{labels:?}"
    );

    // Sans intention : le message du propriétaire, pas la politique.
    t.clear().await;
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c2".into(),
            name: "fs_write".into(),
            arguments: json!({"path": "notes/compte-rendu.md", "content": "ok"}),
        }],
    ));
    g.process_update(&updates::text_message(
        901,
        OWNER,
        OWNER,
        "écris le compte rendu de la réunion",
    ))
    .await
    .unwrap();
    drain(&g).await;
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("Pour ta demande : « écris le compte rendu de la réunion »"),
        "{text}"
    );
    let intention = text.find("Pour ta demande").unwrap();
    assert!(intention < text.find("fs_write").unwrap(), "{text}");
    assert!(
        intention < text.find("politique").unwrap_or(usize::MAX),
        "{text}"
    );
}

/// #116 : un appel MCP rend la même carte, le serveur en qualificatif.
#[tokio::test]
async fn an_mcp_approval_card_names_its_server_as_a_qualifier() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let a = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "mcp__redmine__update_issue",
            penelope_kernel::risk::RiskClass::Write,
            json!({
                "tool": "mcp__redmine__update_issue",
                "arguments": {"issue_id": 7653, "status": "Résolu"},
                "reason": "politique par défaut pour la classe write",
                "why": "Je passe le ticket de pagination en résolu.",
            }),
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    g.send_approval_card(OWNER, None, &a).await.unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap().to_string();
    assert!(
        text.find("Je passe le ticket").unwrap() < text.find("update_issue").unwrap(),
        "{text}"
    );
    assert!(
        text.contains("issue_id = 7653") && text.contains("status = Résolu"),
        "{text}"
    );
    assert!(
        text.contains("serveur <code>redmine</code>") || text.contains("serveur `redmine`"),
        "{text}"
    );
    assert!(!text.contains("null"), "{text}");
}

/// #129 : une commande qui porte un gabarit Go (`{{.Name}}`) part sur la carte telle
/// quelle, sans passer pour une variable manquante.
#[tokio::test]
async fn a_command_with_go_templates_reaches_its_approval_card() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let command = r#"docker compose ps --format "table {{.Name}}\t{{.Status}}\t{{.Ports}}""#;
    let a = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec", "arguments": {"command": command},
                   "why": "Je regarde l'état des conteneurs {{corps}}."}),
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    g.send_approval_card(OWNER, None, &a).await.unwrap();
    g.flush_outbox().await.unwrap();
    let card = t
        .calls_to(tg::SEND_MESSAGE)
        .await
        .pop()
        .expect("carte envoyée");
    let text = card["text"].as_str().unwrap().to_string();
    assert!(
        text.contains("{{.Name}}") && text.contains("{{.Ports}}"),
        "{text}"
    );
    assert!(
        text.contains("{{corps}}"),
        "une valeur n'est pas relue : {text}"
    );
    assert!(!text.contains("Carte simplifiée"), "{text}");
}

/// #130 : chaque forme de commande (multi-lignes, heredoc, guillemet non fermé, un mot,
/// vide, préfixe `cd`) passe par le motif « Toujours », le relevé du `cd` et la carte
/// sans paniquer ; une commande multi-lignes n'a pas de motif, donc pas de règle (#67).
#[tokio::test]
async fn every_command_shape_reaches_its_card_without_panic() {
    use penelope_agent::ToolExecutor;
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let ws = penelope_executor::executor::default_workspaces(&s)[0].clone();
    let x = penelope_executor::executor::NativeToolExecutor::new(
        s.clone(),
        penelope_executor::executor::ToolEnv {
            session_id: "s1".into(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![ws.clone()],
            in_workflow: false,
            turn_model: None,
        },
    );
    let heredoc = format!(
        "cd {} && python3 - <<'PYEOF'\nprint(f\"{{x['id'][:8]}}\")\nPYEOF",
        ws.display()
    );
    for command in [
        "",
        "ls",
        "echo \"non fermé",
        "python3 - <<'PYEOF'\nprint(1)\nPYEOF",
        heredoc.as_str(),
        "cd /ailleurs && make\nmake install",
    ] {
        let args = json!({"command": command, "network": true});
        let _ = x.normalise_call("shell_exec", &args);
        let pattern = penelope_agent::arg_pattern("shell_exec", Some(&args));
        if command.contains('\n') {
            assert!(pattern.is_none(), "multi-lignes sans motif : {command:?}");
        }
        let a = s
            .approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                json!({"tool": "shell_exec", "arguments": args}),
                vec![],
                None,
                None,
                false,
            )
            .await
            .unwrap();
        let _ = approval_card(&a);
        g.send_approval_card(OWNER, None, &a).await.unwrap();
    }
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(sent.contains("PYEOF"), "{sent}");
    assert!(!sent.contains("Carte simplifiée"), "{sent}");
}

/// #141 : le bouton « Toujours » dit la famille qu'il réglera. #150 : il nomme
/// **toutes** les familles d'une liste `&&`, et quand aucune règle n'est possible il
/// n'y a **pas de bouton** à sa place — une coche verte s'y lisait comme un
/// « Toujours » nouvelle formule —, la carte disant ce qui l'empêche.
#[tokio::test]
async fn the_always_button_says_when_no_rule_is_possible() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    for (command, expected) in [
        (
            "glab api --hostname h \"projects?membership=true&per_page=100\"",
            "Toujours pour « glab »",
        ),
        ("GITLAB_HOST=h glab api \"p?x=1\"", "Toujours pour « glab »"),
        // La ligne de la routine YouTube : deux `yt-dlp` et une lecture, une famille.
        (
            "yt-dlp --print x https://y && yt-dlp -o tmp/z https://y && ls -la tmp/z*",
            "Toujours pour « yt-dlp »",
        ),
        // Deux familles : le clic les nomme toutes les deux avant d'écrire.
        (
            "yt-dlp -o tmp/z https://y && ffmpeg -i tmp/z out.mp3",
            "Toujours pour « yt-dlp », « ffmpeg »",
        ),
        // `cd` ne fait que régler le shell : la famille est celle qui agit (#123).
        ("cd /ailleurs && cargo test", "Toujours pour « cargo test »"),
        ("cd /ailleurs && ls", "pas de règle possible"),
        ("ls | sh", "pas de règle possible"),
        ("cargo test; rm -rf ~", "pas de règle possible"),
        ("a && $(b)", "pas de règle possible"),
        ("a && b > f", "pas de règle possible"),
        ("a && sh -c \"b\"", "pas de règle possible"),
    ] {
        t.clear().await;
        let a = s
            .approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                json!({"tool": "shell_exec", "arguments": {"command": command}}),
                vec![],
                None,
                None,
                false,
            )
            .await
            .unwrap();
        g.send_approval_card(OWNER, None, &a).await.unwrap();
        g.flush_outbox().await.unwrap();
        let card = t
            .calls_to(tg::SEND_MESSAGE)
            .await
            .pop()
            .expect("carte envoyée");
        let labels: String = card["reply_markup"]["inline_keyboard"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r.as_array().unwrap().iter())
            .map(|b| b["text"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        let text = card["text"].as_str().unwrap_or_default();
        if expected.contains("pas de règle") {
            // Pas de bouton dans la case de « Toujours », et jamais une coche verte
            // qui s'y substitue.
            assert!(
                !labels.contains("Toujours") && !labels.contains("pas de règle"),
                "{command} : aucun bouton ne prend la place de « Toujours » : {labels}"
            );
            assert!(
                text.contains("pas de règle possible") && text.contains("une commande par appel"),
                "{command} : la carte dit ce qui l'empêche et par où sortir : {text}"
            );
        } else {
            assert!(labels.contains(expected), "{command} : {labels}");
        }
    }
}

/// #134 : la carte dit combien de valeurs sont masquées ; ce qui entre dans la file
/// Telegram est rédigé ; `doctor` signale une ligne restée en clair.
#[tokio::test]
async fn stored_and_sent_secrets_are_masked_and_checked() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let a = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec", "arguments": {"command": penelope_observe::redact(
                "KEY = \"Zx9kQ2mV7pLr4TbW1nHs8YcD3fGa6JuE0oIq5RtKyNw2BvXe7LmPz4SdHj1Ua\""
            )}}),
            vec![],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    g.send_approval_card(OWNER, None, &a).await.unwrap();
    let key = "sk-or-v1-0123456789abcdef0123456789";
    g.reply(OWNER, None, None, &format!("voici la clé {key}"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let sent = texts(&t.calls_to(tg::SEND_MESSAGE).await).join("\n");
    assert!(sent.contains("valeur(s) masquée(s)"), "{sent}");
    assert!(!sent.contains(key), "{sent}");
    assert!(penelope_ops::doctor::stored_secret_check(&s).await.ok);

    let now = s.clock.now_rfc3339();
    let leaked = json!({"text": format!("clé {key}")}).to_string();
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO tg_outbox(id, chat_id, method, payload, state, created_at)
                     VALUES('o_ancien', 1, 'sendMessage', ?1, 'sent', ?2)",
                penelope_store::rusqlite::params![leaked, now],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let c = penelope_ops::doctor::stored_secret_check(&s).await;
    assert!(!c.ok && c.detail.contains("o_ancien"), "{c:?}");
}

/// #129 : une carte qui ne se rend pas part en texte brut, avec ses boutons, et
/// laisse un événement ; jamais un silence.
#[tokio::test]
async fn an_unrenderable_card_is_sent_plain_and_recorded() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let a = s
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({"tool": "shell_exec", "arguments": {"command": "make"}}),
            vec!["Autoriser".into(), "Refuser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();
    let vars: BTreeMap<String, String> = [
        ("intention".to_string(), "Je compile.".to_string()),
        ("action".to_string(), "```\nmake\n```".to_string()),
    ]
    .into_iter()
    .collect();
    let tokens: BTreeMap<String, String> = [
        (k::APPROVE.to_string(), "jeton-oui".to_string()),
        (k::DENY.to_string(), "jeton-non".to_string()),
    ]
    .into_iter()
    .collect();
    g.send_plain_approval(
        OWNER,
        None,
        &a,
        &vars,
        &tokens,
        "variables non fournies : x",
    )
    .await
    .unwrap();
    g.flush_outbox().await.unwrap();
    let card = t
        .calls_to(tg::SEND_MESSAGE)
        .await
        .pop()
        .expect("carte envoyée");
    let text = card["text"].as_str().unwrap();
    assert!(
        text.contains("Carte simplifiée") && text.contains("Je compile."),
        "{text}"
    );
    assert!(card.get("parse_mode").is_none(), "texte brut : {card}");
    assert!(
        card["reply_markup"].to_string().contains("jeton-oui"),
        "{card}"
    );
    let events = s.events.range(0, 1_000).await.unwrap();
    assert!(events.iter().any(|e| e.kind == "telegram.card_degraded"));
}

#[tokio::test]
async fn an_approval_card_click_resumes_the_turn() {
    let (_d, g, t, p) = gateway().await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "fs_write".into(),
            arguments: json!({"path": "note.txt", "content": "ok"}),
        }],
    ));
    g.process_update(&updates::text_message(3, OWNER, OWNER, "écris note.txt"))
        .await
        .unwrap();
    drain(&g).await;

    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    assert!(
        card["text"].as_str().unwrap().contains("fs_write"),
        "{card}"
    );
    let rows = card["reply_markup"]["inline_keyboard"].as_array().unwrap();
    let approve = rows[0][0]["callback_data"].as_str().unwrap().to_string();
    assert!(approve.len() <= 64);
    assert!(rows[0][1]["text"].as_str().unwrap().contains("session"));

    p.reply("Fichier écrit.");
    t.clear().await;
    g.process_update(&updates::callback(4, OWNER, &approve, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;

    let out = texts(&t.calls_to(tg::SEND_MESSAGE).await);
    assert!(out.iter().any(|x| x.contains("Autoriser")), "{out:?}");
    assert!(out.iter().any(|x| x == "Fichier écrit."), "{out:?}");
    assert_eq!(t.calls_to(tg::ANSWER_CALLBACK_QUERY).await.len(), 1);
    let ws = penelope_executor::executor::default_workspaces(&g.daemon.services);
    assert_eq!(
        std::fs::read_to_string(ws[0].join("note.txt")).unwrap(),
        "ok"
    );

    // Un second clic sur le même bouton ne rejoue rien.
    t.clear().await;
    g.process_update(&updates::callback(5, OWNER, &approve, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    drain(&g).await;
    let answers = t.calls_to(tg::ANSWER_CALLBACK_QUERY).await;
    assert_eq!(answers[0]["text"], "Déjà traité.");
    assert!(texts(&t.calls_to(tg::SEND_MESSAGE).await).is_empty());
}

#[tokio::test]
async fn a_refusal_with_a_reason_reaches_the_model() {
    let (_d, g, t, p) = gateway().await;
    p.reply(r#"{"complexity":"medium"}"#);
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": "rm -rf build"}),
        }],
    ));
    g.process_update(&updates::text_message(10, OWNER, OWNER, "nettoie"))
        .await
        .unwrap();
    drain(&g).await;
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let rows = card["reply_markup"]["inline_keyboard"].as_array().unwrap();
    let deny_reason = rows
        .iter()
        .flat_map(|r| r.as_array().unwrap().iter())
        .find(|b| b["text"].as_str().unwrap().contains("raison"))
        .unwrap()["callback_data"]
        .as_str()
        .unwrap()
        .to_string();

    g.process_update(&updates::callback(11, OWNER, &deny_reason, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    p.reply("Compris, je garde le dossier build.");
    g.process_update(&updates::text_message(
        12,
        OWNER,
        OWNER,
        "on en a besoin pour la démo",
    ))
    .await
    .unwrap();
    drain(&g).await;

    let last = p.requests().last().unwrap().clone();
    let seen: Vec<String> = last.messages.iter().map(|m| m.text()).collect();
    assert!(
        seen.iter()
            .any(|t| t.contains("on en a besoin pour la démo")),
        "la raison doit arriver au modèle : {seen:?}"
    );
}

/// #111 : `/mode` montre le mode de la session et le change d'un bouton.
#[tokio::test]
async fn the_approval_mode_is_set_from_telegram() {
    let (_d, g, t, _p) = gateway().await;
    let d = g.daemon.clone();
    g.process_update(&updates::text_message(720, OWNER, OWNER, "/mode"))
        .await
        .unwrap();
    g.flush_outbox().await.unwrap();
    let menu = t.calls_to(tg::SEND_MESSAGE).await.last().unwrap().clone();
    assert!(
        menu["text"]
            .as_str()
            .unwrap()
            .contains("lectures sans demande"),
        "{menu}"
    );
    let auto = inline_buttons(&menu)
        .into_iter()
        .find(|(l, _)| l.contains("Tout sauf le destructif"))
        .unwrap()
        .1;
    g.process_update(&updates::callback(721, OWNER, &auto, 9100))
        .await
        .unwrap();
    settle_click(&g).await;
    let sid = d
        .chat_session_for(&Origin::Telegram {
            chat_id: OWNER,
            topic_id: None,
            message_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        penelope_daemon::approval_mode::of_session(&d.services, &sid).await,
        penelope_agent::ApprovalMode::Auto
    );
}
