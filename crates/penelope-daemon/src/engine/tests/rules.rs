use super::*;

/// #117 : un appel aux arguments invalides ne coûte aucune carte : l'erreur et les
/// paramètres attendus reviennent au modèle ; le balisage d'appel laissé dans une valeur
/// est nommé ; corrigé, le même appel demande l'approbation normalement ; des appels
/// invalides répétés sur le même outil déclenchent la garde de boucle.
#[tokio::test]
async fn invalid_calls_are_refused_before_any_approval_card() {
    let (_dir, d, out, sid) = tool_turn(vec![(
        "workflow_start",
        json!({"id": "build-verify", "params": "objectif : corriger la pagination"}),
    )])
    .await;
    assert!(
        !matches!(out, TurnOutcome::AwaitingApproval { .. }),
        "{out:?}"
    );
    assert!(
        d.services.approvals.pending(10).await.unwrap().is_empty(),
        "aucune carte"
    );
    let results = tool_results(&d, &sid).await;
    assert!(
        results
            .iter()
            .any(|r| r.contains("params") && r.contains("`params` (object)")),
        "{results:?}"
    );

    let (_dir, d, _out, sid) = tool_turn(vec![(
        "workflow_start",
        json!({"id": "build-verify",
               "params": "<arg_key>objectif</arg_key> <arg_value>Corriger la pagination</arg_value>"}),
    )])
    .await;
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
    let results = tool_results(&d, &sid).await;
    assert!(
        results
            .iter()
            .any(|r| r.contains("balisage d'appel d'outil") && r.contains("<arg_key>")),
        "{results:?}"
    );

    let (_dir, d, out, _sid) = tool_turn(vec![(
        "workflow_start",
        json!({"id": "build-verify", "params": {"objectif": "corriger la pagination"}}),
    )])
    .await;
    assert!(
        matches!(out, TurnOutcome::AwaitingApproval { .. }),
        "{out:?}"
    );
    assert_eq!(d.services.approvals.pending(10).await.unwrap().len(), 1);

    let (_dir, d, out, sid) = tool_turn(vec![
        (
            "workflow_start",
            json!({"id": "build-verify", "params": "a"}),
        ),
        (
            "workflow_start",
            json!({"id": "build-verify", "params": "b"}),
        ),
        (
            "workflow_start",
            json!({"id": "build-verify", "params": "c"}),
        ),
    ])
    .await;
    assert!(d.services.approvals.pending(10).await.unwrap().is_empty());
    let results = tool_results(&d, &sid).await;
    assert!(
        results
            .iter()
            .any(|r| r.contains("[avertissement du harnais]") && r.contains("arguments invalides")),
        "le deuxième avertit : {results:?}"
    );
    assert!(
        matches!(out, TurnOutcome::LoopAborted { .. }),
        "le troisième arrête : {out:?}"
    );
}

