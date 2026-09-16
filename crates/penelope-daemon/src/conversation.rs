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

/// Nombre d'entrées relues pour retrouver les appels d'outils en attente.
const TAIL_ENTRIES: usize = 64;

/// Un transcript de session, adossé à l'historique canonique.
pub struct SessionConversation {
    services: Arc<Services>,
    session_id: String,
    model_id: String,
    tiers: Tiers,
    episode: i64,
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
        }
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
        let entries = s.context.history.load(&self.session_id, 0).await?;
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

/// Assemble les tuiles du prompt d'une session.
pub async fn build_tiers(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
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

    // T2 : instantanés mémoire, figés tant que le vault ne bouge pas.
    let block = |entries: &[penelope_memory::IndexedEntry], budget: usize| {
        penelope_memory::recall::Snapshots::build_block(entries, budget as u64)
    };
    let profile = s
        .memory
        .by_level(penelope_memory::Level::Profil)
        .await
        .unwrap_or_default();
    let core = s
        .memory
        .by_level(penelope_memory::Level::Coeur)
        .await
        .unwrap_or_default();
    let project = s
        .memory
        .by_level(penelope_memory::Level::Projet)
        .await
        .unwrap_or_default();
    b = b.memory_snapshot(
        block(&profile, cfg.memory.profile_budget_tokens),
        block(&core, cfg.memory.core_budget_tokens),
        block(&project, cfg.memory.project_budget_tokens),
    );

    // T4 : date locale, état du run, rappel mémoire déclenché par le message.
    b = b.volatile(volatile_header(
        &local_now(s),
        &cfg.owner.timezone,
        run_state,
    ));
    if !user_text.trim().is_empty() {
        let recall = penelope_memory::Recall::new(
            &s.memory,
            penelope_memory::RecallParams::from_config(&cfg.memory),
        )
        .path1(user_text, &Default::default(), None, &[])
        .await;
        let rendered = recall.render();
        if !rendered.trim().is_empty() {
            b = b.volatile(rendered);
        }
    }
    b.build()
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
