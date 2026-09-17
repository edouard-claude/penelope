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
        crate::budget_alert::AlertWatcher::install(&self);
        // Skills livrées et de l'utilisateur, disponibles dès le premier tour.
        if let Err(e) = crate::runtime::reload_skills(&self.services).await {
            tracing::warn!(error = %e, "chargement des skills");
        }
        // Vault d'une version antérieure : mis au format du wiki une fois (issue #29).
        if self.kv_get("wiki.migrated").await.ok().flatten().is_none() {
            let vault = crate::conversation::vault_dir(&self.services);
            match crate::vault_ops::migrate_wiki(&self.services, &vault).await {
                Ok(m) => {
                    tracing::info!(?m, "vault mis au format du wiki");
                    let _ = self.kv_set("wiki.migrated", "1").await;
                }
                Err(e) => tracing::warn!(error = %e, "migration du vault"),
            }
        }
        // Historique du vault : dépôt créé si l'autocommit est actif (issue #27).
        match crate::vault_git::ensure_repo(&self.services).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "initialisation git du vault"),
        }
        // Audit de démarrage : un réglage qui en annule un autre est nommé (issue #16).
        for c in penelope_kernel::coherence::contradictions(&self.services.config.config()) {
            tracing::warn!(reglages = ?c.keys, gravite = ?c.gravity, "{}", c.message);
            let _ = self
                .services
                .events
                .append(penelope_kernel::event::EventDraft::new(
                    "config.contradiction",
                    serde_json::json!(c),
                ))
                .await;
        }

        // Workflows, sous-agents et images : offerts aux outils et à l'ordonnanceur.
        self.hooks
            .set_orchestrator(Arc::new(crate::workflow::WorkflowOrchestrator {
                daemon: self.clone(),
            }));
        let mut tasks = vec![
            tokio::spawn(crate::runner::run_pool(self.clone())),
            tokio::spawn(maintenance_loop(self.clone())),
            tokio::spawn(catalog_loop(self.clone())),
            tokio::spawn(crate::scheduler::scheduler_loop(self.clone())),
            tokio::spawn(crate::workflow::driver_loop(self.clone())),
            tokio::spawn(crate::mcp_auth::callback_server(self.clone())),
            tokio::spawn(crate::upgrade::confirm_when_healthy(self.clone())),
        ];

        // Telegram construit d'abord (sans réseau) : un serveur MCP qui se connecte sait déjà
        // si un propriétaire peut répondre à ses demandes d'élicitation (issue #12).
        let telegram = crate::telegram::TelegramGateway::from_config(self.clone()).await;
        if let Ok(Some(_)) = &telegram {
            self.services.elicitations.expect_owner();
        }

        // Serveurs MCP de `mcp.d/` : chargés en fond, pour ne pas retarder le démarrage.
        let mcp = crate::mcp::McpSupervisor::new(
            self.services.clone(),
            Arc::new(crate::mcp::ProcessConnector::new(self.services.clone())),
        );
        self.hooks.set_mcp(mcp.clone());
        tasks.extend(mcp.start());

        match telegram {
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
        // Outils MCP inscrits, vault réindexé : vecteurs manquants.
        crate::embeddings::spawn_backfill(d.clone());
        // Contenu du vault hors index : signalé à chaque changement, toutes les 30 min.
        let now = d.services.clock.now_ms();
        let last = d
            .kv_get("vault.gaps.checked")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if now - last >= 30 * 60_000 {
            let _ = d.kv_set("vault.gaps.checked", &now.to_string()).await;
            if let Err(e) = crate::vault_inventory::report_gaps(&d).await {
                tracing::warn!(error = %e, "inventaire du vault");
            }
        }
        crate::vault_git::autocommit_tick(&d).await;
        sleep_or_shutdown(&d, Duration::from_secs(60)).await;
    }
}

