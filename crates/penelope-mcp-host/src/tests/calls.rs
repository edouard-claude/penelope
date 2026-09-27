//! Appels d'outils hors du chemin nominal : saisies sans fin, liens exigés, refus
//! d'en-têtes, trousseau fermé, tâches longues.

use super::*;

/// Serveur 2026-07-28 : `tools/call` confié à `call`, `tasks/*` à `tasks`.
fn modern(
    call: impl Fn(&Value) -> penelope_mcp::Result<Value> + Send + Sync + 'static,
    tasks: impl Fn(&str, &Value) -> penelope_mcp::Result<Value> + Send + Sync + 'static,
) -> Handler {
    Arc::new(move |m, p| match m {
        "server/discover" => Ok(json!({
            "protocolVersion": "2026-07-28",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "moderne"}
        })),
        "tools/list" => Ok(json!({"tools": [tool("agir", json!({}))]})),
        "tools/call" => call(p),
        m if m.starts_with("tasks/") => tasks(m, p),
        _ => Ok(json!({})),
    })
}

fn no_tasks(_: &str, _: &Value) -> penelope_mcp::Result<Value> {
    Ok(json!({}))
}

fn rpc(code: i32, data: Option<Value>) -> McpError {
    McpError::Rpc {
        code,
        message: "refus".into(),
        data,
    }
}

/// MRTR : les racines demandées sont rendues, une méthode inconnue reste sans réponse,
/// et un serveur qui redemande sans fin est abandonné après quelques échanges.
#[tokio::test]
async fn endless_input_requests_are_abandoned_after_a_few_rounds() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve(
        "m",
        modern(
            |_| {
                Ok(json!({
                    "resultType": "input_required",
                    "inputRequests": {
                        "racines": {"method": "roots/list"},
                        "modele": {"method": "sampling/createMessage", "params": {}}
                    },
                    "requestState": "encore"
                }))
            },
            no_tasks,
        ),
    );
    declare(&sup, "m", "roots = [\"~/projets\"]\n");
    sup.reload().await;
    let v = sup
        .call("mcp__m__agir", &json!({}), Default::default())
        .await
        .unwrap();
    assert_eq!(v["isError"], true);
    let text = v["content"].to_string();
    assert!(
        text.contains("demande encore une saisie après 4 échanges"),
        "{text}"
    );
    let log = fake.last_transport("m").call_log().await;
    let calls: Vec<&Value> = log
        .iter()
        .filter(|(m, _)| m == "tools/call")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(calls.len(), 5, "l'appel et quatre relances");
    let responses = &calls[1]["inputResponses"];
    assert!(
        responses["racines"]["roots"][0]["uri"]
            .as_str()
            .unwrap()
            .ends_with("/projets"),
        "{responses}"
    );
    assert!(responses.get("modele").is_none(), "{responses}");
    assert_eq!(calls[1]["requestState"], "encore");
}

/// −32042 sans lien exploitable : pas de nouvel essai, l'erreur remonte.
#[tokio::test]
async fn a_url_elicitation_without_links_is_not_retried() {
    let (_d, _s, _c, fake, sup) = setup().await;
    let data = Arc::new(Mutex::new(vec![
        None,
        Some(json!({"elicitations": []})),
        Some(json!({"elicitations": [{"url": "https://x.example"}]})),
    ]));
    let d2 = data.clone();
    fake.serve(
        "m",
        modern(
            move |_| {
                Err(rpc(
                    penelope_mcp::protocol::URL_ELICITATION_REQUIRED,
                    d2.lock().unwrap().pop().flatten(),
                ))
            },
            no_tasks,
        ),
    );
    declare(&sup, "m", "");
    sup.reload().await;
    for _ in 0..3 {
        let err = sup
            .call("mcp__m__agir", &json!({}), Default::default())
            .await
            .unwrap_err();
        assert!(err.starts_with("`mcp__m__agir` : "), "{err}");
    }
    let log = fake.last_transport("m").call_log().await;
    assert_eq!(
        log.iter().filter(|(m, _)| m == "tools/call").count(),
        3,
        "un seul essai par appel"
    );
}

