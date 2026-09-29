//! Découverte de l'environnement (issue #260) : ce que la machine offre au-delà d'une
//! liste connue de binaires.
//!
//! Le reste du workspace ne sait pas où macOS range ses applications ni comment lire un
//! `Info.plist`, ni que le matériel se lit par `system_profiler` : tout passe par ici.
//! Chaque fonction est **bloquante** et bornée par un délai court ; elles tournent au
//! démarrage, toutes les heures et dans `doctor`, jamais pendant un tour de conversation.
//!
//! Découvrir n'autorise rien : ce module lit des dossiers et lance des sondes en lecture
//! (`--help`, `--find`, `-json`), jamais l'outil trouvé pour de vrai.

use crate::process::probe_command;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Un exécutable trouvé dans un dossier du PATH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    pub name: String,
    pub path: PathBuf,
    /// Cible du lien résolue : c'est elle qui dit le gestionnaire (`Cellar/<formule>`,
    /// `node_modules/<paquet>`, `uv/tools/<outil>`). Le chemin lui-même sinon.
    pub target: PathBuf,
    /// Dans un dossier du système (`/usr/bin`, `/bin`…) : installé avec l'OS.
    pub system: bool,
}

/// Les exécutables des dossiers de `path`, dans l'ordre du PATH : le premier d'un nom
/// gagne, comme pour le shell. Rendus triés par nom.
pub fn executables_in(path: &OsStr) -> Vec<Executable> {
    let system = system_bin_dirs();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in std::env::split_paths(path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<(String, PathBuf)> = entries
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
            .filter(|(n, p)| !n.starts_with('.') && is_executable(p))
            .collect();
        names.sort();
        for (name, p) in names {
            if !seen.insert(name.clone()) {
                continue;
            }
            let target = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
            out.push(Executable {
                name,
                path: p,
                target,
                system: system.contains(&dir),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file())
        && p.extension().is_some_and(|e| {
            ["exe", "cmd", "bat", "com"].contains(&e.to_string_lossy().to_lowercase().as_str())
        })
}

/// Dossiers d'exécutables livrés avec l'OS.
pub fn system_bin_dirs() -> Vec<PathBuf> {
    if cfg!(unix) {
        ["/bin", "/sbin", "/usr/bin", "/usr/sbin"]
            .map(PathBuf::from)
            .to_vec()
    } else {
        Vec::new()
    }
}

/// Une application installée.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub path: String,
}

/// Où chercher les applications. macOS : `/Applications`, `/System/Applications`, leurs
/// `Utilities` et `~/Applications`. Ailleurs, aucun dossier : les applications de bureau
/// n'y ont pas d'emplacement commun, la carte se contente du PATH.
pub fn application_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut v: Vec<PathBuf> = [
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ]
    .map(PathBuf::from)
    .to_vec();
    if let Some(h) = home {
        v.push(h.join("Applications"));
    }
    v
}

/// Les applications (`*.app`) posées directement dans ces dossiers, avec ce que dit leur
/// `Info.plist`. Une même application vue deux fois (même identifiant) ne compte qu'une.
pub fn applications(dirs: &[PathBuf], timeout: Duration) -> Vec<App> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut bundles: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "app") && p.is_dir())
            .collect();
        bundles.sort();
        for bundle in bundles {
            let app = read_app(&bundle, timeout);
            let key = app.bundle_id.clone().unwrap_or_else(|| app.name.clone());
            if seen.insert(key) {
                out.push(app);
            }
        }
    }
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

fn read_app(bundle: &Path, timeout: Duration) -> App {
    let stem = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let plist = bundle.join("Contents").join("Info.plist");
    let info = std::fs::read(&plist)
        .ok()
        .and_then(|raw| plist_xml(&plist, raw, timeout))
        .map(|xml| parse_info_plist(&xml))
        .unwrap_or_default();
    App {
        // Le nom du paquet est celui que le propriétaire voit dans le Finder ; le nom
        // déclaré n'est qu'un repli.
        name: if stem.is_empty() {
            info.name.unwrap_or_default()
        } else {
            stem
        },
        bundle_id: info.bundle_id,
        version: info.version,
        path: bundle.to_string_lossy().to_string(),
    }
}

/// Le texte XML d'un `Info.plist`. Un plist binaire (`bplist00`) est converti par
/// `plutil` sur macOS ; ailleurs il reste illisible.
fn plist_xml(path: &Path, raw: Vec<u8>, timeout: Duration) -> Option<String> {
    if !raw.starts_with(b"bplist") {
        return String::from_utf8(raw).ok();
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let p = probe_command(
        Path::new("/usr/bin/plutil"),
        &["-convert", "xml1", "-o", "-", &path.to_string_lossy()],
        timeout,
    )?;
    p.ok.then_some(p.stdout)
}

/// Ce qu'on retient d'un `Info.plist`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlistInfo {
    pub name: Option<String>,
    pub bundle_id: Option<String>,
    pub version: Option<String>,
}

/// Lit les trois clés utiles d'un `Info.plist` XML, sans analyseur complet : la valeur
/// d'une clé est la chaîne qui la suit.
pub fn parse_info_plist(xml: &str) -> PlistInfo {
    let first = |keys: &[&str]| keys.iter().find_map(|k| plist_string(xml, k));
    PlistInfo {
        name: first(&["CFBundleDisplayName", "CFBundleName"]),
        bundle_id: first(&["CFBundleIdentifier"]),
        version: first(&["CFBundleShortVersionString", "CFBundleVersion"]),
    }
}

