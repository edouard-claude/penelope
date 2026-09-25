//! Le transcript sur lequel travaille un tour.

use crate::steering::Steer;
use penelope_context::journal::Provenance;
use penelope_context::tiers::{Tiers, TileMap};
use penelope_kernel::canonical::sha256_hex;
use penelope_llm::types::ChatMessage;
use std::sync::Mutex;

/// Le transcript sur lequel travaille un tour.
#[async_trait::async_trait]
pub trait Conversation: Send + Sync {
    /// Messages à envoyer au modèle pour la prochaine itération (projection complète,
    /// prompt système compris).
    async fn request_messages(&self) -> anyhow::Result<Vec<ChatMessage>>;
    /// Ajoute un message au transcript.
    async fn record(&self, message: &ChatMessage, eager: bool) -> anyhow::Result<()>;
    /// Ajoute un message avec sa provenance (tour, étape, appel au modèle), que le
    /// journal garde (épopée #208, T5). Un transcript sans journal l'ignore.
    async fn record_as(
        &self,
        message: &ChatMessage,
        eager: bool,
        _prov: &Provenance,
    ) -> anyhow::Result<()> {
        self.record(message, eager).await
    }
    /// Écrit un message du propriétaire réclamé pendant le tour (`Inbox::claim`). Un
    /// transcript de session l'écrit une seule fois sous son identifiant de file, avec
    /// son heure d'arrivée (#161).
    async fn record_steer(&self, steer: &Steer) -> anyhow::Result<()> {
        match &steer.text {
            Some(text) => self.record(&ChatMessage::user(text.as_str()), false).await,
            None => Ok(()),
        }
    }
    /// Queue du transcript, sans prompt système : sert à retrouver les appels en attente.
    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>>;
    /// Compacte tout de suite après un dépassement de fenêtre prouvé par le provider.
    /// Vrai si des messages ont été résumés : la requête peut être reconstruite.
    async fn compact_for_overflow(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
    /// Applique le budget d'admission (§5.4 niveau 1) aux `count` derniers résultats
    /// d'outils **ensemble** : cinq résultats de 20 k tokens ne passent pas parce
    /// qu'aucun ne dépasse le seuil à lui seul (issue #52).
    async fn admit_tool_results(&self, _count: usize) -> anyhow::Result<()> {
        Ok(())
    }
    /// Le préfixe stable du prompt système, avec sa découpe en tuiles quand elle est
    /// connue : l'audit du prompt (issue #205) l'enregistre sous son empreinte.
    fn prompt_prefix(&self) -> Option<PromptPrefix> {
        None
    }
}

/// Compaction à la demande d'une session (§5.4 : une tentative bornée sur dépassement).
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    async fn compact_now(&self, session_id: &str) -> anyhow::Result<bool>;
}

/// Transcript en mémoire : sous-agents, tests, appels ponctuels.
pub struct MemoryConversation {
    system: String,
    messages: Mutex<Vec<ChatMessage>>,
}

impl MemoryConversation {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        MemoryConversation {
            system: system.into(),
            messages: Mutex::new(vec![ChatMessage::user(user.into())]),
        }
    }

    pub fn messages(&self) -> Vec<ChatMessage> {
        self.messages
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[async_trait::async_trait]
impl Conversation for MemoryConversation {
    async fn request_messages(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let mut v = vec![ChatMessage::system(self.system.clone())];
        v.extend(self.messages());
        Ok(v)
    }

    async fn record(&self, message: &ChatMessage, _eager: bool) -> anyhow::Result<()> {
        self.messages
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(message.clone());
        Ok(())
    }

    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>> {
        Ok(self.messages())
    }

    fn prompt_prefix(&self) -> Option<PromptPrefix> {
        Some(PromptPrefix::plain(&self.system))
    }
}

/// Le préfixe tel qu'il part au modèle, et sa découpe quand la conversation la connaît.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptPrefix {
    pub rendered: String,
    pub tiles: Option<TileMap>,
}

impl PromptPrefix {
    /// Préfixe d'une conversation de session : la découpe suit les tuiles.
    pub fn of(tiers: &Tiers) -> PromptPrefix {
        PromptPrefix {
            rendered: tiers.prefix(),
            tiles: Some(tiers.tile_map()),
        }
    }

    /// Préfixe d'un transcript sans tuiles (sous-agent, workflow) : le texte seul.
    pub fn plain(rendered: impl Into<String>) -> PromptPrefix {
        PromptPrefix {
            rendered: rendered.into(),
            tiles: None,
        }
    }

    pub fn hash(&self) -> String {
        sha256_hex(self.rendered.as_bytes())
    }
}
