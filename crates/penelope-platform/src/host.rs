//! État de la machine hôte : alimentation, batterie, disque, mémoire, charge, système.
//!
//! Sert à `self_status` : Pénélope doit pouvoir dire sur quelle machine elle tourne et
//! dans quel état elle est, batterie comprise. Sur macOS, les valeurs viennent des
//! outils système documentés (`pmset`, `sysctl`, `sw_vers`, `scutil`, `df`) : pas de
//! FFI, donc pas d'`unsafe`. Chaque mesure est facultative : une commande absente laisse
//! le champ vide au lieu de faire échouer l'ensemble.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    /// `charging`, `discharging`, `charged`, `finishing charge`, `AC attached`…
    pub state: String,
    /// Autonomie ou temps de charge restant annoncé (`6:27`), s'il est connu.
    pub remaining: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostStatus {
    pub hostname: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu: Option<String>,
    pub cores: Option<u32>,
    pub memory_gb: Option<f64>,
    /// Charge moyenne sur 1, 5 et 15 minutes.
    pub load_avg: Vec<f64>,
    pub uptime_s: Option<u64>,
    /// `AC Power` (secteur) ou `Battery Power`.
    pub power_source: Option<String>,
    pub battery: Option<Battery>,
    pub disk_free_gb: Option<f64>,
    pub disk_total_gb: Option<f64>,
}

impl HostStatus {
    pub fn on_ac_power(&self) -> Option<bool> {
        self.power_source.as_deref().map(|s| s.contains("AC"))
    }
}

