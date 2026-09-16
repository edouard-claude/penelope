//! Supervision du daemon (§3.3, §17) : reprise, boucles de fond, socket, arrêt propre.
//!
//! ```text
//! penelope daemon
//!    │
//!    ├── reprise au démarrage (tours, effets, requêtes LLM, runs)
//!    ├── pool de runners ............ tours de conversation
//!    ├── passerelle Telegram ........ si propriétaire et jeton configurés
//!    ├── catalogue de modèles ....... au démarrage puis toutes les 6 h
//!    ├── maintenance ................ approbations échues, jetons de boutons expirés
//!    └── socket RPC ................. CLI, jusqu'au signal d'arrêt
//! ```

use crate::bus::Origin;
use crate::runtime::Daemon;
use std::sync::Arc;
use std::time::Duration;

impl Daemon {
    /// Fait tourner le daemon jusqu'à l'arrêt.
    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        // Un seul daemon par répertoire : la socket est prise avant toute autre chose.
        let socket = self.services.platform.dirs.socket_path();
        let listener = penelope_platform::ipc::IpcListener::bind(&socket)
            .await
            .map_err(|e| {
                let logs = self.services.platform.dirs.logs();
                anyhow::anyhow!(
                    "{e}\n→ un daemon tourne déjà, probablement le service installé : inutile \
                     d'en lancer un second. Utiliser directement `penelope chat`.\n\
                     → journaux du service : tail -f \"{}\"\n\
                     → pour déboguer au premier plan : `penelope stop`, puis `penelope daemon`",
                    logs.join("daemon.err.log").display()
                )
            })?;

        let report = self.recover().await?;
        tracing::info!(?report, "reprise terminée");

        let mut tasks = vec![
            tokio::spawn(crate::runner::run_pool(self.clone())),
            tokio::spawn(maintenance_loop(self.clone())),
            tokio::spawn(catalog_loop(self.clone())),
            tokio::spawn(crate::scheduler::scheduler_loop(self.clone())),
        ];

        // Serveurs MCP de `mcp.d/` : chargés en fond, pour ne pas retarder le démarrage.
        let mcp = crate::mcp::McpSupervisor::new(
            self.services.clone(),
            Arc::new(crate::mcp::ProcessConnector::new(self.services.clone())),
        );
        self.hooks.set_mcp(mcp.clone());
        tasks.extend(mcp.start());

        match crate::telegram::TelegramGateway::from_config(self.clone()).await {
            Ok(Some(gw)) => match gw.start().await {
                Ok(handles) => tasks.extend(handles),
                Err(e) => tracing::error!(error = %e, "Telegram non démarré"),
            },
            Ok(None) => tracing::info!(
                "Telegram non configuré (owner.telegram_user_id ou telegram_bot_token absent)"
            ),
            Err(e) => tracing::error!(error = %e, "Telegram non démarré"),
        }

        let serve = crate::rpc::serve_on(self.clone(), listener);
        tokio::select! {
            r = serve => {
                if let Err(e) = r {
                    tracing::error!(error = %e, "socket RPC arrêtée");
                }
            }
            _ = penelope_platform::process::shutdown_signal() => {
                tracing::info!("arrêt demandé");
            }
        }

        self.handle.shutdown();
        self.bus.notify_enqueued();
        // Les processus des serveurs MCP ne doivent pas survivre au daemon.
        mcp.stop_all().await;
        // Les tours en cours ont un peu de temps pour finir ; le reste sera repris au
        // prochain démarrage (leases, ledger).
        for t in &tasks {
            if !t.is_finished() {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                while !t.is_finished() && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        for t in tasks {
            t.abort();
        }
        Ok(())
    }
}

/// Dort par petites tranches pour réagir vite à l'arrêt.
async fn sleep_or_shutdown(d: &Daemon, total: Duration) {
    let step = Duration::from_millis(500);
    let mut waited = Duration::ZERO;
    while waited < total && !d.handle.is_shutting_down() {
        tokio::time::sleep(step).await;
        waited += step;
    }
}

/// Catalogue OpenRouter : au démarrage, puis à l'intervalle configuré.
async fn catalog_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        let cfg = d.services.config.config();
        let every =
            penelope_kernel::config::parse_duration(&cfg.providers.openrouter.catalog_refresh)
                .unwrap_or(Duration::from_secs(6 * 3600));
        let default = cfg
            .alias_model(&cfg.role_alias("chat_default"))
            .unwrap_or("openrouter:x")
            .to_string();
        match d.provider_for(&default).await {
            Ok(p) => match p.fetch_models().await {
                Ok(models) => tracing::info!(n = models.len(), "catalogue de modèles à jour"),
                Err(e) => tracing::warn!(error = %e, "catalogue de modèles indisponible"),
            },
            Err(e) => tracing::info!(error = %e, "catalogue en attente d'une clé"),
        }
        // Sans clé, on réessaie vite : elle peut arriver pendant que le daemon tourne.
        let wait = if d.services.catalog.is_empty() {
            Duration::from_secs(60)
        } else {
            every
        };
        sleep_or_shutdown(&d, wait).await;
    }
}

/// Maintenance périodique : approbations échues, jetons expirés.
async fn maintenance_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        if let Err(e) = maintenance_pass(&d).await {
            tracing::warn!(error = %e, "maintenance");
        }
        sleep_or_shutdown(&d, Duration::from_secs(60)).await;
    }
}

/// Un passage de maintenance. Une approbation échue relance son tour, qui dira au
/// modèle que la demande a expiré sans réponse (§9.2).
pub async fn maintenance_pass(d: &Daemon) -> anyhow::Result<()> {
    let s = &d.services;
    for a in s.approvals.expire_due().await? {
        let Some(sid) = &a.session_id else { continue };
        if a.payload.get("call_id").is_none() {
            continue;
        }
        let origin = match s.sessions.get(sid).await? {
            Some(sess) if sess.tg_chat_id.is_some() => Origin::Telegram {
                chat_id: sess.tg_chat_id.unwrap_or_default(),
                topic_id: sess.tg_topic_id,
                message_id: None,
            },
            _ => Origin::Cli,
        };
        d.enqueue_resume(sid, a.id.as_str(), &origin).await?;
    }
    s.actions.purge_expired().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    #[tokio::test]
    async fn an_expired_approval_resumes_its_turn() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        s.approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                serde_json::json!({"call_id": "c1"}),
                vec![],
                Some(&sid),
                None,
                false,
            )
            .await
            .unwrap();
        clock.advance_ms(25 * 3_600_000);
        maintenance_pass(&d).await.unwrap();
        let turn = s.turns.claim("t").await.unwrap().expect("reprise en file");
        assert_eq!(turn.kind, penelope_kernel::turn::TurnKind::Resume);
    }
}