/// #150 : une liste `&&` a des familles, un « Toujours » écrit une règle par famille,
/// et la même ligne repasse ensuite sans carte. La scène est celle du 20/09 : la
/// routine YouTube colle trois commandes, le propriétaire clique, et la vidéo
/// suivante redemandait.
#[tokio::test]
async fn an_and_list_gets_a_rule_per_family_and_stops_asking() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();

    let turn_with = |command: String| {
        let (d, p, sid) = (d.clone(), p.clone(), sid.clone());
        async move {
            p.push(Scripted::ToolCalls(
                String::new(),
                vec![ToolCall {
                    id: "c1".into(),
                    name: "shell_exec".into(),
                    arguments: json!({"command": command, "network": true}),
                }],
            ));
            p.reply("C'est fait.");
            d.enqueue_message(&sid, "vas-y", &Origin::Cli, None)
                .await
                .unwrap();
            let turn = claim(&d).await;
            d.run_turn(&turn).await
        }
    };

    let line = |id: &str| {
        format!(
            "yt-dlp --skip-download --print \"TITLE: %(title)s\" https://youtu.be/{id} \
                 && yt-dlp --skip-download --write-subs -o tmp/yt-{id} https://youtu.be/{id} \
                 && ls -la tmp/yt-{id}*"
        )
    };

    // Première vidéo : une carte, et un « Toujours » qui écrit vraiment.
    let out = turn_with(line("F2iVKgQh_TU")).await;
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("la première ligne doit demander : {out:?}");
    };
    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    let rules = d.services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1, "une règle par famille : {rules:?}");
    assert_eq!(
        rules[0].arg_match,
        Some(json!({"command": {"$cmd_prefix": "yt-dlp"}, "network": true})),
        "la règle porte la famille et son réseau"
    );
    // Le ledger dit qu'une règle a été écrite, pas seulement qu'on a cliqué.
    let a = d.services.approvals.get(&approval_id).await.unwrap();
    assert_eq!(a.unwrap().rule_created.as_deref(), Some("always"));

    // La règle couvre maintenant la ligne entière : chaque étape l'est, la lecture
    // finale n'en demande pas. Deuxième vidéo, autre identifiant, plus de carte.
    let cfg = d.services.config.config();
    let verdict = |command: String| {
        let (d, cfg) = (d.clone(), cfg.clone());
        async move {
            d.services
                .policies
                .evaluate(
                    &cfg.mcp.policy,
                    "shell_exec",
                    None,
                    &json!({"command": command, "network": true}),
                    penelope_kernel::risk::RiskClass::External,
                    None,
                    None,
                )
                .await
                .unwrap()
        }
    };
    assert_eq!(
        verdict(line("dQw4w9WgXcQ")).await.decision,
        penelope_kernel::risk::PolicyDecision::Auto,
        "la même ligne, autre URL, ne redemande pas"
    );
    // Une étape hors des familles couvertes fait repartir la ligne entière en carte.
    assert_ne!(
        verdict(format!("{} && curl https://x", line("abc")))
            .await
            .decision,
        penelope_kernel::risk::PolicyDecision::Auto,
        "`curl` n'est pas couvert : la liste redemande"
    );
    // Et le `;` de #67 n'est jamais couvert, quelles que soient les règles.
    assert_ne!(
        verdict("yt-dlp https://y; rm -rf ~".into()).await.decision,
        penelope_kernel::risk::PolicyDecision::Auto,
        "`;` reste composé"
    );
}

/// #150 : un « Toujours » qui n'écrit rien le dit au ledger. Le 20/09, 107 clics
/// « Toujours » pour 27 règles, sans que rien ne distingue les deux.
#[tokio::test]
async fn a_composed_line_records_that_no_rule_was_written() {
    let (_dir, d, out) = shell_turn("cargo test; rm -rf ~", None, &[]).await;
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}");
    };
    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    assert!(
        d.services.policies.active_rules().await.unwrap().is_empty(),
        "`;` reste composé (#67)"
    );
    let a = d
        .services
        .approvals
        .get(&approval_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        a.rule_created, None,
        "aucune règle écrite : le ledger ne dit pas « always »"
    );
}

/// #111 : les lectures ne demandent rien en mode par défaut ; écriture, sous-shell,
/// redirection, réseau et commande composée demandent ; une commande composée
/// approuvée « Toujours » ne crée aucune règle ; « demander tout » redemande même une
/// lecture ; « tout sauf le destructif » laisse passer une écriture, jamais `rm` ni
/// une commande composée ; une famille déclarée passe sans demande.
#[tokio::test]
async fn shell_commands_are_classified_before_asking() {
    for read in [
        "ls -la /tmp",
        "cat x",
        "grep -r foo src",
        // #141 : un tube vers une lecture pure reste une lecture.
        "grep -rn foo src | head -20",
        "cat x | wc -l",
    ] {
        let (_dir, _d, out) = shell_turn(read, None, &[]).await;
        assert!(!asked(&out), "{read} : {out:?}");
    }
    for write in [
        "rm -rf x",
        "sh -c \"ls\"",
        "ls > fichier",
        "curl https://example.com",
    ] {
        let (_dir, _d, out) = shell_turn(write, None, &[]).await;
        assert!(asked(&out), "{write} : {out:?}");
    }

    let (_dir, d, out) = shell_turn("cd /x && ls", None, &[]).await;
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}");
    };
    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    assert!(
        d.services.policies.active_rules().await.unwrap().is_empty(),
        "aucune règle sur `cd`"
    );

    let (_dir, _d, out) = shell_turn("ls -la /tmp", Some("ask"), &[]).await;
    assert!(asked(&out), "demander tout : {out:?}");
    let (_dir, _d, out) = shell_turn("mkdir build", Some("auto"), &[]).await;
    assert!(!asked(&out), "auto : {out:?}");
    for risky in ["rm -rf build", "cd /x && make", "cd build && rm -rf cible"] {
        let (_dir, _d, out) = shell_turn(risky, Some("auto"), &[]).await;
        assert!(asked(&out), "auto, {risky} : {out:?}");
    }
    let (_dir, _d, out) = shell_turn("cargo test -p x", None, &["cargo test"]).await;
    assert!(!asked(&out), "famille déclarée : {out:?}");
    let (_dir, _d, out) = shell_turn("cargo test; rm -rf ~", None, &["cargo test"]).await;
    assert!(asked(&out), "enchaînement : {out:?}");
}

