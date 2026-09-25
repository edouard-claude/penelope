//! Le canal du propriétaire vu du cœur (épopée #208, T36, `design/v1/README.md` §3.5).
//!
//! Les gabarits de cartes, leurs boutons et les jetons d'action vivent dans la
//! passerelle ; le cœur n'en lit que le texte, par le port [`Cards`]. La passerelle se
//! branche dans [`Channel`], que `Services` porte : le cœur ne nomme jamais le canal.

use crate::bus::{ChannelDelivery, Origin};
use crate::ports::Slot;
use std::path::Path;
use std::sync::Arc;

/// Texte d'un gabarit de carte et ses variables (`{{nom}}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardTemplate {
    pub body: String,
    pub variables: Vec<String>,
}

/// Les cartes du canal : gabarits de texte, liens profonds.
#[async_trait::async_trait]
pub trait Cards: Send + Sync {
    /// Identifiants des gabarits que le canal sait rendre : ce que valident les
    /// workflows (étape `user`).
    fn catalog(&self) -> Vec<String>;
    /// Texte d'un gabarit, `None` s'il n'existe pas.
    fn template(&self, id: &str) -> Option<CardTemplate>;
    /// Lien vers un écran ou une commande du canal (issue #30), quand il en a un.
    async fn deep_link(&self, payload: &str) -> Option<String> {
        let _ = payload;
        None
    }
}

/// Construit les cartes du canal au démarrage, avant tout workflow : dossier des
/// gabarits de l'utilisateur et base (liens profonds). Passé par la composition.
pub type CardsOf = fn(&Path, penelope_store::Store) -> Arc<dyn Cards>;

/// Ce que la passerelle du canal branche dans le cœur.
#[derive(Default, Clone)]
pub struct Channel {
    pub cards: Slot<dyn Cards>,
    /// Livraison des tours et destinations : le même `Slot` que `Hooks::delivery` du
    /// daemon, pour que ce qui ne tient que `Services` (planifications) le voie.
    pub delivery: Slot<dyn ChannelDelivery>,
}

impl Channel {
    /// Texte d'un gabarit du canal branché.
    pub fn template(&self, id: &str) -> Option<CardTemplate> {
        self.cards.get().and_then(|c| c.template(id))
    }

    /// Gabarits connus du canal branché ; vide sans canal (la validation des workflows
    /// ne les vérifie alors pas).
    pub fn catalog(&self) -> Vec<String> {
        self.cards.get().map(|c| c.catalog()).unwrap_or_default()
    }

    /// Nom lisible d'une conversation, par le canal branché (issue #124).
    pub async fn describe(&self, origin: &Origin) -> String {
        match self.delivery.get() {
            Some(channel) => channel.describe_origin(origin).await,
            None => None,
        }
        .unwrap_or_else(|| "aucune conversation (canal non configuré)".into())
    }
}
