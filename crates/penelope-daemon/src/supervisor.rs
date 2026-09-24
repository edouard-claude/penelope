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
use penelope_workflow::RunState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Seuil d'alerte, pas de suppression : un build peut remplir un run en pause (#177).
const WORKSPACE_WARNING_BYTES: u64 = 1_073_741_824;

/// Taille des fichiers réguliers du workspace ; les liens ne sont jamais suivis.
fn workspace_size_bytes(root: &Path) -> std::io::Result<u64> {
    let meta = std::fs::symlink_metadata(root)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace non répertoire ou lien symbolique",
        ));
    }
    let mut bytes = 0_u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    Ok(bytes)
}

#[derive(Debug, PartialEq, Eq)]
struct LargeWorkspace {
    run_id: String,
    state: RunState,
    path: PathBuf,
    bytes: u64,
}

fn large_workspaces(
    root: &Path,
    owners: Vec<(String, RunState, String)>,
    threshold: u64,
) -> Vec<LargeWorkspace> {
    owners
        .into_iter()
        .filter_map(|(run_id, state, workdir)| {
            let path = PathBuf::from(workdir);
            if path.parent() != Some(root) || path.file_name().is_none_or(|n| n != run_id.as_str()) {
                tracing::warn!(run = %run_id, path = %path.display(), "workspace hors state/runs : taille ignorée");
                return None;
            }
            match workspace_size_bytes(&path) {
                Ok(bytes) if bytes >= threshold => Some(LargeWorkspace {
                    run_id,
                    state,
                    path,
                    bytes,
                }),
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(run = %run_id, error = %e, "taille du workspace indisponible");
                    None
                }
            }
        })
        .collect()
}

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
        // Un index de recherche abîmé a été reconstruit à l'ouverture au lieu d'empêcher
        // le démarrage (issue #158) : le dire, sans quoi la réparation est invisible.
        for table in self.services.store.repaired_fts() {
            tracing::warn!(table, "index de recherche reconstruit à l'ouverture");
            let _ = self
                .services
                .events
                .append(penelope_kernel::event::EventDraft::new(
                    "store.fts_rebuilt",
                    serde_json::json!({"table": table, "reason": "quick_check à l'ouverture"}),
                ))
                .await;
        }
        // Profils Seatbelt laissés par les versions qui les écrivaient dans le dossier
        // temporaire (issue #90) : ils passent désormais en argument.
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("penelope-sandbox"));
        crate::budget_alert::AlertWatcher::install(&self);
        // Skills livrées et de l'utilisateur, disponibles dès le premier tour.
        if let Err(e) = crate::runtime::reload_skills(&self.services).await {
            tracing::warn!(error = %e, "chargement des skills");
        }
        // Vault d'une version antérieure : mis au format du wiki une fois (issue #29).
        if self
            .services
            .kv_get("wiki.migrated")
            .await
            .ok()
            .flatten()
            .is_none()
        {
            let vault = crate::helpers::vault_dir(&self.services);
            match crate::vault_ops::migrate_wiki(&self.services, &vault).await {
                Ok(m) => {
                    tracing::info!(?m, "vault mis au format du wiki");
                    let _ = self.services.kv_set("wiki.migrated", "1").await;
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
        // Chaque boucle est surveillée : une panique est journalisée, comptée et suivie
        // d'une relance, au lieu d'arrêter la boucle jusqu'au prochain démarrage (#84).
        let supervised = |name: &str, f: fn(Arc<Daemon>) -> _| {
            let d = self.clone();
            crate::tasks::spawn_supervised(self.clone(), name, move || f(d.clone()))
        };
        let mut tasks = vec![
            tokio::spawn(crate::runner::run_pool(self.clone())),
            supervised("maintenance", |d| Box::pin(maintenance_loop(d)) as BoxLoop),
            supervised("catalog", |d| Box::pin(catalog_loop(d)) as BoxLoop),
            supervised("machine", |d| Box::pin(machine_loop(d)) as BoxLoop),
            supervised("scheduler", |d| {
                Box::pin(crate::scheduler::scheduler_loop(d)) as BoxLoop
            }),
            supervised("workflows", |d| {
                Box::pin(crate::workflow::driver_loop(d)) as BoxLoop
            }),
            // Résultats des jobs d'outils rendus à leur session (issue #204).
            supervised("tool_jobs", |d| {
                Box::pin(crate::tool_jobs::deliver_loop(d)) as BoxLoop
            }),
            supervised("codex.refresh", |d| {
                Box::pin(crate::codex_auth::refresh_loop(d)) as BoxLoop
            }),
            supervised("mcp.oauth_callback", |d| {
                Box::pin(async move {
                    crate::mcp_auth::callback_server(d).await;
                }) as BoxLoop
            }),
            tokio::spawn(crate::upgrade::confirm_when_healthy(self.clone())),
        ];
        if !self
            .services
            .config
            .config()
            .observability
            .runtime_consumers
            .is_empty()
        {
            tasks.push(supervised("runtime.stream", |d| {
                Box::pin(async move {
                    if let Err(error) = crate::runtime_events::serve(d).await {
                        tracing::error!(%error, "flux runtime indisponible");
                    }
                }) as BoxLoop
            }));
        }

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
        tasks.push(mcp.boot());
        {
            let mcp = mcp.clone();
            tasks.push(crate::tasks::spawn_supervised(
                self.clone(),
                "mcp.maintenance",
                move || mcp.clone().maintenance_loop(),
            ));
        }

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
        // Les jobs de ce processus meurent avec lui : leurs jetons tuent les groupes de
        // processus plutôt que de laisser des orphelins (issues #204 et #65).
        let cut = self.services.jobs.cancel_all();
        if cut > 0 {
            tracing::info!(jobs = cut, "jobs d'outils interrompus à l'arrêt");
        }
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

/// Boucle de fond, typée pour la table des boucles surveillées.
type BoxLoop = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

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
        // Le fournisseur de `chat_default`, plus Codex dès qu'un alias le vise : son
        // catalogue dit la fenêtre réelle et les efforts acceptés du plan (#142). Chacun
        // fait un `upsert` : le catalogue OpenRouter n'est jamais écrasé.
        let mut wanted = vec![default];
        if cfg.providers.codex.enabled
            && let Some(codex_alias) = cfg
                .models
                .aliases
                .values()
                .find(|m| penelope_llm::catalog::provider_of(m) == "codex")
            && penelope_llm::catalog::provider_of(&wanted[0]) != "codex"
        {
            wanted.push(codex_alias.clone());
        }
        for model in &wanted {
            match d.provider_for(model).await {
                Ok(p) => match p.fetch_models().await {
                    Ok(models) => tracing::info!(n = models.len(), "catalogue de modèles à jour"),
                    Err(e) => tracing::warn!(error = %e, "catalogue de modèles indisponible"),
                },
                Err(e) => tracing::info!(error = %e, "catalogue en attente d'une clé"),
            }
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

/// Inventaire de la machine (issue #156) : au démarrage, puis toutes les heures.
///
/// Toutes les heures et pas plus souvent : l'inventaire ne bouge qu'à un `brew install`
/// ou un `gh auth login`, et chaque passe lance une poignée de processus. Une passe qui
/// échoue laisse le dernier inventaire en place — une ligne système périmée vaut mieux
/// qu'un modèle qui ne sait plus rien de sa machine.
async fn machine_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        match crate::machine::refresh(&d.services).await {
            Ok(inv) => tracing::info!(
                present = inv.present.len(),
                missing = inv.missing.len(),
                "inventaire de la machine"
            ),
            Err(e) => tracing::warn!(error = %e, "inventaire de la machine"),
        }
        sleep_or_shutdown(&d, Duration::from_secs(3600)).await;
    }
}

/// Maintenance périodique : approbations échues, jetons expirés.
async fn maintenance_loop(d: Arc<Daemon>) {
    while !d.handle.is_shutting_down() {
        let pass = {
            use tracing::Instrument;
            maintenance_pass(&d).instrument(tracing::info_span!("maintenance"))
        };
        if let Err(e) = pass.await {
            tracing::warn!(error = %e, "maintenance");
        }
        // Outils MCP inscrits, vault réindexé : vecteurs manquants.
        crate::embeddings::spawn_backfill(d.clone());
        // Contenu du vault hors index : signalé à chaque changement, toutes les 30 min.
        let now = d.services.clock.now_ms();
        let last = d
            .services
            .kv_get("vault.gaps.checked")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0);
        if now - last >= 30 * 60_000 {
            let _ = d
                .services
                .kv_set("vault.gaps.checked", &now.to_string())
                .await;
            if let Err(e) = crate::vault_inventory::report_gaps(&d).await {
                tracing::warn!(error = %e, "inventaire du vault");
            }
        }
        crate::vault_git::autocommit_tick(&d).await;
        sleep_or_shutdown(&d, Duration::from_secs(60)).await;
    }
}

/// Empreinte des dossiers de skills : chemins et contenus (issue #63). Le contenu, pas la
/// date de modification : une réécriture à l'identique ne relance rien (#118). Au-delà
/// d'un Mio, un fichier (image, archive) compte par sa taille et sa date.
pub(crate) fn skills_fingerprint(s: &crate::runtime::Services) -> String {
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
                let content = if size <= 1024 * 1024 {
                    std::fs::read(&path)
                        .map(|b| penelope_kernel::canonical::sha256_hex(&b))
                        .unwrap_or_default()
                } else {
                    meta.and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis().to_string())
                        .unwrap_or_default()
                };
                parts.push(format!("{}|{size}|{content}", path.display()));
            }
        }
    }
    parts.sort();
    penelope_kernel::canonical::sha256_hex(parts.join("\n").as_bytes())
}