/// #126 : un refus d'en-têtes dégrade le serveur et le dit au modèle ; le prochain appel
/// réussi le remet prêt.
#[tokio::test]
async fn a_header_mismatch_degrades_the_server_until_a_call_succeeds() {
    let (_d, _s, _c, fake, sup) = setup().await;
    let fail = Arc::new(AtomicUsize::new(1));
    let f2 = fail.clone();
    fake.serve(
        "m",
        modern(
            move |_| {
                if f2.fetch_sub(1, Ordering::SeqCst) == 1 {
                    Err(rpc(penelope_mcp::protocol::HEADER_MISMATCH, None))
                } else {
                    Ok(json!({"content": [{"type": "text", "text": "ok"}]}))
                }
            },
            no_tasks,
        ),
    );
    declare(&sup, "m", "");
    sup.reload().await;
    let err = sup
        .call("mcp__m__agir", &json!({}), Default::default())
        .await
        .unwrap_err();
    assert!(
        err.contains("-32020") && err.contains("ne réessaie"),
        "{err}"
    );
    let st = &sup.statuses().await[0];
    assert_eq!(st.state, ServerState::Degraded);
    assert!(st.last_error.is_some());

    sup.call("mcp__m__agir", &json!({}), Default::default())
        .await
        .unwrap();
    assert_eq!(sup.statuses().await[0].state, ServerState::Ready);
}

/// Un serveur stdio sous bac à sable qui échoue sur le trousseau : l'erreur dit que le
/// trousseau lui est fermé, que l'échec soit un résultat en erreur ou une erreur RPC.
#[tokio::test]
async fn a_keychain_failure_explains_the_sandbox() {
    let (_d, _s, _c, fake, sup) = setup().await;
    let as_rpc = Arc::new(AtomicUsize::new(0));
    let r2 = as_rpc.clone();
    fake.serve(
        "m",
        modern(
            move |_| {
                if r2.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(json!({"isError": true,
                              "content": [{"type": "text", "text": "SecItemCopyMatching: errSecItemNotFound"}]}))
                } else {
                    Err(McpError::Rpc {
                        code: -32000,
                        message: "keychain locked".into(),
                        data: None,
                    })
                }
            },
            no_tasks,
        ),
    );
    declare(&sup, "m", "");
    sup.reload().await;
    let v = sup
        .call("mcp__m__agir", &json!({}), Default::default())
        .await
        .unwrap();
    assert_eq!(v["isError"], true);
    assert!(
        v["content"].to_string().contains("allow_keychain_for"),
        "{v}"
    );
    let err = sup
        .call("mcp__m__agir", &json!({}), Default::default())
        .await
        .unwrap_err();
    assert!(err.contains("allow_keychain_for"), "{err}");
}

/// `tasks/get` puis, une fois la tâche finie, `tasks/result` ; à défaut de cette méthode,
/// le résultat porté par la tâche. Une tâche annulée ou en cours n'a pas de résultat.
#[tokio::test]
async fn task_status_reads_the_result_of_a_finished_task() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve(
        "m",
        modern(
            |_| Ok(json!({"content": []})),
            |m, p| match (m, p["taskId"].as_str().unwrap_or_default()) {
                ("tasks/get", "finie") => {
                    Ok(json!({"task": {"status": "completed", "result": {"n": 1}}}))
                }
                ("tasks/get", "vieille") => Ok(json!({"state": "completed", "result": {"n": 2}})),
                ("tasks/get", "annulee") => Ok(json!({"task": {"status": "cancelled"}})),
                ("tasks/get", "casse") => Ok(json!({"task": {"status": "failed"}})),
                ("tasks/get", _) => Ok(json!({"task": {}})),
                ("tasks/result", "finie") => {
                    Ok(json!({"content": [{"type": "text", "text": "fini"}]}))
                }
                ("tasks/result", "casse") => Err(rpc(-32000, None)),
                _ => Err(rpc(penelope_mcp::protocol::METHOD_NOT_FOUND, None)),
            },
        ),
    );
    declare(&sup, "m", "");
    sup.reload().await;
    let finie = sup.task_status("m", "finie").await.unwrap();
    assert_eq!(finie["status"], "completed");
    assert_eq!(finie["result"]["content"][0]["text"], "fini");
    let vieille = sup.task_status("m", "vieille").await.unwrap();
    assert_eq!(
        vieille["result"],
        json!({"n": 2}),
        "résultat porté par la tâche"
    );
    let annulee = sup.task_status("m", "annulee").await.unwrap();
    assert_eq!(annulee["result"], Value::Null);
    let en_cours = sup.task_status("m", "autre").await.unwrap();
    assert_eq!(en_cours["status"], "working");
    assert_eq!(en_cours["result"], Value::Null);
    let err = sup.task_status("m", "casse").await.unwrap_err();
    assert!(err.starts_with("tasks/result `casse`"), "{err}");
    assert!(sup.task_status("absent", "x").await.is_err());
}
