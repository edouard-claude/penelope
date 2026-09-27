//! Ouverture des transports : processus stdio sous bac à sable, HTTP, trousseau.

use super::*;

/// Ouvre le transport d'un serveur.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String>;
}

// ------------------------------------------------------------------ processus

/// Connecteur réel : stdio sous le profil de bac à sable du serveur, ou HTTP.
pub struct ProcessConnector {
    services: Arc<Services>,
    host: Arc<penelope_platform::UnixProcessHost>,
}

impl ProcessConnector {
    pub fn new(services: Arc<Services>) -> Self {
        let host = Arc::new(penelope_platform::UnixProcessHost::new(
            services.platform.dirs.pid_dir(),
        ));
        ProcessConnector { services, host }
    }

    fn profile(&self, cfg: &ServerConfig) -> Result<penelope_platform::Profile, String> {
        stdio_profile(&self.services, cfg)
    }
}

/// Profil de bac à sable d'un serveur stdio. `full` exige que le serveur figure dans
/// `sandbox.allow_full_for` (§13.2). Les profils imposés refusent les mêmes lectures que
/// le shell (`sandbox.deny_read` : clés SSH, secrets, base, configuration) et ferment le
/// trousseau : un paquet tiers ne lit pas ce que `shell_exec` ne lit pas (issue #89). Un
/// chemin refusé qui contient le répertoire de données du serveur ou une de ses racines
/// reste lisible pour eux. Un serveur de `sandbox.allow_keychain_for` garde tout cela et
/// joint le trousseau, sans plus (issue #122).
pub fn stdio_profile(
    s: &Services,
    cfg: &ServerConfig,
) -> Result<penelope_platform::Profile, String> {
    use penelope_platform::{Profile, ProfileKind};
    let data_dir = s.platform.dirs.data().join("mcp-data").join(&cfg.name);
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
    let mut profile = match ProfileKind::parse(&cfg.sandbox_profile) {
        Some(ProfileKind::Full) => {
            let allowed = s.config.config().sandbox.allow_full_for.clone();
            if !allowed.iter().any(|n| n == &cfg.name) {
                return Err(format!(
                    "le profil `full` de `{0}` doit être autorisé explicitement : \
                     `penelope config set sandbox.allow_full_for '[\"{0}\"]'`",
                    cfg.name
                ));
            }
            return Ok(Profile::full());
        }
        Some(ProfileKind::ReadOnly) => Profile::read_only(),
        Some(ProfileKind::WorkspaceWrite) => Profile::workspace_write(data_dir.clone()),
        _ => Profile::mcp_stdio(data_dir.clone(), Vec::new()),
    };
    let mut kept: Vec<std::path::PathBuf> = vec![data_dir];
    kept.extend(cfg.roots.iter().map(|r| s.platform.dirs.expand(r)));
    profile.deny_read = penelope_app::helpers::denied_reads(s)
        .into_iter()
        .filter(|d| !kept.iter().any(|k| k.starts_with(d)))
        .collect();
    Ok(profile.with_keychain(declared_for_keychain(s, cfg)))
}

fn declared_for_keychain(s: &Services, cfg: &ServerConfig) -> bool {
    s.config
        .config()
        .sandbox
        .allow_keychain_for
        .iter()
        .any(|n| n == &cfg.name)
}

/// Vrai si le processus du serveur joint le trousseau : serveur stdio déclaré dans
/// `sandbox.allow_keychain_for`, ou profil `full` autorisé. Un serveur distant n'a pas de
/// processus local, donc pas de trousseau (issue #122).
pub fn keychain_open(s: &Services, cfg: &ServerConfig) -> bool {
    if cfg.effective_transport() != "stdio" {
        return false;
    }
    match penelope_platform::ProfileKind::parse(&cfg.sandbox_profile) {
        Some(penelope_platform::ProfileKind::Full) => s
            .config
            .config()
            .sandbox
            .allow_full_for
            .iter()
            .any(|n| n == &cfg.name),
        _ => declared_for_keychain(s, cfg),
    }
}

