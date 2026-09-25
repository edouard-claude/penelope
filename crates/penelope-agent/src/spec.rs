//! Paramètres d'un tour et exécuteur de tours.

use super::*;

/// Paramètres d'un tour.
#[derive(Clone)]
pub struct TurnSpec {
    pub session_id: String,
    pub run_id: Option<String>,
    /// Tour d'origine, pour attribuer les coûts à la requête du propriétaire.
    pub turn_id: Option<String>,
    pub model_id: String,
    /// Modèles de repli, dans l'ordre, si le principal est en panne (§10.3 point 5).
    pub fallback_models: Vec<String>,
    pub tools: Vec<ToolDef>,
    /// Outils autorisés (liste blanche d'étape ou de skill) ; vide = tous.
    pub allowed_tools: Vec<String>,
    pub cancel: CancelToken,
}

/// Un exécuteur de tour.
pub struct AgentLoop {
    pub services: Arc<AgentServices>,
    pub provider: Arc<dyn Provider>,
    pub max_iterations: u32,
    /// Messages du propriétaire arrivés pendant le tour (§3.4) ; `None` : aucun.
    pub(crate) inbox: Option<Arc<dyn Inbox>>,
}

/// Appels au modèle par tour : au-delà, le tour s'arrête ; « Continuer » en redonne
/// autant sur le même transcript.
pub const TURN_CALLS: u32 = 24;

/// Début du message d'un tour arrêté par son plafond d'appels (pas une erreur) : Telegram
/// le reconnaît pour proposer « Continuer » plutôt que « Réessayer » (issue #139).
pub const CALLS_EXHAUSTED: &str = "le tour n'a pas convergé";

impl AgentLoop {
    pub fn new(services: Arc<AgentServices>, provider: Arc<dyn Provider>) -> Self {
        AgentLoop {
            services,
            provider,
            max_iterations: TURN_CALLS,
            inbox: None,
        }
    }

    /// Réclame les messages arrivés pendant le tour aux points de contrôle (§3.4).
    pub fn with_inbox(mut self, inbox: Option<Arc<dyn Inbox>>) -> Self {
        self.inbox = inbox;
        self
    }

    /// Tranche une approbation et crée la règle éventuelle (§9.2). Ne relance pas le
    /// tour : c'est au canal de remettre un tour `resume` en file.
    pub async fn decide_approval(
        &self,
        approval_id: &str,
        decision: &Decision,
    ) -> anyhow::Result<bool> {
        decisions::decide_approval(&self.services, approval_id, decision).await
    }
}
