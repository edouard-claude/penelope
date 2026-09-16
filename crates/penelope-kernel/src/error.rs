//! Type d'erreur commun au noyau.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum KernelError {
    #[error("stockage : {0}")]
    Store(#[from] penelope_store::StoreError),

    #[error("json : {0}")]
    Json(#[from] serde_json::Error),

    #[error("entrée-sortie : {0}")]
    Io(#[from] std::io::Error),

    #[error("configuration invalide : {0}")]
    Config(String),

    #[error("validation de schéma : {0}")]
    Schema(String),

    #[error("chaîne d'audit rompue à l'événement {id} : attendu {expected}, calculé {actual}")]
    AuditChain {
        id: i64,
        expected: String,
        actual: String,
    },

    #[error("état invalide : {0}")]
    InvalidState(String),

    #[error("introuvable : {0}")]
    NotFound(String),

    #[error("conflit : {0}")]
    Conflict(String),

    #[error("budget dépassé : {0}")]
    BudgetExceeded(String),

    #[error("annulé")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = KernelError> = std::result::Result<T, E>;

impl KernelError {
    pub fn other(msg: impl Into<String>) -> Self {
        KernelError::Other(msg.into())
    }
    pub fn config(msg: impl Into<String>) -> Self {
        KernelError::Config(msg.into())
    }
    pub fn not_found(msg: impl Into<String>) -> Self {
        KernelError::NotFound(msg.into())
    }
    pub fn invalid_state(msg: impl Into<String>) -> Self {
        KernelError::InvalidState(msg.into())
    }
}
