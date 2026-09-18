//! Erreurs des outils natifs.
//!
//! Une erreur d'outil n'arrête pas le tour : elle est **renvoyée au modèle** pour qu'il
//! se corrige (§8.4, même principe que `isError` côté MCP).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("arguments invalides : {0}")]
    Invalid(String),

    #[error("refusé : {0}")]
    Denied(String),

    #[error("entrée-sortie : {0}")]
    Io(String),

    #[error("délai dépassé après {0} ms")]
    Timeout(u64),

    #[error("réseau : {0}")]
    Network(String),

    #[error("outil inconnu : {0}")]
    Unknown(String),

    /// Arguments refusés, avec les paramètres que l'outil attend : le modèle se corrige
    /// sans deviner le schéma (issue #110).
    #[error("arguments invalides pour `{tool}` : {reason}")]
    BadArguments {
        tool: String,
        reason: String,
        expected: String,
    },

    /// Outil inconnu, avec les noms proches (issue #110).
    #[error("outil inconnu : {name}")]
    NoSuchTool { name: String, close: Vec<String> },

    #[error("stockage : {0}")]
    Store(#[from] penelope_store::StoreError),

    #[error("plateforme : {0}")]
    Platform(#[from] penelope_platform::PlatformError),

    #[error("annulé")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type ToolResult<T> = Result<T, ToolError>;

impl ToolError {
    /// Rendu destiné au modèle : explicite, actionnable, sans trace interne.
    pub fn for_model(&self) -> String {
        match self {
            ToolError::Invalid(m) => format!("Erreur d'arguments : {m}. Corrige et réessaie."),
            ToolError::Denied(m) => format!(
                "Refusé : {m}. Ne réessaie pas à l'identique ; demande au propriétaire si \
                 c'est nécessaire."
            ),
            ToolError::Timeout(ms) => format!(
                "L'outil n'a pas répondu en {ms} ms. Réduis la portée de l'appel ou \
                 découpe le travail."
            ),
            ToolError::Network(m) => format!("Erreur réseau : {m}."),
            ToolError::Unknown(n) => format!(
                "L'outil `{n}` n'existe pas. Utilise `tool_search` pour trouver le bon nom."
            ),
            ToolError::BadArguments {
                tool,
                reason,
                expected,
            } => format!(
                "Erreur d'arguments pour `{tool}` : {reason}.\nParamètres attendus :\n{expected}\n\
                 Corrige l'appel avec ces paramètres ; ne le rejoue pas à l'identique."
            ),
            ToolError::NoSuchTool { name, close } if !close.is_empty() => format!(
                "L'outil `{name}` n'existe pas. Noms proches : {}. Sinon, `tool_search` trouve \
                 un outil par ce qu'il fait.",
                close
                    .iter()
                    .map(|c| format!("`{c}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ToolError::NoSuchTool { name, .. } => format!(
                "L'outil `{name}` n'existe pas. Utilise `tool_search` pour trouver le bon nom."
            ),
            ToolError::Cancelled => "Appel annulé.".into(),
            other => format!("Erreur : {other}"),
        }
    }

    /// Vrai si une nouvelle tentative identique a une chance d'aboutir.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ToolError::Timeout(_) | ToolError::Network(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_actionable() {
        assert!(
            ToolError::Invalid("champ `path` manquant".into())
                .for_model()
                .contains("Corrige")
        );
        assert!(
            ToolError::Denied("hors workspace".into())
                .for_model()
                .contains("Ne réessaie pas")
        );
        assert!(
            ToolError::Unknown("fs_raed".into())
                .for_model()
                .contains("tool_search")
        );
    }

    #[test]
    fn retry_classification() {
        assert!(ToolError::Timeout(1000).is_retryable());
        assert!(ToolError::Network("coupure".into()).is_retryable());
        assert!(!ToolError::Denied("x".into()).is_retryable());
        assert!(!ToolError::Invalid("x".into()).is_retryable());
    }
}