/// Relit les skills si leur contenu a changé. L'empreinte gardée est celle d'après le
/// rechargement : ce qu'il réécrit ne compte pas comme un changement (#118). Vrai si les
/// skills ont été relues.
pub(crate) async fn skills_tick(d: &Daemon) -> anyhow::Result<bool> {
    let s = &d.services;
    if s.kv_get("skills.fingerprint").await?.as_deref() == Some(skills_fingerprint(s).as_str()) {
        return Ok(false);
    }
    match crate::runtime::reload_skills(s).await {
        Ok(n) => {
            tracing::info!(skills = n, "skills relues après changement du dossier");
            s.kv_set("skills.fingerprint", &skills_fingerprint(s))
                .await?;
            Ok(true)
        }
        Err(e) => {
            tracing::warn!(error = %e, "rechargement des skills");
            Ok(false)
        }
    }
}

/// Conversation d'où vient une demande : le chat de sa session, sinon le propriétaire.
async fn approval_origin(d: &Daemon, a: &penelope_hitl::ApprovalRequest) -> Origin {
    if let Some(sid) = &a.session_id
        && let Ok(Some(sess)) = d.services.sessions.get(sid).await
        && let Some(chat_id) = sess.tg_chat_id
    {
        return Origin::Telegram {
            chat_id,
            topic_id: sess.tg_topic_id,
            message_id: None,
        };
    }
    crate::helpers::owner_origin_of(&d.services)
}

