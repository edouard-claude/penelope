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
    let mut args = format!(
        "        <string>{}</string>\n        <string>daemon</string>\n",
        xml_escape(&exe.to_string_lossy())
    );
    if let Some(h) = home {
        args.push_str(&format!(
            "        <string>--home</string>\n        <string>{}</string>\n",
            xml_escape(&h.to_string_lossy())
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SERVICE_LABEL}</string>
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
        <key>PENELOPE_SERVICE</key>
        <string>1</string>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
</dict>
</plist>
"#,
        out = xml_escape(&logs.join("daemon.out.log").to_string_lossy()),
        err = xml_escape(&logs.join("daemon.err.log").to_string_lossy()),
        path = xml_escape(path),
    )
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

    #[test]
    fn systemd_unit_restarts_always() {
        let u = systemd_unit(Path::new("/usr/bin/penelope"), None);
        assert!(u.contains("Restart=always"));
        assert!(u.contains("RestartSec=2"));
    }
}
