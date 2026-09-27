//! Le préfixe et le contexte volatil d'un tour de conversation (issue #17, #236), sortis
//! du daemon (`cache_audit`).
//!
//! Tant que le cache de la session est chaud, un préfixe modifié (nouvel instantané
//! mémoire, AGENTS.md, skill ou serveur MCP) attend la prochaine pause ou la prochaine
//! compaction, qui le cassent de toute façon ; sa différence part en fin, une seule fois,
//! dans le contexte volatil du message (`<mise-a-jour>`, `tiers::update`). Le volatil est
//! ensuite figé avec ce message : il l'accompagnera dans toutes les requêtes suivantes.

use penelope_app::services::Services;
use penelope_context::store::PromptUpdated;
use penelope_context::tiers::Tiers;
use penelope_context::tiers::update::{PrefixUpdate, SkillChange};
use penelope_llm::cache::CACHE_TTL_MS;

/// Bloc de contexte volatil, tel qu'il précède le texte d'un message utilisateur.
pub fn context_block(volatile: &str) -> String {
    format!("<contexte>\n{}\n</contexte>\n\n", volatile.trim())
}

/// Le préfixe retenu par la session tant que son cache est chaud : `None` après une
/// pause plus longue que le cache ou une compaction, qui sont les frontières où un
/// préfixe nouveau (et une liste d'outils nouvelle) peut partir.
pub async fn held_prefix(s: &Services, session_id: &str) -> anyhow::Result<Option<Tiers>> {
    let warm = s
        .budget
        .previous_call(session_id)
        .await?
        .is_some_and(|p| s.clock.now_ms() - p.ts_ms < CACHE_TTL_MS);
    if !warm {
        return Ok(None);
    }
    Ok(s.context.history.retained_prefix(session_id).await?)
}

/// Prépare le préfixe et le volatil d'un tour : préfixe retenu à cache chaud, différence
/// en fin s'il a bougé, volatil figé avec le dernier message utilisateur, préfixe
/// journalisé s'il change. `held` : [`held_prefix`], lu une fois pour tout le tour.
pub async fn settle(
    s: &Services,
    session_id: &str,
    held: Option<Tiers>,
    tiers: &mut Tiers,
) -> anyhow::Result<()> {
    let announced = s.context.history.announced(session_id).await?;
    let mut record = PromptUpdated::default();
    let mut update = PrefixUpdate::default();
    if let Some(stored) = held
        && (&stored.identity, &stored.index, &stored.context)
            != (&tiers.identity, &tiers.index, &tiers.context)
    {
        // Ce que le modèle sait déjà : le préfixe retenu, ou la dernière différence
        // envoyée par-dessus.
        let stored_hash = stored.prefix_hash();
        let base = match announced.prefix {
            Some((hash, known)) if hash == stored_hash => known,
            _ => stored.clone(),
        };
        update = PrefixUpdate::between(&base, tiers);
        if !update.is_empty() {
            record.base = Some(stored_hash);
            record.rendered = Some(tiers.prefix());
            record.tiles = Some(tiers.tile_map());
            record.changed = update
                .changed_tiles()
                .iter()
                .map(|t| t.to_string())
                .collect();
            record.rewritten = update
                .rewritten_tiles()
                .iter()
                .map(|t| t.to_string())
                .collect();
        }
        tracing::debug!(
            session = session_id,
            "préfixe modifié : attend un cache froid"
        );
        tiers.identity = stored.identity;
        tiers.index = stored.index;
        tiers.context = stored.context;
    }
    // Une skill chargée dont le corps a changé depuis se dit, même sans préfixe retenu :
    // l'historique garde l'ancien corps.
    let mut skills = Vec::new();
    for (name, loaded) in &announced.loaded {
        let now = s.skills.get(name).map(|k| k.body_hash).unwrap_or_default();
        if &now != loaded && announced.skills.get(name) != Some(&now) {
            skills.push(if now.is_empty() {
                SkillChange::Removed(name.clone())
            } else {
                SkillChange::Changed(name.clone())
            });
            record.skills.insert(name.clone(), now);
        }
    }
    update = update.with_skills(skills);
    let block = (!update.is_empty()).then(|| update.block());
    if let Some(block) = &block {
        if !tiers.volatile.trim().is_empty() {
            tiers.volatile.push_str("\n\n");
        }
        tiers.volatile.push_str(block);
    }
    let frozen = freeze_volatile(s, session_id, tiers).await?;
    // Envoyée une seule fois : journalisée seulement si elle part avec ce message.
    if let Some(block) = block.filter(|_| frozen) {
        record.chars = block.chars().count();
        tracing::info!(
            session = session_id,
            tiles = ?record.changed,
            skills = record.skills.len(),
            chars = record.chars,
            "différence du préfixe envoyée en fin"
        );
        s.context.history.announce(session_id, &record).await?;
    }
    journal_prefix(s, session_id, tiers).await;
    Ok(())
}

/// Fige le contexte volatil (T4) avec le dernier message utilisateur de la session, avant
/// son premier envoi : il l'accompagnera dans toutes les requêtes suivantes, reprises
/// après approbation et tours suivants compris, au lieu de se déplacer à chaque tour et
/// de réécrire l'historique. Vrai s'il vient d'être figé.
async fn freeze_volatile(
    s: &Services,
    session_id: &str,
    tiers: &mut Tiers,
) -> anyhow::Result<bool> {
    let sid = session_id.to_string();
    let last_user: Option<i64> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT MAX(seq) FROM messages WHERE session_id = ?1 AND role = 'user' AND sealed IS NOT 2",
                [sid],
                |r| r.get(0),
            )?)
        })
        .await?;
    let Some(seq) = last_user else {
        return Ok(false);
    };
    let mut frozen = false;
    if !tiers.volatile.trim().is_empty() {
        frozen = s
            .context
            .history
            .freeze_context(session_id, seq, &context_block(&tiers.volatile))
            .await?;
    }
    tiers.volatile.clear();
    Ok(frozen)
}

/// Le préfixe retenu entre au journal quand il change, en entier (`conv.system`,
/// épopée #208, T6). Un échec ne coûte que l'événement : le tour continue.
async fn journal_prefix(s: &Services, session_id: &str, tiers: &Tiers) {
    if let Err(e) = s.context.history.journal_system(session_id, tiers).await {
        tracing::warn!(session = session_id, error = %e, "préfixe non journalisé");
    }
}

#[cfg(test)]
mod tests;
