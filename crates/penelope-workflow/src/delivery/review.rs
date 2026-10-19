//! Les commentaires de la PR d'un run comme source (#396, point 3) : lus pendant la
//! livraison, filtrés par auteur, bornés, et une demande de changements d'un auteur
//! autorisé relance une reprise bornée du code.
//!
//! Rien n'est supposé : sans section `[review]` dans `.penelope/delivery.toml`, aucun
//! auteur n'est autorisé, et tout commentaire est ignoré (journalisé).

use super::config::ReviewSection;
use serde_json::Value;

/// Commentaires au plus par événement.
pub const MAX_COMMENTS: usize = 20;
/// Caractères au plus par commentaire.
pub const MAX_COMMENT_CHARS: usize = 1200;

/// Un commentaire ou une revue de la PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrComment {
    /// Identifiant stable pour le dédoublonnage : `issue:12`, `review:34`, `note:56`.
    pub id: String,
    pub author: String,
    /// Rôle de l'auteur sur le dépôt (`owner`, `member`, `collaborator`…), en minuscules ;
    /// vide quand le forgeur ne le dit pas (GitLab).
    pub role: String,
    pub body: String,
    /// Une demande de changements explicite.
    pub changes_requested: bool,
}

impl PrComment {
    /// L'auteur est-il autorisé par la section `[review]` ?
    pub fn authorised(&self, review: &ReviewSection) -> bool {
        let login = self.author.to_lowercase();
        review.logins.iter().any(|l| l.to_lowercase() == login)
            || (!self.role.is_empty() && review.roles.iter().any(|r| r.to_lowercase() == self.role))
    }
}

fn str_of(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

/// GitHub : commentaires de conversation (`/issues/{n}/comments`) et revues
/// (`/pulls/{n}/reviews`). Une revue sans texte ni demande de changements est écartée.
pub fn github(issue_comments: &Value, reviews: &Value) -> Vec<PrComment> {
    let mut out = Vec::new();
    for c in issue_comments.as_array().into_iter().flatten() {
        out.push(PrComment {
            id: format!("issue:{}", c["id"]),
            author: str_of(&c["user"], "login"),
            role: str_of(c, "author_association").to_lowercase(),
            body: str_of(c, "body"),
            changes_requested: false,
        });
    }
    for r in reviews.as_array().into_iter().flatten() {
        let changes = r["state"] == "CHANGES_REQUESTED";
        let body = str_of(r, "body");
        if !changes && body.trim().is_empty() {
            continue;
        }
        out.push(PrComment {
            id: format!("review:{}", r["id"]),
            author: str_of(&r["user"], "login"),
            role: str_of(r, "author_association").to_lowercase(),
            body,
            changes_requested: changes,
        });
    }
    out
}

/// GitLab : les notes de la merge request, hors notes système. GitLab n'a pas d'état
/// « changements demandés » sur une note : une note qui commence par `/request_changes`
/// ou dit « changes requested » en tient lieu. Le rôle n'y figure pas : seuls les
/// logins autorisent.
pub fn gitlab(notes: &Value) -> Vec<PrComment> {
    notes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["system"] != true)
        .map(|n| {
            let body = str_of(n, "body");
            let lower = body.to_lowercase();
            PrComment {
                id: format!("note:{}", n["id"]),
                author: str_of(&n["author"], "username"),
                role: String::new(),
                changes_requested: lower.trim_start().starts_with("/request_changes")
                    || lower.contains("changes requested"),
                body,
            }
        })
        .collect()
}

/// Les commentaires d'auteurs autorisés en lignes bornées, `MAX_COMMENTS` au plus.
pub fn quoted(comments: &[PrComment]) -> String {
    let mut out = String::new();
    for c in comments.iter().take(MAX_COMMENTS) {
        let body: String = c.body.chars().take(MAX_COMMENT_CHARS).collect();
        let cut = if c.body.chars().count() > MAX_COMMENT_CHARS {
            " …"
        } else {
            ""
        };
        let kind = if c.changes_requested {
            " (changements demandés)"
        } else {
            ""
        };
        out.push_str(&format!("— {}{kind} :\n{}{cut}\n", c.author, body.trim()));
    }
    if comments.len() > MAX_COMMENTS {
        out.push_str(&format!(
            "… et {} autre(s)\n",
            comments.len() - MAX_COMMENTS
        ));
    }
    out
}
