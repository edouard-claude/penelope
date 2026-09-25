use super::*;
use penelope_kernel::clock::TestClock;

async fn rpc() -> (tempfile::TempDir, Rpc) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    (dir, Rpc::new(Arc::new(Daemon::from_services(s))))
}

async fn call(rpc: &Rpc, method: &str, params: Value) -> RpcResponse {
    rpc.handle(RpcRequest::new(1, method, params)).await
}

/// #54 : un modèle qui n'appelle pas d'outils est refusé pour un alias de
/// conversation, et accepté pour un rôle de service. `doctor` le signale ensuite.
#[tokio::test]
async fn a_model_without_tool_calling_is_refused_for_a_conversation_alias() {
    let (_d, r) = rpc().await;
    let s = &r.daemon.services;
    let mut sans = penelope_llm::catalog::ModelInfo::minimal("vieux/modele", "openrouter", 8192);
    sans.supported_parameters.clear();
    s.catalog.upsert(vec![
        sans,
        penelope_llm::catalog::ModelInfo::minimal("bon/modele", "openrouter", 128_000),
    ]);

    let refus = call(
        &r,
        method::MODEL_SET,
        json!({"alias": "main", "model": "openrouter:vieux/modele"}),
    )
    .await;
    let message = refus.error.expect("refus attendu").message;
    assert!(message.contains("n'appelle pas d'outils"), "{message}");

    // Un rôle de service n'appelle pas d'outils : le même modèle y est bienvenu.
    let ok = call(
        &r,
        method::MODEL_SET,
        json!({"alias": "stt", "model": "openrouter:vieux/modele"}),
    )
    .await;
    assert!(ok.error.is_none(), "{:?}", ok.error);

    // Configuration déjà en place : `doctor` le dit.
    r.daemon
        .publish_config("test", |c| {
            c.models
                .aliases
                .insert("main".into(), "openrouter:vieux/modele".into());
            Ok(vec!["models.aliases.main".into()])
        })
        .unwrap();
    let checks = penelope_ops::doctor::run(s).await;
    let tools = checks
        .iter()
        .find(|c| c.id == "models.tools")
        .expect("contrôle des outils");
    assert!(!tools.ok, "{tools:?}");
}

/// #142 : l'abonnement ChatGPT ne sert que les tours du propriétaire — un alias de
/// rôle de fond ne peut pas le viser, et un préfixe mal écrit est refusé au lieu de
/// partir en silence chez OpenRouter.
#[tokio::test]
async fn a_background_alias_cannot_aim_at_the_subscription() {
    let (_d, r) = rpc().await;
    for alias in ["stt", "embedding", "tts", "summarizer"] {
        let refus = call(
            &r,
            method::MODEL_SET,
            json!({"alias": alias, "model": "codex:gpt-6-astra"}),
        )
        .await;
        let message = refus
            .error
            .unwrap_or_else(|| panic!("{alias} : refus attendu"))
            .message;
        assert!(
            message.contains("abonnement ChatGPT"),
            "{alias} : {message}"
        );
    }
    // La conversation, elle, a le droit : c'est le propriétaire qui parle.
    let ok = call(
        &r,
        method::MODEL_SET,
        json!({"alias": "main", "model": "codex:gpt-6-astra"}),
    )
    .await;
    assert!(ok.error.is_none(), "{:?}", ok.error);

    // Un préfixe inconnu est une faute de frappe, pas un modèle OpenRouter.
    let refus = call(
        &r,
        method::MODEL_SET,
        json!({"alias": "main", "model": "codx:gpt-6"}),
    )
    .await;
    assert!(
        refus.error.expect("refus").message.contains("codx"),
        "le préfixe est nommé"
    );
}

