//! `penelope local` (#259) : le serveur d'inférence locale en LaunchAgent, démarré à
//! l'ouverture de session et relancé par launchd s'il tombe, comme le daemon. Hors
//! daemon : la commande ne lit que la configuration.
//!
//! Le serveur écoute sur l'adresse de l'endpoint (`providers.local.base_url`, ou celle
//! d'un endpoint de `providers.extra`) : Pénélope et lui ne peuvent pas diverger. Quand il
//! tombe quand même, `penelope doctor` le signale et les tours passent au repli.

use super::*;
use penelope_kernel::config::{Config, LocalProvider};

pub(super) fn run(cli: &Cli, cmd: &LocalCmd) -> CliResult<()> {
    let dirs = penelope_platform::resolve_directories(cli.home.clone())
        .map_err(|e| CliError::Io(e.to_string()))?;
    let raw = std::fs::read_to_string(dirs.config_file()).unwrap_or_default();
    let (cfg, _) = Config::parse(&raw).map_err(|e| CliError::Validation(e.to_string()))?;
    let name = match cmd {
        LocalCmd::Install { endpoint, .. }
        | LocalCmd::Uninstall { endpoint }
        | LocalCmd::Status { endpoint } => endpoint.as_str(),
    };
    let endpoint = endpoint_of(&cfg, name)?;
    let label = penelope_platform::service::inference_label(name);
    let service = |args: Vec<String>| {
        penelope_platform::backend::agent_manager(dirs.as_ref(), &label, args)
            .map_err(|e| CliError::Io(e.to_string()))
    };
    let out = match cmd {
        LocalCmd::Install {
            model,
            server,
            max_tokens,
            ..
        } => {
            let server = match server {
                Some(p) => p.clone(),
                None => penelope_platform::process::which("mlx_lm.server").ok_or_else(|| {
                    CliError::Usage(
                        "`mlx_lm.server` introuvable dans le PATH : `pip install mlx-lm` dans \
                         un environnement Python, puis `--server <chemin>/bin/mlx_lm.server`"
                            .into(),
                    )
                })?,
            };
            let args = server_args(&server, model, &endpoint.base_url, *max_tokens)?;
            let unit = service(args.clone())?
                .install(&server, None)
                .map_err(|e| CliError::Io(e.to_string()))?;
            json!({
                "installed": true,
                "label": label,
                "unit": unit,
                "command": args.join(" "),
                "next": next_steps(&cfg, name, endpoint, model),
            })
        }
        LocalCmd::Uninstall { .. } => {
            service(Vec::new())?
                .uninstall()
                .map_err(|e| CliError::Io(e.to_string()))?;
            json!({"uninstalled": true, "label": label})
        }
        LocalCmd::Status { .. } => {
            let st = service(Vec::new())?
                .status()
                .map_err(|e| CliError::Io(e.to_string()))?;
            json!({"label": label, "base_url": endpoint.base_url, "status": st})
        }
    };
    output::print(&out, cli.json);
    Ok(())
}