/// Un passage de maintenance. Une approbation échue relance son tour, qui dira au
/// modèle que la demande a expiré sans réponse (§9.2).
pub async fn maintenance_pass(d: &Daemon) -> anyhow::Result<()> {
    let s = &d.services;
    for a in s.approvals.expire_due().await? {
        // Une question de contradiction sans réponse est rangée, pas reposée (issue #145).
        crate::dream::file_unanswered_clash(s, &a).await;
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

    // Une demande restée sans réponse est rappelée à T+1 h puis T+6 h, avec une carte
    // neuve, dans la conversation d'origine (§9.2, issue #97). Sans canal de message
    // (CLI seule), rien n'est marqué : le propriétaire y est actif.
    if let Some(m) = d.hooks.messenger() {
        for (a, stage) in s.approvals.due_reminders().await? {
            let origin = approval_origin(d, &a).await;
            let since = if stage == 1 { "1 h" } else { "6 h" };
            let _ = m
                .send_text(
                    &origin,
                    &format!(
                        "⏰ Rappel {stage}/2 : une demande attend ta réponse depuis {since} \
                         (`{}`). Sans réponse, elle expire au bout de 24 h.",
                        a.subject
                    ),
                )
                .await;
            if let Err(e) = m.send_approval(&origin, a.id.as_str()).await {
                tracing::warn!(demande = %a.id.as_str(), error = %e, "rappel d'approbation");
            }
        }
    }

    // Skills déposées en SSH : relues quand le dossier change, sans redémarrage (#63).
    skills_tick(d).await?;

    // Sauvegarde complète à l'heure dite (issue #42).
    if let Err(e) = crate::backup::nightly_tick(d).await {
        tracing::warn!(error = %e, "sauvegarde nocturne");
    }

    // Messages déjà en file, rédigés avec les règles du jour : une seule fois (#148).
    if let Err(e) = crate::purge::reredact_outbox(d).await {
        tracing::warn!(error = %e, "relecture du rédacteur sur la file Telegram");
    }

    // Rétention des traces : une passe par jour (issue #46).
    if let Err(e) = crate::purge::retention_tick(d).await {
        tracing::warn!(error = %e, "rétention");
    }

    // Outils MCP changés depuis leur « Toujours » : règles révoquées, le propriétaire le
    // sait (#92).
    if let Some(sup) = d.hooks.mcp_supervisor() {
        let notices = sup.take_notices();
        if let Some(m) = d.hooks.messenger() {
            let origin = crate::helpers::owner_origin_of(&d.services);
            for n in notices {
                let _ = m.send_text(&origin, &n).await;
            }
        }
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
            let recent = s
                .kv_get(&key)
                .await?
                .and_then(|v| v.parse::<i64>().ok())
                .is_some_and(|t| now - t < 24 * 3_600_000);
            if recent {
                continue;
            }
            s.kv_set(&key, &now.to_string()).await?;
            let Some(cfg) = sup.config_of(&st.name).await else {
                continue;
            };
            match crate::mcp_auth::start(d, &cfg, None).await {
                Ok(start) => {
                    if let Some(m) = d.hooks.messenger() {
                        let origin = crate::helpers::owner_origin_of(&d.services);
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

    // Une passe quotidienne suffit : compter les fichiers d'un build Rust peut
    // demander plusieurs secondes. Aucun run vivant n'est supprimé (issue #177).
    let now = s.clock.now_ms();
    let last = s
        .kv_get("workflow.workspace_size.checked")
        .await?
        .and_then(|v| v.parse::<i64>().ok());
    if last.is_none_or(|last| now.saturating_sub(last) >= 24 * 3_600_000) {
        let owners = s.runs.active_workspaces().await?;
        let root = ephemeral_root.clone();
        let large = tokio::task::spawn_blocking(move || {
            large_workspaces(&root, owners, WORKSPACE_WARNING_BYTES)
        })
        .await?;
        for item in large {
            tracing::warn!(
                run = %item.run_id,
                state = item.state.as_str(),
                path = %item.path.display(),
                bytes = item.bytes,
                threshold_bytes = WORKSPACE_WARNING_BYTES,
                "workspace de run volumineux"
            );
            s.events
                .append(penelope_kernel::event::EventDraft::new(
                    "workflow.workspace_large",
                    serde_json::json!({
                        "run_id": item.run_id,
                        "state": item.state.as_str(),
                        "path": item.path,
                        "bytes": item.bytes,
                        "threshold_bytes": WORKSPACE_WARNING_BYTES,
                    }),
                ))
                .await?;
        }
        s.kv_set("workflow.workspace_size.checked", &now.to_string())
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    /// #177 : les liens symboliques ne gonflent pas le relevé et ne font pas
    /// sortir le parcours du workspace d'un run.
    #[cfg(unix)]
    #[test]
    fn workspace_measurement_stays_inside_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("run");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("first"), b"abc").unwrap();
        std::fs::write(root.join("nested/second"), b"12345").unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, vec![b'x'; 100]).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert_eq!(workspace_size_bytes(&root).unwrap(), 8);
        assert!(workspace_size_bytes(&root.join("link")).is_err());
    }

    /// #177 : le diagnostic nomme le run en pause et ne touche pas ses fichiers.
    #[test]
    fn paused_large_workspace_is_reported_without_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("runs");
        let path = root.join("r_test");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("build.bin"), b"12345").unwrap();
        let owners = vec![(
            "r_test".into(),
            RunState::Paused,
            path.to_string_lossy().into_owned(),
        )];
        let reported = large_workspaces(&root, owners.clone(), 5);
        assert_eq!(reported.len(), 1);
        assert_eq!(reported[0].state, RunState::Paused);
        assert_eq!(reported[0].path, path);
        assert_eq!(reported[0].bytes, 5);
        assert!(path.join("build.bin").exists());
        assert!(large_workspaces(&root, owners, 6).is_empty());
    }

    #[derive(Default)]
    struct Recorder {
        texts: std::sync::Mutex<Vec<String>>,
        cards: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::executor::Messenger for Recorder {
        async fn send_text(&self, _origin: &Origin, markdown: &str) -> Result<(), String> {
            self.texts.lock().unwrap().push(markdown.to_string());
            Ok(())
        }
        async fn send_file(
            &self,
            _origin: &Origin,
            _path: &std::path::Path,
            _caption: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }
        async fn send_approval(&self, _origin: &Origin, approval_id: &str) -> Result<(), String> {
            self.cards.lock().unwrap().push(approval_id.to_string());
            Ok(())
        }
    }

    /// #97 : une demande sans réponse reçoit un rappel à T+1 h et un à T+6 h, pas plus ;
    /// une demande tranchée n'en reçoit aucun.
    #[tokio::test]
    async fn pending_approvals_are_reminded_at_one_and_six_hours() {
        let dir = tempfile::tempdir().unwrap();
        let clock = TestClock::default();
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), Arc::new(clock.clone()))
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let rec = Arc::new(Recorder::default());
        *d.hooks.messenger.write().unwrap() =
            Some(rec.clone() as Arc<dyn crate::executor::Messenger>);
        let ask = |subject: &'static str| {
            let s = s.clone();
            async move {
                s.approvals
                    .create(
                        penelope_hitl::ApprovalKind::ToolCall,
                        subject,
                        penelope_kernel::risk::RiskClass::Write,
                        serde_json::json!({"arguments": {}}),
                        vec![],
                        None,
                        None,
                        false,
                    )
                    .await
                    .unwrap()
            }
        };
        let oubliee = ask("shell_exec").await;
        let tranchee = ask("fs_write").await;
        let cards = |r: &Recorder| r.cards.lock().unwrap().clone();

        maintenance_pass(&d).await.unwrap();
        assert!(cards(&rec).is_empty(), "rien avant une heure");

        clock.advance_ms(3_600_000 + 1);
        s.approvals
            .decide(
                tranchee.id.as_str(),
                &penelope_hitl::Decision::approve_once("cli"),
            )
            .await
            .unwrap();
        maintenance_pass(&d).await.unwrap();
        assert_eq!(cards(&rec), vec![oubliee.id.0.clone()], "premier rappel");
        assert!(rec.texts.lock().unwrap()[0].contains("Rappel 1/2"));

        clock.advance_ms(30 * 60_000);
        maintenance_pass(&d).await.unwrap();
        assert_eq!(cards(&rec).len(), 1, "rien à T+1 h 30");

        clock.advance_ms(5 * 3_600_000);
        maintenance_pass(&d).await.unwrap();
        assert_eq!(cards(&rec).len(), 2, "second rappel à T+6 h");

        clock.advance_ms(3_600_000);
        maintenance_pass(&d).await.unwrap();
        assert_eq!(cards(&rec).len(), 2, "rien à T+7 h");
        let reminded: Option<String> = s
            .store
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT reminded_at FROM approval_requests WHERE id = ?1",
                    [oubliee.id.0.clone()],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        assert!(reminded.is_some());
    }

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

    /// #118 : deux passes sans changement ne relisent les skills qu'une fois ; réécrire
    /// une skill livrée à l'identique ne change pas l'empreinte ; modifier le contenu d'une
    /// skill déposée relance exactement un rechargement ; un rechargement sans changement de
    /// contenu laisse le préfixe du prompt intact.
    #[tokio::test]
    async fn skills_reload_only_when_their_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let root = s.platform.dirs.skills().join("revue-express");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            "---\nname: revue-express\ndescription: Relire un diff\n---\n\nCinq points.\n",
        )
        .unwrap();

        assert!(
            skills_tick(&d).await.unwrap(),
            "premier passage : rechargement"
        );
        let prefix = crate::conversation::build_tiers(&s, "bonjour", &[], None)
            .await
            .prefix_hash();
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(!skills_tick(&d).await.unwrap(), "rien n'a changé");
        assert!(!skills_tick(&d).await.unwrap(), "toujours rien");

        let before = skills_fingerprint(&s);
        let bundled = s
            .platform
            .dirs
            .bundled_skills()
            .join("wiki-markdown")
            .join("SKILL.md");
        let modified = std::fs::metadata(&bundled).unwrap().modified().unwrap();
        crate::runtime::reload_skills(&s).await.unwrap();
        assert_eq!(
            std::fs::metadata(&bundled).unwrap().modified().unwrap(),
            modified,
            "une skill livrée identique n'est pas réécrite"
        );
        std::fs::write(&bundled, std::fs::read(&bundled).unwrap()).unwrap();
        assert_eq!(
            skills_fingerprint(&s),
            before,
            "même contenu, même empreinte"
        );
        assert!(!skills_tick(&d).await.unwrap());
        assert_eq!(
            crate::conversation::build_tiers(&s, "bonjour", &[], None)
                .await
                .prefix_hash(),
            prefix,
            "préfixe intact"
        );

        std::fs::write(
            root.join("SKILL.md"),
            "---\nname: revue-express\ndescription: Relire un diff en sept points\n---\n\nSept.\n",
        )
        .unwrap();
        assert!(
            skills_tick(&d).await.unwrap(),
            "contenu modifié : rechargement"
        );
        assert!(!skills_tick(&d).await.unwrap(), "un seul");
        assert!(
            s.skills
                .get("revue-express")
                .is_some_and(|k| k.description.contains("sept")),
        );
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
