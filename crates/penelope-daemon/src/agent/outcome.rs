//! Issue d'un tour et ce qu'il montre en se déroulant.

use super::*;

/// Issue d'un tour.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// Réponse finale produite.
    Answered {
        text: String,
        iterations: u32,
        cost_usd: f64,
    },
    /// Le tour attend une approbation : il reprendra après décision.
    AwaitingApproval {
        approval_id: String,
    },
    /// Arrêté par le détecteur de boucles (issue #31) : une réponse sans outil qui cite
    /// l'erreur réelle, 2 ou 3 suites à proposer en boutons, et le rapport technique,
    /// gardé pour les événements et les journaux.
    LoopAborted {
        report: String,
        answer: String,
        choices: Vec<String>,
    },
    /// Annulé (bouton stop, `/stop`, annulation du run).
    Cancelled,
    /// Budget épuisé : périmètre (`jour`, `session`, `run`), dépense et plafond.
    BudgetExceeded {
        scope: String,
        spent_usd: f64,
        limit_usd: f64,
    },
    Failed {
        error: String,
    },
}

/// Ce que le tour montre pendant qu'il se déroule.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// Fragment de réponse en cours d'écriture.
    Delta(String),
    /// Fragment de raisonnement (affiché seulement si l'interface le demande).
    Reasoning(String),
    ToolCall {
        name: String,
        args: Value,
    },
    ToolResult {
        name: String,
        ok: bool,
        preview: String,
    },
    /// Une approbation est demandée : c'est au canal de la présenter.
    Approval {
        id: String,
        tool: String,
        risk: RiskClass,
        arguments: Value,
        reason: String,
        double: bool,
    },
    /// Le modèle réellement utilisé, après routage et repli éventuel.
    Model {
        model_id: String,
    },
}

/// Destinataire des événements d'un tour. Synchrone : un canal derrière suffit.
pub trait TurnSink: Send + Sync {
    fn emit(&self, event: TurnEvent);
}

/// Sink qui ignore tout.
pub struct NullSink;

impl TurnSink for NullSink {
    fn emit(&self, _event: TurnEvent) {}
}

/// Sink qui garde tout, pour les tests et la collecte.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<TurnEvent>>,
}

impl RecordingSink {
    pub fn events(&self) -> Vec<TurnEvent> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl TurnSink for RecordingSink {
    fn emit(&self, event: TurnEvent) {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(event);
    }
}
