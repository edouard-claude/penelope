//! La liste d'outils gelée d'une session (issue #236).
//!
//! Chez Anthropic, les définitions d'outils précèdent le prompt système : une entrée ou
//! une sortie de la liste casse tout le cache de la requête. La liste d'un tour de
//! conversation est donc calculée à une frontière (premier tour, pause plus longue que
//! le cache, compaction), gardée ici, et resservie telle quelle jusqu'à la suivante. La
//! compaction la relit pour que son appel de résumé partage le préfixe de la conversation.

use crate::services::Services;
use penelope_llm::ToolDef;

fn key(session_id: &str) -> String {
    format!("session.tools.frozen.{session_id}")
}

/// La liste gelée de la session, si elle en a une.
pub async fn frozen(s: &Services, session_id: &str) -> Option<Vec<ToolDef>> {
    s.kv_get(&key(session_id))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Gèle la liste d'une session jusqu'à la prochaine frontière. Un échec ne coûte que le
/// gel : le tour suivant recalcule.
pub async fn freeze(s: &Services, session_id: &str, tools: &[ToolDef]) {
    let raw = serde_json::to_string(tools).unwrap_or_default();
    if let Err(e) = s.kv_set(&key(session_id), &raw).await {
        tracing::warn!(session = session_id, error = %e, "liste d'outils non gelée");
    }
}
