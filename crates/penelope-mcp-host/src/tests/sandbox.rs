use super::*;

/// #89 : un serveur stdio sous `mcp-stdio` refuse les mêmes lectures que le shell, après
/// l'autorisation générale, et ferme le trousseau.
#[tokio::test]
async fn a_stdio_server_cannot_read_what_the_shell_cannot() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    let cfg = ServerConfig {
        name: "tiers".into(),
        command: "/bin/sh".into(),
        ..Default::default()
    };
    let p = stdio_profile(&s, &cfg).unwrap();
    let sbpl = penelope_platform::sandbox::seatbelt_profile(&p);
    let allow = sbpl.find("(allow file-read*)\n").expect("lecture générale");
    let ssh = s.platform.dirs.expand("~/.ssh");
    let deny = sbpl
        .find(&format!(
            "(deny file-read* (subpath \"{}\"))",
            ssh.display()
        ))
        .unwrap_or_else(|| panic!("~/.ssh refusé :\n{sbpl}"));
    assert!(deny > allow, "{sbpl}");
    assert!(sbpl.contains("secrets.enc"), "{sbpl}");
    assert!(sbpl.contains("com.apple.SecurityServer"), "{sbpl}");
}

/// #138 : `mcp edit` sur un champ liste accepte une valeur seule, sans la découper, et
/// nomme la forme attendue au lieu de l'erreur du désérialiseur.
#[tokio::test]
async fn a_list_field_takes_a_single_value() {
    let (_d, _s, _c, fake, sup) = setup().await;
    fake.serve("pont", server(two_tools()));
    declare(&sup, "pont", "");
    sup.reload().await;
    sup.edit("pont", &json!({"roots": "/a"})).await.unwrap();
    let single = sup.config_of("pont").await.unwrap().roots;
    sup.edit("pont", &json!({"roots": ["/a"]})).await.unwrap();
    assert_eq!(sup.config_of("pont").await.unwrap().roots, single);
    assert_eq!(single, vec!["/a".to_string()]);
    sup.edit("pont", &json!({"args": "--mode lecture"}))
        .await
        .unwrap();
    assert_eq!(
        sup.config_of("pont").await.unwrap().args,
        vec!["--mode lecture".to_string()],
        "une valeur seule n'est jamais découpée"
    );
    let e = sup.edit("pont", &json!({"roots": 42})).await.unwrap_err();
    assert!(e.contains("`roots` attend une liste"), "{e}");
    let e = sup
        .edit("pont", &json!({"timeout": ["30s"]}))
        .await
        .unwrap_err();
    assert!(
        e.contains("`timeout` attend une chaîne") && !e.contains("invalid type"),
        "{e}"
    );
}

/// #126 : `mcp test` appelle vraiment un outil en lecture sans argument ; un serveur
/// qui refuse les appels (-32020) échoue au test au lieu de passer pour sain, et
/// l'erreur rendue au modèle dit que ce n'est pas une affaire d'arguments.
#[tokio::test]
async fn mcp_test_really_calls_a_read_tool() {
    let (_d, _s, _c, fake, sup) = setup().await;
    let tools = Arc::new(Mutex::new(vec![
        tool("create_task", json!({})),
        tool("clickup_search", json!({"readOnlyHint": true})),
        tool("get_workspace_hierarchy", json!({"readOnlyHint": true})),
    ]));
    fake.serve("sain", server(tools.clone()));
    let base = server(tools);
    fake.serve(
        "refuse",
        Arc::new(move |m, p| {
            if m == "tools/call" {
                return Err(McpError::Rpc {
                    code: penelope_mcp::protocol::HEADER_MISMATCH,
                    message: "the body carries params.name but the Mcp-Name header \
                                  names \"penelope\""
                        .into(),
                    data: None,
                });
            }
            base(m, p)
        }),
    );
    let base = server(Arc::new(Mutex::new(vec![tool(
        "list_items",
        json!({"readOnlyHint": true}),
    )])));
    fake.serve(
        "exigeant",
        Arc::new(move |m, p| {
            if m == "tools/call" {
                return Err(McpError::Rpc {
                    code: penelope_mcp::protocol::INVALID_PARAMS,
                    message: "workspace_id manquant".into(),
                    data: None,
                });
            }
            base(m, p)
        }),
    );
    declare(&sup, "sain", "");
    declare(&sup, "refuse", "");
    declare(&sup, "exigeant", "");
    sup.reload().await;

    // Un refus de l'outil (arguments) n'est pas une faute de protocole.
    let picky = sup.test(&sup.config_of("exigeant").await.unwrap()).await;
    assert_eq!(picky["ok"], true, "{picky}");
    assert!(
        picky["call"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("workspace_id manquant"),
        "{picky}"
    );

    let ok = sup.test(&sup.config_of("sain").await.unwrap()).await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["call"]["tool"], "get_workspace_hierarchy", "{ok}");
    assert_eq!(ok["call"]["ok"], true, "{ok}");

    let ko = sup.test(&sup.config_of("refuse").await.unwrap()).await;
    assert_eq!(ko["ok"], false, "{ko}");
    let error = ko["error"].as_str().unwrap();
    assert!(error.contains("3 outil(s) listés"), "{error}");
    assert!(error.contains("-32020"), "{error}");

    let e = sup
        .call(
            "mcp__refuse__clickup_search",
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap_err();
    assert!(e.contains("ne réessaie pas"), "{e}");
    assert!(e.contains("pas des arguments"), "{e}");
    let st = sup.statuses().await;
    let refuse = st.iter().find(|x| x.name == "refuse").unwrap();
    assert_eq!(refuse.state, ServerState::Degraded);
    assert!(
        refuse
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("-32020")
    );
}

/// #122 : un serveur déclaré dans `sandbox.allow_keychain_for` joint le trousseau et
/// garde le reste de son bac à sable ; les autres ne le joignent pas, et
/// `allow_full_for` n'y est pour rien.
#[tokio::test]
async fn only_a_declared_server_reaches_the_keychain() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    s.config
        .mutate("test", |c| {
            c.sandbox.allow_keychain_for = vec!["mailbridge".into()];
            Ok(vec!["sandbox.allow_keychain_for".into()])
        })
        .unwrap();
    assert!(s.config.config().sandbox.allow_full_for.is_empty());
    let server = |name: &str| ServerConfig {
        name: name.into(),
        command: "/opt/mcp/bin".into(),
        ..Default::default()
    };
    let keychain = "(global-name \"com.apple.SecurityServer\")";
    let ssh = s.platform.dirs.expand("~/.ssh");

    let declared = stdio_profile(&s, &server("mailbridge")).unwrap();
    assert!(declared.enforced(), "le bac à sable reste imposé");
    assert_eq!(declared.kind, penelope_platform::ProfileKind::McpStdio);
    let sbpl = penelope_platform::sandbox::seatbelt_profile(&declared);
    assert!(!sbpl.contains(keychain), "{sbpl}");
    assert!(sbpl.contains(&format!(
        "(deny file-read* (subpath \"{}\"))",
        ssh.display()
    )));
    assert!(
        sbpl.contains("(deny network-outbound (remote unix-socket))"),
        "{sbpl}"
    );
    assert!(keychain_open(&s, &server("mailbridge")));

    let other = stdio_profile(&s, &server("tiers")).unwrap();
    let sbpl = penelope_platform::sandbox::seatbelt_profile(&other);
    assert!(
        sbpl.contains(&format!("(deny mach-lookup {keychain})")),
        "{sbpl}"
    );
    assert!(!keychain_open(&s, &server("tiers")));

    // Un serveur distant n'a pas de processus local : pas de trousseau à ouvrir.
    let distant = ServerConfig {
        name: "mailbridge".into(),
        url: "https://mcp.example.test/mcp".into(),
        ..Default::default()
    };
    assert!(!keychain_open(&s, &distant));
}

