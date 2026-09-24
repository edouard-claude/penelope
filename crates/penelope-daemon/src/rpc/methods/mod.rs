//! Les méthodes servies, rangées par domaine, et leur aiguillage.

use super::*;

mod approvals;
mod codex;
mod mcp;
mod memory;
mod ops;
mod sessions;
mod status_config;
mod workflows;

impl Rpc {
    /// Le futur du répartiteur porte l'état de **toutes** les méthodes servies : construit
    /// sur la pile de l'appelant, il la faisait déborder au test dès qu'une branche
    /// s'ajoutait (issues #145 et #146). Ses deux appelants le mettent donc sur le tas —
    /// une allocation par appel RPC, et une méthode de plus ne coûte plus rien.
    ///
    /// Aiguillage par domaine, sur le préfixe de la méthode (`mem.search` → `mem`) : chaque
    /// domaine répond « méthode inconnue » pour ce qu'il ne sert pas.
    pub(super) async fn dispatch(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        match method.split_once('.').map_or(method, |(domain, _)| domain) {
            "chat" | "tail" => self.chat(method, p).await,
            "session" | "export" => self.sessions(method, p).await,
            "config" | "secret" => self.config(method, p).await,
            "model" if method == method::MODEL_AUTH => self.codex(method, p).await,
            "model" => self.models(method, p).await,
            "mcp" => self.mcp_admin(method, p).await,
            "mem" | "vault" | "intent" | "onboard" => self.memory(method, p).await,
            "wf" | "schedule" => self.workflows(method, p).await,
            "skill" => self.skills(method, p).await,
            "approvals" | "approve" | "deny" | "quiet" | "policies" | "policy" => {
                self.approvals(method, p).await
            }
            _ => self.ops(method, p).await,
        }
    }
}
