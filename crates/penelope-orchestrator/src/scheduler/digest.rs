//! Ce que le digest du matin lit au-dessus du rêve, calculé ici et transmis en données
//! (T26, puis T27) : planifications en échec, sessions dont le résumé échoue, départs du
//! jour.

use super::*;
use penelope_dream::{DigestInputs, DigestSource};

/// Les entrées du digest, lues dans les services.
pub async fn digest_inputs(s: &Services) -> DigestInputs {
    // Planifications dont la dernière exécution a échoué, ou n'a rien livré (#39, #120).
    let mut failing = Vec::new();
    for sched in s.schedules.list().await.unwrap_or_default() {
        if sched.state != "active" {
            continue;
        }
        if let Some(err) = &sched.last_error {
            failing.push(format!("- {} : {err}", label(s, &sched).await));
        }
        // Un abonnement MCP perdu depuis plus d'une heure (#293) : le déclencheur est
        // éteint sans qu'aucune exécution ait échoué, le digest le dit.
        if sched.kind == TriggerKind::McpSubscribe
            && let Some((since, err)) = triggers::mcp_subscribe::lost_for(s, &sched).await
        {
            let hours = (s.clock.now_ms() - since) / 3_600_000;
            failing.push(format!(
                "- {} : abonnement MCP perdu depuis {hours} h, le déclencheur ne réagit plus{}",
                label(s, &sched).await,
                if err.is_empty() {
                    String::new()
                } else {
                    format!(" ({err})")
                }
            ));
        }
    }
    DigestInputs {
        failing_schedules: failing,
        struggling_sessions: penelope_conversation::compaction::struggling_sessions(s).await,
        due_today: due_today(s).await,
    }
}

/// Source des entrées du digest pour les crons système (`system_crons`).
pub struct DigestFeed(pub Arc<Services>);

#[async_trait::async_trait]
impl DigestSource for DigestFeed {
    async fn digest_inputs(&self) -> DigestInputs {
        digest_inputs(&self.0).await
    }
}