/// #122 : un serveur confiné qui échoue sur le trousseau voit son erreur complétée par
/// le bac à sable et le réglage ; déclaré, il n'a pas cette phrase ; une erreur qui ne
/// parle pas du trousseau non plus.
#[tokio::test]
async fn a_keychain_failure_names_the_sandbox_not_the_secret() {
    let (_d, s, _c, fake, sup) = setup().await;
    let tools = Arc::new(Mutex::new(vec![
        tool("search_emails", json!({"readOnlyHint": true})),
        tool("list_accounts", json!({"readOnlyHint": true})),
    ]));
    let base = server(tools);
    fake.serve(
        "mailbridge",
        Arc::new(move |m, p| {
            if m == "tools/call" {
                let text = if p["name"] == "search_emails" {
                    "IMAP connection failed: get password for essai@example.test: \
                         secret not found in keyring"
                } else {
                    "IMAP connection failed: timeout"
                };
                return Ok(json!({
                    "content": [{"type": "text", "text": text}],
                    "isError": true
                }));
            }
            base(m, p)
        }),
    );
    declare(&sup, "mailbridge", "");
    sup.reload().await;

    let v = sup
        .call(
            "mcp__mailbridge__search_emails",
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(v["isError"], true);
    let said = v["content"].to_string();
    assert!(said.contains("secret not found in keyring"), "{said}");
    assert!(said.contains("bac à sable"), "{said}");
    assert!(said.contains("sandbox.allow_keychain_for"), "{said}");
    assert!(said.contains("SECRET:nom"), "{said}");
    assert!(!said.contains("allow_full_for"), "{said}");

    let v = sup
        .call(
            "mcp__mailbridge__list_accounts",
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(!v["content"].to_string().contains("bac à sable"), "{v}");

    s.config
        .mutate("test", |c| {
            c.sandbox.allow_keychain_for = vec!["mailbridge".into()];
            Ok(vec!["sandbox.allow_keychain_for".into()])
        })
        .unwrap();
    let v = sup
        .call(
            "mcp__mailbridge__search_emails",
            &json!({}),
            Default::default(),
        )
        .await
        .unwrap();
    assert!(!v["content"].to_string().contains("bac à sable"), "{v}");
}

/// #89 : un chemin refusé qui contient le répertoire de données ou une racine du
/// serveur reste lisible pour lui.
#[tokio::test]
async fn a_server_keeps_its_own_directories_readable() {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    s.config
        .mutate("test", |c| {
            c.sandbox.deny_read = vec![
                "{data}/mcp-data".into(),
                "{data}/partage".into(),
                "{data}/secrets.enc".into(),
            ];
            Ok(vec!["sandbox.deny_read".into()])
        })
        .unwrap();
    let cfg = ServerConfig {
        name: "notes".into(),
        command: "/bin/sh".into(),
        roots: vec!["{data}/partage/projet".into()],
        ..Default::default()
    };
    let p = stdio_profile(&s, &cfg).unwrap();
    let data = s.platform.dirs.data();
    assert_eq!(p.deny_read, vec![data.join("secrets.enc")]);
}
