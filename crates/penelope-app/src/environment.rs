//! La carte de l'environnement (issue #260).
//!
//! L'inventaire de #156 cherchait dix-sept binaires connus : tout le reste était
//! invisible, un outil installé par `npm -g` comme Safari, qui expose pourtant un serveur
//! MCP natif, ou un serveur d'inférence local qui tourne à côté. Pénélope dresse
//! maintenant la carte de sa machine : matériel, exécutables du PATH et des gestionnaires
//! avec leur source et leur version, applications et leur version, MCP exposés par des
//! applications, serveurs d'inférence locaux et leurs modèles.
//!
//! Les règles de `machine` tiennent toujours :
//!
//! - la passe tourne au démarrage, toutes les heures et dans `doctor`, jamais dans un
//!   tour ; chaque sonde a un délai court ;
//! - la carte vit dans `kv` ([`KV_KEY`]) et sort à la demande (`env_explore`) ; le message
//!   système n'en garde que des noms de familles, stables (voir `machine::Inventory`) ;
//! - découvrir n'autorise rien : lancer un outil trouvé passe par `shell_exec`, sa
//!   politique et ses approbations ; brancher un MCP ou un serveur se **propose** au
//!   propriétaire ([`propose`]), rien n'est branché d'office.

pub mod explore;
pub mod inference;
pub mod managers;
pub mod propose;

use crate::services::Services;
use penelope_platform::discover::{self, App, Hardware, McpOffer, OfferProbe};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// Clé de la carte dans le magasin clé/valeur.
pub const KV_KEY: &str = "machine.environment";

/// Délai d'une sonde courte (`--help`, `--find`, `system_profiler`, `plutil`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Délai d'un gestionnaire : `npm ls -g` prend une seconde sur une machine chargée.
const MANAGER_TIMEOUT: Duration = Duration::from_secs(8);

/// Un outil trouvé : un exécutable du PATH, ou un paquet sans exécutable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub name: String,
    /// `brew`, `brew-cask`, `npm`, `uv`, `pipx`, `cargo`, `système` ou `PATH`.
    pub source: String,
    /// Paquet d'origine quand il ne porte pas le nom de l'exécutable (`ripgrep` pour `rg`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Ce qu'une application ou un serveur offre au-delà de l'exécutable : un MCP, de
/// l'inférence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// `mcp` ou `inference`.
    pub kind: String,
    /// Identifiant stable : `safari`, `mcp.d/forge`, `inference:11434`.
    pub id: String,
    pub name: String,
    /// La commande (`safaridriver --mcp`) ou l'adresse (`http://127.0.0.1:11434/v1`).
    pub via: String,
    /// D'où on la tient : `application` (exposée par une application), `configuration`
    /// (déclarée), `outil` (moteur installé), `sonde` (un port qui a répondu).
    pub origin: String,
    /// Déjà branchée : déclarée dans `mcp.d` ou dans `[providers]`.
    pub declared: bool,
    /// Serveur d'inférence : a répondu pendant la passe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachable: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// Ce qui a changé d'une passe à l'autre, pour `doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    /// Horodatage de la passe de comparaison.
    pub since: String,
    /// Horodatage de la passe qui a vu les changements.
    pub at: String,
    pub appeared: Vec<String>,
    pub disappeared: Vec<String>,
    pub updated: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.appeared.is_empty() && self.disappeared.is_empty() && self.updated.is_empty()
    }
}

/// La carte.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<Hardware>,
    #[serde(default)]
    pub tools: Vec<Found>,
    #[serde(default)]
    pub apps: Vec<App>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    pub checked_at: String,
    /// Les derniers changements vus, gardés tant qu'une passe n'en voit pas d'autres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<Changes>,
}

impl Environment {
    /// Les applications qui exposent un MCP, pour la ligne T1 : des noms seulement.
    pub fn mcp_names(&self) -> Vec<String> {
        self.names(|c| c.kind == "mcp" && c.origin == "application")
    }

    /// Les moteurs d'inférence installés ou déclarés, pour la ligne T1. Un port qui répond
    /// sans moteur connu n'y entre pas : il va et vient avec le serveur, la ligne changerait
    /// à chaque démarrage.
    pub fn inference_names(&self) -> Vec<String> {
        self.names(|c| c.kind == "inference" && c.origin != "sonde")
    }

