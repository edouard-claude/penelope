use super::*;
use penelope_kernel::clock::TestClock;

mod config;
mod mcp;

#[tokio::test]
async fn invalid_clone_source_is_rejected_before_approval_even_via_tool_call() {
    let (_dir, executor) = executor().await;
    let args = json!({"url":"../autre", "dest":"imap-src"});
    let direct = executor.precheck("git_clone", &args).await;
    assert!(
        matches!(direct, Err(ToolError::BadArguments { .. })),
        "{direct:?}"
    );
    assert!(matches!(
        executor
            .precheck("tool_call", &json!({"name":"git_clone", "args":args}))
            .await,
        Err(ToolError::BadArguments { .. })
    ));
}

/// #130 : la forme exacte de l'incident, une commande en échec qui écrit 41 lignes,
/// passe par le résumé sans paniquer et rend toute sa sortie.
#[tokio::test]
async fn a_failing_command_of_41_lines_is_returned_whole() {
    let (dir, e) = executor().await;
    let ws = dir.path().join("ws");
    let log: String = (1..=41).map(|i| format!("ligne {i}\n")).collect();
    std::fs::write(ws.join("sortie.txt"), &log).unwrap();
    let out = e
        .execute("shell_exec", &json!({"command": "cat sortie.txt; exit 1"}))
        .await
        .unwrap();
    assert!(
        out.text.contains("ligne 1\n") && out.text.contains("ligne 41"),
        "{}",
        out.text
    );
}