/// #123 : un `cd <workspace> &&` seul en tête est le répertoire de travail de la
/// commande qui suit, qui se classe pour ce qu'elle est ; hors workspace, suivi d'une
/// écriture, d'une redirection ou d'un second enchaînement, la ligne est demandée.
#[tokio::test]
async fn a_cd_into_the_workspace_is_the_working_directory() {
    for read in [
        "cd {ws} && grep -rn foo src",
        "cd src && grep -n \"LIMIT\" app.tsx",
        "cd '{ws}' && git log -5",
    ] {
        let (_dir, _d, out) = shell_turn(read, None, &[]).await;
        assert!(!asked(&out), "{read} : {out:?}");
    }
    for asks in [
        "cd /ailleurs && grep foo",
        "cd {ws} && rm -rf build",
        "cd {ws} && grep foo > sortie.txt",
        "cd {ws} && echo x; grep foo",
        "cd {ws} && grep foo && curl https://example.com",
        "cd $HOME && grep foo",
    ] {
        let (_dir, _d, out) = shell_turn(asks, None, &[]).await;
        assert!(asked(&out), "{asks} : {out:?}");
    }
    // Une famille déclarée d'avance vaut aussi derrière le `cd`.
    let (_dir, _d, out) = shell_turn("cd {ws} && cargo test -p x", None, &["cargo test"]).await;
    assert!(!asked(&out), "famille déclarée : {out:?}");
}

/// #123 : la carte d'une ligne `cd <workspace> && …` montre la vraie commande et son
/// répertoire ; « Toujours » règle la vraie commande, pas `cd`, et ne couvre pas une
/// autre commande derrière le même `cd`.
#[tokio::test]
async fn always_after_a_cd_rules_the_real_command() {
    let (_dir, d, p) = daemon().await;
    let ws = penelope_executor::executor::default_workspaces(&d.services)[0].clone();
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    let call = |command: &str, id: &str| {
        Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: id.into(),
                name: "shell_exec".into(),
                arguments: json!({"command": format!("cd {} && {command}", ws.display())}),
            }],
        )
    };

    p.push(call("make check", "c1"));
    d.enqueue_message(&sid, "vérifie", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}");
    };
    let a = d
        .services
        .approvals
        .get(&approval_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.payload["arguments"]["command"], "make check");
    assert_eq!(
        a.payload["arguments"]["cwd"],
        json!(ws.canonicalize().unwrap().to_string_lossy()),
        "{}",
        a.payload
    );
    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    let rules = d.services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(
        rules[0].arg_match,
        Some(json!({"command": {penelope_hitl::policy::CMD_PREFIX_OP: "make check"}})),
        "la vraie commande, pas `cd`"
    );

    // Reprise : l'appel approuvé part, puis la même famille derrière le même `cd`
    // passe par la règle.
    d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
        .await
        .unwrap();
    p.push(call("make check -s", "c2"));
    p.reply("C'est fait.");
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    assert!(!asked(&out), "même famille : {out:?}");

    p.push(call("curl https://example.com", "c3"));
    d.enqueue_message(&sid, "et ça", &Origin::Cli, None)
        .await
        .unwrap();
    let out = d.run_turn(&claim(&d).await).await;
    assert!(asked(&out), "autre commande derrière le même cd : {out:?}");
}

