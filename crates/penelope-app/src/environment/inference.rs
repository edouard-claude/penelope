//! Les serveurs d'inférence locaux (issue #260) : installés, déclarés, joignables, et
//! les modèles qu'ils servent.
//!
//! La sonde est un `GET <base>/models` sur la boucle locale, délai court, sans clé :
//! elle dit qu'un serveur répond et ce qu'il sert, jamais plus. Elle tourne dans la passe
//! de fond, pas dans un tour.

use super::{Capability, Found};
use penelope_kernel::config::Config;
use penelope_platform::discover::App;
use std::collections::BTreeMap;
use std::time::Duration;

/// Délai d'une sonde `/models` : un serveur local répond en quelques millisecondes.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Un moteur d'inférence local connu : ses exécutables, son application, son port.
pub struct Engine {
    pub id: &'static str,
    pub name: &'static str,
    pub bins: &'static [&'static str],
    pub apps: &'static [&'static str],
    pub port: u16,
}

pub const ENGINES: &[Engine] = &[
    Engine {
        id: "ollama",
        name: "Ollama",
        bins: &["ollama"],
        apps: &["Ollama"],
        port: 11434,
    },
    Engine {
        id: "lmstudio",
        name: "LM Studio",
        bins: &["lms"],
        apps: &["LM Studio"],
        port: 1234,
    },
    Engine {
        id: "mlx_lm",
        name: "mlx_lm",
        bins: &["mlx_lm.server"],
        apps: &[],
        port: 8080,
    },
    Engine {
        id: "llama.cpp",
        name: "llama.cpp",
        bins: &["llama-server"],
        apps: &[],
        port: 8080,
    },
    Engine {
        id: "mlx_audio",
        name: "mlx-audio",
        bins: &["mlx_audio.server"],
        apps: &[],
        port: 8000,
    },
    Engine {
        id: "vllm",
        name: "vLLM",
        bins: &["vllm"],
        apps: &[],
        port: 8000,
    },
];

/// Un port local à sonder, et ce qu'on sait déjà de lui.
#[derive(Debug, Clone, Default)]
pub struct Endpoint {
    pub port: u16,
    pub base_url: String,
    /// Moteurs installés dont c'est le port par défaut.
    pub engines: Vec<&'static str>,
    /// Fournisseurs de la configuration qui pointent ici (`providers.local`).
    pub providers: Vec<String>,
}

/// Les ports à sonder : ceux des fournisseurs locaux déclarés, et, si `defaults`, ceux
/// des moteurs connus — installés ou non, un serveur peut tourner d'un autre paquet.
pub fn endpoints(cfg: &Config, tools: &[Found], apps: &[App], defaults: bool) -> Vec<Endpoint> {
    let mut by_port: BTreeMap<u16, Endpoint> = BTreeMap::new();
    // Un fournisseur désactivé n'est pas branché : `providers.local` l'est par défaut,
    // sur le port de `mlx_lm`, et ne doit pas se faire passer pour une capacité.
    let declared = std::iter::once(("local".to_string(), &cfg.providers.local))
        .chain(cfg.providers.extra.iter().map(|(n, p)| (n.clone(), p)))
        .filter(|(_, p)| p.enabled)
        .map(|(n, p)| (n, p.base_url.as_str()));
    for (name, base) in declared {
        let Some(port) = loopback_port(base) else {
            continue;
        };
        let e = by_port.entry(port).or_insert_with(|| Endpoint {
            port,
            base_url: base.trim_end_matches('/').to_string(),
            ..Default::default()
        });
        e.providers.push(format!("providers.{name}"));
    }
    for engine in ENGINES {
        let installed = tools.iter().any(|t| engine.bins.contains(&t.name.as_str()))
            || apps.iter().any(|a| engine.apps.contains(&a.name.as_str()));
        if !installed && !defaults {
            continue;
        }
        let e = by_port.entry(engine.port).or_insert_with(|| Endpoint {
            port: engine.port,
            base_url: format!("http://127.0.0.1:{}/v1", engine.port),
            ..Default::default()
        });
        if installed {
            e.engines.push(engine.name);
        }
    }
    by_port.into_values().collect()
}

/// Le port d'une adresse sur la boucle locale ; `None` pour toute autre adresse : la
/// sonde ne sort jamais de la machine.
fn loopback_port(base: &str) -> Option<u16> {
    let u = url::Url::parse(base).ok()?;
    let host = u.host_str()?.trim_matches(|c| c == '[' || c == ']');
    matches!(host, "127.0.0.1" | "localhost" | "::1").then(|| u.port_or_known_default())?
}

/// Ce que rend une sonde : `None` si rien ne répond, sinon les modèles servis.
pub async fn probe(base_url: &str) -> Option<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .no_proxy()
        .build()
        .ok()?;
    let resp = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .send()
        .await
        .ok()?;
    // Un serveur qui répond sans liste (clé exigée, route absente) est joignable.
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return Some(Vec::new());
    };
    let mut models: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
        .map(str::to_string)
        .collect();
    models.sort();
    Some(models)
}

/// Les capacités d'inférence : un port qui a un moteur installé, un fournisseur déclaré
/// ou qui répond.
pub async fn capabilities(endpoints: Vec<Endpoint>) -> Vec<Capability> {
    let probes = futures::future::join_all(endpoints.iter().map(|e| probe(&e.base_url))).await;
    endpoints
        .into_iter()
        .zip(probes)
        .filter(|(e, answer)| !e.engines.is_empty() || !e.providers.is_empty() || answer.is_some())
        .map(|(e, answer)| {
            let (name, origin) = if !e.engines.is_empty() {
                (e.engines.join(" / "), "outil")
            } else if !e.providers.is_empty() {
                (e.providers.join(", "), "configuration")
            } else {
                (format!("127.0.0.1:{}", e.port), "sonde")
            };
            Capability {
                kind: "inference".into(),
                id: format!("inference:{}", e.port),
                name,
                via: e.base_url,
                origin: origin.into(),
                declared: !e.providers.is_empty(),
                reachable: Some(answer.is_some()),
                models: answer.unwrap_or_default(),
            }
        })
        .collect()
}