/// Issue #32 : `cargo test` avec 2 échecs sur 500 rend au modèle les 2 échecs et le
/// résumé, moins de 2 000 tokens, et la sortie complète part en artefact.
#[tokio::test]
async fn a_test_run_is_digested_and_its_full_output_kept_as_an_artifact() {
    let (dir, e) = executor().await;
    let ws = dir.path().join("ws");
    let mut log = String::from("running 500 tests\n");
    for i in 0..498 {
        log.push_str(&format!("test tests::case_{i} ... ok\n"));
    }
    log.push_str(
        "test tests::parses_dates ... FAILED\ntest tests::rounds_totals ... FAILED\n\nfailures:\n\n\
             ---- tests::parses_dates stdout ----\nthread 'tests::parses_dates' panicked at src/dates.rs:42:9:\n\
             assertion failed\n\n---- tests::rounds_totals stdout ----\n\
             thread 'tests::rounds_totals' panicked at src/totals.rs:7:5:\nattendu 12.50, obtenu 12.49\n\n\
             test result: FAILED. 498 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );
    std::fs::write(ws.join("sortie.txt"), &log).unwrap();
    // La commande nomme `cargo test` : c'est elle qui décide du filtre.
    let command = "cat sortie.txt; : cargo test; exit 101";
    let out = e
        .execute("shell_exec", &json!({"command": command}))
        .await
        .unwrap();
    assert!(out.text.contains("498 passed; 2 failed"), "{}", out.text);
    assert!(out.text.contains("src/dates.rs:42:9") && out.text.contains("12.49"));
    assert!(
        !out.text.contains("case_17 ... ok"),
        "pas de lignes de succès"
    );
    assert!(out.text.chars().count() / 4 < 2_000);
    let id = out
        .text
        .split("artifact_read(\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .expect("référence d'artefact")
        .to_string();
    let page = e
        .execute("artifact_read", &json!({"id": id}))
        .await
        .unwrap();
    assert!(
        page.text.contains("case_17 ... ok"),
        "sortie complète en artefact"
    );

    let full = e
        .execute("shell_exec", &json!({"command": command, "output": "full"}))
        .await
        .unwrap();
    assert!(
        full.text.contains("case_17 ... ok"),
        "sortie brute sur demande"
    );
}

/// Issue #34 : un workflow à l'étape de type inconnu est refusé avec l'erreur et le lien
/// vers « Les neuf types d'étapes » de la version compilée.
#[tokio::test]
async fn an_invalid_workflow_draft_points_to_its_documentation() {
    let (_dir, e) = executor().await;
    let draft = json!({
        "metadata": {"id": "essai", "name": "Essai"},
        "entryStep": "a",
        "steps": [{"id": "a", "name": "A", "type": "teleportation"}]
    });
    let err = e
        .execute("workflow_author", &json!({"draft": draft}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("teleportation"), "{err}");
    assert!(err.contains("« Les neuf types d'étapes »"), "{err}");
    assert!(
        err.contains(&format!(
            "https://github.com/edouard-claude/penelope/blob/v{}/docs/workflows.md#les-neuf-types-détapes",
            crate::VERSION
        )),
        "{err}"
    );
    let docs = e
        .execute(
            "self_docs",
            &json!({"action": "search", "query": "sous-groupe"}),
        )
        .await
        .unwrap();
    assert!(docs.text.contains("docs/workflows.md"), "{}", docs.text);
}

async fn executor() -> (tempfile::TempDir, NativeToolExecutor) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    s.config
        .mutate("test", |cfg| {
            cfg.sandbox.workspaces = vec![ws.to_string_lossy().into_owned()];
            Ok(vec!["sandbox.workspaces".into()])
        })
        .unwrap();
    let env = ToolEnv {
        session_id: "s1".into(),
        run_id: None,
        origin: Origin::Cli,
        workspaces: vec![penelope_platform::sandbox::normalise(&ws)],
        in_workflow: false,
        turn_model: None,
    };
    (dir, NativeToolExecutor::new(s, env))
}

#[tokio::test]
async fn workflow_plan_is_durable_revisable_and_gates_telegram_start() {
    use penelope_workflow::plan::{PlanGate, PlanStore};
    let (_dir, mut e) = executor().await;
    e.env.origin = Origin::Telegram {
        chat_id: 1,
        topic_id: Some(2),
        message_id: Some(3),
    };
    assert!(
        e.execute("workflow_start", &json!({"id":"build-verify"}))
            .await
            .is_err()
    );
    let first = e
        .execute(
            "workflow_plan",
            &json!({
                "id":"build-verify", "goal":"Vérifier le projet",
                "steps":[{"phase":"tests", "title":"Exécuter les tests"}]
            }),
        )
        .await
        .unwrap();
    assert!(first.text.contains("Vérifier le projet"));
    let plans = PlanStore::new(e.services.store.clone());
    let saved = plans.get("s1").await.unwrap().unwrap();
    assert_eq!(saved.plan.version(), 1);
    assert_eq!(saved.plan.gate(), PlanGate::Review);
    assert!(!saved.plan.can_execute());
    e.execute(
        "workflow_plan",
        &json!({
            "id":"build-verify", "expected_version":1,
            "goal":"Vérifier et documenter le projet",
            "steps":[{"phase":"tests", "title":"Exécuter les tests"},
                     {"phase":"verification", "title":"Relire le résultat"}]
        }),
    )
    .await
    .unwrap();
    assert_eq!(plans.get("s1").await.unwrap().unwrap().plan.version(), 2);
    assert!(
        e.execute(
            "workflow_plan",
            &json!({
                "id":"build-verify", "expected_version":1,
                "goal":"Version périmée", "steps":[{"phase":"tests", "title":"Test"}]
            })
        )
        .await
        .is_err()
    );
    assert_eq!(plans.get("s1").await.unwrap().unwrap().plan.version(), 2);
    let mut approved = plans.get("s1").await.unwrap().unwrap();
    let previous = approved.clone();
    approved.approve(2).unwrap();
    plans.replace("s1", &previous, &approved).await.unwrap();
    e.execute(
        "workflow_plan",
        &json!({
            "id":"build-verify", "goal":"Vérifier un autre changement",
            "steps":[{"phase":"tests", "title":"Nouveaux tests"}]
        }),
    )
    .await
    .unwrap();
    assert_eq!(plans.get("s1").await.unwrap().unwrap().plan.version(), 1);
    assert_eq!(plans.approved("s1").await.unwrap(), vec![approved]);
}

/// #137 : le contrat des critères. `label` devient `text`, un statut manquant vaut
/// `pending` ; cocher passe par `update` sur un `id` connu ; une entrée sans `id` ou un
/// statut hors vocabulaire sont refusés avec ce qu'il faut.
#[test]
fn criteria_follow_their_contract() {
    use penelope_kernel::session::MetadataOp;
    let set = criteria_entry(
        MetadataOp::Set,
        json!([{"id": "build", "label": "compile"}, {"id": "tests", "text": "tests verts"}]),
        &Value::Null,
    )
    .unwrap();
    assert_eq!(set[0]["text"], "compile");
    assert_eq!(set[0]["status"], "pending");
    let e = criteria_entry(
        MetadataOp::Update,
        json!({"status": "all_passed", "summary": "7/7"}),
        &set,
    )
    .unwrap_err();
    assert!(e.contains("build, tests") && e.contains("completed"), "{e}");
    let e = criteria_entry(
        MetadataOp::Update,
        json!({"id": "build", "status": "all_passed"}),
        &set,
    )
    .unwrap_err();
    assert!(e.contains("pending, completed, passed, failed"), "{e}");
    criteria_entry(
        MetadataOp::Update,
        json!({"id": "build", "status": "completed"}),
        &set,
    )
    .unwrap();
    assert!(
        criteria_entry(
            MetadataOp::Append,
            json!({"id": "build", "text": "x"}),
            &set
        )
        .is_err()
    );
    assert!(
        criteria_entry(
            MetadataOp::Set,
            json!([{"id": "a", "text": "x"}, {"id": "a", "text": "y"}]),
            &Value::Null
        )
        .is_err()
    );
}

/// #123 : `cd <workspace> && …` devient la commande et son `cwd`, en direct comme par
/// `tool_call` (`args_json` compris) ; un `cwd` déjà donné ou un répertoire hors des
/// workspaces laisse l'appel tel quel.
#[tokio::test]
async fn a_cd_into_a_workspace_becomes_the_cwd() {
    let (_dir, x) = executor().await;
    let ws = x.env.workspaces[0].clone();
    let real_ws = ws.canonicalize().unwrap();
    let alias = ws.with_file_name(ws.file_name().unwrap().to_string_lossy().to_uppercase());
    if alias != ws && std::fs::canonicalize(&alias).is_ok() {
        let lifted = x.normalise_call(
            "shell_exec",
            &json!({"command": format!("cd {} && pwd", alias.display())}),
        );
        assert_eq!(lifted.unwrap()["cwd"], json!(real_ws.to_string_lossy()));
        let result = x
            .execute("shell_exec", &json!({"command": "pwd", "cwd": alias}))
            .await
            .unwrap();
        assert!(result.text.contains(&real_ws.to_string_lossy().to_string()));
    }
    let line = format!("cd {} && grep -rn foo src", ws.display());
    let lifted = x
        .normalise_call("shell_exec", &json!({"command": line, "network": false}))
        .expect("relevé");
    assert_eq!(
        lifted,
        json!({"command": "grep -rn foo src", "cwd": real_ws.to_string_lossy(), "network": false})
    );
    let sub = x
        .normalise_call("shell_exec", &json!({"command": "cd app && ls"}))
        .expect("chemin relatif au workspace");
    assert_eq!(sub["cwd"], json!(real_ws.join("app").to_string_lossy()));

    let via = x
        .normalise_call(
            "tool_call",
            &json!({"name": "shell_exec", "args_json": json!({"command": line}).to_string()}),
        )
        .expect("par tool_call");
    assert_eq!(via["args"]["command"], "grep -rn foo src");
    assert!(via.get("args_json").is_none(), "{via}");
    assert_eq!(
        effective_arguments("tool_call", &via)["cwd"],
        json!(real_ws.to_string_lossy())
    );

    for kept in [
        json!({"command": "cd /ailleurs && grep foo"}),
        json!({"command": line, "cwd": ws.join("autre").to_string_lossy()}),
        json!({"command": "grep foo"}),
    ] {
        assert_eq!(x.normalise_call("shell_exec", &kept), None, "{kept}");
    }
    assert_eq!(x.normalise_call("fs_read", &json!({"path": line})), None);
}

/// Issue #8 : une page HTML arrive en texte lisible, le brut reste en artefact ; une
/// longue liste de fichiers part en artefact avec un résumé.
#[tokio::test]
async fn web_pages_and_long_listings_stay_small_in_context() {
    let (dir, x) = executor().await;
    let page = format!(
        "<!doctype html><html><head><title>Guide</title><script>{}</script></head>\
             <body><h1>Jetons</h1><p>Un jeton par <a href=\"/cles\">clé</a>.</p></body></html>",
        "var x = 1;".repeat(2_000)
    );
    // Réponse telle que `penelope_tools::http::fetch` la rend (le réseau local est
    // refusé par l'outil lui-même).
    let fetched = x
        .fetched_page(
            json!({
                "url": "https://docs.exemple.fr/guide",
                "status": 200,
                "contentType": "text/html; charset=utf-8",
                "bytes": page.len(),
                "truncated": false,
                "body": page,
            }),
            "https://docs.exemple.fr/guide",
        )
        .await
        .unwrap();
    assert!(fetched.text.contains("# Jetons"), "{}", fetched.text);
    assert!(
        fetched.text.contains("[clé](https://docs.exemple.fr/cles)"),
        "{}",
        fetched.text
    );
    assert!(fetched.text.len() < 1_000, "{}", fetched.text.len());
    assert!(!fetched.text.contains("var x"), "{}", fetched.text);
    let raw_id = fetched.value["raw_artifact"].as_str().unwrap();
    let (raw, _, _) = x
        .services
        .context
        .history
        .read_artifact(raw_id, 0, 100_000)
        .await
        .unwrap()
        .unwrap();
    assert!(raw.contains("var x = 1;"));

    let ws = dir.path().join("ws");
    for d in 0..12 {
        let sub = ws.join(format!("module_{d:02}"));
        std::fs::create_dir_all(&sub).unwrap();
        for f in 0..60 {
            std::fs::write(sub.join(format!("fichier_numero_{f:03}.rs")), "x").unwrap();
        }
    }
    let listed = x
        .execute(
            "fs_list",
            &json!({"path": ".", "recursive": true, "max_entries": 2000}),
        )
        .await
        .unwrap();
    assert_eq!(listed.value["entries"], 732);
    assert!(listed.text.chars().count() < 6_000, "{}", listed.text.len());
    assert!(listed.text.contains("- module_00/ 61"), "{}", listed.text);
    assert!(listed.text.contains("artifact_read(\""), "{}", listed.text);

    let small = x
        .execute("fs_list", &json!({"path": "module_03"}))
        .await
        .unwrap();
    assert!(
        small.text.contains("fichier_numero_007.rs  1 o"),
        "{}",
        small.text
    );
    assert!(!small.text.contains("artifact_read"));
}

/// Issue #1 : une conversation d'une session fermée se retrouve depuis une autre
/// session, par mots-clés en `all` comme par une question en phrase.
#[tokio::test]
async fn past_sessions_are_found_with_their_title_and_date() {
    let (_d, x) = executor().await;
    let s = x.services.clone();
    let old = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Refonte du site Zéphyr".into()),
        )
        .await
        .unwrap();
    for text in [
        "Pour le projet Zéphyr, on garde la maquette verte.",
        "Le client Zéphyr veut une livraison en octobre.",
    ] {
        s.context
            .history
            .append(
                old.id.as_str(),
                &penelope_llm::types::ChatMessage::user(text),
                12,
                0,
                false,
                None,
            )
            .await
            .unwrap();
    }
    s.sessions
        .set_state(old.id.as_str(), "closed")
        .await
        .unwrap();

    let here = x
        .execute("history_grep", &json!({"query": "Zéphyr"}))
        .await
        .unwrap();
    assert!(
        here.value["note"].as_str().unwrap().contains("all"),
        "{}",
        here.value
    );

    let all = x
        .execute(
            "history_grep",
            &json!({"query": "Zéphyr maquette", "scope": "all"}),
        )
        .await
        .unwrap();
    let hits = all.value.as_array().unwrap();
    assert_eq!(hits.len(), 1, "{}", all.value);
    assert_eq!(hits[0]["session_titre"], "Refonte du site Zéphyr");
    assert_eq!(hits[0]["session_date"].as_str().unwrap().len(), 10);

    let asked = x
        .execute(
            "history_expand_query",
            &json!({"question": "On avait parlé d'un projet Zéphyr dans une session précédente ?"}),
        )
        .await
        .unwrap();
    assert_eq!(asked.value["mots"], json!(["projet", "zéphyr"]));
    let extraits = asked.value["extraits"].as_array().unwrap();
    assert_eq!(extraits.len(), 2, "{}", asked.value);
    assert_eq!(
        extraits[0]["matched"].as_array().unwrap().len(),
        2,
        "le passage qui a les deux mots d'abord"
    );
    assert_eq!(extraits[0]["session_titre"], "Refonte du site Zéphyr");
}

#[tokio::test]
async fn a_dated_intent_is_redirected_to_a_schedule() {
    let (_d, x) = executor().await;
    let e = x
        .execute(
            "intent_create",
            &json!({"texte": "rappelle-moi vendredi d'appeler Paul"}),
        )
        .await
        .unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("schedule_create") && msg.contains("once"),
        "{msg}"
    );
    let ok = x
        .execute(
            "intent_create",
            &json!({"texte": "quand on reparle du déploiement, rappelle-moi le changelog"}),
        )
        .await
        .unwrap();
    assert!(ok.value["id"].as_str().unwrap().starts_with("i_"));
}

