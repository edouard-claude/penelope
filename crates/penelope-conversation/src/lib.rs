//! Transcript persistant d'une session et assemblage du prompt (§5.2, §6.3).
//!
//! L'historique canonique vit en base (`messages`) ; la requête envoyée au modèle en est
//! une **projection** : tuiles T0 à T2 stables, résumés LCM, queue verbatim, T4 volatile.
//!
//! `penelope-conversation` (épopée #208, T23) : la conversation d'une session
//! (`SessionConversation`, relue depuis le journal d'événements), la compaction de fond et son état (`compaction`), les titres de
//! session (`titles`), l'alerte de budget (`budget_alert`). C'est l'implémentation du
//! port `Conversation` que la boucle consomme. La crate ne connaît que `penelope-app`,
//! `penelope-vault` et les crates métier : ni le daemon, ni la boucle, ni le canal.

#![forbid(unsafe_code)]

pub mod budget_alert;
pub mod compaction;
pub mod titles;

use penelope_app::bus::{ChannelDelivery, Origin};
use penelope_app::conversation::Conversation;
pub use penelope_app::helpers::{local_now, vault_dir};
use penelope_app::steering::{Checkpoint, Inbox, Steer};
// Tuiles du prompt et instantanés mémoire, descendus dans `penelope-vault` (T22).
use penelope_app::services::Services;
use penelope_context::CompactionParams;
use penelope_context::journal::{Provenance, UserSource};
use penelope_context::tiers::Tiers;
use penelope_context::transcript::Entry;
use penelope_kernel::turn::{Turn, TurnKind};
use penelope_llm::CancelToken;
use penelope_llm::types::{ChatMessage, Role};
pub use penelope_vault::snapshot::{core_overflow, fresh_snapshot, snapshot_uids};
pub use penelope_vault::tiers::{build_tiers, build_tiers_in, build_turn_prompt};
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
    compactor: Option<Arc<dyn penelope_app::conversation::Compactor>>,
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

    pub fn with_compactor(
        mut self,
        compactor: Arc<dyn penelope_app::conversation::Compactor>,
    ) -> Self {
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
        let (s, sid) = (&self.services, &self.session_id);
        Ok(s.context.projected_entries(sid).await?)
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
            .saturating_sub(penelope_context::compaction::reserved_output(params.window));
        let (messages, _) = s
            .context
            .emergency(ctx.messages, limit, &self.model_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(messages)
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

    /// Absorbé pendant le tour : écrit une fois sous sa clé de file, daté de son
    /// arrivée, marqué `mid_turn` (§2.3, #161).
    async fn record_steer(&self, steer: &Steer) -> anyhow::Result<()> {
        let Some(text) = &steer.text else {
            return Ok(());
        };
        let s = &self.services;
        let tokens = s.context.estimator.text_tokens(&self.model_id, text);
        let prov = Provenance {
            mid_turn: true,
            ..Provenance::queued(UserSource::Merged, &steer.id, &steer.arrived_at)
        };
        s.context
            .history
            .append_queued(
                &self.session_id,
                &ChatMessage::user(text.as_str()),
                tokens,
                self.episode,
                &prov,
            )
            .await?;
        Ok(())
    }

    async fn tail(&self) -> anyhow::Result<Vec<ChatMessage>> {
        // Les dernières entrées seulement : `resolve_pending` appelle cette queue à
        // chaque itération (issue #55).
        let (s, sid) = (&self.services, &self.session_id);
        let entries = s.context.tail(sid, TAIL_ENTRIES).await?;
        Ok(entries.iter().map(|e| e.message.clone()).collect())
    }

    fn prompt_prefix(&self) -> Option<penelope_app::conversation::PromptPrefix> {
        Some(penelope_app::conversation::PromptPrefix::of(&self.tiers))
    }
}

/// Les messages arrivés pendant un tour de message, réclamés sous le bail du tour
/// (`Inbox`, §3.4). La règle de rafale du canal s'applique à la réclamation : au-delà,
/// la carte de rafale part et le tour s'arrête.
pub struct TurnInbox {
    services: Arc<Services>,
    session_id: String,
    turn: Turn,
    channel: Option<Arc<dyn ChannelDelivery>>,
    cancel: CancelToken,
}

impl TurnInbox {
    pub fn new(
        services: Arc<Services>,
        turn: Turn,
        channel: Option<Arc<dyn ChannelDelivery>>,
        cancel: CancelToken,
    ) -> Self {
        TurnInbox {
            services,
            session_id: turn.session_id.clone(),
            turn,
            channel,
            cancel,
        }
    }

    /// La boîte d'un tour de message : seul un message du propriétaire en reçoit
    /// d'autres pendant qu'il tourne (#161).
    pub fn for_turn(
        services: &Arc<Services>,
        turn: &Turn,
        channel: Option<Arc<dyn ChannelDelivery>>,
        cancel: &CancelToken,
    ) -> Option<Arc<dyn Inbox>> {
        (turn.kind == TurnKind::Message).then(|| {
            Arc::new(TurnInbox::new(
                services.clone(),
                turn.clone(),
                channel,
                cancel.clone(),
            )) as Arc<dyn Inbox>
        })
    }

    /// Rafale du canal : trop de messages ou trop de texte depuis le début du tour. Les
    /// seuils sont ceux du canal (`ChannelDelivery::burst_limits`) ; sans canal, aucun.
    async fn burst_rule(&self) -> anyhow::Result<()> {
        let (s, turn) = (&self.services, &self.turn);
        let Some(channel) = self.channel.as_ref() else {
            return Ok(());
        };
        let Some(limits) = channel.burst_limits(&Origin::from_payload(&turn.payload)) else {
            return Ok(());
        };
        let merged = s.turns.merged_messages(turn.id.as_str()).await?;
        let payloads =
            std::iter::once(&turn.payload).chain(merged.iter().map(|message| &message.payload));
        let parts: Vec<String> = payloads
            .filter_map(|payload| payload.get("text").and_then(|text| text.as_str()))
            .map(ToString::to_string)
            .collect();
        if limits.exceeded(&parts) {
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
            self.cancel.cancel();
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Inbox for TurnInbox {
    async fn claim(&self, _at: Checkpoint) -> anyhow::Result<Vec<Steer>> {
        let absorbed = self.services.turns.absorb_pending(&self.turn).await?;
        if absorbed.is_empty() {
            return Ok(Vec::new());
        }
        self.burst_rule().await?;
        Ok(absorbed
            .into_iter()
            .map(|message| Steer {
                id: message.id.as_str().to_string(),
                text: message
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                arrived_at: message.enqueued_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