/// #142 (lot 3) : `doctor` dit l'état du fournisseur Codex, signale un alias de rôle
/// de fond qui l'aurait contourné, et ne se tait jamais sur l'identité empruntée.
#[tokio::test]
async fn doctor_reports_the_codex_provider() {
    let (_d, r) = rpc().await;
    let s = &r.daemon.services;
    // Éteint et sans alias : rien à dire.
    assert!(penelope_ops::doctor::codex_checks(s).await.is_empty());

    r.daemon
        .publish_config("test", |c| {
            c.providers.codex.enabled = true;
            c.models
                .aliases
                .insert("summarizer".into(), "codex:gpt-6-astra".into());
            Ok(vec!["providers.codex.enabled".into()])
        })
        .unwrap();
    let checks = penelope_ops::doctor::codex_checks(s).await;
    let by = |id: &str| {
        checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("contrôle `{id}` attendu"))
            .clone()
    };
    let provider = by("provider.codex");
    assert!(!provider.ok, "activé sans compte connecté");
    assert!(provider.detail.contains("aucun compte"), "{provider:?}");

    let identity = by("provider.codex.identity");
    assert!(identity.detail.contains("codex_cli_rs"), "{identity:?}");
    assert!(
        identity.detail.contains("toléré") && identity.detail.contains("jamais garanti"),
        "l'avertissement ne se tait pas : {identity:?}"
    );

    let scope = by("provider.codex.scope");
    assert!(!scope.ok);
    assert!(scope.detail.contains("compaction"), "{scope:?}");

    // Connecté : l'état le dit, et le périmètre reste signalé.
    penelope_ops::codex_auth::store(
        s,
        &penelope_ops::codex_auth::Grant {
            access_token: "a".into(),
            refresh_token: "rr".into(),
            plan_type: "pro".into(),
            email: "moi@example.test".into(),
            expires_at: s.clock.now_ms() + 3_600_000,
            last_refresh: s.clock.now_ms(),
            ..Default::default()
        },
    )
    .unwrap();
    let checks = penelope_ops::doctor::codex_checks(s).await;
    let provider = checks
        .iter()
        .find(|c| c.id == "provider.codex")
        .expect("contrôle");
    assert!(provider.ok, "{provider:?}");
    assert!(provider.detail.contains("plan pro"), "{provider:?}");
}

/// #142 : sans compte connecté, `model auth codex --status` le dit ; connecté, une
/// seconde connexion exige une déconnexion explicite.
#[tokio::test]
async fn only_one_chatgpt_account_at_a_time() {
    let (_d, r) = rpc().await;
    let empty = call(
        &r,
        method::MODEL_AUTH,
        json!({"provider": "codex", "action": "status"}),
    )
    .await;
    assert!(empty.error.is_none());
    assert!(empty.result.expect("état")["status"].is_null());

    penelope_ops::codex_auth::store(
        &r.daemon.services,
        &penelope_ops::codex_auth::Grant {
            access_token: "a".into(),
            refresh_token: "r".into(),
            account_id: "acc_1".into(),
            plan_type: "pro".into(),
            email: "moi@example.test".into(),
            expires_at: r.daemon.services.clock.now_ms() + 3_600_000,
            last_refresh: r.daemon.services.clock.now_ms(),
            ..Default::default()
        },
    )
    .unwrap();
    let busy = call(
        &r,
        method::MODEL_AUTH,
        json!({"provider": "codex", "action": "start"}),
    )
    .await;
    let message = busy.error.expect("refus").message;
    assert!(
        message.contains("moi@example.test") && message.contains("logout"),
        "{message}"
    );

    // Un autre fournisseur ne se connecte pas par compte.
    let other = call(
        &r,
        method::MODEL_AUTH,
        json!({"provider": "openrouter", "action": "status"}),
    )
    .await;
    assert!(other.error.expect("refus").message.contains("seul `codex`"));
}

#[tokio::test]
async fn mcp_servers_are_administered_over_rpc() {
    use penelope_mcp_host::testing::{FakeConnector, server, tool};
    let (_d, r) = rpc().await;
    let without = call(&r, method::MCP_LIST, json!({})).await;
    assert!(without.error.unwrap().message.contains("non démarré"));

    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "forge",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "create_pr",
            json!({}),
        )]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(r.daemon.services.clone(), fake.clone());
    r.daemon.hooks.set_mcp(sup.clone());

    let toml = "command = \"/opt/mcp/forge\"\ntimeout = \"20s\"\n";
    let tested = call(&r, method::MCP_TEST, json!({"toml": toml, "name": "forge"})).await;
    let tested = tested.result.unwrap();
    assert_eq!(tested["ok"], true, "{tested}");
    assert_eq!(tested["tools"], 1);
    assert!(sup.statuses().await.is_empty(), "un essai n'ajoute rien");

    let added = call(&r, method::MCP_ADD, json!({"toml": toml, "name": "forge"})).await;
    let added = added.result.unwrap();
    assert_eq!(added["report"]["added"][0], "forge");
    assert_eq!(added["status"]["tool_count"], 1);

    let list = call(&r, method::MCP_LIST, json!({})).await.result.unwrap();
    assert_eq!(list["servers"][0]["name"], "forge");
    assert_eq!(list["servers"][0]["state"], "ready");

    let edited = call(
        &r,
        method::MCP_EDIT,
        json!({"name": "forge", "patch": {"timeout": "45s"}}),
    )
    .await;
    assert!(edited.error.is_none(), "{:?}", edited.error);
    assert_eq!(sup.config_of("forge").await.unwrap().timeout, "45s");

    let show = call(&r, method::MCP_SHOW, json!({"name": "forge"}))
        .await
        .result
        .unwrap();
    assert_eq!(show["tools"][0]["name"], "mcp__forge__create_pr");

    let status = call(&r, method::STATUS, json!({})).await.result.unwrap();
    assert_eq!(
        (status["mcp_ready"].clone(), status["mcp_total"].clone()),
        (json!(1), json!(1))
    );

    let rm = call(&r, method::MCP_RM, json!({"name": "forge"})).await;
    assert_eq!(rm.result.unwrap()["report"]["removed"][0], "forge");
    let missing = call(&r, method::MCP_RESTART, json!({"name": "forge"})).await;
    assert!(missing.error.unwrap().message.contains("inconnu"));
}

