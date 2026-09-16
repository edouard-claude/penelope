//! Configuration d'un serveur MCP (`mcp.d/*.toml`, §8.7).

use crate::error::{McpError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    /// `stdio` | `http` | `sse` (déprécié) | `auto`.
    pub transport: String,
    pub enabled: bool,

    // --- stdio ---
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,

    // --- http ---
    pub url: String,
    /// En-têtes personnalisés, placeholders `${SECRET:…}` acceptés.
    pub headers: BTreeMap<String, String>,
    pub client_id: String,
    pub scopes: Vec<String>,

    // --- comportement ---
    pub lazy_start: bool,
    pub idle_timeout: String,
    pub timeout: String,
    pub max_concurrency: usize,
    /// Schémas injectés en T1 pour un petit serveur critique (§8.9).
    pub eager_schemas: bool,
    pub protocol: String,
    pub sandbox_profile: String,
    /// Racines exposées à ce serveur via `roots/list` : jamais le home entier (§8.4).
    pub roots: Vec<String>,
    /// Budget de tokens pour les requêtes `sampling/createMessage` (§8.4).
    pub sampling_budget_tokens: u64,
    /// Surcharges de politique par outil : `{"create_pr": "ask_twice"}`.
    pub tool_policy: BTreeMap<String, String>,
    /// Surcharges de classe de risque par outil : les annotations ne sont que des indices.
    pub tool_risk: BTreeMap<String, String>,
    pub timeout_per_tool: BTreeMap<String, String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            name: String::new(),
            transport: "auto".into(),
            enabled: true,
            command: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            env: BTreeMap::new(),
            url: String::new(),
            headers: BTreeMap::new(),
            client_id: String::new(),
            scopes: Vec::new(),
            lazy_start: true,
            idle_timeout: "10m".into(),
            timeout: "30s".into(),
            max_concurrency: 4,
            eager_schemas: false,
            protocol: "2026-07-28".into(),
            sandbox_profile: "mcp-stdio".into(),
            roots: Vec::new(),
            sampling_budget_tokens: 20_000,
            tool_policy: BTreeMap::new(),
            tool_risk: BTreeMap::new(),
            timeout_per_tool: BTreeMap::new(),
        }
    }
}

impl ServerConfig {
    pub fn stdio(name: &str, command: &str, args: &[&str]) -> Self {
        ServerConfig {
            name: name.into(),
            transport: "stdio".into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    pub fn http(name: &str, url: &str) -> Self {
        ServerConfig {
            name: name.into(),
            transport: "http".into(),
            url: url.into(),
            ..Default::default()
        }
    }

    /// Transport effectif : `auto` choisit stdio si une commande est donnée, HTTP sinon.
    pub fn effective_transport(&self) -> &str {
        if self.transport != "auto" {
            return &self.transport;
        }
        if !self.command.is_empty() {
            "stdio"
        } else {
            "http"
        }
    }

    pub fn validate(&self) -> Result<()> {
        let bad = |r: &str| McpError::Config {
            server: self.name.clone(),
            reason: r.to_string(),
        };
        if self.name.is_empty() {
            return Err(bad("le nom est obligatoire"));
        }
        if !self
            .name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(bad("le nom doit être en [A-Za-z0-9_-]"));
        }
        match self.effective_transport() {
            "stdio" => {
                if self.command.is_empty() {
                    return Err(bad("`command` est obligatoire en transport stdio"));
                }
            }
            "http" | "sse" => {
                if self.url.is_empty() {
                    return Err(bad("`url` est obligatoire en transport HTTP"));
                }
                if !self.url.starts_with("http://") && !self.url.starts_with("https://") {
                    return Err(bad("`url` doit être en http(s)"));
                }
            }
            other => return Err(bad(&format!("transport inconnu : `{other}`"))),
        }
        if crate::protocol::ProtocolVersion::parse(&self.protocol).is_none() {
            return Err(bad(&format!(
                "version de protocole inconnue : `{}`",
                self.protocol
            )));
        }
        penelope_kernel::config::parse_duration(&self.timeout)
            .map_err(|e| bad(&format!("`timeout` : {e}")))?;
        penelope_kernel::config::parse_duration(&self.idle_timeout)
            .map_err(|e| bad(&format!("`idle_timeout` : {e}")))?;
        if penelope_platform::sandbox::ProfileKind::parse(&self.sandbox_profile).is_none() {
            return Err(bad(&format!(
                "profil de bac à sable inconnu : `{}`",
                self.sandbox_profile
            )));
        }
        for (tool, p) in &self.tool_policy {
            if penelope_kernel::risk::PolicyDecision::parse(p).is_none() {
                return Err(bad(&format!("politique inconnue pour `{tool}` : `{p}`")));
            }
        }
        for (tool, r) in &self.tool_risk {
            if penelope_kernel::risk::RiskClass::parse(r).is_none() {
                return Err(bad(&format!(
                    "classe de risque inconnue pour `{tool}` : `{r}`"
                )));
            }
        }
        Ok(())
    }

    pub fn timeout_duration(&self) -> std::time::Duration {
        penelope_kernel::config::parse_duration(&self.timeout)
            .unwrap_or_else(|_| std::time::Duration::from_secs(30))
    }

    pub fn idle_duration(&self) -> std::time::Duration {
        penelope_kernel::config::parse_duration(&self.idle_timeout)
            .unwrap_or_else(|_| std::time::Duration::from_secs(600))
    }

    /// Délai pour un outil précis, sinon le délai du serveur (§8.3).
    pub fn timeout_for(&self, tool: &str) -> std::time::Duration {
        self.timeout_per_tool
            .get(tool)
            .and_then(|d| penelope_kernel::config::parse_duration(d).ok())
            .unwrap_or_else(|| self.timeout_duration())
    }

    pub fn protocol_version(&self) -> crate::protocol::ProtocolVersion {
        crate::protocol::ProtocolVersion::parse(&self.protocol).unwrap_or_default()
    }

    /// Résout les placeholders de secrets dans les en-têtes et l'environnement.
    pub fn resolve_secrets(
        &self,
        secrets: &dyn penelope_platform::SecretStore,
    ) -> Result<ServerConfig> {
        let mut out = self.clone();
        for v in out.headers.values_mut() {
            *v = secrets.expand(v)?;
            penelope_observe::register_secret(v);
        }
        for v in out.env.values_mut() {
            *v = secrets.expand(v)?;
            penelope_observe::register_secret(v);
        }
        Ok(out)
    }
}

/// Charge tous les fichiers `mcp.d/*.toml`.
///
/// Un fichier invalide est **signalé et ignoré** : un serveur cassé ne doit pas empêcher
/// les 79 autres de démarrer.
pub fn load_dir(dir: &Path) -> (Vec<ServerConfig>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ok, errors);
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
        .collect();
    paths.sort();