/// #141 : une URL de requête entre guillemets (`?a=1&b=2`) n'enchaîne rien. Le
/// « Toujours » d'un appel `glab` crée une règle `glab`, elle couvre l'appel suivant
/// de la famille (affectation d'environnement comprise), et une famille déclarée
/// d'avance avec réseau la couvre aussi. Un vrai enchaînement redemande.
#[tokio::test]
async fn a_quoted_query_url_is_ruled_by_its_family() {
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    let call = |command: &str, id: &str| {
        Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: id.into(),
                name: "shell_exec".into(),
                arguments: json!({"command": command}),
            }],
        )
    };

    p.push(call(
        "glab api --hostname h \"groups?search=14&per_page=20\"",
        "c1",
    ));
    d.enqueue_message(&sid, "liste les groupes", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}");
    };
    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_always("cli"),
    )
    .await
    .unwrap();
    let rules = d.services.policies.active_rules().await.unwrap();
    assert_eq!(rules.len(), 1, "{rules:?}");
    assert_eq!(
        rules[0].arg_match,
        Some(json!({"command": {penelope_hitl::policy::CMD_PREFIX_OP: "glab"}})),
        "la famille est `glab`, pas la ligne entière"
    );

    // Reprise : l'appel approuvé part, puis la même famille passe par la règle,
    // derrière une affectation d'environnement anodine.
    d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
        .await
        .unwrap();
    p.push(call(
        "GITLAB_HOST=h glab api \"projects?membership=true&per_page=100\"",
        "c2",
    ));
    p.reply("C'est fait.");
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    assert!(!asked(&out), "même famille : {out:?}");

    // Le tube vers une lecture pure passe par la même règle : c'est `glab` qui agit,
    // `jq` ne fait que formater (commentaire de #141). Huit « Toujours » cliqués pour
    // rien en cinq minutes venaient de là.
    p.push(call(
        "glab api --hostname h \"pipelines?per_page=30\" | jq -r '.[].id'",
        "c3",
    ));
    p.reply("Voilà.");
    d.enqueue_message(&sid, "les pipelines", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    assert!(!asked(&out), "tube de lecture pure : {out:?}");

    // Un tube vers autre chose qu'une lecture reste un enchaînement : il redemande.
    p.push(call("glab api h \"p\" | sh", "c4"));
    d.enqueue_message(&sid, "et ça", &Origin::Cli, None)
        .await
        .unwrap();
    let out = d.run_turn(&claim(&d).await).await;
    assert!(asked(&out), "enchaînement : {out:?}");
}

/// #141 : `tools.shell_allow_network` couvre une commande dont l'URL porte un `&`.
#[tokio::test]
async fn a_declared_family_covers_a_quoted_query_url() {
    let (_dir, d, p) = daemon().await;
    d.publish_config("test", |c| {
        c.tools.shell_allow_network = vec!["glab api".into()];
        Ok(vec!["tools.shell_allow_network".into()])
    })
    .unwrap();
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({
                "command": "glab api --hostname h \"projects?membership=true&per_page=100\"",
                "network": true
            }),
        }],
    ));
    p.reply("C'est fait.");
    d.enqueue_message(&sid, "liste les projets", &Origin::Cli, None)
        .await
        .unwrap();
    let out = d.run_turn(&claim(&d).await).await;
    assert!(!asked(&out), "famille déclarée avec réseau : {out:?}");
}