#[tokio::test]
async fn status_and_paths() {
    let (_d, r) = rpc().await;
    let resp = call(&r, method::STATUS, json!({})).await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["config_generation"], 1);

    let resp = call(&r, method::PATHS, json!({})).await;
    let v = resp.result.unwrap();
    assert!(v["data"].as_str().unwrap().ends_with("/data"));
    assert!(v["socket"].as_str().unwrap().ends_with("rpc.sock"));
}

#[tokio::test]
async fn unknown_method_returns_32601() {
    let (_d, r) = rpc().await;
    let resp = call(&r, "methode.inventee", json!({})).await;
    assert_eq!(resp.error.unwrap().code, METHOD_NOT_FOUND);
}

#[tokio::test]
async fn missing_parameter_returns_32602() {
    let (_d, r) = rpc().await;
    let resp = call(&r, method::MEM_SEARCH, json!({})).await;
    assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
}

#[tokio::test]
async fn sessions_can_be_created_and_listed() {
    let (_d, r) = rpc().await;
    call(&r, method::SESSION_NEW, json!({"title":"refonte"})).await;
    let resp = call(&r, method::SESSION_LIST, json!({})).await;
    let list = resp.result.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["title"], "refonte");
}

#[tokio::test]
async fn config_get_set_and_status() {
    let (_d, r) = rpc().await;
    let before = call(&r, method::CONFIG_GET, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(before["budget"]["daily_usd"], 20.0);

    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path":"budget.daily_usd","value":50.0}),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(resp.result.unwrap()["generation"], 2);

    let after = call(&r, method::CONFIG_GET, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(after["budget"]["daily_usd"], 50.0);

    let st = call(&r, method::CONFIG_STATUS, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(st["generation"], 2);
    assert!(st["subsystems"]["mcp"].is_object());
}

/// Issue #16 : un réglage qui annule sa propre intention est refusé nommément ; celui
/// qui en rend un autre inutile passe avec un avertissement.
#[tokio::test]
async fn self_cancelling_settings_are_refused_or_warned() {
    let (_d, r) = rpc().await;
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "tools.http_allowlist", "value": ["http://192.168.0.10:8080"]}),
    )
    .await;
    let err = resp.error.expect("refus").message;
    assert!(
        err.contains("réglage refusé") && err.contains("192.168.0.10"),
        "{err}"
    );
    let cfg = call(&r, method::CONFIG_GET, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(
        cfg["tools"]["http_allowlist"],
        json!([]),
        "rien n'est écrit"
    );

    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "budget.session_usd", "value": 50.0}),
    )
    .await;
    let v = resp.result.expect("accepté");
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .contains("ne sera jamais atteint"),
        "{v}"
    );

    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "models.roles.classifier", "value": "absent"}),
    )
    .await;
    assert!(resp.error.unwrap().message.contains("n'existe pas"));
}

#[tokio::test]
async fn unknown_config_path_is_refused() {
    let (_d, r) = rpc().await;
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path":"budget.inexistant","value":1}),
    )
    .await;
    let err = resp.error.expect("refus").message;
    assert!(err.contains("clé inconnue : budget.inexistant"), "{err}");
    // La lecture du fichier tolère les clés inconnues (#76), la saisie non.
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path":"futur.actif","value":true}),
    )
    .await;
    let err = resp.error.expect("refus").message;
    assert!(err.contains("clé inconnue : futur.actif"), "{err}");
}

