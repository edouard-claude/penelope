//! Configuration de livraison d'un projet : `.penelope/delivery.toml` du dépôt, complétée
//! par ce que le dépôt dit de lui-même (URL du remote, fichiers de CI présents).
//!
//! ```toml
//! [forge]
//! kind = "gitlab"                    # github | gitlab ; déduit de github.com, gitlab.com
//! api = "https://git.example.com/api/v4"   # déduit de l'hôte du remote
//! repo = "equipe/service"            # déduit du chemin du remote
//! remote = "origin"
//! token_secret = "gitlab_token"      # nom du secret dans le coffre
//!
//! [branches]
//! dev = "develop"                    # obligatoire : jamais supposée
//! prod = "main"                      # obligatoire pour la PR vers la production (#193)
//!
//! [ci]
//! provider = "gitlab"                # github | gitlab | none ; déduit des fichiers de CI
//! timeout_minutes = 60
//!
//! [e2e]
//! url = "https://dev.example.com"    # obligatoire
//! [[e2e.checks]]
//! kind = "http"                      # http | graphql | command
//! path = "/health"
//! status = 200
//! contains = "ok"
//!
//! [prod]
//! max_age_minutes = 60               # au-delà, le bilan est périmé et se re-vérifie
//! ```
//!
//! Ce qui manque est rendu en [`Missing`] : la clé, et pourquoi elle compte. Chaque étape
//! ne demande que ce dont elle a besoin : la PR n'attend pas l'URL de dev.

use serde::Deserialize;

/// Chemin de la configuration, relatif à la racine du dépôt.
pub const CONFIG_PATH: &str = ".penelope/delivery.toml";

/// Délai par défaut avant qu'une CI sans verdict ne bloque la livraison.
pub const CI_TIMEOUT_MINUTES: u64 = 60;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub forge: ForgeSection,
    pub branches: Branches,
    pub ci: CiSection,
    pub e2e: E2eSection,
    pub prod: ProdSection,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeSection {
    pub kind: Option<String>,
    pub api: Option<String>,
    pub repo: Option<String>,
    pub remote: Option<String>,
    pub token_secret: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Branches {
    pub dev: Option<String>,
    /// Branche de production, cible de la PR dev → prod (#193) : jamais supposée.
    pub prod: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CiSection {
    pub provider: Option<String>,
    pub timeout_minutes: Option<u64>,
}

/// Gate de production (#193).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProdSection {
    /// Âge maximal du bilan approuvé : au-delà, CI et E2E sont re-vérifiés avant toute PR.
    pub max_age_minutes: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct E2eSection {
    pub url: Option<String>,
    pub checks: Vec<Check>,
}

/// Un contrôle de l'environnement de dev. `http` et `graphql` sont faits par Pénélope ;
/// `command` lance l'outil que le projet a déjà (Playwright, Maestro…), avec
/// `E2E_BASE_URL` dans son environnement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Check {
    pub kind: CheckKind,
    pub name: Option<String>,
    pub method: Option<String>,
    /// Chemin ajouté à l'URL de dev (`/health`) ; vide : l'URL elle-même.
    pub path: String,
    /// Statut attendu ; sans lui, tout 2xx.
    pub status: Option<u16>,
    /// Texte que la réponse doit contenir.
    pub contains: Option<String>,
    /// `graphql` : la requête envoyée.
    pub query: Option<String>,
    /// `command` : la commande, lancée à la racine du dépôt.
    pub command: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    #[default]
    Http,
    Graphql,
    Command,
}

impl Check {
    /// Nom montré dans les preuves : le sien, sinon le type et le chemin.
    pub fn label(&self) -> String {
        if let Some(name) = self.name.as_deref().filter(|n| !n.is_empty()) {
            return name.to_string();
        }
        match self.kind {
            CheckKind::Http => format!(
                "{} {}",
                self.method.as_deref().unwrap_or("GET"),
                if self.path.is_empty() {
                    "/"
                } else {
                    &self.path
                }
            ),
            CheckKind::Graphql => format!(
                "GraphQL {}",
                if self.path.is_empty() {
                    "/graphql"
                } else {
                    &self.path
                }
            ),
            CheckKind::Command => self.command.clone().unwrap_or_default(),
        }
    }
}

impl FileConfig {
    pub fn parse(raw: &str) -> Result<FileConfig, String> {
        toml::from_str(raw).map_err(|e| format!("{CONFIG_PATH} illisible : {e}"))
    }
}

/// Forgeurs pris en charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    GitHub,
    GitLab,
}

