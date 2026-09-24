//! Transcript persistant d'une session et assemblage du prompt (§5.2, §6.3).
//!
//! L'historique canonique vit en base (`messages`) ; la requête envoyée au modèle en est
//! une **projection** : tuiles T0 à T2 stables, résumés LCM, queue verbatim, T4 volatile.

use crate::agent::Conversation;
use crate::bus::{ChannelDelivery, Origin};
pub use crate::helpers::{local_now, vault_dir};
use crate::runtime::Services;
use penelope_context::CompactionParams;
use penelope_context::journal::{Provenance, UserSource};
use penelope_context::tiers::{Tiers, TiersBuilder, volatile_header};
use penelope_context::transcript::Entry;
use penelope_kernel::event::EventDraft;
use penelope_kernel::turn::Turn;
use penelope_llm::CancelToken;
use penelope_llm::types::{ChatMessage, Role};
use serde_json::json;
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
    merge_turn: Option<Turn>,
    merge_channel: Option<Arc<dyn ChannelDelivery>>,
    merge_cancel: Option<CancelToken>,
    absorbed_during_run: AtomicBool,
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
            merge_turn: None,
            merge_channel: None,
            merge_cancel: None,
            absorbed_during_run: AtomicBool::new(false),
        }
    }

    pub fn with_compactor(mut self, compactor: Arc<dyn crate::agent::Compactor>) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// Rattache les messages arrivés pendant les appels d'outils au prochain appel modèle.
    pub fn with_merge_turn(
        mut self,
        turn: Turn,
        channel: Option<Arc<dyn ChannelDelivery>>,
        cancel: CancelToken,
    ) -> Self {
        self.merge_turn = Some(turn);
        self.merge_channel = channel;
        self.merge_cancel = Some(cancel);
        self
    }

    /// Vrai si une projection de ce tour a atteint le seuil moins la marge (§5.4).
    pub fn wants_compaction(&self) -> bool {
        self.wants_compaction.load(Ordering::SeqCst)
    }

    fn with_merge_note(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        if self.absorbed_during_run.load(Ordering::SeqCst) {
            let index = messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            messages.insert(
                index,
                ChatMessage::system(
                    "Un nouveau message utilisateur est arrivé pendant le tour ; ce qui précède est déjà exécuté. Tiens compte du nouveau message dans la réponse en cours.",
                ),
            );
        }
        messages
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
        // Les résumés actifs d'abord : ce qu'ils couvrent n'a pas à être relu ni
        // désérialisé pour être aussitôt jeté (issue #55).
        let nodes = s.context.lcm.active_nodes(&self.session_id).await?;
        let covered_to = nodes.iter().filter_map(|n| n.to_seq).max().unwrap_or(0);
        let from_seq = if nodes.is_empty() { 0 } else { covered_to + 1 };
        let mut entries = s.context.history.load(&self.session_id, from_seq).await?;
        // Contexte volatil figé avec chaque message utilisateur (issue #17).
        let contexts = s
            .context
            .history
            .contexts_from(&self.session_id, from_seq)
            .await?;
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
        if let Some(turn) = &self.merge_turn {
            let absorbed = s.turns.absorb_pending(turn).await?;
            for message in &absorbed {
                let Some(text) = message.payload.get("text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let tokens = s.context.estimator.text_tokens(&self.model_id, text);
                // Absorbé pendant le tour : la requête porte la note de fusion (§2.3).
                let prov = Provenance {
                    mid_turn: true,
                    ..Provenance::queued(
                        UserSource::Merged,
                        message.id.as_str(),
                        &message.enqueued_at,
                    )
                };
                let user = ChatMessage::user(text);
                s.context
                    .history
                    .append_queued(&self.session_id, &user, tokens, self.episode, &prov)
                    .await?;
            }
            if !absorbed.is_empty() {
                self.absorbed_during_run.store(true, Ordering::SeqCst);
                s.events
                    .append(
                        EventDraft::new(
                            "turn.merged",
                            json!({"turn": turn.id.as_str(), "count": absorbed.len(), "phase": "running"}),
                        )
                        .session(&self.session_id),
                    )
                    .await?;
                if matches!(Origin::from_payload(&turn.payload), Origin::Telegram { .. }) {
                    let merged = s.turns.merged_messages(turn.id.as_str()).await?;
                    let payloads = std::iter::once(&turn.payload)
                        .chain(merged.iter().map(|message| &message.payload));
                    let parts: Vec<String> = payloads
                        .filter_map(|payload| payload.get("text").and_then(|text| text.as_str()))
                        .map(ToString::to_string)
                        .collect();
                    let cfg = s.config.config();
                    let too_many = cfg.telegram.burst_messages > 0
                        && parts.len() >= cfg.telegram.burst_messages;
                    let too_long = cfg.telegram.burst_chars > 0
                        && parts.iter().map(|part| part.chars().count()).sum::<usize>()
                            >= cfg.telegram.burst_chars;
                    if too_many || too_long {
                        let channel = self
                            .merge_channel
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("canal Telegram indisponible"))?;
                        let origin = merged
                            .last()
                            .map(|message| Origin::from_payload(&message.payload))
                            .unwrap_or_else(|| Origin::from_payload(&turn.payload));
                        channel
                            .offer_burst(&self.session_id, &origin, parts)
                            .await
                            .map_err(anyhow::Error::msg)?;
                        s.kv_set(&format!("turn.burst_card.{}", turn.id), "1")
                            .await?;
                        if let Some(cancel) = &self.merge_cancel {
                            cancel.cancel();
                        }
                    }
                }
            }
        }
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
            return Ok(self.with_merge_note(ctx.messages));
        }
        // Niveau 4 : la requête ne tient pas, on la réduit en le prouvant (§5.4).
        let limit = params
            .window
            .saturating_sub(penelope_context::compaction::reserved_output(params.window));
        let (messages, _) = s
            .context
            .emergency(ctx.messages, limit, &self.model_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(self.with_merge_note(messages))
    }

    async fn record(&self, message: &ChatMessage, eager: bool) -> anyhow::Result<()> {
        self.record_as(message, eager, &Provenance::default()).await
    }

    async fn record_as(
        &self,
        message: &ChatMessage,
        eager: bool,
        prov: &Provenance,
    ) -> anyhow::Result<()> {
        let s = &self.services;
        let tokens = s.context.estimator.message_tokens(&self.model_id, message);
        let (sid, episode) = (&self.session_id, self.episode);
        let seq = s
            .context
            .history
            .append_as(sid, message, tokens, episode, eager, None, prov)
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
        // Les dernières entrées seulement : `resolve_pending` appelle cette queue à
        // chaque itération (issue #55).
        let entries = self
            .services
            .context
            .history
            .tail(&self.session_id, TAIL_ENTRIES)
            .await?;
        Ok(entries.iter().map(|e| e.message.clone()).collect())
    }

    fn prompt_prefix(&self) -> Option<crate::prompt_snapshot::PromptPrefix> {
        Some(crate::prompt_snapshot::PromptPrefix::of(&self.tiers))
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
    build_turn_prompt(s, user_text, mcp_lines, run_state, episode, query_vector)
        .await
        .0
}

