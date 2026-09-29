//! Installation en service système (§2.3).
//!
//! macOS : LaunchAgent utilisateur `com.penelope.daemon.plist` (`KeepAlive=true`,
//! `RunAtLoad=true`, `ProcessType=Interactive`).
//! Linux/Windows : conception de référence, backends non livrés (§2.1).

use crate::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub installed: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub mechanism: String,
    pub unit_path: Option<PathBuf>,
    pub detail: String,
}

pub trait ServiceManager: Send + Sync {
    fn mechanism(&self) -> &'static str;
    fn install(&self, exe: &std::path::Path, home: Option<&std::path::Path>) -> Result<PathBuf>;
    fn uninstall(&self) -> Result<()>;
    fn start(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn restart(&self) -> Result<()> {
        self.stop()?;
        self.start()
    }
    fn status(&self) -> Result<ServiceStatus>;
}

/// Identifiant du service, identique sur les trois OS.
pub const SERVICE_LABEL: &str = "com.penelope.daemon";

/// Génère le `plist` du LaunchAgent macOS.
///
/// `path` est le PATH du terminal qui lance `penelope install`, enrichi : `launchd` ne
/// fournit sinon que les répertoires système, et le daemon ne trouverait ni `npx`, ni
/// `uvx`, ni `docker`.
pub fn launchd_plist(
    exe: &std::path::Path,
    home: Option<&std::path::Path>,
    logs: &std::path::Path,
    path: &str,
) -> String {
    let mut args = vec![exe.to_string_lossy().to_string(), "daemon".to_string()];
    if let Some(h) = home {
        args.push("--home".into());
        args.push(h.to_string_lossy().to_string());
    }
    agent_plist(
        SERVICE_LABEL,
        &args,
        &logs.join("daemon.out.log"),
        &logs.join("daemon.err.log"),
        &[("PENELOPE_SERVICE", "1"), ("PATH", path)],
    )
}

/// `plist` d'un LaunchAgent relancé par launchd (`KeepAlive`), au démarrage de session
/// (`RunAtLoad`), sans bridage (`ProcessType=Interactive`) : le daemon, ou un serveur
/// d'inférence local (#259).
pub fn agent_plist(
    label: &str,
    args: &[String],
    out: &std::path::Path,
    err: &std::path::Path,
    env: &[(&str, &str)],
) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("        <string>{}</string>\n", xml_escape(a)))
        .collect();
    let env: String = env
        .iter()
        .map(|(k, v)| {
            format!(
                "        <key>{}</key>\n        <string>{}</string>\n",
                xml_escape(k),
                xml_escape(v)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
    <key>EnvironmentVariables</key>
    <dict>
{env}    </dict>
</dict>
</plist>
"#,
        label = xml_escape(label),
        out = xml_escape(&out.to_string_lossy()),
        err = xml_escape(&err.to_string_lossy()),
    )
}

/// Label du LaunchAgent qui fait tourner le serveur d'inférence d'un endpoint (#259) :
/// `com.penelope.inference.local` pour `providers.local`.
pub fn inference_label(endpoint: &str) -> String {
    format!("com.penelope.inference.{endpoint}")
}

/// Fichier d'un LaunchAgent de l'utilisateur, d'après son label.
pub fn launch_agent_path(label: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(h)
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"))
    })
}

/// Fichier du LaunchAgent de l'utilisateur (`~/Library/LaunchAgents/com.penelope.daemon.plist`).
pub fn launchd_plist_path() -> Option<PathBuf> {
    launch_agent_path(SERVICE_LABEL)
}

