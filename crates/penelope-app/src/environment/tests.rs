use super::explore::explore;
use super::*;
use crate::machine::Inventory;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

async fn services() -> (tempfile::TempDir, Services) {
    let dir = tempfile::tempdir().unwrap();
    let clock: penelope_kernel::clock::SharedClock =
        Arc::new(penelope_kernel::clock::TestClock::default());
    let s = Services::for_tests(dir.path().to_path_buf(), clock)
        .await
        .unwrap();
    (dir, s)
}

/// Un faux serveur `/v1/models` sur la boucle locale ; rend son adresse de base.
async fn models_server(ids: &[&str]) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let body =
        json!({"object": "list", "data": ids.iter().map(|i| json!({"id": i})).collect::<Vec<_>>()})
            .to_string();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if req.starts_with("GET /v1/models ") {
                    ("200 OK", body)
                } else {
                    ("404 Not Found", "{}".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Une fausse machine : un PATH avec `brew`, `cargo`, un outil maison, un binaire lié
/// dans un `Cellar` ; un dossier d'applications avec une application factice.
#[cfg(unix)]
struct FakeMachine {
    bin: tempfile::TempDir,
    apps: tempfile::TempDir,
}

#[cfg(unix)]
impl FakeMachine {
    fn new(outil_version: &str) -> FakeMachine {
        let m = FakeMachine {
            bin: tempfile::tempdir().unwrap(),
            apps: tempfile::tempdir().unwrap(),
        };
        m.set_brew(outil_version);
        script(
            m.bin.path(),
            "cargo",
            r#"[ "$1" = "install" ] && printf 'cargo-nextest v0.9.144:\n    cargo-nextest\n' && exit 0
exit 1"#,
        );
        script(m.bin.path(), "cargo-nextest", "echo nextest");
        script(m.bin.path(), "outil-maison", "echo 1");
        script(m.bin.path(), "sans-paquet", "echo 1");
        let cellar = m.bin.path().join("Cellar/ripgrep/14.1.0/bin");
        std::fs::create_dir_all(&cellar).unwrap();
        let rg = script(&cellar, "rg", "echo rg");
        std::os::unix::fs::symlink(rg, m.bin.path().join("rg")).unwrap();
        script(
            m.bin.path(),
            "xcrun",
            r#"[ "$1" = "--find" ] && [ "$2" = "mcpbridge" ] && echo /Xcode/mcpbridge && exit 0
exit 1"#,
        );
        let contents = m.apps.path().join("Navigateur.app/Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>fr.exemple.nav</string>\
             <key>CFBundleShortVersionString</key><string>27.0</string></dict></plist>",
        )
        .unwrap();
        m
    }

    /// Le faux `brew list` : l'outil maison à la version donnée, `ripgrep`, une
    /// bibliothèque sans exécutable.
    fn set_brew(&self, outil_version: &str) {
        script(
            self.bin.path(),
            "brew",
            &format!(
                r#"[ "$1" = "list" ] && [ "$2" = "--formula" ] && printf 'outil-maison {outil_version}\nripgrep 14.1.0\nlibyaml 0.2.5\n' && exit 0
exit 1"#
            ),
        );
    }

    fn sources(&self, mcp_dir: &Path) -> Sources {
        Sources {
            path: self.bin.path().as_os_str().to_owned(),
            app_dirs: vec![self.apps.path().to_path_buf()],
            offers: penelope_platform::discover::APPLE_OFFERS.to_vec(),
            hardware: false,
            default_ports: false,
            mcp_dir: mcp_dir.to_path_buf(),
        }
    }
}

/// Critère de #260 : un binaire hors de `KNOWN` (faux PATH, faux `brew list`) apparaît
/// dans la carte avec sa source et sa version ; le binaire d'un paquet au nom différent
/// garde son paquet ; un binaire sans gestionnaire reste `PATH`.
#[cfg(unix)]
#[test]
fn a_binary_outside_the_known_list_appears_with_its_source() {
    let m = FakeMachine::new("1.2.3");
    let env = scan(&m.sources(Path::new("/nulle-part")));
    let tool = |name: &str| env.tools.iter().find(|t| t.name == name).cloned();
    assert!(!crate::machine::KNOWN.contains(&"outil-maison"));

    let own = tool("outil-maison").expect("outil maison dans la carte");
    assert_eq!(own.source, "brew");
    assert_eq!(own.version.as_deref(), Some("1.2.3"));
    let rg = tool("rg").unwrap();
    assert_eq!(
        (rg.source.as_str(), rg.package.as_deref()),
        ("brew", Some("ripgrep"))
    );
    assert_eq!(rg.version.as_deref(), Some("14.1.0"));
    let nextest = tool("cargo-nextest").unwrap();
    assert_eq!(nextest.source, "cargo");
    assert_eq!(nextest.version.as_deref(), Some("0.9.144"));
    assert_eq!(tool("sans-paquet").unwrap().source, "PATH");
    // Un paquet sans exécutable reste dans la carte, sans chemin.
    let lib = tool("libyaml").unwrap();
    assert_eq!((lib.source.as_str(), lib.path.as_deref()), ("brew", None));
    // `ripgrep` est réclamé par `rg` : il n'apparaît pas une seconde fois.
    assert!(tool("ripgrep").is_none());
}

/// Critère de #260 : une application factice avec `Info.plist` apparaît avec sa version ;
/// un faux `xcrun mcpbridge` et un faux serveur `/v1/models` apparaissent comme capacités.
#[cfg(unix)]
#[tokio::test]
async fn a_fake_app_bridge_and_models_server_are_on_the_map() {
    let (_d, s) = services().await;
    let m = FakeMachine::new("1.2.3");
    let base = models_server(&["qwen3-coder-30b", "gemma-4-12b"]).await;
    crate::helpers::set_config_path(&s, "providers.local.enabled", json!(true)).unwrap();
    crate::helpers::set_config_path(&s, "providers.local.base_url", json!(base)).unwrap();

    let env = refresh_from(&s, m.sources(Path::new("/nulle-part")))
        .await
        .unwrap();
    let app = env.apps.iter().find(|a| a.name == "Navigateur").unwrap();
    assert_eq!(app.version.as_deref(), Some("27.0"));
    assert_eq!(app.bundle_id.as_deref(), Some("fr.exemple.nav"));

    let xcode = env.capabilities.iter().find(|c| c.id == "xcode").unwrap();
    assert_eq!(
        (xcode.kind.as_str(), xcode.via.as_str(), xcode.declared),
        ("mcp", "xcrun mcpbridge", false)
    );
    let local = env
        .capabilities
        .iter()
        .find(|c| c.kind == "inference")
        .unwrap();
    assert_eq!(local.name, "providers.local");
    assert_eq!(local.reachable, Some(true));
    assert_eq!(local.models, ["gemma-4-12b", "qwen3-coder-30b"]);
    assert!(local.declared, "le fournisseur déclaré le branche déjà");

    // La carte fait un aller-retour par `kv` : c'est elle que lit `env_explore`.
    assert_eq!(cached(&s).await.unwrap(), env);
}

/// Critère de #260 : la ligne T1 est identique octet pour octet après la mise à jour d'un
/// outil ; la carte, elle, dit ce qui a changé, et `doctor` le lira.
#[cfg(unix)]
#[tokio::test]
async fn the_t1_line_survives_a_tool_upgrade_and_the_map_says_what_changed() {
    let (_d, s) = services().await;
    let m = FakeMachine::new("1.2.3");
    let first = refresh_from(&s, m.sources(Path::new("/nulle-part")))
        .await
        .unwrap();
    assert_eq!(first.changes, None, "première passe : rien à comparer");

    m.set_brew("1.3.0");
    let second = refresh_from(&s, m.sources(Path::new("/nulle-part")))
        .await
        .unwrap();
    let changes = second.changes.clone().unwrap();
    assert_eq!(changes.updated, ["outil-maison (brew) : 1.2.3 → 1.3.0"]);
    assert!(changes.appeared.is_empty() && changes.disappeared.is_empty());

    let line = |env: &Environment| {
        let mut inv = Inventory {
            os: "macOS 27.0".into(),
            arch: "aarch64".into(),
            sandbox_profile: "full".into(),
            ..Default::default()
        };
        inv.with_environment(env);
        inv.prompt_line()
    };
    assert_eq!(line(&first), line(&second), "une version ne touche pas T1");
    let l = line(&second);
    assert!(
        l.contains("MCP exposés par des applications : Xcode"),
        "{l}"
    );
    assert!(l.contains("`env_explore`"), "{l}");
    assert!(!l.contains("1.3.0") && !l.contains("Navigateur"), "{l}");

    // Une passe sans changement garde les derniers changements vus.
    let third = refresh_from(&s, m.sources(Path::new("/nulle-part")))
        .await
        .unwrap();
    assert_eq!(third.changes, Some(changes));
}

fn capability(kind: &str, id: &str, name: &str, origin: &str) -> Capability {
    Capability {
        kind: kind.into(),
        id: id.into(),
        name: name.into(),
        via: format!("{id} --mcp"),
        origin: origin.into(),
        ..Default::default()
    }
}

/// Une carte de propriétaire : Safari et Xcode exposent un MCP, Ollama tourne.
fn map() -> Environment {
    let tool = |name: &str, source: &str, version: Option<&str>| Found {
        name: name.into(),
        source: source.into(),
        version: version.map(str::to_string),
        ..Default::default()
    };
    let app = |name: &str, id: &str, version: &str| App {
        name: name.into(),
        bundle_id: Some(id.into()),
        version: Some(version.into()),
        path: format!("/Applications/{name}.app"),
    };
    let mut ollama = capability("inference", "inference:11434", "Ollama", "outil");
    ollama.via = "http://127.0.0.1:11434/v1".into();
    ollama.reachable = Some(true);
    ollama.models = vec!["qwen3:32b".into()];
    Environment {
        hardware: Some(Hardware {
            chip: Some("Apple M5 Max".into()),
            memory_gb: Some(128),
            gpu_cores: Some(40),
            ..Default::default()
        }),
        tools: vec![
            tool("ffmpeg", "brew", Some("7.1")),
            tool("ollama", "brew", Some("0.12.0")),
            tool("rg", "brew", Some("14.1.0")),
            tool("safaridriver", "système", None),
            tool("swiftc", "système", None),
        ],
        apps: vec![
            app("Safari", "com.apple.Safari", "27.0"),
            app("Xcode", "com.apple.dt.Xcode", "27.0"),
        ],
        capabilities: vec![
            capability("mcp", "safari", "Safari", "application"),
            capability("mcp", "xcode", "Xcode", "application"),
            ollama,
        ],
        checked_at: "2026-09-29T09:00:00Z".into(),
        changes: None,
    }
}

/// `env_explore` rend les capacités par besoin, pas seulement par nom.
#[test]
fn the_map_is_searched_by_need() {
    let env = map();
    let none = BTreeMap::new();
    let names = |v: &serde_json::Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect()
    };

    let nav = explore(Some(&env), Some("un navigateur"), None, 50, &none);
    assert_eq!(names(&nav, "apps"), ["Safari"]);
    assert_eq!(names(&nav, "tools"), ["safaridriver"]);
    assert_eq!(names(&nav, "capabilities"), ["Safari"], "par son nom");

    let swift = explore(Some(&env), Some("compilateur Swift"), None, 50, &none);
    assert_eq!(names(&swift, "tools"), ["swiftc"]);
    assert_eq!(names(&swift, "apps"), ["Xcode"]);

    let mcp = explore(Some(&env), Some("MCP"), None, 50, &none);
    assert_eq!(names(&mcp, "capabilities"), ["Safari", "Xcode"]);

    let local = explore(Some(&env), Some("inférence locale"), None, 50, &none);
    assert_eq!(names(&local, "capabilities"), ["Ollama"]);
    assert_eq!(local["capabilities"][0]["models"][0], "qwen3:32b");
    assert_eq!(names(&local, "tools"), ["ollama"]);
    assert_eq!(
        local["hardware"]["memory_gb"], 128,
        "la mémoire décide des modèles"
    );
    assert!(nav.get("hardware").is_none());

    let hw = explore(Some(&env), Some("mémoire et GPU"), None, 50, &none);
    assert_eq!(hw["hardware"]["gpu_cores"], 40);
    assert!(
        hw["note"].as_str().unwrap().contains("shell_exec"),
        "découvrir n'autorise rien"
    );
}

/// Sans besoin, un sommaire ; `kind` et `limit` bornent ; sans carte, la voie pour en
/// avoir une.
#[test]
fn the_summary_the_limits_and_the_empty_map() {
    let env = map();
    let mut proposed = BTreeMap::new();
    proposed.insert("safari".to_string(), "2026-09-29T09:05:00Z".to_string());

    let sum = explore(Some(&env), None, None, 50, &proposed);
    assert_eq!(sum["tools"]["total"], 5);
    assert_eq!(sum["tools"]["by_source"]["brew"], 3);
    assert_eq!(
        sum["capabilities"][0]["proposed_at"],
        "2026-09-29T09:05:00Z"
    );

    let only = explore(Some(&env), None, Some("tools"), 2, &proposed);
    assert_eq!(only["tools"].as_array().unwrap().len(), 2);
    assert!(only.get("apps").is_none());
    assert!(only["truncated"].as_str().unwrap().contains("limit"));

    let blank = explore(None, Some("navigateur"), None, 50, &proposed);
    assert!(blank["map"].is_null());
    assert!(blank["note"].as_str().unwrap().contains("doctor"));
}

/// Proposer, pas imposer : un MCP exposé et non déclaré, un serveur qui répond avec des
/// modèles, une seule fois ; ce qui est déjà branché ne se propose pas.
#[tokio::test]
async fn capabilities_are_proposed_once_and_never_branched() {
    let (_d, s) = services().await;
    let mut env = map();
    env.capabilities[1].declared = true; // Xcode est déjà dans `mcp.d`.
    s.kv_set(KV_KEY, &serde_json::to_string(&env).unwrap())
        .await
        .unwrap();

    let m = crate::testing::RecordingMessenger::new();
    let n = propose::propose(&s, m.as_ref()).await.unwrap();
    assert_eq!(n, 2);
    let texts = m.texts();
    assert_eq!(texts.len(), 1);
    assert!(
        texts[0].contains("Safari expose un serveur MCP (`safari --mcp`)"),
        "{}",
        texts[0]
    );
    assert!(texts[0].contains("Ollama sert 1 modèle(s)"), "{}", texts[0]);
    assert!(!texts[0].contains("Xcode"), "déjà branché : {}", texts[0]);
    assert!(
        texts[0].contains("Rien n'est branché sans toi"),
        "{}",
        texts[0]
    );

    // Rien n'a été écrit dans `mcp.d` : la proposition ne branche rien.
    assert!(!s.platform.dirs.mcp_d().join("safari.toml").exists());
    // La passe suivante ne repropose pas.
    assert_eq!(propose::propose(&s, m.as_ref()).await.unwrap(), 0);
    assert_eq!(m.texts().len(), 1);
    let done = propose::proposed(&s).await;
    assert_eq!(
        done.keys().collect::<Vec<_>>(),
        ["inference:11434", "safari"]
    );
}

/// Un MCP exposé est reconnu branché quand `mcp.d` déclare la même commande, quel que
/// soit son chemin.
#[test]
fn an_offer_declared_in_mcp_d_is_already_branched() {
    let offer = penelope_platform::discover::McpOffer {
        id: "safari".into(),
        app: "Safari".into(),
        command: "safaridriver".into(),
        args: vec!["--mcp".into()],
    };
    let declared = penelope_mcp::config::ServerConfig {
        name: "navigateur".into(),
        command: "/usr/bin/safaridriver".into(),
        args: vec!["--mcp".into()],
        ..Default::default()
    };
    let caps = mcp_capabilities(std::slice::from_ref(&offer), &[declared]);
    assert_eq!(caps.len(), 2);
    assert!(caps[0].declared);
    assert_eq!(caps[1].id, "mcp.d/navigateur");
    assert_eq!(caps[1].origin, "configuration");
    assert!(!mcp_capabilities(&[offer], &[])[0].declared);
}

/// La sonde ne sort jamais de la machine, et un fournisseur désactivé n'est pas une
/// capacité.
#[test]
fn only_enabled_loopback_providers_are_probed() {
    let mut cfg = penelope_kernel::config::Config::default();
    assert!(inference::endpoints(&cfg, &[], &[], false).is_empty());
    cfg.providers.local.enabled = true;
    cfg.providers.local.base_url = "https://inference.exemple.fr/v1".into();
    assert!(inference::endpoints(&cfg, &[], &[], false).is_empty());
    cfg.providers.local.base_url = "http://localhost:8080/v1".into();
    let ollama = Found {
        name: "ollama".into(),
        source: "brew".into(),
        ..Default::default()
    };
    let eps = inference::endpoints(&cfg, &[ollama], &[], false);
    let ports: Vec<u16> = eps.iter().map(|e| e.port).collect();
    assert_eq!(ports, [8080, 11434]);
    assert_eq!(eps[0].providers, ["providers.local"]);
    assert_eq!(eps[1].engines, ["Ollama"]);
}

/// Les sorties des gestionnaires telles qu'ils les rendent.
#[test]
fn the_managers_outputs_are_read() {
    use managers::*;
    let uv = parse_uv("mistral-vibe v2.9.4\n- vibe\n- vibe-acp\nruff v0.6.0\n- ruff\n");
    assert_eq!(uv[0].bins, ["vibe", "vibe-acp"]);
    assert_eq!(uv[1].version.as_deref(), Some("0.6.0"));
    let npm = parse_npm(r#"{"dependencies":{"@openai/codex":{"version":"0.80.7"}}}"#);
    assert_eq!(npm[0].name, "@openai/codex");
    assert_eq!(npm[0].version.as_deref(), Some("0.80.7"));
    let brew = parse_brew("python@3.13 3.13.1 3.13.2\n");
    assert_eq!(
        brew[0].version.as_deref(),
        Some("3.13.2"),
        "la dernière est l'active"
    );
    let pipx = parse_pipx("nothing has been installed with pipx 😴\nblack 24.8.0\n");
    assert_eq!(pipx.len(), 1, "une phrase n'est pas un paquet : {pipx:?}");
    assert_eq!(pipx[0].name, "black");
    assert_eq!(parse_npm("pas du json"), Vec::new());
}