/// Empreinte des dossiers de skills : chemins, tailles et dates de modification. Un
/// changement suffit à déclencher une relecture (issue #63).
fn skills_fingerprint(s: &crate::runtime::Services) -> String {
    let mut parts: Vec<String> = Vec::new();
    for root in [s.platform.dirs.skills(), s.platform.dirs.bundled_skills()] {
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in read.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let meta = e.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                parts.push(format!("{}|{size}|{mtime}", path.display()));
            }
        }
    }
    parts.sort();
    penelope_kernel::canonical::sha256_hex(parts.join("\n").as_bytes())
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

    // Skills déposées en SSH : relues quand le dossier change, sans redémarrage (#63).
    let fingerprint = skills_fingerprint(s);
    if d.kv_get("skills.fingerprint").await?.as_deref() != Some(fingerprint.as_str()) {
        match crate::runtime::reload_skills(s).await {
            Ok(n) => {
                tracing::info!(skills = n, "skills relues après changement du dossier");
                d.kv_set("skills.fingerprint", &fingerprint).await?;
            }
            Err(e) => tracing::warn!(error = %e, "rechargement des skills"),
        }
    }

    // Rétention des traces : une passe par jour (issue #46).
    if let Err(e) = crate::purge::retention_tick(d).await {
        tracing::warn!(error = %e, "rétention");
    }

    // Serveurs MCP qui attendent une autorisation : le propriétaire reçoit le lien, une
    // fois par jour au plus (§8.5).
    if let Some(sup) = d.hooks.mcp_supervisor() {
        for st in sup.statuses().await {
            if st.state != penelope_mcp::ServerState::AuthRequired {
                continue;
            }
            let key = format!("mcp.oauth.notified.{}", st.name);
            let now = s.clock.now_ms();
            let recent = d
                .kv_get(&key)
                .await?
                .and_then(|v| v.parse::<i64>().ok())
                .is_some_and(|t| now - t < 24 * 3_600_000);
            if recent {
                continue;
            }
            d.kv_set(&key, &now.to_string()).await?;
            let Some(cfg) = sup.config_of(&st.name).await else {
                continue;
            };
            match crate::mcp_auth::start(d, &cfg, None).await {
                Ok(start) => {
                    if let Some(m) = d.hooks.messenger() {
                        let origin = crate::scheduler::owner_origin(d);
                        let _ = m
                            .send_text(&origin, &crate::mcp_auth::prompt_text(&start))
                            .await;
                    }
                }
                Err(e) => tracing::warn!(server = %st.name, error = %e, "autorisation MCP"),
            }
        }
    }

    // Workspaces éphémères des runs terminés depuis longtemps (§12.7). Un workspace
    // persistant n'est jamais effacé, ni celui qu'un sous-run partage avec son parent :
    // seul le propriétaire du répertoire (`runs/<son id>`) le supprime.
    let retention = s.config.config().workflows.workspace_retention_days.max(1);
    let ephemeral_root = s.platform.dirs.state().join("runs");
    for (run_id, workdir) in s.runs.expired_workspaces(retention).await? {
        let path = std::path::PathBuf::from(&workdir);
        if path.starts_with(&ephemeral_root)
            && path.file_name().is_some_and(|n| n == run_id.as_str())
            && path.exists()
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            tracing::warn!(run = %run_id, error = %e, "workspace de run non nettoyé");
            continue;
        }
        s.runs.forget_workdir(&run_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    /// #63 : une skill déposée après le démarrage est visible après une passe
    /// d'entretien, sans redémarrage ; un fichier invalide n'efface pas les autres.
    #[tokio::test]
    async fn a_skill_dropped_by_scp_is_picked_up_by_the_maintenance_pass() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        crate::runtime::reload_skills(&s).await.unwrap();
        let before = s.skills.all().len();
        assert!(s.skills.get("revue-express").is_none());

        // Dépôt en SSH, daemon en marche.
        let root = s.platform.dirs.skills().join("revue-express");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            "---\nname: revue-express\ndescription: Relire un diff en cinq points\n---\n\n\
             # Revue express\n\nLis le diff, donne cinq points.\n",
        )
        .unwrap();

        maintenance_pass(&d).await.unwrap();
        assert!(
            s.skills.get("revue-express").is_some(),
            "la skill doit être visible sans redémarrage"
        );

        // Fichier invalide : les autres restent.
        let bad = s.platform.dirs.skills().join("cassee");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("SKILL.md"), "pas de frontmatter du tout\n").unwrap();
        maintenance_pass(&d).await.unwrap();
        assert!(s.skills.get("revue-express").is_some(), "toujours là");
        assert!(s.skills.all().len() > before);
    }

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