    for p in paths {
        let name = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        match std::fs::read_to_string(&p) {
            Ok(raw) => match parse_file(&raw, &name) {
                Ok(mut configs) => {
                    for c in &mut configs {
                        if let Err(e) = c.validate() {
                            errors.push((c.name.clone(), e.to_string()));
                        }
                    }
                    ok.extend(configs.into_iter().filter(|c| c.validate().is_ok()));
                }
                Err(e) => errors.push((name, e.to_string())),
            },
            Err(e) => errors.push((name, e.to_string())),
        }
    }
    (ok, errors)
}

/// Un fichier contient soit un serveur, soit une table `[servers.<nom>]`.
pub fn parse_file(raw: &str, default_name: &str) -> Result<Vec<ServerConfig>> {
    #[derive(Deserialize)]
    struct Multi {
        #[serde(default)]
        servers: BTreeMap<String, ServerConfig>,
    }

    if let Ok(m) = toml::from_str::<Multi>(raw) {
        if !m.servers.is_empty() {
            return Ok(m
                .servers
                .into_iter()
                .map(|(name, mut c)| {
                    if c.name.is_empty() {
                        c.name = name;
                    }
                    c
                })
                .collect());
        }
    }
    let mut c: ServerConfig = toml::from_str(raw).map_err(|e| McpError::Config {
        server: default_name.to_string(),
        reason: e.to_string(),
    })?;
    if c.name.is_empty() {
        c.name = default_name.to_string();
    }
    Ok(vec![c])
}