/// Un serveur confiné qui échoue sur le trousseau n'y voit qu'« introuvable », même pour
/// un secret bien rangé : la phrase qui nomme le bac à sable et le réglage, au lieu de
/// laisser chercher le secret ailleurs (issue #122). `None` si le texte ne parle pas du
/// trousseau ou si le serveur le joint.
pub fn keychain_hint(s: &Services, cfg: &ServerConfig, text: &str) -> Option<String> {
    const CUES: [&str; 5] = ["keychain", "keyring", "trousseau", "secitem", "errsec"];
    let lower = text.to_lowercase();
    if cfg.effective_transport() != "stdio"
        || keychain_open(s, cfg)
        || !CUES.iter().any(|c| lower.contains(c))
    {
        return None;
    }
    Some(format!(
        "[Pénélope : `{0}` tourne sous bac à sable (`{1}`) et le trousseau macOS lui est \
         fermé : ce qu'il y cherche lui paraît introuvable, même rangé. Le secret n'est \
         donc pas forcément absent. Si `{0}` doit lire ses propres identifiants : \
         `penelope config set sandbox.allow_keychain_for '[\"{0}\"]'`. Sinon, lui passer \
         le secret par son environnement : `${{SECRET:nom}}` dans la table [env] de sa \
         déclaration.]",
        cfg.name, cfg.sandbox_profile
    ))
}

#[async_trait::async_trait]
impl Connector for ProcessConnector {
    async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String> {
        let s = &self.services;
        let resolved = cfg
            .resolve_secrets(s.platform.secrets.as_ref())
            .map_err(|e| format!("{e} (poser le secret : `penelope secret set <nom>`)"))?;
        let dirs = &s.platform.dirs;
        match resolved.effective_transport() {
            "stdio" => {
                let program = dirs.expand(&resolved.command).to_string_lossy().to_string();
                let args: Vec<String> = resolved
                    .args
                    .iter()
                    .map(|a| dirs.expand(a).to_string_lossy().to_string())
                    .collect();
                let mut spec = penelope_platform::ProcessSpec::new(program)
                    .args(args)
                    .pid_tag(format!("mcp-{}", cfg.name));
                if !resolved.cwd.is_empty() {
                    spec = spec.cwd(dirs.expand(&resolved.cwd));
                }
                for (k, v) in &resolved.env {
                    spec = spec.env(k.clone(), v.clone());
                }
                let profile = self.profile(cfg)?;
                let t =
                    penelope_mcp::StdioTransport::spawn(self.host.clone(), spec, Some(&profile))
                        .await
                        .map_err(|e| e.to_string())?;
                Ok(t as Arc<dyn Transport>)
            }
            kind @ ("http" | "sse") => {
                let t = if kind == "sse" {
                    penelope_mcp::HttpTransport::legacy(resolved.url.clone())
                } else {
                    penelope_mcp::HttpTransport::new(resolved.url.clone())
                }
                .map_err(|e| e.to_string())?;
                let mut extra = Vec::new();
                let mut static_auth = false;
                for (k, v) in &resolved.headers {
                    if k.eq_ignore_ascii_case("authorization") {
                        t.set_authorization(Some(v.clone())).await;
                        static_auth = true;
                    } else {
                        extra.push((k.clone(), v.clone()));
                    }
                }
                t.set_extra_headers(extra).await;
                // Autorisation OAuth obtenue par `mcp auth` : jeton rafraîchi au besoin.
                if !static_auth
                    && let Some(header) =
                        crate::auth::authorization_header(s, &cfg.name, &resolved.url).await?
                {
                    t.set_authorization(Some(header)).await;
                }
                Ok(t as Arc<dyn Transport>)
            }
            other => Err(format!("transport inconnu : `{other}`")),
        }
    }
}