/// Comme [`build_tiers_in`], avec les uid des souvenirs que le rappel automatique a
/// servis : le tour les compte et juge leur usage sur la réponse (issue #105).
pub async fn build_turn_prompt(
    s: &Services,
    user_text: &str,
    mcp_lines: &[String],
    run_state: Option<&str>,
    episode: Option<(&str, i64)>,
    query_vector: Option<Vec<f32>>,
) -> (Tiers, Vec<String>) {
    let cfg = s.config.config();
    let vault = vault_dir(s);

    let mut b = TiersBuilder::new();
    if let Ok(soul) = std::fs::read_to_string(vault.join("SOUL.md")) {
        b = b.soul(strip_frontmatter(&soul));
    }
    if let Ok(agents) = std::fs::read_to_string(vault.join("AGENTS.md")) {
        b = b.agents_md(strip_frontmatter(&agents));
    }

    // Conversation : les outils rares sont nommés, pas décrits (#104).
    if episode.is_some() {
        b = b.on_demand(penelope_tools::ON_DEMAND);
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

    // Ce que la machine sait faire, en une ligne stable (issue #156) : lue du dernier
    // inventaire, jamais sondée ici — un tour de conversation ne lance pas de processus.
    if let Some(inv) = crate::machine::cached(s).await {
        b = b.machine(inv.prompt_line());
    }

    // T2 : instantanés mémoire, figés par épisode quand il y en a un.
    let [profile, core, project] = match episode {
        Some((session_id, n)) => frozen_snapshot(s, session_id, n, user_text).await,
        None => fresh_snapshot(s, &crate::session_project::Scope::All).await,
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
    let mut recalled = Vec::new();
    if !user_text.trim().is_empty() {
        // Voie 1 avec ce qu'il faut pour être utile : les pratiques du vault et le
        // contexte du tour, sans quoi aucune règle défaisable n'est rappelée et le
        // facteur « projet actif » reste inopérant (issue #58).
        let practices = practices_of(&vault);
        let mut ctx = current_context(s, user_text, episode.map(|(sid, _)| sid)).await;
        let scope = match episode {
            Some((sid, _)) => crate::session_project::Scope::Session(
                crate::session_project::of_session(s, sid).await.0,
            ),
            None => crate::session_project::Scope::All,
        };
        ctx.injected_uids = snapshot_uids(s, &scope).await;
        let recall = penelope_memory::Recall::new(
            &s.memory,
            penelope_memory::RecallParams::from_config(&cfg.memory),
        )
        .path1(user_text, &ctx, query_vector, &practices)
        .await;
        // Retour d'usage (issues #37 et #105) : servis, comptés par le tour qui les a
        // demandés, et utiles seulement si la réponse s'en sert.
        recalled = recall
            .triggered
            .iter()
            .map(|t| t.entry.uid.clone())
            .collect();
        // Vue sans être retenue : le dénominateur du retrait proposé (issue #86).
        let _ = s.memory.record_seen(&recall.seen).await;
        let rendered = recall.render();
        if !rendered.trim().is_empty() {
            b = b.volatile(rendered);
        }
    }
    (b.build(), recalled)
}

/// Pratiques du vault, relues seulement quand un fichier a changé (issue #58).
fn practices_of(vault: &std::path::Path) -> Vec<penelope_memory::vault::Practice> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    type Cache = HashMap<std::path::PathBuf, (std::time::SystemTime, Option<PracticeEntry>)>;
    static CACHE: std::sync::OnceLock<Mutex<Cache>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());

    let dir = vault.join("pratiques");
    let Ok(read) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for e in read.flatten() {
        let path = e.path();
        let Some(stem) = path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .filter(|f| f.ends_with(".md"))
            .map(|f| f.trim_end_matches(".md").to_string())
        else {
            continue;
        };
        seen.push(path.clone());
        let mtime = e
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let fresh = guard.get(&path).map(|(t, _)| *t == mtime).unwrap_or(false);
        if !fresh {
            let parsed = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| penelope_memory::vault::Practice::parse(&raw, &stem).ok());
            guard.insert(path.clone(), (mtime, parsed));
        }
        if let Some((_, Some(p))) = guard.get(&path) {
            out.push(p.clone());
        }
    }
    guard.retain(|k, _| seen.contains(k));
    out
}