/// Écrit la configuration d'un serveur (ajout à chaud, §8.7).
pub fn write_server(dir: &Path, cfg: &ServerConfig) -> Result<std::path::PathBuf> {
    cfg.validate()?;
    std::fs::create_dir_all(dir).map_err(|e| McpError::Config {
        server: cfg.name.clone(),
        reason: e.to_string(),
    })?;
    let path = dir.join(format!("{}.toml", cfg.name));
    let body = toml::to_string_pretty(cfg).map_err(|e| McpError::Config {
        server: cfg.name.clone(),
        reason: e.to_string(),
    })?;
    penelope_kernel::config::atomic_write(&path, body.as_bytes()).map_err(|e| {
        McpError::Config {
            server: cfg.name.clone(),
            reason: e.to_string(),
        }
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_config_validates() {
        let c = ServerConfig::stdio("redmine", "npx", &["-y", "@org/redmine-mcp"]);
        c.validate().unwrap();
        assert_eq!(c.effective_transport(), "stdio");
        assert_eq!(c.timeout_duration().as_secs(), 30);
    }

    #[test]
    fn http_config_requires_a_url() {
        let mut c = ServerConfig::http("api", "");
        assert!(c.validate().is_err());
        c.url = "ftp://x".into();
        assert!(c.validate().is_err());
        c.url = "https://api.example.com/mcp".into();
        c.validate().unwrap();
    }

    #[test]
    fn auto_transport_picks_by_field() {
        let mut c = ServerConfig {
            name: "x".into(),
            command: "npx".into(),
            ..Default::default()
        };
        assert_eq!(c.effective_transport(), "stdio");
        c.command.clear();
        c.url = "https://x".into();
        assert_eq!(c.effective_transport(), "http");
    }

    #[test]
    fn invalid_names_and_protocols_are_rejected() {
        let mut c = ServerConfig::stdio("mon serveur", "npx", &[]);
        assert!(c.validate().is_err(), "espace interdit dans le nom");
        c.name = "mon-serveur".into();
        c.validate().unwrap();
        c.protocol = "2030-01-01".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn per_tool_timeout_overrides() {
        let mut c = ServerConfig::stdio("s", "cmd", &[]);
        c.timeout_per_tool.insert("long_tool".into(), "5m".into());
        assert_eq!(c.timeout_for("long_tool").as_secs(), 300);
        assert_eq!(c.timeout_for("autre").as_secs(), 30);
    }

    #[test]
    fn parse_single_server_file() {
        let raw = r#"
            command = "npx"
            args = ["-y", "@org/serveur"]
            lazy_start = true
            [env]
            API_URL = "https://exemple"
        "#;
        let v = parse_file(raw, "monserveur").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "monserveur");
        assert_eq!(v[0].env["API_URL"], "https://exemple");
    }

    #[test]
    fn parse_multi_server_file() {
        let raw = r#"
            [servers.a]
            command = "cmd-a"
            [servers.b]
            url = "https://b.example/mcp"
            transport = "http"
        "#;
        let mut v = parse_file(raw, "fichier").unwrap();
        v.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].name, "a");
        assert_eq!(v[1].effective_transport(), "http");
    }

    #[test]
    fn a_broken_file_does_not_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bon.toml"), "command = \"npx\"\n").unwrap();
        std::fs::write(dir.path().join("casse.toml"), "== pas du toml ==").unwrap();
        std::fs::write(
            dir.path().join("invalide.toml"),
            "transport = \"stdio\"\n", // pas de command
        )
        .unwrap();
        let (ok, errors) = load_dir(dir.path());
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].name, "bon");
        assert_eq!(errors.len(), 2, "{errors:?}");
    }

    #[test]
    fn write_then_reload_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = ServerConfig::stdio("redmine", "npx", &["-y", "@org/redmine"]);
        c.eager_schemas = true;
        c.tool_policy.insert("create_issue".into(), "auto".into());
        let p = write_server(dir.path(), &c).unwrap();
        assert!(p.exists());
        let (loaded, errors) = load_dir(dir.path());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(loaded[0], c);
    }

    #[test]
    fn secrets_are_resolved_in_headers_and_env() {
        let secrets = penelope_platform::MemorySecretStore::with(&[("forge_token", "tok-123456")]);
        let mut c = ServerConfig::http("forge", "https://forge.example/mcp");
        c.headers.insert(
            "Authorization".into(),
            "Bearer ${SECRET:forge_token}".into(),
        );
        c.env.insert("TOKEN".into(), "${SECRET:forge_token}".into());
        let r = c.resolve_secrets(&secrets).unwrap();
        assert_eq!(r.headers["Authorization"], "Bearer tok-123456");
        assert_eq!(r.env["TOKEN"], "tok-123456");
        // Enregistré pour la redaction.
        assert!(penelope_observe::redact("valeur tok-123456").contains("masqué"));
    }

    #[test]
    fn missing_secret_is_an_explicit_error() {
        let secrets = penelope_platform::MemorySecretStore::new();
        let mut c = ServerConfig::http("forge", "https://x/mcp");
        c.headers
            .insert("Authorization".into(), "Bearer ${SECRET:absent}".into());
        assert!(c.resolve_secrets(&secrets).is_err());
    }

    #[test]
    fn unknown_policy_override_is_rejected() {
        let mut c = ServerConfig::stdio("s", "cmd", &[]);
        c.tool_policy.insert("t".into(), "peut-etre".into());
        assert!(c.validate().is_err());
        c.tool_policy.insert("t".into(), "ask_twice".into());
        c.validate().unwrap();
    }
}