/// #128 : une entrée nouvelle d'une table à clés libres se pose (`image_locate` absent
/// d'une configuration écrite avant #125), vérifiée comme les autres ; une clé de
/// structure inconnue reste refusée.
#[tokio::test]
async fn a_new_role_can_be_set_on_an_older_configuration() {
    let (_dir, r) = rpc().await;
    let d = r.daemon.clone();
    d.publish_config("test", |c| {
        c.models.roles.remove("image_locate");
        c.models.aliases.insert(
            "pointage".into(),
            "openrouter:bytedance/ui-tars-1.5-7b".into(),
        );
        Ok(vec!["models.roles".into()])
    })
    .unwrap();
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "models.roles.image_locate", "value": "pointage"}),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(
        d.services
            .config
            .config()
            .models
            .roles
            .get("image_locate")
            .map(String::as_str),
        Some("pointage")
    );
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "models.roles.image_describe", "value": "inconnu"}),
    )
    .await;
    assert!(resp.error.is_some(), "un alias inconnu reste refusé");
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "models.inexistant", "value": 1}),
    )
    .await;
    assert!(resp.error.unwrap().message.contains("clé inconnue"));
}

/// #138 : `config set` suit la même règle que `mcp edit` : une valeur seule remplit
/// une liste.
#[tokio::test]
async fn config_set_takes_a_single_value_for_a_list() {
    let (_dir, r) = rpc().await;
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "sandbox.allow_keychain_for", "value": "mailbridge"}),
    )
    .await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    assert_eq!(
        r.daemon.services.config.config().sandbox.allow_keychain_for,
        vec!["mailbridge".to_string()]
    );
    let resp = call(
        &r,
        method::CONFIG_SET,
        json!({"path": "sandbox.allow_keychain_for", "value": 3}),
    )
    .await;
    assert!(resp.error.unwrap().message.contains("une liste"));
}

#[tokio::test]
async fn workflow_validation_reports_paths() {
    let (_d, r) = rpc().await;
    let bad = json!({
        "metadata": {"id":"demo"},
        "entryStep": "absente",
        "steps": [{"id":"un","type":"shell","command":"echo x",
                   "transitions":[{"goto":"$done"}]}]
    });
    let resp = call(
        &r,
        method::WF_VALIDATE,
        json!({"json": bad.to_string(), "name":"demo"}),
    )
    .await;
    let v = resp.result.unwrap();
    assert_eq!(v["valid"], false);
    assert!(
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["path"] == "/entryStep")
    );
}

#[tokio::test]
async fn bundled_workflows_are_listed() {
    let (_d, r) = rpc().await;
    let v = call(&r, method::WF_LIST, json!({})).await.result.unwrap();
    let ids: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["id"].as_str())
        .collect();
    assert!(ids.contains(&"ticket-to-deploy"));
    assert!(ids.contains(&"deploy-generic"));
}