fn plist_string(xml: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let rest = xml[xml.find(&tag)? + tag.len()..].trim_start();
    let value = rest.strip_prefix("<string>")?;
    let value = &value[..value.find("</string>")?];
    let clean = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}

/// Le matériel qui décide des modèles locaux possibles (#259).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hardware {
    /// `MacBook Pro`, `Mac mini`…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `Apple M5 Max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chip: Option<String>,
    /// Mémoire vive, unifiée sur Apple Silicon, en Go.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_gb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efficiency_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_cores: Option<u32>,
}

/// Le matériel de la machine. macOS : `system_profiler` (matériel et cartes graphiques),
/// dont on ne garde que ces champs, jamais le numéro de série ni les écrans branchés.
/// Ailleurs : `None`, faute de source équivalente.
pub fn hardware(timeout: Duration) -> Option<Hardware> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let p = probe_command(
        Path::new("/usr/sbin/system_profiler"),
        &["SPHardwareDataType", "SPDisplaysDataType", "-json"],
        timeout,
    )?;
    p.ok.then(|| parse_system_profiler(&p.stdout)).flatten()
}

/// Lit la sortie `-json` de `system_profiler SPHardwareDataType SPDisplaysDataType`.
pub fn parse_system_profiler(json: &str) -> Option<Hardware> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let hw = v.get("SPHardwareDataType")?.as_array()?.first()?;
    let text = |k: &str| {
        hw.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let mut h = Hardware {
        model: text("machine_name"),
        // Apple Silicon : `chip_type` ; Intel : `cpu_type`.
        chip: text("chip_type").or_else(|| text("cpu_type")),
        memory_gb: text("physical_memory").and_then(|m| leading_number(&m)),
        ..Default::default()
    };
    // `proc 14:10:4` (total, performance, efficacité) ; `proc 8:0:4:4` sur les puces qui
    // annoncent un niveau de plus : le total en tête, les deux derniers pour P et E. Un
    // Mac Intel rend un entier.
    match hw.get("number_processors") {
        Some(serde_json::Value::String(s)) => {
            let nums: Vec<u32> = s
                .trim_start_matches("proc")
                .trim()
                .split(':')
                .filter_map(|n| n.trim().parse().ok())
                .collect();
            h.cpu_cores = nums.first().copied();
            if nums.len() >= 3 {
                h.performance_cores = nums.get(nums.len() - 2).copied();
                h.efficiency_cores = nums.last().copied();
            }
        }
        Some(n) => h.cpu_cores = n.as_u64().and_then(|n| u32::try_from(n).ok()),
        None => {}
    }
    h.gpu_cores = v
        .get("SPDisplaysDataType")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|gpu| gpu.get("sppci_cores"))
        .find_map(|c| match c {
            serde_json::Value::String(s) => s.trim().parse().ok(),
            n => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        });
    Some(h)
}

fn leading_number(s: &str) -> Option<u32> {
    s.split_whitespace().next()?.parse().ok()
}

/// Un serveur MCP qu'une application **expose** sans être déclaré : à proposer au
/// propriétaire, jamais branché d'office.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpOffer {
    /// Identifiant stable (`safari`, `xcode`) : c'est lui qui retient une proposition.
    pub id: String,
    /// L'application qui l'expose.
    pub app: String,
    pub command: String,
    pub args: Vec<String>,
}

/// Comment reconnaître un MCP exposé : la commande à sonder, ses arguments, et ce que sa
/// sortie doit contenir (vide : il suffit qu'elle réussisse et dise quelque chose).
#[derive(Debug, Clone, Copy)]
pub struct OfferProbe {
    pub id: &'static str,
    pub app: &'static str,
    pub program: &'static str,
    pub probe: &'static [&'static str],
    pub needle: &'static str,
    pub args: &'static [&'static str],
}

/// Les MCP exposés par les outils d'Apple : Safari 27 (`safaridriver --mcp`, stdio) et
/// le pont de Xcode (`xcrun mcpbridge`). Sondés par leur aide ou leur emplacement, sans
/// démarrer le serveur.
pub const APPLE_OFFERS: &[OfferProbe] = &[
    OfferProbe {
        id: "safari",
        app: "Safari",
        program: "safaridriver",
        probe: &["--help"],
        needle: "--mcp",
        args: &["--mcp"],
    },
    OfferProbe {
        id: "xcode",
        app: "Xcode",
        program: "xcrun",
        probe: &["--find", "mcpbridge"],
        needle: "",
        args: &["mcpbridge"],
    },
];

/// Les sondes de MCP exposés propres à cet OS. macOS : [`APPLE_OFFERS`] ; ailleurs,
/// aucune sonde connue.
pub fn offer_probes() -> &'static [OfferProbe] {
    if cfg!(target_os = "macos") {
        APPLE_OFFERS
    } else {
        &[]
    }
}

/// Sonde chaque candidat trouvé dans `path`.
pub fn probe_offers(candidates: &[OfferProbe], path: &OsStr, timeout: Duration) -> Vec<McpOffer> {
    candidates
        .iter()
        .filter_map(|c| {
            let program = crate::process::which_in(c.program, path)?;
            let p = probe_command(&program, c.probe, timeout)?;
            let text = format!("{}\n{}", p.stdout, p.stderr);
            let found = if c.needle.is_empty() {
                p.ok && !p.stdout.trim().is_empty()
            } else {
                text.contains(c.needle)
            };
            found.then(|| McpOffer {
                id: c.id.into(),
                app: c.app.into(),
                command: c.program.into(),
                args: c.args.iter().map(|a| a.to_string()).collect(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