/// #130 : « Toujours » puis reprise sur la commande exacte de l'incident, avec son
/// préfixe `cd` vers un workspace ou ailleurs, réseau demandé : pas de panique, et
/// une commande multi-lignes ne crée pas de règle (pas de famille, #67 et #111).
#[tokio::test]
async fn always_on_a_multiline_heredoc_resumes_without_panic() {
    for dir in ["{ws}", "/Users/essai/depot"] {
        let (_dir, d, p) = daemon().await;
        let ws = penelope_executor::executor::default_workspaces(&d.services)[0].clone();
        let dir = dir.replace("{ws}", &ws.to_string_lossy());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        d.pin_model(&sid, Some("main")).await.unwrap();
        let command = format!("cd {dir} && {HEREDOC}");
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "shell_exec".into(),
                arguments: json!({"command": command, "network": true}),
            }],
        ));
        d.enqueue_message(&sid, "contre-preuve API", &Origin::Cli, None)
            .await
            .unwrap();
        let turn = claim(&d).await;
        let out = d.run_turn(&turn).await;
        d.services.turns.complete(&turn).await.unwrap();
        let TurnOutcome::AwaitingApproval { approval_id } = out else {
            panic!("{dir} : {out:?}");
        };
        crate::agent::decide_approval(
            &d.services,
            &approval_id,
            &penelope_hitl::Decision::approve_always("cli"),
        )
        .await
        .unwrap();
        assert!(
            d.services.policies.active_rules().await.unwrap().is_empty(),
            "{dir} : pas de règle pour une commande multi-lignes"
        );
        d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
            .await
            .unwrap();
        p.reply("Fait.");
        let turn = claim(&d).await;
        let out = d.run_turn(&turn).await;
        assert!(
            matches!(out, TurnOutcome::Answered { .. }),
            "{dir} : {out:?}"
        );
    }
}

/// #134 : une clé recopiée dans une commande est masquée dans la demande stockée, et
/// la commande exécutée reste entière ; un `fs_write` d'un fichier qui porte une clé
/// n'est pas altéré.
#[tokio::test]
async fn a_copied_key_is_stored_masked_and_executed_whole() {
    let key = "Zx9kQ2mV7pLr4TbW1nHs8YcD3fGa6JuE0oIq5RtKyNw2BvXe7LmPz4SdHj1Ua";
    let (_dir, d, p) = daemon().await;
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "shell_exec".into(),
            arguments: json!({"command": format!("echo {key}; touch trace")}),
        }],
    ));
    d.enqueue_message(&sid, "teste l'API", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let out = d.run_turn(&turn).await;
    d.services.turns.complete(&turn).await.unwrap();
    let TurnOutcome::AwaitingApproval { approval_id } = out else {
        panic!("{out:?}");
    };
    let a = d
        .services
        .approvals
        .get(&approval_id)
        .await
        .unwrap()
        .unwrap();
    let stored = a.payload.to_string();
    assert!(!stored.contains(key), "{stored}");
    assert!(stored.contains(penelope_observe::redact::MASK), "{stored}");

    crate::agent::decide_approval(
        &d.services,
        &approval_id,
        &penelope_hitl::Decision::approve_once("cli"),
    )
    .await
    .unwrap();
    d.enqueue_resume(&sid, &approval_id, &Origin::Cli)
        .await
        .unwrap();
    p.reply("Fait.");
    let turn = claim(&d).await;
    let _ = d.run_turn(&turn).await;
    let ran = tool_results(&d, &sid).await.join("\n");
    assert!(
        ran.contains(key),
        "la commande exécutée garde la clé : {ran}"
    );

    let ws = default_workspaces(&d.services)[0].clone();
    let config = format!("API_KEY={key}\n");
    let exec = penelope_executor::executor::NativeToolExecutor::new(
        d.services.clone(),
        penelope_executor::executor::ToolEnv {
            session_id: sid.clone(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: vec![ws.clone()],
            in_workflow: false,
            turn_model: None,
        },
    );
    use penelope_agent::ToolExecutor;
    exec.execute("fs_write", &json!({"path": ".env", "content": config}))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(ws.join(".env")).unwrap(), config);
}

