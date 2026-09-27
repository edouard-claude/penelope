//! Ce que penelope-app écrit et calcule par le moteur de contexte (épopée #208, lot K).
//!
//! Les ports de la boucle (`attempts`, `conversation`, `outcome`, `steering`,
//! `tool_executor`) ne parlent que des types du noyau et de `penelope-llm` : la boucle ne
//! voit rien de `penelope-context`, même par un réexport (test d'architecture
//! `the_agent_crate_reaches_no_context_type_through_its_ports`). Ce module est, pour
//! eux, le seul pont vers lui, du côté de la composition : l'écriture des tentatives en
//! `conv.attempt`, et le préfixe d'une conversation découpé en tuiles.

use crate::conversation::PromptPrefix;
use penelope_context::tiers::Tiers;

impl PromptPrefix {
    /// Préfixe d'une conversation de session : la découpe suit les tuiles.
    pub fn of(tiers: &Tiers) -> PromptPrefix {
        PromptPrefix {
            rendered: tiers.prefix(),
            tiles: Some(tiers.tile_map()),
        }
    }
}

use crate::attempts::{Attempt, AttemptSink};
use penelope_kernel::event::{EventDraft, EventLog};

/// Le port [`AttemptSink`] sur le journal : chaque tentative devient un `conv.attempt`
/// de la session, versionné (`"v"`) et haché comme les autres événements de contenu.
pub struct JournalAttempts(pub EventLog);

#[async_trait::async_trait]
impl AttemptSink for JournalAttempts {
    async fn record(&self, session_id: &str, attempt: &Attempt) -> anyhow::Result<()> {
        let event = penelope_context::journal::ConvEvent::Attempt(attempt.clone());
        self.0
            .append(EventDraft::new(event.kind(), event.payload()).session(session_id))
            .await?;
        Ok(())
    }
}