/// L'endpoint nommé : `local`, ou une entrée de `providers.extra`.
fn endpoint_of<'a>(cfg: &'a Config, name: &str) -> CliResult<&'a LocalProvider> {
    if name == "local" {
        return Ok(&cfg.providers.local);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(CliError::Usage(format!(
            "`{name}` : un nom d'endpoint s'écrit en lettres, chiffres, `-` et `_`"
        )));
    }
    cfg.providers.extra.get(name).ok_or_else(|| {
        CliError::Usage(format!(
            "aucun endpoint `{name}` : `local`, ou une entrée de `providers.extra` ({})",
            cfg.providers
                .extra
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}

/// Arguments de `mlx_lm.server` pour l'adresse de l'endpoint. Seule une adresse de
/// bouclage est acceptée : le serveur n'a pas d'authentification.
pub(super) fn server_args(
    server: &std::path::Path,
    model: &str,
    base_url: &str,
    max_tokens: u32,
) -> CliResult<Vec<String>> {
    let rest = base_url.strip_prefix("http://").ok_or_else(|| {
        CliError::Usage(format!(
            "`{base_url}` : un serveur local s'adresse en `http://127.0.0.1:<port>/v1`"
        ))
    })?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = authority.rsplit_once(':').ok_or_else(|| {
        CliError::Usage(format!(
            "`{base_url}` : port absent (`http://127.0.0.1:8080/v1`)"
        ))
    })?;
    let port: u16 = port
        .parse()
        .map_err(|_| CliError::Usage(format!("`{base_url}` : port illisible")))?;
    let host = match host {
        "127.0.0.1" | "localhost" => "127.0.0.1",
        "[::1]" => "::1",
        other => {
            return Err(CliError::Usage(format!(
                "`{other}` : mlx_lm.server n'a pas d'authentification, il n'écoute que sur \
                 127.0.0.1"
            )));
        }
    };
    Ok(vec![
        server.to_string_lossy().to_string(),
        "--model".into(),
        model.into(),
        "--host".into(),
        host.into(),
        "--port".into(),
        port.to_string(),
        "--max-tokens".into(),
        max_tokens.to_string(),
    ])
}

/// Ce qui reste à régler pour que Pénélope se serve du serveur installé.
fn next_steps(cfg: &Config, name: &str, endpoint: &LocalProvider, model: &str) -> Vec<String> {
    let key = if name == "local" {
        "providers.local".to_string()
    } else {
        format!("providers.extra.{name}")
    };
    let mut steps = Vec::new();
    if !endpoint.enabled {
        steps.push(format!("penelope config set {key}.enabled true"));
    }
    if name != "local" && !endpoint.models.iter().any(|m| m == model) {
        steps.push(format!(
            "ajouter `{model}` à `{key}.models` : l'endpoint ne sert que les modèles listés"
        ));
    }
    let alias = format!("local:{model}");
    if !cfg.models.aliases.values().any(|m| *m == alias) {
        steps.push(format!("penelope model set <alias> {alias}"));
    }
    steps.push("penelope doctor".into());
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_server_listens_on_the_endpoint_address_only() {
        let args = server_args(
            Path::new("/opt/mlx/bin/mlx_lm.server"),
            "mlx-community/Qwen3-8B-4bit",
            "http://127.0.0.1:8081/v1",
            16_384,
        )
        .unwrap();
        assert_eq!(
            args.join(" "),
            "/opt/mlx/bin/mlx_lm.server --model mlx-community/Qwen3-8B-4bit --host 127.0.0.1 \
             --port 8081 --max-tokens 16384"
        );
        let s = Path::new("mlx_lm.server");
        assert!(server_args(s, "m", "http://localhost:8080/v1", 1).is_ok());
        for refused in [
            "http://0.0.0.0:8080/v1",
            "http://192.168.1.2:8080/v1",
            "https://127.0.0.1:8080/v1",
            "http://127.0.0.1/v1",
        ] {
            assert!(server_args(s, "m", refused, 1).is_err(), "{refused}");
        }
    }

    #[test]
    fn next_steps_name_what_is_missing() {
        let mut cfg = Config::default();
        let steps = next_steps(
            &cfg,
            "local",
            &cfg.providers.local,
            "mlx-community/Qwen3-8B-4bit",
        );
        assert_eq!(
            steps,
            [
                "penelope config set providers.local.enabled true",
                "penelope model set <alias> local:mlx-community/Qwen3-8B-4bit",
                "penelope doctor",
            ]
        );
        let mlx = LocalProvider {
            enabled: true,
            ..Default::default()
        };
        cfg.providers.extra.insert("mlx".into(), mlx);
        let e = endpoint_of(&cfg, "mlx").unwrap();
        let steps = next_steps(&cfg, "mlx", e, "m");
        assert!(
            steps[0].contains("`providers.extra.mlx.models`"),
            "{steps:?}"
        );
        assert!(endpoint_of(&cfg, "absent").is_err());
        assert!(endpoint_of(&cfg, "../x").is_err());
    }
}