/// Mesure l'état de la machine. `data_dir` désigne le volume dont on veut l'espace libre.
/// Bloquant (quelques commandes système) : à appeler hors des fils asynchrones.
pub fn status(data_dir: &Path, now_unix: i64) -> HostStatus {
    let mut h = HostStatus {
        arch: Some(std::env::consts::ARCH.to_string()),
        ..Default::default()
    };

    if let Some((free, total)) = run("/bin/df", &["-k", &data_dir.to_string_lossy()])
        .as_deref()
        .and_then(parse_df)
    {
        h.disk_free_gb = Some(free);
        h.disk_total_gb = Some(total);
    }

    #[cfg(target_os = "macos")]
    {
        h.hostname = run("/usr/sbin/scutil", &["--get", "ComputerName"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        h.os = run("/usr/bin/sw_vers", &["-productVersion"]).map(|v| format!("macOS {}", v.trim()));
        h.cpu = sysctl("machdep.cpu.brand_string");
        h.cores = sysctl("hw.ncpu").and_then(|v| v.parse().ok());
        h.memory_gb = sysctl("hw.memsize")
            .and_then(|v| v.parse::<f64>().ok())
            .map(|b| (b / 1024.0 / 1024.0 / 1024.0 * 10.0).round() / 10.0);
        h.load_avg = sysctl("vm.loadavg")
            .map(|v| parse_loadavg(&v))
            .unwrap_or_default();
        h.uptime_s = sysctl("kern.boottime")
            .and_then(|v| parse_boottime(&v))
            .map(|boot| (now_unix - boot).max(0) as u64);
        if let Some(text) = run("/usr/bin/pmset", &["-g", "batt"]) {
            let (source, battery) = parse_pmset_batt(&text);
            h.power_source = source;
            h.battery = battery;
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = now_unix;
        h.hostname = run("hostname", &[])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        h.os = Some(std::env::consts::OS.to_string());
    }

    h
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(target_os = "macos")]
fn sysctl(key: &str) -> Option<String> {
    run("/usr/sbin/sysctl", &["-n", key])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `pmset -g batt` (une tabulation sépare l'identifiant de la charge) :
///
/// ```text
/// Now drawing from 'Battery Power'
///  -InternalBattery-0 (id=22610019)<TAB>99%; discharging; 6:27 remaining present: true
/// ```
pub fn parse_pmset_batt(text: &str) -> (Option<String>, Option<Battery>) {
    let source = text.lines().next().and_then(|l| {
        let start = l.find('\'')?;
        let end = l.rfind('\'')?;
        (end > start).then(|| l[start + 1..end].to_string())
    });
    let battery = text
        .lines()
        .find(|l| l.contains("InternalBattery"))
        .and_then(|l| {
            let after_tab = l.split('\t').nth(1).unwrap_or(l);
            let mut parts = after_tab.split(';').map(str::trim);
            let percent = parts
                .next()?
                .trim_end_matches('%')
                .trim()
                .parse::<u8>()
                .ok()?;
            let state = parts.next().unwrap_or("").to_string();
            let remaining = parts
                .next()
                .and_then(|r| r.split_whitespace().next())
                .filter(|t| t.contains(':') && !t.starts_with("(no"))
                .map(String::from);
            Some(Battery {
                percent,
                state,
                remaining,
            })
        });
    (source, battery)
}

/// `sysctl -n kern.boottime` : `{ sec = 1789098903, usec = 730831 } Fri Sep 11 …`.
pub fn parse_boottime(text: &str) -> Option<i64> {
    let after = text.split("sec =").nth(1)?;
    after.split(',').next()?.trim().parse().ok()
}

/// `sysctl -n vm.loadavg` : `{ 3.37 4.08 4.44 }`.
pub fn parse_loadavg(text: &str) -> Vec<f64> {
    text.trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect()
}

/// `df -k <chemin>` : renvoie (libre, total) en Go.
pub fn parse_df(text: &str) -> Option<(f64, f64)> {
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let total_kb: f64 = cols.get(1)?.parse().ok()?;
    let avail_kb: f64 = cols.get(3)?.parse().ok()?;
    let gb = |kb: f64| (kb / 1024.0 / 1024.0 * 10.0).round() / 10.0;
    Some((gb(avail_kb), gb(total_kb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_on_battery_power() {
        let t = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=22610019)\t99%; discharging; 6:27 remaining present: true\n";
        let (source, b) = parse_pmset_batt(t);
        assert_eq!(source.as_deref(), Some("Battery Power"));
        let b = b.unwrap();
        assert_eq!(b.percent, 99);
        assert_eq!(b.state, "discharging");
        assert_eq!(b.remaining.as_deref(), Some("6:27"));
    }

    #[test]
    fn battery_on_ac_power_while_estimating() {
        let t = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t100%; charged; 0:00 remaining present: true\n";
        let (source, b) = parse_pmset_batt(t);
        assert_eq!(source.as_deref(), Some("AC Power"));
        assert_eq!(b.as_ref().unwrap().state, "charged");
        let t = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=4653155)\t80%; charging; (no estimate) present: true\n";
        let (_, b) = parse_pmset_batt(t);
        assert_eq!(b.unwrap().remaining, None);
    }

    #[test]
    fn a_desktop_mac_has_no_battery() {
        let (source, b) = parse_pmset_batt("Now drawing from 'AC Power'\n");
        assert_eq!(source.as_deref(), Some("AC Power"));
        assert!(b.is_none());
    }

    #[test]
    fn sysctl_and_df_outputs_are_read() {
        assert_eq!(
            parse_boottime("{ sec = 1789098903, usec = 730831 } Fri Sep 11 07:55:03 2026"),
            Some(1789098903)
        );
        assert_eq!(parse_loadavg("{ 3.37 4.08 4.44 }"), vec![3.37, 4.08, 4.44]);
        let df = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n/dev/disk3s5 971350180 612345678 339004502 65% 1 2 0% /System/Volumes/Data\n";
        let (free, total) = parse_df(df).unwrap();
        assert!((free - 323.3).abs() < 0.1, "{free}");
        assert!((total - 926.4).abs() < 0.1, "{total}");
    }

    #[test]
    fn status_never_fails_even_without_tools() {
        let h = status(Path::new("/nonexistent/penelope"), 0);
        assert!(h.arch.is_some());
    }
}
