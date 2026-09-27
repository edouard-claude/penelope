use super::*;

/// Passerelle MCP de test : compte les appels et garde les arguments reçus.
#[derive(Default)]
struct RecordingGateway {
    calls: std::sync::Mutex<Vec<(String, Value)>>,
}

#[async_trait::async_trait]
impl McpGateway for RecordingGateway {
    async fn call_tool(
        &self,
        qualified: &str,
        args: &Value,
        _from: penelope_app::elicitation::Destination,
    ) -> Result<Value, String> {
        self.calls
            .lock()
            .unwrap()
            .push((qualified.to_string(), args.clone()));
        Ok(json!({"content": [{"type": "text", "text": "ticket 7653 : ouvert"}]}))
    }
    async fn server_lines(&self) -> Vec<String> {
        Vec::new()
    }
}

async fn with_redmine(x: &mut NativeToolExecutor) -> Arc<RecordingGateway> {
    let mut props = serde_json::Map::new();
    props.insert(
        "issue_id".into(),
        json!({"type": "integer", "description": "Identifiant numérique du ticket, ex. 7653"}),
    );
    for i in 0..19 {
        props.insert(
            format!("option_{i:02}"),
            json!({"type": "string", "description": "une option facultative au texte long ".repeat(8)}),
        );
    }
    let d = penelope_mcp::protocol::ToolDescriptor::parse(&json!({
        "name": "get_issue", "description": "Lit un ticket",
        "inputSchema": {"type": "object", "properties": props, "required": ["issue_id"]},
        "annotations": {"readOnlyHint": true}
    }))
    .unwrap();
    x.services
        .mcp_tools
        .replace_server_tools(
            "redmine",
            vec![penelope_mcp::registry::RegisteredTool::from_descriptor(
                "redmine", &d,
            )],
            "2026-09-18T10:00:00Z",
        )
        .await
        .unwrap();
    let gw = Arc::new(RecordingGateway::default());
    x.mcp = Some(gw.clone());
    gw
}

/// #110 : un outil natif sans argument requis rend le nom, le type et la description
/// du paramètre attendu.
#[tokio::test]
async fn a_missing_native_argument_comes_back_with_the_expected_parameters() {
    let (_d, x) = executor().await;
    let e = x.execute("config_set", &json!({})).await.unwrap_err();
    let text = e.for_model();
    assert!(text.contains("`path` (string, requis)"), "{text}");
    assert!(text.contains("Chemin pointé"), "{text}");
    assert!(text.contains("`value`"), "{text}");
}

/// #110 : un appel MCP invalide est refusé chez nous avec le schéma du serveur, sans
/// aller au serveur, et borné même avec vingt propriétés ; `tool_call` arrivé vide le
/// dit comme tel, et `args_json` fait passer la valeur jusqu'au serveur.
#[tokio::test]
async fn an_invalid_mcp_call_is_explained_without_reaching_the_server() {
    let (_d, mut x) = executor().await;
    let gw = with_redmine(&mut x).await;

    let e = x
        .execute("mcp__redmine__get_issue", &json!({}))
        .await
        .unwrap_err();
    let text = e.for_model();
    assert!(text.contains("issue_id"), "{text}");
    assert!(
        text.contains("`issue_id` (integer, requis) : Identifiant numérique"),
        "{text}"
    );
    assert!(
        text.chars().count() < 2_000,
        "{} caractères",
        text.chars().count()
    );
    assert!(text.contains("autre(s) paramètre(s)"), "{text}");
    assert!(!text.contains("perdus en route"), "appel direct : {text}");
    assert!(
        gw.calls.lock().unwrap().is_empty(),
        "rien n'est parti au serveur"
    );

    let lost = x
        .execute(
            "tool_call",
            &json!({"name": "mcp__redmine__get_issue", "args": {}}),
        )
        .await
        .unwrap_err()
        .for_model();
    assert!(
        lost.contains("perdus en route") && lost.contains("args_json"),
        "{lost}"
    );
    assert!(gw.calls.lock().unwrap().is_empty());

    x.execute(
        "tool_call",
        &json!({"name": "mcp__redmine__get_issue", "args": {}, "args_json": "{\"issue_id\": 7653}"}),
    )
    .await
    .unwrap();
    x.execute(
        "tool_call",
        &json!({"name": "mcp__redmine__get_issue", "args": {"issue_id": 7654}}),
    )
    .await
    .unwrap();
    let calls = gw.calls.lock().unwrap().clone();
    assert_eq!(calls[0].1["issue_id"], 7653, "{calls:?}");
    assert_eq!(calls[1].1["issue_id"], 7654, "{calls:?}");
}