/// Programme lancé par un `plist` : première chaîne de `ProgramArguments`.
pub fn launchd_program(plist: &str) -> Option<String> {
    let after = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    Some(
        after[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// `plist` dont le programme lancé devient `exe` ; `None` si le fichier n'en déclare pas.
pub fn launchd_with_program(plist: &str, exe: &std::path::Path) -> Option<String> {
    let current = launchd_program(plist)?;
    let marker = "<key>ProgramArguments</key>";
    let at = plist.find(marker)? + marker.len();
    let old = format!("<string>{}</string>", xml_escape(&current));
    let rest = &plist[at..];
    let pos = rest.find(&old)?;
    let new = format!("<string>{}</string>", xml_escape(&exe.to_string_lossy()));
    Some(format!(
        "{}{}{}{}",
        &plist[..at],
        &rest[..pos],
        new,
        &rest[pos + old.len()..]
    ))
}

/// Unité systemd utilisateur (conception de référence, §2.3).
pub fn systemd_unit(exe: &std::path::Path, home: Option<&std::path::Path>) -> String {
    let home_arg = home
        .map(|h| format!(" --home {}", h.display()))
        .unwrap_or_default();
    format!(
        "[Unit]\n\
         Description=Penelope, agent personnel autonome\n\
         After=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={}{home_arg} daemon\n\
         Restart=always\n\
         RestartSec=2\n\
         Environment=PENELOPE_SERVICE=1\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display()
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_launched_program_is_read_and_rewritten() {
        let p = launchd_plist(
            Path::new("/Users/x/code/penelope/target/release/penelope"),
            Some(Path::new("/Users/x/Penelope & co")),
            Path::new("/tmp/logs"),
            "/usr/bin:/bin",
        );
        assert_eq!(
            launchd_program(&p).as_deref(),
            Some("/Users/x/code/penelope/target/release/penelope")
        );
        let moved = launchd_with_program(&p, Path::new("/Users/x/.local/bin/penelope")).unwrap();
        assert_eq!(
            launchd_program(&moved).as_deref(),
            Some("/Users/x/.local/bin/penelope")
        );
        assert!(moved.contains("<string>daemon</string>"));
        assert!(moved.contains("Penelope &amp; co"), "le reste est gardé");
        assert_eq!(
            moved.matches("<string>").count(),
            p.matches("<string>").count()
        );
        assert!(launchd_program("<plist/>").is_none());
    }

    #[test]
    fn plist_has_the_prd_keys() {
        let p = launchd_plist(
            Path::new("/usr/local/bin/penelope"),
            None,
            Path::new("/tmp/logs"),
            "/opt/homebrew/bin:/usr/bin:/bin",
        );
        assert!(p.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(
            p.contains("<key>PATH</key>\n        <string>/opt/homebrew/bin:/usr/bin:/bin</string>")
        );
        assert!(p.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(p.contains("<string>Interactive</string>"));
        assert!(p.contains("com.penelope.daemon"));
        assert!(p.contains("<string>daemon</string>"));
    }

    #[test]
    fn plist_passes_home_when_set() {
        let p = launchd_plist(
            Path::new("/usr/local/bin/penelope"),
            Some(Path::new("/srv/penelope")),
            Path::new("/tmp/logs"),
            "/usr/bin",
        );
        assert!(p.contains("--home"));
        assert!(p.contains("/srv/penelope"));
    }

    #[test]
    fn plist_escapes_xml() {
        let p = launchd_plist(
            Path::new("/opt/a&b/penelope"),
            None,
            Path::new("/tmp"),
            "/usr/bin",
        );
        assert!(p.contains("/opt/a&amp;b/penelope"));
    }

    /// #259 : le serveur d'inférence a son label, ses arguments et ses journaux, et
    /// launchd le relance comme le daemon.
    #[test]
    fn an_inference_agent_is_kept_alive_under_its_own_label() {
        let label = inference_label("local");
        let args = [
            "/opt/mlx/bin/mlx_lm.server",
            "--model",
            "mlx-community/Qwen3-8B-4bit",
        ]
        .map(String::from);
        let p = agent_plist(
            &label,
            &args,
            Path::new("/tmp/logs/inference-local.out.log"),
            Path::new("/tmp/logs/inference-local.err.log"),
            &[("PATH", "/usr/bin")],
        );
        assert!(p.contains("<string>com.penelope.inference.local</string>"));
        assert!(p.contains("<key>KeepAlive</key>\n    <true/>"));
        assert_eq!(
            launchd_program(&p).as_deref(),
            Some("/opt/mlx/bin/mlx_lm.server")
        );
        assert!(p.contains("<string>mlx-community/Qwen3-8B-4bit</string>"));
        assert!(p.contains("inference-local.err.log"));
        assert!(!p.contains("PENELOPE_SERVICE"), "ce n'est pas le daemon");
        assert!(launch_agent_path(&label).is_some_and(|f| {
            f.ends_with("Library/LaunchAgents/com.penelope.inference.local.plist")
        }));
    }

    #[test]
    fn systemd_unit_restarts_always() {
        let u = systemd_unit(Path::new("/usr/bin/penelope"), None);
        assert!(u.contains("Restart=always"));
        assert!(u.contains("RestartSec=2"));
    }
}