#[tokio::test]
async fn files_are_written_edited_and_read_inside_the_workspace() {
    let (_d, x) = executor().await;
    x.execute(
        "fs_write",
        &json!({"path":"notes/a.txt","content":"bonjour\nmonde\n"}),
    )
    .await
    .unwrap();
    x.execute(
        "fs_edit",
        &json!({"path":"notes/a.txt","old":"monde","new":"Pénélope"}),
    )
    .await
    .unwrap();
    let r = x
        .execute("fs_read", &json!({"path":"notes/a.txt"}))
        .await
        .unwrap();
    assert!(r.text.contains("Pénélope"), "{}", r.text);
    assert!(r.eager, "une lecture est un résultat volatil");

    let e = x
        .execute("fs_read", &json!({"path":"/etc/passwd"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::Denied(_)), "{e}");
}

#[tokio::test]
async fn invalid_arguments_are_rejected_before_running() {
    let (_d, x) = executor().await;
    let e = x
        .execute("fs_write", &json!({"path": "a.txt"}))
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::BadArguments { .. }), "{e}");
}

/// #106 : une commande qui demande le réseau est une action externe, approuvée comme
/// telle ; réseau déjà ouvert par la configuration, elle reste une écriture.
#[tokio::test]
async fn a_command_asking_for_network_is_an_external_action() {
    let (_dir, x) = executor().await;
    let plain = x
        .describe_call("shell_exec", &json!({"command": "git push"}))
        .await;
    assert_eq!(plain.risk, RiskClass::Write);
    let net = json!({"command": "git push", "network": true});
    assert_eq!(
        x.describe_call("shell_exec", &net).await.risk,
        RiskClass::External
    );
    assert_eq!(
        x.describe_call("tool_call", &json!({"name": "shell_exec", "args": net}))
            .await
            .risk,
        RiskClass::External
    );
    x.services
        .config
        .mutate("test", |c| {
            c.sandbox.shell_network = true;
            Ok(vec!["sandbox.shell_network".into()])
        })
        .unwrap();
    assert_eq!(
        x.describe_call("shell_exec", &net).await.risk,
        RiskClass::Write
    );
}