impl ForgeKind {
    pub fn parse(s: &str) -> Option<ForgeKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "github" => Some(ForgeKind::GitHub),
            "gitlab" => Some(ForgeKind::GitLab),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github",
            ForgeKind::GitLab => "gitlab",
        }
    }

    /// Nom du secret du jeton quand la configuration n'en nomme pas.
    pub fn default_token_secret(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "github_token",
            ForgeKind::GitLab => "gitlab_token",
        }
    }

    /// Ce que le forgeur appelle une PR.
    pub fn request_noun(self) -> &'static str {
        match self {
            ForgeKind::GitHub => "pull request",
            ForgeKind::GitLab => "merge request",
        }
    }

    /// API d'un hôte : les deux hôtes publics ont la leur, une instance privée sert la
    /// sienne sous son propre nom.
    fn api_for_host(self, host: &str) -> String {
        match (self, host) {
            (ForgeKind::GitHub, "github.com") => "https://api.github.com".into(),
            (ForgeKind::GitHub, h) => format!("https://{h}/api/v3"),
            (ForgeKind::GitLab, h) => format!("https://{h}/api/v4"),
        }
    }

    /// Le forgeur d'un hôte public ; une instance privée se déclare.
    fn of_host(host: &str) -> Option<ForgeKind> {
        match host {
            "github.com" => Some(ForgeKind::GitHub),
            "gitlab.com" => Some(ForgeKind::GitLab),
            _ => None,
        }
    }
}

/// Ce que le dépôt dit de lui-même, relevé par l'orchestrateur.
#[derive(Debug, Clone, Default)]
pub struct Discovered {
    /// URL du remote de livraison (`git remote get-url`).
    pub remote_url: Option<String>,
    pub has_github_workflows: bool,
    pub has_gitlab_ci: bool,
}

/// Une information manquante : la clé à renseigner et pourquoi elle compte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing {
    pub key: String,
    pub why: String,
}

impl Missing {
    pub fn new(key: impl Into<String>, why: impl Into<String>) -> Missing {
        Missing {
            key: key.into(),
            why: why.into(),
        }
    }
}

/// La demande ciblée au propriétaire : ce qui manque pour `purpose`, et où le dire.
pub fn ask(purpose: &str, dir: &str, missing: &[Missing]) -> String {
    let mut text = format!("Pour {purpose}, il me manque :");
    for m in missing {
        text.push_str(&format!("\n- `{}` : {}", m.key, m.why));
    }
    text.push_str(&format!(
        "\n\nÀ renseigner dans `{dir}/{CONFIG_PATH}`, puis « Réessayer ». Je ne suppose ni \
         le forgeur, ni la branche, ni la CI, ni l'URL de dev."
    ));
    text
}

/// Où ouvrir la PR : le forgeur, son API, le dépôt, le jeton et la branche de dev.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeTarget {
    pub kind: ForgeKind,
    pub api: String,
    pub repo: String,
    pub remote: String,
    pub token_secret: String,
    pub dev_branch: String,
}

/// L'hôte et le chemin d'un remote (`https://hôte/a/b.git`, `git@hôte:a/b.git`,
/// `ssh://git@hôte/a/b`). Un chemin local n'a pas d'hôte.
pub fn remote_host_path(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme == "file" {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?.split(':').next()?;
        (host.to_string(), path.to_string())
    } else {
        let (user_host, path) = url.split_once(':')?;
        let (_, host) = user_host.split_once('@')?;
        (host.to_string(), path.to_string())
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| (host.to_ascii_lowercase(), path.to_string()))
}

/// Le remote de livraison : celui de la configuration, sinon `origin`, le nom que `git
/// clone` donne.
pub fn remote_name(cfg: &FileConfig) -> String {
    cfg.forge
        .remote
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| "origin".into())
}

pub(super) fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Résout le forgeur et la branche de dev ; rend tout ce qui manque d'un coup.
pub fn resolve_forge(cfg: &FileConfig, found: &Discovered) -> Result<ForgeTarget, Vec<Missing>> {
    let mut missing = Vec::new();
    let remote = remote_name(cfg);
    let hosted = found.remote_url.as_deref().and_then(remote_host_path);
    if found.remote_url.is_none() {
        missing.push(Missing::new(
            "forge.remote",
            format!("le dépôt n'a pas de remote `{remote}` vers lequel pousser la branche"),
        ));
    }
    let kind = match non_empty(&cfg.forge.kind) {
        Some(k) => ForgeKind::parse(&k).or_else(|| {
            missing.push(Missing::new(
                "forge.kind",
                format!("`{k}` n'est pas un forgeur pris en charge (github, gitlab)"),
            ));
            None
        }),
        None => {
            let kind = hosted.as_ref().and_then(|(h, _)| ForgeKind::of_host(h));
            if kind.is_none() && found.remote_url.is_some() {
                missing.push(Missing::new(
                    "forge.kind",
                    "l'hôte du remote ne dit pas quel forgeur c'est (github ou gitlab)",
                ));
            }
            kind
        }
    };
    let api = non_empty(&cfg.forge.api).or_else(|| {
        let (host, _) = hosted.as_ref()?;
        Some(kind?.api_for_host(host))
    });
    if api.is_none() && kind.is_some() {
        missing.push(Missing::new(
            "forge.api",
            "l'adresse de l'API du forgeur ne se déduit pas du remote",
        ));
    }
    let repo = non_empty(&cfg.forge.repo).or_else(|| hosted.as_ref().map(|(_, p)| p.clone()));
    if repo.is_none() && found.remote_url.is_some() {
        missing.push(Missing::new(
            "forge.repo",
            "le chemin du dépôt sur le forgeur (`groupe/depot`) ne se déduit pas du remote",
        ));
    }
    let dev_branch = non_empty(&cfg.branches.dev);
    if dev_branch.is_none() {
        missing.push(Missing::new(
            "branches.dev",
            "la branche de développement vers laquelle ouvrir la PR",
        ));
    }
    match (kind, api, repo, dev_branch) {
        (Some(kind), Some(api), Some(repo), Some(dev_branch)) if missing.is_empty() => {
            Ok(ForgeTarget {
                kind,
                api: api.trim_end_matches('/').to_string(),
                repo,
                token_secret: non_empty(&cfg.forge.token_secret)
                    .unwrap_or_else(|| kind.default_token_secret().into()),
                remote,
                dev_branch,
            })
        }
        _ => Err(missing),
    }
}

