//! Erreurs du canal Telegram.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TgError {
    #[error("transport : {0}")]
    Transport(String),

    #[error("api {code} : {description}")]
    Api { code: i32, description: String },

    #[error("limite de débit : réessayer dans {0} s")]
    RateLimited(u64),

    #[error("template : {0}")]
    Template(String),

    #[error("formulaire : {0}")]
    Form(String),

    #[error("stockage : {0}")]
    Store(#[from] penelope_store::StoreError),

    #[error("{0}")]
    Other(String),
}

pub type TgResult<T> = Result<T, TgError>;

impl TgError {
    pub fn is_retryable(&self) -> bool {
        match self {
            TgError::Transport(_) | TgError::RateLimited(_) => true,
            TgError::Api { code, .. } => *code >= 500,
            _ => false,
        }
    }

    /// Vrai si le chat n'existe plus : inutile de réessayer, il faut alerter.
    pub fn is_fatal_for_chat(&self) -> bool {
        match self {
            TgError::Api { code, description } => {
                *code == 403
                    || (*code == 400
                        && (description.contains("chat not found")
                            || description.contains("bot was blocked")))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_classification() {
        assert!(TgError::Transport("coupure".into()).is_retryable());
        assert!(TgError::RateLimited(3).is_retryable());
        assert!(
            TgError::Api {
                code: 502,
                description: "Bad Gateway".into()
            }
            .is_retryable()
        );
        assert!(
            !TgError::Api {
                code: 400,
                description: "Bad Request".into()
            }
            .is_retryable()
        );
    }

    #[test]
    fn fatal_chat_errors() {
        assert!(
            TgError::Api {
                code: 403,
                description: "Forbidden: bot was blocked by the user".into()
            }
            .is_fatal_for_chat()
        );
        assert!(
            TgError::Api {
                code: 400,
                description: "Bad Request: chat not found".into()
            }
            .is_fatal_for_chat()
        );
        assert!(!TgError::Transport("x".into()).is_fatal_for_chat());
    }
}
