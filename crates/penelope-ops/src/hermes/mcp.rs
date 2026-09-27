//! Import des serveurs MCP d'Hermes : conversion, secrets rangés, essai.

use super::*;

// ------------------------------------------------------------------ serveurs MCP

/// `KEY=valeur` d'un `.env`, guillemets et `export` retirés.
pub fn parse_dotenv(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let l = l.strip_prefix("export ").unwrap_or(l);
            let (k, v) = l.split_once('=')?;
            let k = k.trim();
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return None;
            }
            Some((k.to_string(), unquote(v.trim())))
        })
        .collect()
}

fn looks_secret(key: &str, value: &str) -> bool {
    if value.trim().is_empty() || value.contains("${") {
        return false;
    }
    let k = key.to_ascii_uppercase().replace('-', "_");
    let by_key = [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "API_KEY",
        "APIKEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
        "CREDENTIAL",
        "AUTHORIZATION",
        "COOKIE",
    ]
    .iter()
    .any(|h| k.contains(h))
        || k == "KEY"
        || k.ends_with("_KEY")
        || k == "PAT"
        || k.ends_with("_PAT");
    by_key || penelope_observe::redact::secret_kind(value).is_some()
}

fn secret_slug(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// Serveur Hermes converti.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub config: ServerConfig,
    /// `(nom, valeur)` à ranger dans le magasin avant d'écrire la déclaration.
    pub secrets: Vec<(String, String)>,
    pub notes: Vec<String>,
}

struct Secretizer<'a> {
    server: String,
    dotenv: &'a BTreeMap<String, String>,
    workspace: &'a Path,
    secrets: Vec<(String, String)>,
    notes: Vec<String>,
}