#[tokio::test]
async fn config_set_asks_twice_for_sensitive_settings_even_with_an_always_rule() {
    let (_dir, d, p) = daemon().await;
    let s = &d.services;
    s.policies
        .create_rule(
            penelope_hitl::policy::RuleScope::Tool,
            Some("config_set"),
            None,
            None,
            penelope_kernel::risk::PolicyDecision::Auto,
            penelope_kernel::risk::PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();

    // Réglage ordinaire : la règle « toujours » s'applique, c'est exécuté.
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c1".into(),
            name: "config_set".into(),
            arguments: json!({"path": "models.routing.classifier", "value": "false"}),
        }],
    ));
    p.reply("C'est fait.");
    d.enqueue_message(&sid, "coupe le routage adaptatif", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    assert!(matches!(
        d.run_turn(&turn).await,
        TurnOutcome::Answered { .. }
    ));
    d.services.turns.complete(&turn).await.unwrap();
    assert!(!s.config.config().models.routing.classifier);

    // Bac à sable : double confirmation malgré la règle, rien n'est appliqué.
    let profile_before = s.config.config().sandbox.default_profile.clone();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![ToolCall {
            id: "c2".into(),
            name: "config_set".into(),
            arguments: json!({"path": "sandbox.default_profile", "value": "full"}),
        }],
    ));
    d.enqueue_message(&sid, "enlève le bac à sable", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    let approval_id = match d.run_turn(&turn).await {
        TurnOutcome::AwaitingApproval { approval_id } => approval_id,
        other => panic!("{other:?}"),
    };
    let a = s.approvals.get(&approval_id).await.unwrap().unwrap();
    assert_eq!(a.payload["double"], true);
    assert_eq!(s.config.config().sandbox.default_profile, profile_before);

    // Un secret ne passe jamais, même approuvé.
    let x = penelope_executor::executor::NativeToolExecutor::new(
        s.clone(),
        penelope_executor::executor::ToolEnv {
            session_id: sid.clone(),
            run_id: None,
            origin: Origin::Cli,
            workspaces: penelope_executor::executor::default_workspaces(s),
            in_workflow: false,
            turn_model: None,
        },
    );
    use penelope_agent::ToolExecutor;
    let err = x
        .execute(
            "config_set",
            &json!({"path": "providers.openrouter.api_key", "value": "sk-or-v1-x"}),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("penelope secret set"), "{err}");
}

#[tokio::test]
async fn the_model_reaches_mcp_tools_through_the_supervisor() {
    use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};
    let (_dir, d, p) = daemon().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        server(Arc::new(std::sync::Mutex::new(vec![
            tool("list_issues", json!({"readOnlyHint": true})),
            tool("delete_issue", json!({"destructiveHint": true})),
        ]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(d.services.clone(), fake.clone());
    declare(&sup, "redmine", "[tool_policy]\ndelete_issue = \"deny\"\n");
    sup.reload().await;
    d.hooks.set_mcp(sup.clone());

    let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
    d.pin_model(&sid, Some("main")).await.unwrap();
    p.push(Scripted::ToolCalls(
        String::new(),
        vec![
            ToolCall {
                id: "c1".into(),
                name: "tool_call".into(),
                arguments: json!({"name": "mcp__redmine__list_issues", "args": {"project": "penelope"}}),
            },
            ToolCall {
                id: "c2".into(),
                name: "tool_call".into(),
                arguments: json!({"name": "mcp__redmine__delete_issue", "args": {}}),
            },
        ],
    ));
    p.reply("Voici les tickets.");
    d.enqueue_message(&sid, "liste mes tickets", &Origin::Cli, None)
        .await
        .unwrap();
    let turn = claim(&d).await;
    match d.run_turn(&turn).await {
        TurnOutcome::Answered { text, .. } => assert!(text.contains("tickets")),
        other => panic!("{other:?}"),
    }

    // Le prompt annonce le serveur et les méta-outils.
    let first = p.requests()[0].clone();
    assert!(first.messages[0].text().contains("redmine : 2 outils"));
    assert!(first.tools.iter().any(|t| t.name == "tool_search"));

    let history = d.services.context.history.load(&sid, 0).await.unwrap();
    let results: Vec<String> = history
        .iter()
        .filter(|e| e.message.role == penelope_llm::types::Role::Tool)
        .map(|e| e.message.text())
        .collect();
    assert!(
        results
            .iter()
            .any(|r| r.contains("list_issues") && r.contains("penelope")),
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .any(|r| r.contains("Refusé") || r.contains("refus")),
        "la déclaration interdit delete_issue : {results:?}"
    );
    assert_eq!(
        sup.statuses().await[0].calls,
        1,
        "delete_issue n'a jamais atteint le serveur"
    );
}