    fn names(&self, keep: impl Fn(&Capability) -> bool) -> Vec<String> {
        let mut v: Vec<String> = self
            .capabilities
            .iter()
            .filter(|c| keep(c))
            .map(|c| c.name.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// Où regarder. En production, la machine ([`Sources::of`]) ; en test, un faux PATH, de
/// fausses applications, pas de sonde du matériel ni des ports par défaut.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub path: OsString,
    pub app_dirs: Vec<PathBuf>,
    pub offers: Vec<OfferProbe>,
    pub hardware: bool,
    /// Sonder aussi les ports par défaut des moteurs connus, même non installés.
    pub default_ports: bool,
    pub mcp_dir: PathBuf,
}

impl Sources {
    /// La machine : le PATH effectif de Pénélope (celui du propriétaire, complété des
    /// emplacements usuels), ses dossiers d'applications, les sondes de son OS.
    pub fn of(s: &Services) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Sources {
            path: penelope_platform::process::search_path(),
            app_dirs: discover::application_dirs(home.as_deref()),
            offers: discover::offer_probes().to_vec(),
            hardware: true,
            default_ports: true,
            mcp_dir: s.platform.dirs.mcp_d(),
        }
    }
}

/// Dresse la carte (bloquant : à appeler sous `spawn_blocking`), sans les serveurs
/// d'inférence, qui se sondent en asynchrone.
pub fn scan(src: &Sources) -> Environment {
    let exes = discover::executables_in(&src.path);
    let packages = managers::listed(&src.path, MANAGER_TIMEOUT);
    let offers = discover::probe_offers(&src.offers, &src.path, PROBE_TIMEOUT);
    let (declared, _) = penelope_mcp::config::load_dir(&src.mcp_dir);
    Environment {
        hardware: if src.hardware {
            discover::hardware(PROBE_TIMEOUT)
        } else {
            None
        },
        tools: managers::tools(&exes, &packages),
        apps: discover::applications(&src.app_dirs, PROBE_TIMEOUT),
        capabilities: mcp_capabilities(&offers, &declared),
        checked_at: String::new(),
        changes: None,
    }
}

/// Les MCP : ceux qu'exposent des applications, marqués branchés si `mcp.d` déclare déjà
/// la même commande, puis ceux que `mcp.d` déclare.
pub fn mcp_capabilities(
    offers: &[McpOffer],
    declared: &[penelope_mcp::config::ServerConfig],
) -> Vec<Capability> {
    let same = |o: &McpOffer, d: &penelope_mcp::config::ServerConfig| {
        let program = std::path::Path::new(&d.command)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        program == o.command && d.args == o.args
    };
    let mut out: Vec<Capability> = offers
        .iter()
        .map(|o| Capability {
            kind: "mcp".into(),
            id: o.id.clone(),
            name: o.app.clone(),
            via: std::iter::once(o.command.as_str())
                .chain(o.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" "),
            origin: "application".into(),
            declared: declared.iter().any(|d| same(o, d)),
            ..Default::default()
        })
        .collect();
    out.extend(declared.iter().map(|d| Capability {
        kind: "mcp".into(),
        id: format!("mcp.d/{}", d.name),
        name: d.name.clone(),
        via: if d.command.is_empty() {
            d.url.clone()
        } else {
            std::iter::once(d.command.as_str())
                .chain(d.args.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        },
        origin: "configuration".into(),
        declared: true,
        ..Default::default()
    }));
    out
}

/// Ce qui a changé entre deux cartes : outils (par nom et source), applications (par
/// identifiant), capacités (par identifiant, modèles servis compris).
pub fn diff(before: &Environment, after: &Environment) -> Changes {
    let mut c = Changes {
        since: before.checked_at.clone(),
        at: after.checked_at.clone(),
        ..Default::default()
    };
    let tools = |e: &Environment| -> BTreeMap<String, Option<String>> {
        e.tools
            .iter()
            .map(|t| (format!("{} ({})", t.name, t.source), t.version.clone()))
            .collect()
    };
    let apps = |e: &Environment| -> BTreeMap<String, Option<String>> {
        e.apps
            .iter()
            .map(|a| (format!("application {}", a.name), a.version.clone()))
            .collect()
    };
    let caps = |e: &Environment| -> BTreeMap<String, Option<String>> {
        e.capabilities
            .iter()
            .map(|k| {
                let label = format!("{} {}", k.kind, k.name);
                let models =
                    (!k.models.is_empty()).then(|| format!("{} modèle(s)", k.models.len()));
                (label, models)
            })
            .collect()
    };
    for (old, new) in [
        (tools(before), tools(after)),
        (apps(before), apps(after)),
        (caps(before), caps(after)),
    ] {
        for (k, v) in &new {
            match old.get(k) {
                None => c.appeared.push(k.clone()),
                Some(was) if was != v => c.updated.push(format!(
                    "{k} : {} → {}",
                    was.as_deref().unwrap_or("?"),
                    v.as_deref().unwrap_or("?")
                )),
                Some(_) => {}
            }
        }
        c.disappeared
            .extend(old.keys().filter(|k| !new.contains_key(*k)).cloned());
    }
    c
}

/// Relance la découverte sur la machine et range la carte dans `kv`.
pub async fn refresh(s: &Services) -> anyhow::Result<Environment> {
    refresh_from(s, Sources::of(s)).await
}

/// [`refresh`] sur des sources données.
pub async fn refresh_from(s: &Services, src: Sources) -> anyhow::Result<Environment> {
    let default_ports = src.default_ports;
    let mut env = tokio::task::spawn_blocking(move || scan(&src)).await?;
    let cfg = s.config.config();
    let endpoints = inference::endpoints(&cfg, &env.tools, &env.apps, default_ports);
    env.capabilities
        .extend(inference::capabilities(endpoints).await);
    env.checked_at = s.clock.now_rfc3339();
    let previous = cached(s).await;
    env.changes = match &previous {
        Some(before) => {
            let now = diff(before, &env);
            if now.is_empty() {
                before.changes.clone()
            } else {
                Some(now)
            }
        }
        None => None,
    };
    s.kv_set(KV_KEY, &serde_json::to_string(&env)?).await?;
    Ok(env)
}

/// La dernière carte, sans rien sonder : c'est elle que lit `env_explore`.
pub async fn cached(s: &Services) -> Option<Environment> {
    let raw = s.kv_get(KV_KEY).await.ok()??;
    serde_json::from_str(&raw).ok()
}

#[cfg(test)]
mod tests;