/// La CI à attendre : celle du forgeur, ou aucune si le projet le déclare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiPlan {
    /// `None` : le projet livre sans CI (`ci.provider = "none"`).
    pub provider: Option<ForgeKind>,
    pub timeout_ms: u64,
}

/// Résout la CI du projet sur le forgeur `forge`.
pub fn resolve_ci(
    cfg: &FileConfig,
    found: &Discovered,
    forge: ForgeKind,
) -> Result<CiPlan, Vec<Missing>> {
    let timeout_ms = cfg.ci.timeout_minutes.unwrap_or(CI_TIMEOUT_MINUTES).max(1) * 60_000;
    let provider = match non_empty(&cfg.ci.provider).as_deref() {
        Some("none") => None,
        Some(p) => match ForgeKind::parse(p) {
            Some(k) if k == forge => Some(k),
            Some(k) => {
                return Err(vec![Missing::new(
                    "ci.provider",
                    format!(
                        "la CI `{}` ne se lit pas sur un forgeur {} : seule celle du forgeur est \
                         prise en charge",
                        k.as_str(),
                        forge.as_str()
                    ),
                )]);
            }
            None => {
                return Err(vec![Missing::new(
                    "ci.provider",
                    format!("`{p}` n'est pas une CI prise en charge (github, gitlab, none)"),
                )]);
            }
        },
        None => {
            let discovered = match forge {
                ForgeKind::GitHub => found.has_github_workflows,
                ForgeKind::GitLab => found.has_gitlab_ci,
            };
            if !discovered {
                return Err(vec![Missing::new(
                    "ci.provider",
                    "aucune CI trouvée dans le dépôt (`.github/workflows`, `.gitlab-ci.yml`) : \
                     nomme-la, ou `none` pour livrer sans CI",
                )]);
            }
            Some(forge)
        }
    };
    Ok(CiPlan {
        provider,
        timeout_ms,
    })
}

/// L'environnement de dev et ses contrôles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct E2ePlan {
    pub url: String,
    pub checks: Vec<Check>,
}

/// Résout la vérification externe. Sans contrôle déclaré, un `GET` de l'URL de dev qui
/// doit répondre 2xx : le minimum qui prouve que l'environnement répond.
pub fn resolve_e2e(cfg: &FileConfig) -> Result<E2ePlan, Vec<Missing>> {
    let mut missing = Vec::new();
    let url = non_empty(&cfg.e2e.url);
    match url.as_deref() {
        None => missing.push(Missing::new(
            "e2e.url",
            "l'adresse de l'environnement de dev à vérifier depuis l'extérieur",
        )),
        Some(u) if !(u.starts_with("http://") || u.starts_with("https://")) => {
            missing.push(Missing::new(
                "e2e.url",
                format!("`{u}` n'est pas une adresse http(s)"),
            ));
        }
        Some(_) => {}
    }
    for (i, c) in cfg.e2e.checks.iter().enumerate() {
        let key = format!("e2e.checks[{i}]");
        match c.kind {
            CheckKind::Graphql if non_empty(&c.query).is_none() => {
                missing.push(Missing::new(
                    format!("{key}.query"),
                    "un contrôle GraphQL envoie une requête",
                ));
            }
            CheckKind::Command if non_empty(&c.command).is_none() => {
                missing.push(Missing::new(
                    format!("{key}.command"),
                    "un contrôle `command` lance l'outil E2E du projet",
                ));
            }
            _ => {}
        }
    }
    let checks = if cfg.e2e.checks.is_empty() {
        vec![Check::default()]
    } else {
        cfg.e2e.checks.clone()
    };
    match url {
        Some(url) if missing.is_empty() => Ok(E2ePlan {
            url: url.trim_end_matches('/').to_string(),
            checks,
        }),
        _ => Err(missing),
    }
}