/// #106 : sous Seatbelt, une commande sans réseau ne joint même pas une adresse locale,
/// et l'échec le dit ; la même commande avec `network: true` passe.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn shell_network_is_granted_per_call_under_the_sandbox() {
    let (_dir, x) = executor().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let command = format!("nc -z 127.0.0.1 {port}");
    let closed = x
        .execute("shell_exec", &json!({"command": command}))
        .await
        .unwrap();
    assert_ne!(closed.value["exitCode"], 0, "{}", closed.text);
    assert!(closed.text.contains("Réseau coupé"), "{}", closed.text);
    assert_eq!(closed.value["network"], false);
    let open = x
        .execute("shell_exec", &json!({"command": command, "network": true}))
        .await
        .unwrap();
    assert_eq!(open.value["exitCode"], 0, "{}", open.text);
}

/// #104 : la liste d'un tour de conversation tient sous 20 définitions, méta-outils
/// compris ; un outil découvert s'y ajoute sans doublon.
#[test]
fn chat_tool_definitions_are_the_core_plus_what_the_session_found() {
    let base = chat_tool_defs(&[]);
    assert!(base.len() <= 20, "{} définitions", base.len());
    assert!(base.iter().any(|t| t.name == "tool_search"));
    assert!(!base.iter().any(|t| t.name == "schedule_create"));
    let more = chat_tool_defs(&["schedule_create".into(), "shell_exec".into()]);
    assert_eq!(more.len(), base.len() + 1);
    assert!(more.iter().any(|t| t.name == "schedule_create"));
}

#[test]
fn tool_definitions_hide_workflow_only_tools_in_chat() {
    let chat = tool_defs(false, false);
    assert!(chat.iter().any(|t| t.name == "fs_read"));
    assert!(!chat.iter().any(|t| t.name == "step_done"));
    assert!(!chat.iter().any(|t| t.name == "tool_search"));
    let wf = tool_defs(true, true);
    assert!(wf.iter().any(|t| t.name == "step_done"));
    assert!(wf.iter().any(|t| t.name == "tool_call"));
}

#[test]
fn mcp_results_render_their_text_blocks() {
    let v = json!({"content":[{"type":"text","text":"ligne 1"},{"type":"text","text":"ligne 2"}]});
    assert_eq!(render_mcp_result(&v), "ligne 1\nligne 2\n");
    let v = json!({"content":[], "structuredContent":{"total": 3}});
    assert!(render_mcp_result(&v).contains("\"total\": 3"));
}
