//! Le port des hooks vu de la boucle (#384) : isolé, sans autre module du crate, pour que
//! la boucle l'atteigne sans atteindre `Services` (règle `reach` d'archtest).

use serde_json::Value;

/// Ce qu'un hook `pre_tool` décide d'un appel (#384). Pas d'`Allow` : un hook ne desserre
/// jamais la politique (décision 0023).
#[derive(Debug, Clone, PartialEq)]
pub enum PreToolGate {
    Continue,
    /// Refus, motif au modèle.
    Deny {
        hook: String,
        reason: String,
    },
    /// Carte forcée, même si une règle ou le mode l'aurait laissé passer.
    Ask {
        hook: String,
        reason: String,
    },
    /// Nouveaux arguments pour le même outil : l'appel repasse toute la politique.
    Args {
        hook: String,
        args: Value,
    },
}

/// Les hooks du propriétaire vus de la boucle (#384) : elle ne voit pas l'exécuteur qui
/// les lance.
#[async_trait::async_trait]
pub trait HookGate: Send + Sync {
    /// Les hooks `pre_tool` d'un appel, après la politique, avant la carte ou l'outil.
    async fn pre_tool(
        &self,
        _session_id: &str,
        _run_id: Option<&str>,
        _tool: &str,
        _args: &Value,
    ) -> PreToolGate {
        PreToolGate::Continue
    }
    /// Les hooks `turn_end` d'une réponse prête : `Some((hook, note))` la retient.
    async fn turn_end(
        &self,
        _session_id: &str,
        _run_id: Option<&str>,
        _answer: &str,
    ) -> Option<(String, String)> {
        None
    }
    /// Un `ask` de hook dans un tour sans personne pour répondre : la mission passe
    /// bloquée avec ce motif (#368).
    async fn blocked(&self, _session_id: &str, _run_id: Option<&str>, _reason: &str) {}
}

/// Aucun hook : la boucle de test, les sous-agents sans exécuteur.
pub struct NoHooks;

#[async_trait::async_trait]
impl HookGate for NoHooks {}
