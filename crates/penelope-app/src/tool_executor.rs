//! Exécution d'un appel d'outil, vue de la boucle.

use penelope_kernel::risk::{PolicyDecision, RiskClass};
use penelope_llm::provider::CancelToken;
use penelope_tools::ToolOutcome;
use serde_json::Value;
use std::sync::Arc;

/// Ce qu'il faut savoir d'un appel avant de l'autoriser.
#[derive(Debug, Clone, PartialEq)]
pub struct CallInfo {
    /// Nom sur lequel portent la politique et le ledger : pour `tool_call`, c'est
    /// l'outil MCP visé, pas le méta-outil.
    pub effective_name: String,
    pub risk: RiskClass,
    pub idempotent: bool,
    /// Politique imposée par la déclaration du serveur MCP (`tool_policy`).
    pub policy: Option<PolicyDecision>,
}

/// Exécution concrète d'un outil, fournie par le daemon (ou simulée en test).
#[async_trait::async_trait]
pub trait ToolExecutor {
    async fn execute(
        &self,
        name: &str,
        args: &Value,
    ) -> Result<ToolOutcome, penelope_tools::ToolError>;

    /// Exécute en écoutant l'arrêt demandé : `/stop` pendant un `shell_exec` ou un
    /// sous-agent doit les interrompre, pas attendre leur délai (issue #57).
    async fn execute_cancellable(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancelToken,
    ) -> Result<ToolOutcome, penelope_tools::ToolError> {
        let _ = cancel;
        self.execute(name, args).await
    }

    /// Vérifie un appel **avant** toute demande d'approbation : un appel qui ne pourrait
    /// pas aboutir ne coûte pas une carte au propriétaire (issue #117). Par défaut : rien
    /// à vérifier.
    async fn precheck(&self, name: &str, args: &Value) -> Result<(), penelope_tools::ToolError> {
        let _ = (name, args);
        Ok(())
    }

    /// Forme canonique d'un appel, avant toute décision : ce que la garde de boucle, la
    /// politique, la carte et l'exécution voient (issue #123). `None` : l'appel tel quel.
    fn normalise_call(&self, name: &str, args: &Value) -> Option<Value> {
        let _ = (name, args);
        None
    }

    /// Racine pour les motifs de chemins relatifs des règles d'approbation.
    fn policy_workspace(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Copie détachable de cet exécuteur, pour un appel qui sort du tour (issue #204).
    ///
    /// Un job survit au tour qui l'a lancé : il ne peut donc pas emprunter l'exécuteur du
    /// tour. `None` — le défaut — signifie « cet exécuteur ne sait pas se détacher » :
    /// l'appel s'exécute alors comme avant, dans le tour.
    fn detached(&self) -> Option<Arc<dyn ToolExecutor + Send + Sync>> {
        None
    }

    /// Risque et nom effectif d'un appel. Par défaut : le catalogue natif.
    async fn describe_call(&self, name: &str, args: &Value) -> CallInfo {
        let _ = args;
        CallInfo {
            effective_name: name.to_string(),
            risk: penelope_tools::effective_risk(name, &Default::default()),
            idempotent: penelope_tools::tool_spec(name)
                .map(|s| s.idempotent)
                .unwrap_or(false),
            policy: None,
        }
    }
}