/// #110 : `tool_call` déclare des arguments libres et un repli en chaîne ; un nom
/// inconnu rend les noms proches.
#[tokio::test]
async fn tool_call_accepts_any_arguments_and_unknown_names_get_suggestions() {
    let (_d, mut x) = executor().await;
    with_redmine(&mut x).await;
    let (_, _, schema) = penelope_mcp::registry::ToolRegistry::meta_tools()
        .into_iter()
        .find(|(n, _, _)| *n == "tool_call")
        .unwrap();
    assert_eq!(schema["properties"]["args"]["additionalProperties"], true);
    assert_eq!(schema["properties"]["args_json"]["type"], "string");
    assert_eq!(schema["required"], json!(["name"]));

    let text = x
        .execute("mcp__redmine__getissue", &json!({}))
        .await
        .unwrap_err()
        .for_model();
    assert!(text.contains("`mcp__redmine__get_issue`"), "{text}");
    let text = x
        .execute("tool_call", &json!({"name": "schedule_add", "args": {}}))
        .await
        .unwrap_err()
        .for_model();
    assert!(text.contains("`schedule_create`"), "{text}");
}

#[tokio::test]
async fn memory_tools_write_through_the_vault() {
    let (_d, x) = executor().await;
    let r = x
        .execute(
            "mem_remember",
            &json!({"niveau":"profil","texte":"Préférer le tutoiement"}),
        )
        .await
        .unwrap();
    let uid = r.value["uid"].as_str().unwrap().to_string();
    let found = x
        .execute("mem_search", &json!({"query":"tutoiement"}))
        .await
        .unwrap();
    assert!(found.text.contains(&uid), "{}", found.text);
    let gone = x.execute("mem_forget", &json!({"uid": uid})).await.unwrap();
    assert_eq!(gone.value["forgotten"], true);
}

#[tokio::test]
async fn workflow_only_tools_are_refused_in_chat() {
    let (_d, x) = executor().await;
    let e = x.execute("step_done", &json!({})).await.unwrap_err();
    assert!(matches!(e, ToolError::Denied(_)), "{e}");
}

#[tokio::test]
async fn mcp_meta_tools_are_read_only_but_calls_carry_the_target_risk() {
    let (_d, x) = executor().await;
    let d = penelope_mcp::protocol::ToolDescriptor::parse(&json!({
        "name": "delete_repo", "description": "Supprime un dépôt",
        "inputSchema": {"type":"object"},
        "annotations": {"destructiveHint": true, "readOnlyHint": false}
    }))
    .unwrap();
    x.services
        .mcp_tools
        .replace_server_tools(
            "forge",
            vec![penelope_mcp::registry::RegisteredTool::from_descriptor(
                "forge", &d,
            )],
            "2026-09-16T10:00:00Z",
        )
        .await
        .unwrap();

    let search = x.describe_call("tool_search", &json!({"query":"x"})).await;
    assert_eq!(search.risk, RiskClass::Read);

    let call = x
        .describe_call(
            "tool_call",
            &json!({"name":"mcp__forge__delete_repo","args":{}}),
        )
        .await;
    assert_eq!(call.effective_name, "mcp__forge__delete_repo");
    assert_eq!(call.risk, RiskClass::Destructive);

    // Sans passerelle MCP, l'appel échoue proprement.
    let e = x
        .execute(
            "tool_call",
            &json!({"name":"mcp__forge__delete_repo","args":{}}),
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("MCP"), "{e}");
}

/// #104 : « planifier un rappel » trouve `schedule_create` sans serveur MCP ; par
/// `tool_call`, un outil natif garde le nom, la classe de risque et la politique d'un
/// appel direct ; décrit, il rejoint la liste de la session.
#[tokio::test]
async fn a_rare_native_tool_is_found_and_called_like_a_direct_one() {
    let (_dir, x) = executor().await;
    let found = x
        .execute("tool_search", &json!({"query": "planifier un rappel"}))
        .await
        .unwrap();
    assert_eq!(found.value[0]["name"], "schedule_create", "{}", found.text);
    assert_eq!(found.value[0]["server"], "natif");

    for (name, args) in [
        (
            "schedule_create",
            json!({"when": "demain 9h", "prompt": "appeler"}),
        ),
        (
            "config_set",
            json!({"path": "telegram.owner_ids", "value": "1"}),
        ),
        (
            "config_set",
            json!({"path": "budget.daily_eur", "value": "5"}),
        ),
        ("git_push", json!({"remote": "origin"})),
        ("fs_read", json!({"path": "a.txt"})),
    ] {
        let direct = x.describe_call(name, &args).await;
        let via = x
            .describe_call("tool_call", &json!({"name": name, "args": args}))
            .await;
        assert_eq!(direct, via, "{name}");
    }
    let sensitive = x
        .describe_call(
            "tool_call",
            &json!({"name": "config_set", "args": {"path": "sandbox.default_profile"}}),
        )
        .await;
    assert_eq!(sensitive.risk, RiskClass::Destructive);

    assert!(
        crate::tools_on_demand::exposed_for_turn(&x.services, "s1")
            .await
            .is_empty()
    );
    let described = x
        .execute("tool_describe", &json!({"names": ["schedule_create"]}))
        .await
        .unwrap();
    assert!(
        described.value[0]["inputSchema"].is_object(),
        "{}",
        described.text
    );
    assert_eq!(
        crate::tools_on_demand::exposed_for_turn(&x.services, "s1").await,
        vec!["schedule_create".to_string()]
    );
}