type PracticeEntry = penelope_memory::vault::Practice;

/// Contexte du tour : projet actif du workspace de la session, type de tâche déduit du
/// message. Ce que la voie 1 évalue pour décider d'une exception (§6.7, issue #58).
async fn current_context(
    s: &Services,
    user_text: &str,
    session_id: Option<&str>,
) -> penelope_memory::recall::CurrentContext {
    let mut ctx = penelope_memory::recall::CurrentContext {
        tache: penelope_memory::recall::classify_task(user_text),
        ..Default::default()
    };
    if let Some(sid) = session_id
        && let Ok(Some(sess)) = s.sessions.get(sid).await
        && let Some(ws) = sess.workspace.filter(|w| !w.is_empty())
    {
        let key = penelope_memory::recall::project_key(None, &ws);
        ctx.touch_project(&key);
    }
    ctx
}

/// uid servis d'office dans l'instantané T2 : ni rappelés une seconde fois, ni comptés
/// « jamais rappelés » (issue #62).
pub(crate) async fn snapshot_uids(
    s: &Services,
    scope: &crate::session_project::Scope,
) -> Vec<String> {
    let cfg = s.config.config();
    let hidden = s.memory.hidden_uids().await.unwrap_or_default();
    let mut out = Vec::new();
    for (level, budget) in [
        (
            penelope_memory::Level::Profil,
            cfg.memory.profile_budget_tokens,
        ),
        (penelope_memory::Level::Coeur, cfg.memory.core_budget_tokens),
        (
            penelope_memory::Level::Projet,
            cfg.memory.project_budget_tokens,
        ),
    ] {
        let mut entries = s.memory.by_level(level).await.unwrap_or_default();
        entries.retain(|e| !hidden.contains(&e.uid) && crate::session_project::keeps(scope, e));
        let (_, uids) =
            penelope_memory::recall::Snapshots::build_block_with_uids(&entries, budget as u64);
        out.extend(uids);
    }
    out
}

/// Profil, cœur et projets tels que l'index les donne maintenant, pour cette portée : le
/// cœur et les projets d'un autre sujet restent au rappel (issue #119).
pub(crate) async fn fresh_snapshot(
    s: &Services,
    scope: &crate::session_project::Scope,
) -> [String; 3] {
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
        entries.retain(|e| !hidden.contains(&e.uid) && crate::session_project::keeps(scope, e));
        out[i] = penelope_memory::recall::Snapshots::build_block(&entries, budget as u64);
    }
    out
}

/// Instantané de l'épisode : calculé au premier tour, relu ensuite.
async fn frozen_snapshot(
    s: &Services,
    session_id: &str,
    episode: i64,
    user_text: &str,
) -> [String; 3] {
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
    // Le sujet se fixe ici, avec l'instantané de l'épisode (#119).
    let project = crate::session_project::resolve(s, session_id, user_text).await;
    let blocks = fresh_snapshot(s, &crate::session_project::Scope::Session(project)).await;
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

fn strip_frontmatter(raw: &str) -> String {
    penelope_kernel::frontmatter::parse(raw)
        .map(|fm| fm.body)
        .unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests;
