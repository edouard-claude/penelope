//! Ce que l'appelant sait d'un message au-delà de son contenu (épopée #208, tâche T5) :
//! la forme que le port `Conversation` reçoit et que `penelope-context` traduit en
//! événement `conv.*`.

use super::CallRecord;
use serde::{Deserialize, Serialize};

/// Origine d'un message utilisateur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSource {
    Owner,
    Merged,
    Trigger,
    Nudge,
    Photo,
    Import,
}

/// Ce que l'appelant sait d'un message au-delà de son contenu : le tour, l'étape, la
/// provenance d'un message utilisateur, l'appel au modèle d'une réponse.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Provenance {
    /// Tour d'origine (`origin_turn`).
    pub turn: Option<String>,
    /// Itération de la boucle.
    pub step: u32,
    /// Origine d'un message utilisateur ; `owner` par défaut.
    pub source: Option<UserSource>,
    /// Clé d'idempotence d'un message venu de la file.
    pub turn_message_id: Option<String>,
    pub arrived_at: Option<String>,
    /// Message absorbé pendant le tour.
    pub mid_turn: bool,
    /// Issue d'un résultat d'outil ; vrai par défaut.
    pub ok: Option<bool>,
    /// L'appel qui a produit une réponse : modèle, usage, empreintes. Le contenu, lui,
    /// vient toujours du message écrit.
    pub call: Option<Box<CallRecord>>,
}

impl Provenance {
    /// Un message utilisateur de cette origine.
    pub fn user(source: UserSource) -> Self {
        Provenance {
            source: Some(source),
            ..Default::default()
        }
    }

    /// Un message utilisateur venu de la file, avec sa clé et son heure d'arrivée.
    pub fn queued(source: UserSource, turn_message_id: &str, arrived_at: &str) -> Self {
        Provenance {
            source: Some(source),
            turn_message_id: Some(turn_message_id.to_string()),
            arrived_at: Some(arrived_at.to_string()),
            ..Default::default()
        }
    }
}
