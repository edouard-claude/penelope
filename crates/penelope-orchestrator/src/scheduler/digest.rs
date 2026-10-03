//! Ce que le digest du matin lit au-dessus du rêve, calculé ici et transmis en données
//! (T26, puis T27) : planifications en échec, sessions dont le résumé échoue, départs du
//! jour mêlés aux rendez-vous de l'agenda (#295).

use super::*;
use penelope_dream::{DigestInputs, DigestSource};

/// Les entrées du digest, lues dans les services ; sans superviseur MCP, l'agenda n'est
/// pas lu, et le digest le dit si `digest.agenda` est posé.
pub async fn digest_inputs(s: &Services) -> DigestInputs {
    digest_inputs_with(s, None).await
}

/// Les entrées du digest, l'agenda lu par le superviseur MCP quand `digest.agenda` le
/// demande.
pub async fn digest_inputs_with(s: &Services, mcp: Option<Arc<dyn McpAdmin>>) -> DigestInputs {
    // Planifications dont la dernière exécution a échoué, ou n'a rien livré (#39, #120).
    let mut failing = Vec::new();
    for sched in s.schedules.list().await.unwrap_or_default() {
        if sched.state == "active"
            && let Some(err) = &sched.last_error
        {
            failing.push(format!("- {} : {err}", label(s, &sched).await));
        }
    }
    // Les départs du jour et les rendez-vous, rangés par heure, journées entières en tête.
    let mut today = due_today_dated(s).await;
    let agenda_error = match agenda::today(s, mcp).await {
        Ok(events) => {
            today.extend(events);
            None
        }
        Err(e) => Some(e),
    };
    today.sort_by_key(|(at, _)| *at);
    DigestInputs {
        failing_schedules: failing,
        struggling_sessions: penelope_conversation::compaction::struggling_sessions(s).await,
        // Sujets devenus projets au dernier démarrage, le lendemain (#301).
        notes: penelope_vault::session_project::subjects::digest_note(s)
            .await
            .into_iter()
            .collect(),
        due_today: today.into_iter().map(|(_, line)| line).collect(),
        agenda_error,
    }
}

/// Source des entrées du digest pour les crons système (`system_crons`), avec le
/// branchement MCP de la composition pour l'agenda.
pub struct DigestFeed {
    pub services: Arc<Services>,
    pub mcp: Slot<dyn McpAdmin>,
}

#[async_trait::async_trait]
impl DigestSource for DigestFeed {
    async fn digest_inputs(&self) -> DigestInputs {
        digest_inputs_with(&self.services, self.mcp.get()).await
    }
}
