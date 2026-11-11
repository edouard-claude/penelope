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
    /// Fournisseurs des modèles de repli (#259) ; `None` : le repli passe par `provider`.
    pub(crate) providers: Option<Arc<dyn FallbackProviders>>,
    /// Le tour peut finir sans réponse (#467) : voir [`AgentLoop::may_end_silent`].
    pub(crate) silent_end: bool,
}

/// Appels au modèle par tour : au-delà, le tour s'arrête ; « Continuer » en redonne
/// autant sur le même transcript.
pub const TURN_CALLS: u32 = 24;

/// Début du message d'un tour arrêté par son plafond d'appels : défini avec les issues de
/// tour, la mission (#364) le reconnaît aussi.
pub use penelope_app::outcome::CALLS_EXHAUSTED;

impl AgentLoop {
    pub fn new(services: Arc<AgentServices>, provider: Arc<dyn Provider>) -> Self {
        AgentLoop {
            services,
            provider,
            max_iterations: TURN_CALLS,
            inbox: None,
            providers: None,
            silent_end: false,
        }
    }

    /// Un repli part chez le fournisseur de son modèle, pas chez celui du modèle principal
    /// (#259) : un repli `openrouter:` d'un modèle `local:` visait le serveur local arrêté.
    pub fn with_providers(mut self, providers: Arc<dyn FallbackProviders>) -> Self {
        self.providers = Some(providers);
        self
    }

    /// Réclame les messages arrivés pendant le tour aux points de contrôle (§3.4).
    pub fn with_inbox(mut self, inbox: Option<Arc<dyn Inbox>>) -> Self {
        self.inbox = inbox;
        self
    }

    /// Personne n'attend la réponse de ce tour (l'exécution d'une planification) et rien
    /// ne l'oblige à écrire : après au moins un outil exécuté, une réponse sans texte ni
    /// appel d'outil, finie normalement, est sa fin, pas une panne à relancer (#467). Un
    /// prompt planifié qui dit « n'envoie rien s'il n'y a rien de neuf » était relancé
    /// avec « réponds maintenant, en texte », et le propriétaire recevait le « rien de
    /// neuf » qu'il avait demandé de ne pas recevoir.
    pub fn may_end_silent(mut self, silent: bool) -> Self {
        self.silent_end = silent;
        self
    }

    /// Tranche une approbation et crée la règle éventuelle (§9.2). Ne relance pas le
    /// tour : c'est au canal de remettre un tour `resume` en file.
    pub async fn decide_approval(
        &self,
        approval_id: &str,
        decision: &Decision,
    ) -> anyhow::Result<decisions::Decided> {
        decisions::decide_approval(&self.services, approval_id, decision).await
    }
}
