//! `llm.attachment_rejected` : des images que le fournisseur a refusées (issue #231).
//!
//! Pas un contenu : l'historique garde les images. Le pliage le lit comme les bornes d'un
//! tour, et la requête dérivée remplace chaque image nommée par sa mention, comme la
//! boucle le fait sur la copie qu'elle envoie.

use serde::{Deserialize, Serialize};

/// Le kind de l'événement, écrit par la boucle et lu par le pliage.
pub const KIND_ATTACHMENT_REJECTED: &str = "llm.attachment_rejected";

/// `llm.attachment_rejected` : les images d'une requête refusée, par empreinte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRejectedPayload {
    #[serde(default)]
    pub model: String,
    /// Ce qu'on dit de l'image : « trop lourde », « format refusé »…
    pub motif: String,
    #[serde(default)]
    pub error: String,
    /// SHA-256 de l'URL de chaque image retirée, jamais son contenu.
    pub images: Vec<String>,
}
