//! Client des forgeurs : GitHub et GitLab, derrière la même interface.
//!
//! Trois questions seulement : quelle PR relie cette branche à la branche de dev, ouvre-la,
//! et où en est la CI de ce commit. Aucun nom d'hôte n'est écrit ici : l'adresse de l'API
//! vient de la configuration résolue ([`ForgeTarget`]).

use penelope_workflow::delivery::config::{ForgeKind, ForgeTarget};
use penelope_workflow::delivery::verdicts::{self, CiVerdict};
use serde_json::{Value, json};
use std::time::Duration;

/// Délai d'une requête au forgeur.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Part du corps d'une réponse d'erreur gardée dans le message.
const ERROR_BODY_CHARS: usize = 300;

/// Une PR (ou merge request) sur le forgeur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub state: String,
}

impl PullRequest {
    pub fn to_json(&self) -> Value {
        json!({"number": self.number, "url": self.url, "state": self.state})
    }
}

/// Pourquoi le forgeur n'a pas répondu ce qu'on attendait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// Réseau, délai, 5xx, 429 : un nouvel essai plus tard peut réussir.
    Unavailable(String),
    /// Le forgeur refuse (jeton, droits, dépôt inconnu) : rien ne changera sans le
    /// propriétaire.
    Refused(String),
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::Unavailable(e) => write!(f, "forgeur injoignable : {e}"),
            ForgeError::Refused(e) => write!(f, "forgeur : {e}"),
        }
    }
}

pub struct Forge {
    target: ForgeTarget,
    token: String,
    http: reqwest::Client,
}

/// Encode un chemin de projet GitLab (`groupe/depot` → `groupe%2Fdepot`).
fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Forge {
    pub fn new(target: ForgeTarget, token: String) -> Result<Forge, ForgeError> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("penelope/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ForgeError::Unavailable(e.to_string()))?;
        Ok(Forge {
            target,
            token,
            http,
        })
    }

    pub fn kind(&self) -> ForgeKind {
        self.target.kind
    }

    pub fn target(&self) -> &ForgeTarget {
        &self.target
    }

    /// Racine du dépôt dans l'API.
    fn project(&self) -> String {
        match self.target.kind {
            ForgeKind::GitHub => format!("{}/repos/{}", self.target.api, self.target.repo),
            ForgeKind::GitLab => {
                format!("{}/projects/{}", self.target.api, encode(&self.target.repo))
            }
        }
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let r = self.http.request(method, url);
        match self.target.kind {
            ForgeKind::GitHub => r
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"),
            ForgeKind::GitLab => r.header("PRIVATE-TOKEN", &self.token),
        }
    }

    async fn send(&self, r: reqwest::RequestBuilder) -> Result<Value, ForgeError> {
        let resp = r
            .send()
            .await
            .map_err(|e| ForgeError::Unavailable(e.without_url().to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| ForgeError::Unavailable(e.without_url().to_string()))?;
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(ForgeError::Unavailable(format!("HTTP {status}")));
        }
        if !status.is_success() {
            let short: String = penelope_observe::redact(&body)
                .chars()
                .take(ERROR_BODY_CHARS)
                .collect();
            return Err(ForgeError::Refused(format!("HTTP {status} : {short}")));
        }
        serde_json::from_str(&body)
            .map_err(|e| ForgeError::Unavailable(format!("réponse illisible : {e}")))
    }

    fn read_pr(&self, v: &Value) -> Option<PullRequest> {
        let (number, url) = match self.target.kind {
            ForgeKind::GitHub => (v["number"].as_u64()?, v["html_url"].as_str()?),
            ForgeKind::GitLab => (v["iid"].as_u64()?, v["web_url"].as_str()?),
        };
        Some(PullRequest {
            number,
            url: url.to_string(),
            state: v["state"].as_str().unwrap_or_default().to_string(),
        })
    }

    /// La PR de `head` vers `base`, ouverte ou non : celle d'une vie antérieure du run
    /// est retrouvée plutôt que recréée.
    pub async fn find_pr(&self, head: &str, base: &str) -> Result<Option<PullRequest>, ForgeError> {
        let (url, query) = match self.target.kind {
            ForgeKind::GitHub => {
                let owner = self.target.repo.split('/').next().unwrap_or_default();
                (
                    format!("{}/pulls", self.project()),
                    vec![
                        ("head", format!("{owner}:{head}")),
                        ("base", base.to_string()),
                        ("state", "all".to_string()),
                    ],
                )
            }
            ForgeKind::GitLab => (
                format!("{}/merge_requests", self.project()),
                vec![
                    ("source_branch", head.to_string()),
                    ("target_branch", base.to_string()),
                    ("state", "all".to_string()),
                ],
            ),
        };
        let v = self
            .send(self.request(reqwest::Method::GET, &url).query(&query))
            .await?;
        Ok(v.as_array()
            .and_then(|a| a.iter().find_map(|p| self.read_pr(p))))
    }

    /// Ouvre la PR de `head` vers `base`.
    pub async fn open_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PullRequest, ForgeError> {
        let (url, payload) = match self.target.kind {
            ForgeKind::GitHub => (
                format!("{}/pulls", self.project()),
                json!({"title": title, "head": head, "base": base, "body": body}),
            ),
            ForgeKind::GitLab => (
                format!("{}/merge_requests", self.project()),
                json!({"title": title, "source_branch": head, "target_branch": base,
                       "description": body}),
            ),
        };
        let v = self
            .send(self.request(reqwest::Method::POST, &url).json(&payload))
            .await?;
        self.read_pr(&v).ok_or_else(|| {
            ForgeError::Unavailable("réponse sans numéro ni adresse de PR".to_string())
        })
    }

    /// Le verdict de la CI sur le commit `sha`.
    pub async fn ci(&self, sha: &str) -> Result<CiVerdict, ForgeError> {
        match self.target.kind {
            ForgeKind::GitHub => {
                let base = format!("{}/commits/{sha}", self.project());
                let runs = self
                    .send(self.request(reqwest::Method::GET, &format!("{base}/check-runs")))
                    .await?;
                let status = self
                    .send(self.request(reqwest::Method::GET, &format!("{base}/status")))
                    .await?;
                Ok(verdicts::github_ci(&runs, &status))
            }
            ForgeKind::GitLab => {
                let url = format!("{}/pipelines", self.project());
                let pipelines = self
                    .send(
                        self.request(reqwest::Method::GET, &url)
                            .query(&[("sha", sha)]),
                    )
                    .await?;
                Ok(verdicts::gitlab_ci(&pipelines))
            }
        }
    }
}