#[tokio::test]
async fn audit_verify_is_reachable() {
    let (_d, r) = rpc().await;
    let v = call(&r, method::AUDIT_VERIFY, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn history_verify_and_reindex_are_reachable() {
    let (_d, r) = rpc().await;
    let v = call(&r, method::HISTORY_VERIFY, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(v["ok"], true, "{v:#}");
    let v = call(&r, method::HISTORY_REINDEX, json!({}))
        .await
        .result
        .unwrap();
    assert_eq!(v["ok"], true, "{v:#}");
}

#[tokio::test]
async fn approvals_flow_over_rpc() {
    let (_d, r) = rpc().await;
    let a = r
        .services()
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({}),
            vec!["Autoriser".into()],
            None,
            None,
            false,
        )
        .await
        .unwrap();

    let list = call(&r, method::APPROVALS, json!({})).await.result.unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let resp = call(&r, method::APPROVE, json!({"id": a.id.0})).await;
    assert!(resp.error.is_none());
    // Une seconde décision est un conflit.
    let resp = call(&r, method::DENY, json!({"id": a.id.0})).await;
    assert_eq!(resp.error.unwrap().code, CONFLICT);
}

#[tokio::test]
async fn shutdown_and_restart_flags() {
    let (_d, r) = rpc().await;
    call(&r, method::RESTART, json!({})).await;
    assert!(r.daemon.handle.wants_restart());
    assert!(r.daemon.handle.is_shutting_down());
}

/// #103 : le registre de métriques a un lecteur, et un tour y laisse sa trace.
#[tokio::test]
async fn metrics_are_readable_over_rpc() {
    let (_d, r) = rpc().await;
    penelope_observe::metrics::register_default_metrics();
    penelope_observe::metrics::counter_inc("penelope_turns_total", &[("outcome", "answered")], 1.0);
    let text = call(&r, method::METRICS, json!({})).await.result.unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("penelope_turns_total"), "{text}");
    assert!(text.contains("penelope_approvals_pending"), "{text}");
}

/// #111 : le mode d'approbation d'une session se lit et se change ; les règles
/// inutiles portent leur remarque.
#[tokio::test]
async fn the_approval_mode_and_useless_rules_are_readable() {
    let (_dir, r) = rpc().await;
    let s = &r.daemon.services;
    let sid = r
        .daemon
        .chat_session_for(&penelope_app::bus::Origin::Cli)
        .await
        .unwrap();
    let v = call(&r, method::SESSION_MODE, json!({"session": sid}))
        .await
        .result
        .unwrap();
    assert_eq!(v["mode"], "reads");
    let v = call(
        &r,
        method::SESSION_MODE,
        json!({"session": sid, "mode": "ask"}),
    )
    .await
    .result
    .unwrap();
    assert_eq!(v["mode"], "ask");
    assert!(
        call(
            &r,
            method::SESSION_MODE,
            json!({"session": sid, "mode": "yolo"})
        )
        .await
        .error
        .is_some()
    );
    let v = call(
        &r,
        method::SESSION_MODE,
        json!({"session": sid, "mode": "default"}),
    )
    .await
    .result
    .unwrap();
    assert_eq!(v["mode"], "reads");

    for family in ["cd", "ls", "cargo test", "PASS=\"$(cut"] {
        s.policies
            .create_rule(
                penelope_hitl::RuleScope::Tool,
                Some("shell_exec"),
                None,
                Some(json!({"command": {penelope_hitl::policy::CMD_PREFIX_OP: family}})),
                penelope_kernel::risk::PolicyDecision::Auto,
                penelope_kernel::risk::PolicyWindow::Always,
                None,
            )
            .await
            .unwrap();
    }
    let rules = call(&r, method::POLICIES, json!({})).await.result.unwrap();
    let note = |family: &str| {
        rules
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["arg_match"]["command"][penelope_hitl::policy::CMD_PREFIX_OP] == family)
            .map(|x| x["remarque"].clone())
            .unwrap()
    };
    assert!(note("cd").as_str().unwrap().contains("composée"));
    assert!(note("ls").as_str().unwrap().contains("lectures"));
    assert!(note("PASS=\"$(cut").as_str().unwrap().contains("jamais"));
    assert!(note("cargo test").is_null(), "règle utile, sans remarque");
}

/// #105 : les signaux d'une entrée se lisent, avec le facteur qu'ils donnent.
#[tokio::test]
async fn memory_signals_are_readable() {
    let (_dir, r) = rpc().await;
    let s = &r.daemon.services;
    s.memory
        .upsert(
            &penelope_memory::index::simple_entry(
                "u1",
                "Le client Martin est basé à Lyon",
                penelope_memory::Level::Cure,
                "2026-09-16",
            ),
            &penelope_memory::Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z"),
        )
        .await
        .unwrap();
    for _ in 0..10 {
        s.memory
            .record_recall("u1", "où est Martin ?", true)
            .await
            .unwrap();
    }
    let v = call(&r, method::MEM_SIGNALS, json!({"uid": "u1"}))
        .await
        .result
        .unwrap();
    assert_eq!(v["rappels"], 10);
    assert_eq!(v["rappels_utiles"], 10);
    assert!(v["facteur_usage"].as_f64().unwrap() > 1.15, "{v}");
    let missing = call(&r, method::MEM_SIGNALS, json!({"uid": "u2"})).await;
    assert!(missing.error.is_some());
}

#[tokio::test]
async fn every_declared_method_is_either_served_or_explicitly_absent() {
    let (_d, r) = rpc().await;
    let mut unimplemented = Vec::new();
    for m in method::ALL {
        let resp = call(&r, m, json!({})).await;
        if let Some(e) = resp.error
            && e.code == METHOD_NOT_FOUND
        {
            unimplemented.push(*m);
        }
    }
    // Les méthodes non encore servies sont connues et listées : elles ne doivent pas
    // apparaître silencieusement.
    let expected: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let actual: std::collections::BTreeSet<&str> = unimplemented.into_iter().collect();
    assert_eq!(
        actual, expected,
        "la liste des méthodes non servies a changé : mettre à jour docs/progress.md"
    );
}
