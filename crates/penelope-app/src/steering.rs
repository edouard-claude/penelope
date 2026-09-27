//! Steering explicite : les messages du propriétaire arrivés pendant un tour
//! (`design/v1/boucle-et-outils.md` §3.4, épopée #208, lot K).
//!
//! La boucle les réclame à des points nommés (`Checkpoint`) ; elle écrit elle-même les
//! messages réclamés dans le transcript, émet `turn.merged` et décide de ce qui arrive
//! aux appels d'outils non démarrés. Lire la conversation ne réclame plus rien.

/// Où la boucle réclame les messages arrivés pendant le tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    /// Avant chaque appel au modèle.
    BeforeModelCall,
    /// Avant chaque appel d'outil d'un lot : un message laisse finir l'appel en cours,
    /// les appels non démarrés ne partent plus.
    BetweenCalls,
}

/// Un message du propriétaire réclamé par le tour en cours.
#[derive(Debug, Clone, PartialEq)]
pub struct Steer {
    /// Identifiant du message dans la file : la clé d'idempotence de son écriture (#161).
    pub id: String,
    /// Texte du message ; `None` : rien à écrire (le message compte pour la fusion).
    pub text: Option<String>,
    /// Heure d'arrivée dans la file, pas celle de la réclamation (#161).
    pub arrived_at: String,
}

/// Messages du propriétaire arrivés pendant le tour, sous le bail du tour.
#[async_trait::async_trait]
pub trait Inbox: Send + Sync {
    /// Réclame les messages arrivés depuis la dernière réclamation. Un message réclamé
    /// l'est une fois : la boucle doit l'écrire.
    async fn claim(&self, at: Checkpoint) -> anyhow::Result<Vec<Steer>>;
}
