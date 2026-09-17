//! Issue #36 : relais de mise à jour contre le vrai launchd (macOS).
//!
//! Un faux daemon tourne sous un LaunchAgent de test ; c'est **lui** qui charge le relais,
//! depuis son propre job, comme le fait `/upgrade install`. Le service doit ensuite tourner
//! sur le chemin stable sans intervention, ou revenir à l'ancien binaire si le nouveau ne
//! démarre pas.
//!
//! Charge des jobs launchd dans la session : lancé seulement avec
//! `PENELOPE_LAUNCHD_TESTS=1` (la CI macOS le fait).
//!
//! ```bash
//! PENELOPE_LAUNCHD_TESTS=1 cargo test -p penelope-platform --test launchd_relay
//! ```

#![cfg(target_os = "macos")]

use penelope_platform::handoff::{HandOff, LAUNCHCTL, gui_domain};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var("PENELOPE_LAUNCHD_TESTS").as_deref() == Ok("1")
}

fn launchctl(args: &[&str]) -> (bool, String) {
    let out = Command::new(LAUNCHCTL).args(args).output().unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Domaine de la session graphique, sinon celui de l'utilisateur (runner sans session).
fn domain() -> String {
    let gui = gui_domain().unwrap();
    if launchctl(&["print", &gui]).0 {
        return gui;
    }
    gui.replacen("gui/", "user/", 1)
}

fn q(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

fn write_exec(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// LaunchAgent de test : relancé seulement après une sortie en erreur, pour qu'un test
/// interrompu ne laisse pas un job relancé à l'infini.
fn service_plist(label: &str, program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
</dict>
</plist>
"#,
        program = program.display()
    )
}

/// Décharge les jobs de test, même si le test échoue.
struct Cleanup {
    targets: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for t in &self.targets {
            let _ = launchctl(&["bootout", t]);
        }
    }
}

fn wait_for(what: &str, timeout: Duration, mut ok: impl FnMut() -> bool, log: &Path) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "{what} : délai dépassé. Journal du relais :\n{}",
        std::fs::read_to_string(log).unwrap_or_default()
    );
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn scenario(new_starts: bool) {
    if !enabled() {
        eprintln!("PENELOPE_LAUNCHD_TESTS=1 pour lancer ce test (jobs launchd réels)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let d = std::fs::canonicalize(dir.path()).unwrap();
    let domain = domain();
    let label = format!(
        "com.penelope.test.relay.{}.{}",
        if new_starts { "ok" } else { "ko" },
        std::process::id()
    );
    let _cleanup = Cleanup {
        targets: vec![
            format!("{domain}/{label}"),
            format!("{domain}/{label}.reloader"),
        ],
    };

    let build = d.join("code/target/release/penelope");
    let stable = d.join("bin/penelope");
    let plist = d.join(format!("{label}.plist"));
    let backup = PathBuf::from(format!("{}.sources", plist.display()));
    let state = d.join("state/upgrade.json");
    let note = d.join("state/upgrade.rolled-back");
    let mark = d.join("running");
    let trigger = d.join("trigger");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();

    let h = HandOff {
        domain: domain.clone(),
        service_label: label.clone(),
        service_plist: plist.clone(),
        backup_plist: Some(backup.clone()),
        reload: true,
        binary: stable.clone(),
        previous: Some(build.clone()),
        state_file: state.clone(),
        rolled_back_note: note.clone(),
        to_version: "9.9.9".into(),
        from_version: "0.13.0".into(),
        delay_s: 1,
        guard_s: if new_starts { 30 } else { 6 },
        work_dir: d.join("relay"),
        launchctl: PathBuf::from(LAUNCHCTL),
    };

    // Ancien daemon (binaire de compilation) : sur demande, il charge le relais depuis son
    // propre job, exactement comme le daemon réel. Il met cinq secondes à s'arrêter, comme
    // un daemon qui termine proprement : c'est ce qui faisait échouer le `bootstrap`
    // enchaîné juste après `bootout`.
    write_exec(
        &build,
        &format!(
            "#!/bin/sh\necho old > {mark}\n\
             trap 'sleep 5; exit 0' TERM\n\
             i=0\nwhile [ $i -lt 1200 ] && [ -d {dir} ]; do\n\
             if [ -f {trigger} ]; then rm -f {trigger}; {launchctl} bootstrap {domain} {relay}; fi\n\
             sleep 0.1; i=$((i + 1))\ndone\nexit 0\n",
            mark = q(&mark),
            dir = q(&d),
            trigger = q(&trigger),
            launchctl = LAUNCHCTL,
            domain = domain,
            relay = q(&h.plist_path()),
        ),
    );
    let original = service_plist(&label, &build);
    std::fs::write(&plist, &original).unwrap();
    let (ok, out) = launchctl(&["bootstrap", &domain, &plist.to_string_lossy()]);
    assert!(ok, "bootstrap du service de test : {out}");
    wait_for(
        "ancien daemon démarré",
        Duration::from_secs(15),
        || read(&mark).trim() == "old",
        &h.log_path(),
    );

    // Mise à jour préparée comme par `switch_to_releases`.
    if new_starts {
        write_exec(
            &stable,
            &format!(
                "#!/bin/sh\necho new > {mark}\nprintf '{{\"first_boot_ms\": 1}}' > {state}\n\
                 i=0\nwhile [ $i -lt 1200 ] && [ -d {dir} ]; do sleep 0.1; i=$((i + 1)); done\n\
                 exit 0\n",
                mark = q(&mark),
                state = q(&state),
                dir = q(&d),
            ),
        );
    } else {
        write_exec(&stable, "#!/bin/sh\nexit 1\n");
    }
    std::fs::write(&backup, &original).unwrap();
    std::fs::write(
        &state,
        "{\n  \"to_version\": \"9.9.9\",\n  \"first_boot_ms\": null\n}",
    )
    .unwrap();
    h.write().unwrap();
    std::fs::write(&plist, service_plist(&label, &stable)).unwrap();
    std::fs::remove_file(&mark).unwrap();
    std::fs::write(&trigger, "").unwrap();

    let target = format!("{domain}/{label}");
    let running = || {
        let (ok, out) = launchctl(&["print", &target]);
        ok && out.contains("state = running")
    };
    if new_starts {
        wait_for(
            "service relancé sur le chemin stable",
            Duration::from_secs(60),
            || read(&mark).trim() == "new" && running(),
            &h.log_path(),
        );
        wait_for(
            "relais terminé",
            Duration::from_secs(30),
            || read(&h.log_path()).contains("fin du relais"),
            &h.log_path(),
        );
        let (_, out) = launchctl(&["print", &target]);
        assert!(out.contains(&stable.to_string_lossy().to_string()), "{out}");
        assert!(!note.exists());
        assert!(!backup.exists(), "sauvegarde retirée après démarrage");
    } else {
        wait_for(
            "ancien binaire remis et relancé",
            Duration::from_secs(90),
            || read(&mark).trim() == "old" && running() && note.exists(),
            &h.log_path(),
        );
        assert_eq!(read(&note), "9.9.9\n0.13.0\n");
        assert_eq!(
            read(&stable),
            read(&build),
            "binaire précédent au chemin stable"
        );
        assert!(!state.exists());
        let (_, out) = launchctl(&["print", &target]);
        assert!(out.contains(&stable.to_string_lossy().to_string()), "{out}");
    }
}

#[test]
fn a_reload_asked_from_inside_the_service_job_brings_the_service_back() {
    scenario(true);
}

#[test]
fn a_new_binary_that_never_starts_is_replaced_by_the_previous_one() {
    scenario(false);
}