impl Secretizer<'_> {
    fn keep(&mut self, name: String, value: String) -> String {
        let name: String = name.chars().take(128).collect();
        if !self.secrets.iter().any(|(n, _)| *n == name) {
            self.secrets.push((name.clone(), value));
        }
        format!("${{SECRET:{name}}}")
    }

    /// `${VAR}` et `$VAR` : valeur du `.env` d'Hermes (rangée si c'est un secret), sinon
    /// variable d'environnement du daemon.
    fn placeholders(&mut self, value: &str) -> String {
        let whole = value
            .strip_prefix('$')
            .filter(|v| !v.starts_with('{') && !v.is_empty())
            .filter(|v| v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        let value = match whole {
            Some(var) => format!("${{{var}}}"),
            None => value.to_string(),
        };
        let mut out = String::new();
        let mut rest = value.as_str();
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let inner = &after[..end];
            let var = inner
                .strip_prefix("env:")
                .or_else(|| inner.strip_prefix("ENV:"))
                .unwrap_or(inner);
            let plain = var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if let Some(v) = self.builtin(inner) {
                out.push_str(&v);
            } else if !plain || var.is_empty() {
                out.push_str(&rest[start..start + 2 + end + 1]);
            } else if let Some(v) = self.dotenv.get(var) {
                if looks_secret(var, v) {
                    let name = format!("hermes_{}", secret_slug(var));
                    out.push_str(&self.keep(name, v.clone()));
                } else {
                    out.push_str(v);
                }
            } else {
                self.notes.push(format!(
                    "`{var}` absente du .env : lue dans l'environnement du daemon"
                ));
                out.push_str(&format!("${{ENV:{var}}}"));
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Variables propres à Hermes (`${userHome}`, `${workspaceFolder}`, `${/}`…).
    fn builtin(&mut self, inner: &str) -> Option<String> {
        let workspace = || self.workspace.to_string_lossy().to_string();
        Some(match inner {
            "userHome" => penelope_platform::dirs::home_dir()?
                .to_string_lossy()
                .to_string(),
            "workspaceFolder" => {
                let w = workspace();
                self.notes.push(format!(
                    "`${{workspaceFolder}}` remplacé par l'espace de travail {w}"
                ));
                w
            }
            "workspaceFolderBasename" => "workspace".to_string(),
            "pathSeparator" | "/" => std::path::MAIN_SEPARATOR.to_string(),
            _ => return None,
        })
    }

    fn value(&mut self, kind: &str, key: &str, raw: &str) -> String {
        let value = self.placeholders(raw);
        if !looks_secret(key, &value) {
            return value;
        }
        let name = format!(
            "mcp_{}_{}{}",
            secret_slug(&self.server),
            if kind == "header" { "header_" } else { "" },
            secret_slug(key)
        );
        for scheme in ["Bearer ", "Basic ", "Token "] {
            if let Some(token) = value.strip_prefix(scheme) {
                return format!("{scheme}{}", self.keep(name, token.trim().to_string()));
            }
        }
        self.keep(name, value)
    }
}

fn duration(node: &yaml::Node) -> Option<String> {
    let v = node.as_str()?.trim();
    if v.is_empty() {
        return None;
    }
    match v.parse::<f64>() {
        Ok(secs) => Some(format!("{}s", secs.ceil().max(1.0) as u64)),
        Err(_) => Some(v.to_string()),
    }
}

/// Convertit une entrée `mcp_servers.<nom>` d'Hermes.
pub fn convert_server(
    raw_name: &str,
    node: &yaml::Node,
    dotenv: &BTreeMap<String, String>,
    workspace: &Path,
) -> Result<Converted, String> {
    let name: String = raw_name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.trim_matches('-').is_empty() {
        return Err("nom de serveur vide".into());
    }
    let yaml::Node::Map(fields) = node else {
        return Err("déclaration illisible (table attendue)".into());
    };
    let mut cfg = ServerConfig {
        name: name.clone(),
        ..Default::default()
    };
    let mut sec = Secretizer {
        server: name,
        dotenv,
        workspace,
        secrets: Vec::new(),
        notes: Vec::new(),
    };
    let mut ignored = Vec::new();
    for (k, v) in fields {
        match k.as_str() {
            "command" => {
                cfg.command = sec.placeholders(v.as_str().unwrap_or_default());
            }
            "args" => {
                cfg.args = match v {
                    yaml::Node::List(items) => items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .map(|a| sec.placeholders(a))
                        .collect(),
                    other => other
                        .as_str()
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(|a| sec.placeholders(a))
                        .collect(),
                };
            }
            "env" | "environment" => {
                for (ek, ev) in v.entries() {
                    let value = sec.value("env", ek, ev.as_str().unwrap_or_default());
                    cfg.env.insert(ek.clone(), value);
                }
            }
            "headers" => {
                for (hk, hv) in v.entries() {
                    let value = sec.value("header", hk, hv.as_str().unwrap_or_default());
                    cfg.headers.insert(hk.clone(), value);
                }
            }
            "url" => cfg.url = sec.placeholders(v.as_str().unwrap_or_default()),
            "cwd" => cfg.cwd = v.as_str().unwrap_or_default().to_string(),
            "timeout" => {
                if let Some(t) = duration(v) {
                    cfg.timeout = t;
                }
            }
            "idle_timeout_seconds" => {
                if let Some(t) = duration(v) {
                    cfg.idle_timeout = t;
                }
            }
            "lazy" => cfg.lazy_start = v.as_bool().unwrap_or(true),
            "auth" if v.as_str() == Some("oauth") => sec
                .notes
                .push("OAuth : autorisation à refaire côté Pénélope (`penelope mcp auth`)".into()),
            "oauth" => {
                for (ok, ov) in v.entries() {
                    match ok.as_str() {
                        "client_id" => {
                            cfg.client_id = ov.as_str().unwrap_or_default().to_string();
                        }
                        "scope" | "scopes" => {
                            cfg.scopes = match ov {
                                yaml::Node::List(items) => items
                                    .iter()
                                    .filter_map(|i| i.as_str())
                                    .map(String::from)
                                    .collect(),
                                other => other
                                    .as_str()
                                    .unwrap_or_default()
                                    .split_whitespace()
                                    .map(String::from)
                                    .collect(),
                            };
                        }
                        other => ignored.push(format!("`oauth.{other}`")),
                    }
                }
            }
            "tools" => sec.notes.push(
                "filtre `tools` d'Hermes sans équivalent : tous les outils sont exposés".into(),
            ),
            "enabled" => cfg.enabled = v.as_bool().unwrap_or(true),
            "disabled" => cfg.enabled = !v.as_bool().unwrap_or(false),
            "transport" | "type" => {
                cfg.transport = match v.as_str().unwrap_or_default().to_lowercase().as_str() {
                    "stdio" => "stdio",
                    "sse" => "sse",
                    "http" | "streamable-http" | "streamable_http" | "streamablehttp" => "http",
                    _ => "auto",
                }
                .to_string();
            }
            other => ignored.push(format!("`{other}`")),
        }
    }
    if !ignored.is_empty() {
        sec.notes
            .push(format!("sans équivalent, ignoré : {}", ignored.join(", ")));
    }
    Ok(Converted {
        config: cfg,
        secrets: sec.secrets,
        notes: sec.notes,
    })
}

pub(super) async fn import_mcp(
    s: &Services,
    sup: Option<Arc<dyn McpAdmin>>,
    opts: &Options,
    r: &mut Report,
) {
    let Some(raw) = ["config.yaml", "config.yml"]
        .iter()
        .find_map(|f| std::fs::read_to_string(opts.root.join(f)).ok())
    else {
        return;
    };
    let doc = yaml::parse(&raw);
    let servers = doc
        .get("mcp_servers")
        .or_else(|| doc.get("mcpServers"))
        .or_else(|| doc.get("mcp").and_then(|m| m.get("servers")));
    let Some(servers) = servers else {
        r.warn("config.yaml sans section `mcp_servers`".into());
        return;
    };
    let dotenv = std::fs::read_to_string(opts.root.join(".env"))
        .map(|raw| parse_dotenv(&raw))
        .unwrap_or_default();
    let dir = sup
        .as_ref()
        .map(|s| s.dir().to_path_buf())
        .unwrap_or_else(|| s.platform.dirs.mcp_d());

    let workspace = s.platform.dirs.data().join("workspace");
    let mut written: Vec<(ServerConfig, String)> = Vec::new();
    for (raw_name, node) in servers.entries() {
        let c = match convert_server(raw_name, node, &dotenv, &workspace) {
            Ok(c) => c,
            Err(e) => {
                r.push("mcp", raw_name, "invalid", e);
                continue;
            }
        };
        let name = c.config.name.clone();
        let known = match &sup {
            Some(sup) => sup.config_of(&name).await.is_some(),
            None => false,
        };
        if known || dir.join(format!("{name}.toml")).exists() {
            r.push("mcp", &name, "exists", String::new());
            continue;
        }
        if let Err(e) = c.config.validate() {
            r.push("mcp", &name, "invalid", e.to_string());
            continue;
        }
        let names: Vec<String> = c.secrets.iter().map(|(n, _)| n.clone()).collect();
        let mut detail = c.notes.join(" ; ");
        if !names.is_empty() {
            let moved = format!("{} secret(s) vers le magasin", names.len());
            detail = if detail.is_empty() {
                moved
            } else {
                format!("{moved} ; {detail}")
            };
        }
        if !opts.apply {
            r.secrets.extend(names);
            r.push("mcp", &name, "planned", detail);
            continue;
        }
        let stored = c
            .secrets
            .iter()
            .try_for_each(|(n, v)| s.platform.secrets.set(n, v).map_err(|e| e.to_string()));
        if let Err(e) = stored {
            r.push("mcp", &name, "failed", format!("secret non rangé : {e}"));
            continue;
        }
        r.secrets.extend(names);
        if let Err(e) = penelope_mcp::config::write_server(&dir, &c.config) {
            r.push("mcp", &name, "failed", e.to_string());
            continue;
        }
        written.push((c.config, detail));
    }
    if written.is_empty() {
        return;
    }
    let Some(sup) = sup else {
        for (cfg, detail) in written {
            r.push("mcp", &cfg.name, "imported", detail);
        }
        return;
    };
    sup.reload().await;
    let (to_test, untested): (Vec<_>, Vec<_>) = written
        .into_iter()
        .partition(|(cfg, _)| opts.test && cfg.enabled);
    for (cfg, detail) in untested {
        let detail = if cfg.enabled {
            detail
        } else {
            ["désactivé, comme dans Hermes".to_string(), detail]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ; ")
        };
        r.push("mcp", &cfg.name, "imported", detail);
    }
    for chunk in to_test.chunks(MCP_TEST_PARALLEL) {
        let tests = chunk
            .iter()
            .map(|(cfg, _)| tokio::time::timeout(MCP_TEST_TIMEOUT, sup.test(cfg)));
        let results = futures::future::join_all(tests).await;
        for ((cfg, detail), result) in chunk.iter().zip(results) {
            let (status, why) = match result {
                Ok(v) if v["ok"].as_bool() == Some(true) => (
                    "ok",
                    format!("{} outil(s)", v["tools"].as_u64().unwrap_or(0)),
                ),
                Ok(v) if v["auth_required"].as_bool() == Some(true) => (
                    "auth_required",
                    format!(
                        "`penelope mcp auth {}` ou `/mcp auth {}`",
                        cfg.name, cfg.name
                    ),
                ),
                Ok(v) => ("failed", v["error"].as_str().unwrap_or("échec").to_string()),
                Err(_) => ("failed", "pas de réponse dans le délai".to_string()),
            };
            let detail = [why, detail.clone()]
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ; ");
            r.push("mcp", &cfg.name, status, detail);
        }
    }
}
