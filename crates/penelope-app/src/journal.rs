//! Le vocabulaire du journal que parlent les ports de la boucle (épopée #208, lot J).
//!
//! `Conversation::push_with` prend déjà une [`Provenance`] ; la boucle écrit aussi ses
//! tentatives (`conv.attempt`) et les bornes de ses tours (`turn.started`,
//! `turn.finished`). Ces charges restent définies dans `penelope-context`, qui les relit
//! et les plie : elles s'appuient sur les types de `penelope-llm` et ne peuvent pas
//! descendre dans `penelope-kernel`. Elles sont réexportées ici, à l'identique, pour que
//! la boucle n'importe que `penelope-app` (`design/v1/README.md` §3.2).

pub use penelope_context::journal::{
    AssistantPayload, AttemptCause, AttemptPayload, ConvEvent, KIND_TURN_FINISHED,
    KIND_TURN_STARTED, Provenance, TurnCall, TurnEnd, TurnIdentity, finished_payload,
    interrupted_payload, is_purged, started_payload,
};

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
