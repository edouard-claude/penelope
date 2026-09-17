//! Transcript persistant d'une session et assemblage du prompt (§5.2, §6.3).
//!
//! L'historique canonique vit en base (`messages`) ; la requête envoyée au modèle en est
//! une **projection** : tuiles T0 à T2 stables, résumés LCM, queue verbatim, T4 volatile.

use crate::agent::Conversation;
use crate::runtime::Services;
use penelope_context::CompactionParams;
use penelope_context::tiers::{Tiers, TiersBuilder, volatile_header};
use penelope_context::transcript::Entry;
use penelope_llm::types::{ChatMessage, Role};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Nombre d'entrées relues pour retrouver les appels d'outils en attente.
const TAIL_ENTRIES: usize = 64;

/// Un transcript de session, adossé à l'historique canonique.
pub struct SessionConversation {
    services: Arc<Services>,
    session_id: String,
    model_id: String,
    tiers: Tiers,
    episode: i64,
    /// Compaction immédiate, sur dépassement de fenêtre prouvé par le provider.
    compactor: Option<Arc<dyn crate::agent::Compactor>>,
    /// La dernière projection a atteint le seuil de la compaction de fond.
    wants_compaction: AtomicBool,
}

impl SessionConversation {
    pub fn new(
        services: Arc<Services>,
        session_id: &str,
        model_id: &str,
        tiers: Tiers,
        episode: i64,
    ) -> Self {
        SessionConversation {
            services,
            session_id: session_id.to_string(),
            model_id: model_id.to_string(),
            tiers,
            episode,
            compactor: None,
            wants_compaction: AtomicBool::new(false),
        }
    }

    pub fn with_compactor(mut self, compactor: Arc<dyn crate::agent::Compactor>) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// Vrai si une projection de ce tour a atteint le seuil moins la marge (§5.4).
    pub fn wants_compaction(&self) -> bool {
        self.wants_compaction.load(Ordering::SeqCst)
    }

    fn bare_model(&self) -> &str {
        penelope_llm::catalog::strip_provider(&self.model_id)
    }

    fn params(&self) -> CompactionParams {
        let cfg = self.services.config.config();
        let window = self.services.catalog.window_of(self.bare_model());
        CompactionParams::from_config(&cfg, window, &self.model_id)
    }

    /// Le cache de préfixe explicite (`cache_control`) ne vaut que pour Anthropic.
    fn anthropic_cache(&self) -> bool {
        self.bare_model().starts_with("anthropic/")
    }

    /// Entrées à projeter : résumés LCM actifs, puis tout ce qu'ils ne couvrent pas.
    async fn projected_entries(&self) -> anyhow::Result<Vec<Entry>> {
        let s = &self.services;
        let mut entries = s.context.history.load(&self.session_id, 0).await?;
        // Contexte volatil figé avec chaque message utilisateur (issue #17).
        let contexts = s.context.history.contexts(&self.session_id).await?;
        for e in entries.iter_mut() {
            if let Some(block) = contexts.get(&e.seq)
                && e.message.role == Role::User
            {
                let m = &mut e.message;
                match m.content.iter_mut().find_map(|c| match c {
                    penelope_llm::types::Content::Text { text } => Some(text),
                    _ => None,
                }) {
                    Some(text) => text.insert_str(0, block),
                    None => m
                        .content
                        .insert(0, penelope_llm::types::Content::text(block.clone())),
                }
            }
        }
        let nodes = s.context.lcm.active_nodes(&self.session_id).await?;
        if nodes.is_empty() {
            return Ok(entries);
        }
        let mut out: Vec<Entry> = nodes
            .iter()
            .map(|n| {
                Entry::new(
                    0,
                    ChatMessage::system(format!(
                        "Résumé de la conversation antérieure (nœud {}) :\n{}",
                        n.id, n.summary
                    )),
                    n.tokens_self,
                )
            })
            .collect();
        out.extend(entries.into_iter().filter(|e| !e.compacted));
        Ok(out)
    }
}

