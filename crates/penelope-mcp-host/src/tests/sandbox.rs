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

/// #270 : les valeurs de `env` développent `{data}`, `{config}`, `{state}`, `{logs}` et
/// `~/` comme `command`, `args` et `cwd`, qui l'étaient déjà ; `WA_DATA_DIR =
/// "{data}/…"` arrivait tel quel au serveur, qui créait un dossier nommé `{data}`. Une
/// accolade qui n'est pas un gabarit reste intacte.
#[test]
fn env_values_expand_the_same_templates_as_args_and_cwd() {
    let dirs = penelope_platform::RootedDirs::new("/r");
    let mut cfg = ServerConfig::stdio(
        "whatsapp",
        "{data}/bin/wa-mcp",
        &["--state", "{state}/wa", "--json", "{\"a\":1}"],
    );
    cfg.cwd = "{config}/wa".into();
    cfg.env
        .insert("WA_DATA_DIR".into(), "{data}/mcp-data/whatsapp".into());
    cfg.env.insert("WA_LOG".into(), "{logs}/wa.log".into());
    cfg.env.insert("WA_HOME".into(), "~/wa".into());
    cfg.env
        .insert("WA_TOKEN".into(), "tok-{pas-un-gabarit}".into());
    let spec = stdio_spec(&dirs, &cfg);
    assert_eq!(spec.program, "/r/data/bin/wa-mcp");
    assert_eq!(
        spec.args,
        vec!["--state", "/r/state/wa", "--json", "{\"a\":1}"]
    );
    assert_eq!(spec.cwd, Some(PathBuf::from("/r/config/wa")));
    assert_eq!(spec.env["WA_DATA_DIR"], "/r/data/mcp-data/whatsapp");
    assert_eq!(spec.env["WA_LOG"], "/r/logs/wa.log");
    assert_eq!(spec.env["WA_TOKEN"], "tok-{pas-un-gabarit}");
    if let Some(home) = penelope_platform::dirs::home_dir() {
        assert_eq!(spec.env["WA_HOME"], home.join("wa").to_string_lossy());
    }
    assert_eq!(spec.pid_tag.as_deref(), Some("mcp-whatsapp"));
}

/// #270 : un serveur qui meurt sur un chemin où `{data}` est resté tel quel est expliqué
/// par le gabarit, pas par le bac à sable : Pénélope conseillait `sandbox_profile =
/// "full"` pour un dossier `{data}` introuvable. Un gabarit qui n'est pas de Pénélope est
/// nommé avec ceux qu'elle connaît ; un refus d'écriture sans gabarit garde le conseil.
#[test]
fn a_literal_template_in_stderr_names_the_template_not_the_sandbox() {
    let cfg = ServerConfig::stdio("whatsapp", "/opt/mcp/wa", &[]);
    let died =
        McpError::Transport("le serveur s'est arrêté : sorti avec le code 1 après 40 ms".into());
    let logs = vec![
        "Error: création de {data}/mcp-data/whatsapp".to_string(),
        "Caused by: Operation not permitted (os error 1)".to_string(),
    ];
    let said = explain(&cfg, &died, &logs);
    assert!(said.contains("`{data}` tel quel"), "{said}");
    assert!(said.contains("`env`"), "{said}");
    assert!(!said.contains("sandbox_profile"), "{said}");
    assert!(!said.contains("allow_full_for"), "{said}");

    let logs = vec!["mkdir {home}/wa: Operation not permitted".to_string()];
    let said = explain(&cfg, &died, &logs);
    assert!(said.contains("`{home}` n'est pas un gabarit"), "{said}");
    assert!(said.contains("`{data}`"), "{said}");
    assert!(!said.contains("allow_full_for"), "{said}");

    // Ce qui marchait : sans gabarit, le refus d'écriture désigne le bac à sable ; un
    // `{0}` de format ou une accolade vide ne sont pas des gabarits.
    let logs = vec!["mkdir /Users/moi/wa: Operation not permitted (os error 1) {0} {}".to_string()];
    let said = explain(&cfg, &died, &logs);
    assert!(said.contains("allow_full_for"), "{said}");
    assert!(!said.contains("tel quel"), "{said}");
    let mut full = cfg.clone();
    full.sandbox_profile = "full".into();
    assert!(!explain(&full, &died, &logs).contains("allow_full_for"));
}
