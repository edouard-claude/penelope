//! Steering explicite (`design/v1/boucle-et-outils.md` §3.4, épopée #208, lot K).
//!
//! Un message du propriétaire arrivé pendant le tour est réclamé à un point nommé
//! (`Checkpoint`) par la boucle, qui l'écrit dans le transcript et émet `turn.merged`.
//! Lire la conversation (`Conversation::request_messages`) ne réclame plus rien.

use super::*;
pub use penelope_app::steering::{Checkpoint, Inbox, Steer};
use std::sync::atomic::{AtomicBool, Ordering};

/// Note de fusion : les requêtes qui suivent un message absorbé pendant le tour la
/// portent, après les messages système (§2.3).
pub const MERGE_NOTE: &str = "Un nouveau message utilisateur est arrivé pendant le tour ; ce qui précède est déjà exécuté. Tiens compte du nouveau message dans la réponse en cours.";

/// Résultat d'un appel non démarré quand un message du propriétaire arrive pendant le
/// lot (`Checkpoint::BetweenCalls`).
pub const NOT_RUN_NEW_MESSAGE: &str = "Non exécuté : nouveau message du propriétaire.";

/// Résultat d'un appel non démarré quand le propriétaire arrête le tour (`/stop`).
pub const NOT_RUN_STOPPED: &str = "Non exécuté : arrêté par le propriétaire.";

/// Note du tour qui suit un arrêt pendant des outils.
pub const INTERRUPTED_NOTE: &str =
    "Le tour précédent a été interrompu ; des outils ont pu s'exécuter partiellement.";

/// Ce que le harnais ajoute à la requête, nommé (§3.4).
#[derive(Debug, Clone, PartialEq)]
pub enum Injection {
    /// Note système placée **après** le dernier message utilisateur : le préfixe et la
    /// chaîne qui précède ne bougent pas (décision 0008). Jamais écrite dans
    /// l'historique : elle se déduit du transcript à chaque requête.
    Note(String),
}

impl Injection {
    pub fn apply(&self, messages: &mut Vec<ChatMessage>) {
        match self {
            Injection::Note(text) => {
                let at = messages
                    .iter()
                    .rposition(|m| m.role == Role::User)
                    .map_or(messages.len(), |i| i + 1);
                messages.insert(at, ChatMessage::system(text.as_str()));
            }
        }
    }
}

/// La note d'interruption, si les résultats d'outils qui précèdent les derniers
/// messages du propriétaire portent la trace d'un `/stop` (`NOT_RUN_STOPPED`).
pub fn interruption_note(messages: &[ChatMessage]) -> Option<Injection> {
    let last_user = messages.iter().rposition(|m| m.role == Role::User)?;
    let before = messages[..=last_user]
        .iter()
        .rev()
        .skip_while(|m| m.role == Role::User)
        .take_while(|m| m.role == Role::Tool);
    before
        .into_iter()
        .any(|m| m.text() == NOT_RUN_STOPPED)
        .then(|| Injection::Note(INTERRUPTED_NOTE.to_string()))
}

/// Les messages réclamés pendant un tour.
pub(crate) struct Steering<'a> {
    inbox: Option<&'a dyn Inbox>,
    /// Un message a été absorbé pendant ce tour : la note de fusion accompagne la suite.
    absorbed: AtomicBool,
}

impl<'a> Steering<'a> {
    pub(crate) fn new(inbox: Option<&'a dyn Inbox>) -> Self {
        Steering {
            inbox,
            absorbed: AtomicBool::new(false),
        }
    }

    /// Réclame les messages arrivés depuis la dernière réclamation, sans les écrire.
    pub(crate) async fn claim(&self, at: Checkpoint) -> anyhow::Result<Vec<Steer>> {
        match self.inbox {
            Some(inbox) => inbox.claim(at).await,
            None => Ok(Vec::new()),
        }
    }

    /// Écrit les messages réclamés, dans l'ordre d'arrivée, et le dit au journal.
    pub(crate) async fn record(
        &self,
        services: &AgentServices,
        spec: &TurnSpec,
        conv: &dyn Conversation,
        steers: &[Steer],
    ) -> anyhow::Result<()> {
        if steers.is_empty() {
            return Ok(());
        }
        for steer in steers {
            conv.record_steer(steer).await?;
        }
        self.absorbed.store(true, Ordering::SeqCst);
        services
            .events
            .append(
                TurnEventKind::Merged
                    .draft(json!({"turn": spec.turn_id, "count": steers.len(), "phase": "running"}))
                    .session(&spec.session_id),
            )
            .await?;
        Ok(())
    }

    /// Ajoute la note de fusion après les messages système si un message a été absorbé.
    pub(crate) fn with_merge_note(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        if self.absorbed.load(Ordering::SeqCst) {
            let index = messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            messages.insert(index, ChatMessage::system(MERGE_NOTE));
        }
        messages
    }
}
