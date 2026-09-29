use super::*;

const T: Duration = Duration::from_secs(5);

#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// #260 : le PATH est lu en entier, pas contre une liste ; le premier d'un nom gagne,
/// un fichier non exécutable ou caché n'est pas un outil, un lien garde sa cible.
#[cfg(unix)]
#[test]
fn every_executable_of_the_path_is_found_once() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    script(a.path(), "outil-maison", "echo 1");
    script(a.path(), "double", "echo a");
    script(b.path(), "double", "echo b");
    script(b.path(), ".cache", "echo cachee");
    std::fs::write(b.path().join("notes.txt"), "pas un outil").unwrap();
    let cellar = b.path().join("Cellar/ripgrep/14.1.0/bin");
    std::fs::create_dir_all(&cellar).unwrap();
    let real = script(&cellar, "rg", "echo rg");
    std::os::unix::fs::symlink(&real, b.path().join("rg")).unwrap();

    let path = std::env::join_paths([a.path(), b.path()]).unwrap();
    let found = executables_in(&path);
    let names: Vec<&str> = found.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["double", "outil-maison", "rg"]);
    let double = &found[0];
    assert_eq!(
        double.path,
        a.path().join("double"),
        "le premier du PATH gagne"
    );
    assert!(!double.system);
    let rg = &found[2];
    assert!(
        rg.target
            .to_string_lossy()
            .contains("Cellar/ripgrep/14.1.0"),
        "la cible du lien dit le gestionnaire : {:?}",
        rg.target
    );
}

/// Une application factice avec son `Info.plist` : nom du paquet, identifiant, version.
/// Un paquet sans `Info.plist` lisible reste listé sous son nom.
#[test]
fn an_application_is_read_from_its_info_plist() {
    let dir = tempfile::tempdir().unwrap();
    let contents = dir.path().join("Navigateur Maison.app/Contents");
    std::fs::create_dir_all(&contents).unwrap();
    std::fs::write(
        contents.join("Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key>
  <string>fr.exemple.navigateur</string>
  <key>CFBundleName</key>
  <string>Navigateur</string>
  <key>CFBundleShortVersionString</key>
  <string>27.1 &amp; plus</string>
</dict></plist>"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("Muette.app")).unwrap();
    std::fs::create_dir_all(dir.path().join("pas-une-app")).unwrap();

    let apps = applications(&[dir.path().to_path_buf()], T);
    assert_eq!(apps.len(), 2, "{apps:?}");
    assert_eq!(apps[0].name, "Muette");
    assert_eq!(apps[0].version, None);
    let nav = &apps[1];
    assert_eq!(nav.name, "Navigateur Maison");
    assert_eq!(nav.bundle_id.as_deref(), Some("fr.exemple.navigateur"));
    assert_eq!(nav.version.as_deref(), Some("27.1 & plus"));
}

/// `CFBundleVersion` sert de repli, le nom déclaré aussi.
#[test]
fn a_plist_falls_back_on_the_build_version() {
    let info = parse_info_plist(
        "<dict><key>CFBundleDisplayName</key><string>Xcode</string>\
         <key>CFBundleVersion</key>\n   <string>24500</string></dict>",
    );
    assert_eq!(info.name.as_deref(), Some("Xcode"));
    assert_eq!(info.version.as_deref(), Some("24500"));
    assert_eq!(info.bundle_id, None);
}

/// La sortie de `system_profiler` relevée sur un MacBook Air M2 (numéro de série et
/// écrans retirés, ils ne sont jamais lus) et celle d'un M5 Max.
#[test]
fn the_hardware_is_read_from_system_profiler() {
    let m2 = r#"{"SPDisplaysDataType":[{"_name":"Apple M2","sppci_cores":"10",
        "sppci_device_type":"spdisplays_gpu","sppci_model":"Apple M2"}],
      "SPHardwareDataType":[{"_name":"hardware_overview","chip_type":"Apple M2",
        "machine_name":"MacBook Air","number_processors":"proc 8:0:4:4",
        "physical_memory":"16 GB"}]}"#;
    let h = parse_system_profiler(m2).unwrap();
    assert_eq!(h.chip.as_deref(), Some("Apple M2"));
    assert_eq!(h.model.as_deref(), Some("MacBook Air"));
    assert_eq!(h.memory_gb, Some(16));
    assert_eq!(h.cpu_cores, Some(8));
    assert_eq!(h.performance_cores, Some(4));
    assert_eq!(h.efficiency_cores, Some(4));
    assert_eq!(h.gpu_cores, Some(10));

    let m5 = r#"{"SPHardwareDataType":[{"chip_type":"Apple M5 Max",
        "machine_name":"MacBook Pro","number_processors":"proc 18:6:12",
        "physical_memory":"128 GB"}],
      "SPDisplaysDataType":[{"sppci_cores":40}]}"#;
    let h = parse_system_profiler(m5).unwrap();
    assert_eq!(h.memory_gb, Some(128));
    assert_eq!((h.cpu_cores, h.performance_cores), (Some(18), Some(6)));
    assert_eq!(h.gpu_cores, Some(40));
    assert!(parse_system_profiler("pas du json").is_none());
}

/// #260 : un faux `xcrun` qui trouve `mcpbridge` et un faux `safaridriver` qui annonce
/// `--mcp` sont deux MCP exposés ; un `safaridriver` sans l'option n'en est pas un.
#[cfg(unix)]
#[test]
fn fake_apple_tools_expose_their_mcp() {
    let dir = tempfile::tempdir().unwrap();
    script(
        dir.path(),
        "xcrun",
        r#"[ "$1" = "--find" ] && [ "$2" = "mcpbridge" ] && echo /Xcode/usr/bin/mcpbridge && exit 0
exit 1"#,
    );
    script(
        dir.path(),
        "safaridriver",
        "echo 'Usage: safaridriver [options]'\necho '  --mcp  Run as an MCP server'",
    );
    let path = dir.path().as_os_str();
    let offers = probe_offers(APPLE_OFFERS, path, T);
    assert_eq!(
        offers,
        vec![
            McpOffer {
                id: "safari".into(),
                app: "Safari".into(),
                command: "safaridriver".into(),
                args: vec!["--mcp".into()],
            },
            McpOffer {
                id: "xcode".into(),
                app: "Xcode".into(),
                command: "xcrun".into(),
                args: vec!["mcpbridge".into()],
            },
        ]
    );

    let old = tempfile::tempdir().unwrap();
    script(old.path(), "safaridriver", "echo 'Usage: safaridriver -p'");
    script(
        old.path(),
        "xcrun",
        "echo 'xcrun: error: unable to find utility' >&2\nexit 72",
    );
    assert!(probe_offers(APPLE_OFFERS, old.path().as_os_str(), T).is_empty());
}

/// Hors macOS, pas de dossier d'applications ni de sonde du matériel : un stub explicite,
/// pas une erreur.
#[cfg(not(target_os = "macos"))]
#[test]
fn outside_macos_the_discovery_is_an_explicit_stub() {
    assert!(application_dirs(Some(Path::new("/home/moi"))).is_empty());
    assert!(hardware(T).is_none());
    assert!(offer_probes().is_empty());
}