#[async_trait::async_trait]
impl Conversation for SessionConversation {
    async fn request_messages(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let s = &self.services;
        let params = self.params();
        let entries = self.projected_entries().await?;
        let ctx = s.context.build_from_entries(
            &entries,
            &self.tiers,
            &params,
            &self.model_id,
            self.anthropic_cache(),
        );
        if ctx.needs_background_compaction || !ctx.fits {
            self.wants_compaction.store(true, Ordering::SeqCst);
        }
        if ctx.fits {
            return Ok(ctx.messages);
        }
        // Niveau 4 : la requête ne tient pas, on la réduit en le prouvant (§5.4).
        let limit = params
            .window
            .saturating_sub((params.window / 10).clamp(1_000, 32_000));
        let (messages, _) = s
            .context
            .emergency(ctx.messages, limit, &self.model_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(messages)
    }

    async fn record(&self, message: &ChatMessage, eager: bool) -> anyhow::Result<()> {
        let s = &self.services;
        let tokens = s.context.estimator.message_tokens(&self.model_id, message);
        let seq = s
            .context
            .history
            .append(&self.session_id, message, tokens, self.episode, eager, None)
            .await?;
        s.sessions.touch(&self.session_id).await?;

        // Niveau 1 : un résultat d'outil trop gros part en artefact, le transcript garde
        // la tête, la queue et le moyen de relire le reste (§5.4).
        if message.role == Role::Tool {
            let params = self.params();
            if tokens > params.large_payload_tokens {
                s.context
                    .admit_tool_group(
                        &self.session_id,
                        &[(seq, message.text())],
                        &params,
                        &self.model_id,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Le groupe d'appels parallèles est admis d'un bloc : le budget se répartit entre
    /// les résultats, les petits restent entiers, les gros partent en artefact (#52).
    async fn admit_tool_results(&self, count: usize) -> anyhow::Result<()> {
        if count < 2 {
            return Ok(());
        }
        let s = &self.services;
        let results = s
            .context
            .history
            .recent_tool_results(&self.session_id, count)
            .await?;
        if results.len() < 2 {
            return Ok(());
        }
        let params = self.params();
        let total: u64 = results
            .iter()
            .map(|(_, text)| s.context.estimator.text_tokens(&self.model_id, text))
            .sum();
        if total <= params.tool_group_budget() {
            return Ok(());
        }
        s.context
            .admit_tool_group(&self.session_id, &results, &params, &self.model_id)
            .await?;
        Ok(())
    }

    async fn compact_for_overflow(&self) -> anyhow::Result<bool> {
        match &self.compactor {
            Some(c) => c.compact_now(&self.session_id).await,
            None => Ok(false),
        }
    }

    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>> {
        let entries = self
            .services
            .context
            .history
            .load(&self.session_id, 0)
            .await?;
        let start = entries.len().saturating_sub(TAIL_ENTRIES);
        Ok(entries[start..].iter().map(|e| e.message.clone()).collect())
    }
}

/// Assemble les tuiles du prompt d'une session, instantanés mémoire recalculés.
pub async fn build_tiers(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
) -> Tiers {
    build_tiers_in(s, user_text, mcp_lines, run_state, None, None).await
}

/// Comme [`build_tiers`], avec les instantanés T2 figés pour l'épisode `(session, n)` :
/// une écriture de profil n'altère pas le préfixe avant l'épisode suivant (§6.6, CA 6).
pub async fn build_tiers_in(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
    episode: Option<(&str, i64)>,
    query_vector: Option<Vec<f32>>,
) -> Tiers {
    let cfg = s.config.config();
    let vault = vault_dir(s);

    let mut b = TiersBuilder::new();
    if let Ok(soul) = std::fs::read_to_string(vault.join("SOUL.md")) {
        b = b.soul(strip_frontmatter(&soul));
    }
    if let Ok(agents) = std::fs::read_to_string(vault.join("AGENTS.md")) {
        b = b.agents_md(strip_frontmatter(&agents));
    }

    // T1 : méta-outils MCP seulement s'il y a des serveurs, skills, serveurs connectés.
    if !mcp_lines.is_empty() {
        for (name, desc, _) in penelope_mcp::registry::ToolRegistry::meta_tools() {
            b = b.meta_tool(name, desc);
        }
        for l in mcp_lines {
            b = b.mcp_server(l.clone());
        }
    }
    for sk in s.skills.all() {
        b = b.skill(sk.name.clone(), sk.description.clone());
    }
    // Workflows : le modèle sait qu'ils existent et ce qu'ils attendent (issue #34).
    for e in s.workflows.all() {
        let m = &e.workflow.metadata;
        let required: Vec<&str> = m
            .parameters
            .iter()
            .filter(|p| p.required)
            .map(|p| p.id.as_str())
            .collect();
        let role = m.description.lines().next().unwrap_or_default().trim();
        let line = if required.is_empty() {
            role.to_string()
        } else {
            format!("{role} (paramètres requis : {})", required.join(", "))
        };
        b = b.workflow(m.id.clone(), line);
    }

    // T2 : instantanés mémoire, figés par épisode quand il y en a un.
    let [profile, core, project] = match episode {
        Some((session_id, n)) => frozen_snapshot(s, session_id, n).await,
        None => fresh_snapshot(s).await,
    };
    b = b.memory_snapshot(profile, core, project);

    // T4 : date locale, état du run, rappel mémoire déclenché par le message.
    b = b.volatile(volatile_header(
        &local_now(s),
        &cfg.owner.timezone,
        run_state,
    ));
    // Notes de travail de la session, à jour à chaque tour : elles survivent aux
    // compactions (issue #32).
    if let Some((session_id, _)) = episode
        && let Some(notes) = crate::session_notes::prompt_block(s, session_id).await
    {
        b = b.volatile(notes);
    }
    if !user_text.trim().is_empty() {
        let recall = penelope_memory::Recall::new(
            &s.memory,
            penelope_memory::RecallParams::from_config(&cfg.memory),
        )
        .path1(user_text, &Default::default(), query_vector, &[])
        .await;
        // Retour d'usage (issue #37) : un souvenir servi au modèle compte comme rappelé.
        for t in &recall.triggered {
            let _ = s.memory.record_recall(&t.entry.uid, user_text, true).await;
        }
        let rendered = recall.render();
        if !rendered.trim().is_empty() {
            b = b.volatile(rendered);
        }
    }
    b.build()
}

/// Profil, cœur et projets tels que l'index les donne maintenant.
pub(crate) async fn fresh_snapshot(s: &Services) -> [String; 3] {
    let cfg = s.config.config();
    let mut out: [String; 3] = Default::default();
    // Entrées expirées : jamais injectées d'office ; `sensible` n'est qu'un marqueur
    // (issues #25 et #37).
    let hidden = s.memory.hidden_uids().await.unwrap_or_default();
    for (i, (level, budget)) in [
        (
            penelope_memory::Level::Profil,
            cfg.memory.profile_budget_tokens,
        ),
        (penelope_memory::Level::Coeur, cfg.memory.core_budget_tokens),
        (
            penelope_memory::Level::Projet,
            cfg.memory.project_budget_tokens,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut entries = s.memory.by_level(level).await.unwrap_or_default();
        entries.retain(|e| !hidden.contains(&e.uid));
        out[i] = penelope_memory::recall::Snapshots::build_block(&entries, budget as u64);
    }
    out
}

/// Instantané de l'épisode : calculé au premier tour, relu ensuite.
async fn frozen_snapshot(s: &Services, session_id: &str, episode: i64) -> [String; 3] {
    let key = crate::episodes::snapshot_key(session_id, episode);
    let k = key.clone();
    let stored: Option<String> = s
        .store
        .read(move |c| {
            let mut st = c.prepare("SELECT v FROM kv WHERE k = ?1")?;
            let mut rows = st.query([&k])?;
            Ok(match rows.next()? {
                Some(r) => Some(r.get::<_, String>(0)?),
                None => None,
            })
        })
        .await
        .ok()
        .flatten();
    if let Some(blocks) = stored.and_then(|raw| serde_json::from_str::<[String; 3]>(&raw).ok()) {
        return blocks;
    }
    let blocks = fresh_snapshot(s).await;
    if let Ok(raw) = serde_json::to_string(&blocks) {
        let _ = s
            .store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv(k, v, ts)
                     VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
                    penelope_store::rusqlite::params![key, raw],
                )?;
                Ok(())
            })
            .await;
    }
    blocks
}

/// Répertoire du vault, placeholders développés.
pub fn vault_dir(s: &Services) -> std::path::PathBuf {
    let cfg = s.config.config();
    s.platform.dirs.expand(&cfg.memory.vault_path)
}

/// Date et heure locales du propriétaire, lisibles.
pub fn local_now(s: &Services) -> String {
    let cfg = s.config.config();
    let utc = chrono::DateTime::from_timestamp_millis(s.clock.now_ms()).unwrap_or_default();
    match cfg.owner.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => utc
            .with_timezone(&tz)
            .format("%A %d %B %Y, %H:%M")
            .to_string(),
        Err(_) => utc.format("%A %d %B %Y, %H:%M UTC").to_string(),
    }
}

fn strip_frontmatter(raw: &str) -> String {
    penelope_kernel::frontmatter::parse(raw)
        .map(|fm| fm.body)
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
    }

    async fn session(s: &Services) -> String {
        s.sessions
            .create(penelope_kernel::session::SessionKind::Chat, None)
            .await
            .unwrap()
            .id
            .to_string()
    }

    /// #52 : cinq résultats d'outils parallèles de 20 k tokens chacun ne passent pas
    /// entiers parce qu'aucun ne dépasse le seuil à lui seul : le groupe est admis sous le
    /// budget, têtes et queues gardées, le reste lisible en artefact.
    #[tokio::test]
    async fn parallel_tool_results_are_admitted_as_one_group() {
        let (_d, s) = services().await;
        let sid = session(&s).await;
        let tiers = build_tiers(&s, "lis ces fichiers", &[], None).await;
        let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
        let params = conv.params();
        let budget = params.tool_group_budget();

        // ~20 k tokens chacun : sous le seuil d'un résultat isolé, cinq fois trop à cinq.
        let body = "donnée ".repeat(12_000);
        for i in 0..5 {
            conv.record(
                &ChatMessage::tool_result(format!("c{i}"), "fs_read", &body),
                false,
            )
            .await
            .unwrap();
        }
        conv.admit_tool_results(5).await.unwrap();

        let entries = s.context.history.load(&sid, 0).await.unwrap();
        let tools: Vec<_> = entries
            .iter()
            .filter(|e| e.message.role == Role::Tool)
            .collect();
        assert_eq!(tools.len(), 5);
        let externalised = tools.iter().filter(|e| e.artifact_id.is_some()).count();
        assert!(externalised > 0, "le groupe doit être admis sous budget");
        let total: u64 = tools
            .iter()
            .map(|e| {
                s.context
                    .estimator
                    .text_tokens("openrouter:mock/model", &e.message.text())
            })
            .sum();
        // La découpe se fait en caractères (~3,6 par token) : on vise le budget à 20 %
        // près, loin des 100 k tokens qui entraient avant.
        assert!(
            total <= budget + budget / 5,
            "{total} tokens admis pour un budget de {budget}"
        );
        let first = tools.iter().find(|e| e.artifact_id.is_some()).unwrap();
        assert!(first.message.text().contains("artifact_read"));
        let id = first.artifact_id.clone().unwrap();
        let (chunk, _, _) = s
            .context
            .history
            .read_artifact(&id, 0, 50)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(chunk.len(), 50, "le contenu complet reste lisible");

        // Idempotent : une seconde passe ne réexternalise rien.
        conv.admit_tool_results(5).await.unwrap();
        let after = s.context.history.load(&sid, 0).await.unwrap();
        let again = after
            .iter()
            .filter(|e| e.message.role == Role::Tool && e.artifact_id.is_some())
            .count();
        assert_eq!(again, externalised, "admission déjà faite, rien ne bouge");
    }

    /// #52 : un résultat seul, même gros, reste entier tant qu'il tient dans le budget.
    #[tokio::test]
    async fn a_lone_tool_result_is_left_whole() {
        let (_d, s) = services().await;
        let sid = session(&s).await;
        let tiers = build_tiers(&s, "lis ce fichier", &[], None).await;
        let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
        conv.record(
            &ChatMessage::tool_result("c1", "fs_read", "donnée ".repeat(12_000)),
            false,
        )
        .await
        .unwrap();
        conv.admit_tool_results(1).await.unwrap();
        let entries = s.context.history.load(&sid, 0).await.unwrap();
        assert!(
            entries.iter().all(|e| e.artifact_id.is_none()),
            "un résultat isolé sous le seuil reste entier"
        );
    }

    #[tokio::test]
    async fn recorded_messages_come_back_in_the_request() {
        let (_d, s) = services().await;
        let sid = session(&s).await;
        let tiers = build_tiers(&s, "bonjour", &[], None).await;
        let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);

        conv.record(&ChatMessage::user("bonjour"), false)
            .await
            .unwrap();
        conv.record(&ChatMessage::assistant("salut !"), false)
            .await
            .unwrap();

        let msgs = conv.request_messages().await.unwrap();
        assert_eq!(msgs[0].role, Role::System, "le préfixe vient en tête");
        assert!(msgs[0].text().contains("Tu es Pénélope"));
        let texts: Vec<String> = msgs.iter().map(|m| m.text()).collect();
        assert!(texts.iter().any(|t| t.ends_with("bonjour")));
        assert!(texts.contains(&"salut !".to_string()));
        // T4 en tête du dernier message utilisateur : la date locale.
        let last_user = msgs.iter().rev().find(|m| m.role == Role::User).unwrap();
        assert!(last_user.text().starts_with("<contexte>"));
        assert!(last_user.text().contains("Date et heure"));

        // Relu depuis la base, par une autre instance : rien n'était en mémoire.
        let tiers = build_tiers(&s, "", &[], None).await;
        let again = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
        assert_eq!(again.tail().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_stable_prefix_does_not_move_between_turns() {
        let (_d, s) = services().await;
        let a = build_tiers(&s, "premier message", &[], None).await;
        let b = build_tiers(&s, "second message, tout autre", &[], None).await;
        assert_eq!(a.prefix_hash(), b.prefix_hash());
    }

    #[tokio::test]
    async fn soul_and_mcp_servers_enter_the_prefix() {
        let (_d, s) = services().await;
        let vault = vault_dir(&s);
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("SOUL.md"), "Tu tutoies ton propriétaire.").unwrap();
        let t = build_tiers(&s, "", &["forge : 12 outils".into()], None).await;
        assert!(t.identity.contains("Tu tutoies"));
        assert!(t.index.contains("forge : 12 outils"));
        assert!(t.index.contains("tool_search"));
    }

    #[tokio::test]
    async fn huge_tool_results_are_externalised() {
        let (_d, s) = services().await;
        let sid = session(&s).await;
        let tiers = build_tiers(&s, "", &[], None).await;
        let conv = SessionConversation::new(s.clone(), &sid, "openrouter:mock/model", tiers, 0);
        let big = "ligne de journal très bavarde\n".repeat(20_000);
        conv.record(
            &ChatMessage::tool_result("c1", "shell_exec", big.clone()),
            false,
        )
        .await
        .unwrap();
        let stored = s.context.history.load(&sid, 0).await.unwrap();
        assert!(
            stored[0].artifact_id.is_some(),
            "le corps doit partir en artefact"
        );
        assert!(stored[0].message.text().len() < big.len() / 4);
    }
}
