//! Le résumé demandé sur le préfixe de la conversation (issue #236).
//!
//! L'appel classique a son propre système (`SUMMARIZER_PROMPT`), aucun outil, et la
//! conversation rendue en un seul message : tout est relu au prix fort, souvent par un
//! autre modèle. Ici, la demande reprend la dernière requête de la conversation telle
//! qu'envoyée (même modèle, préfixe retenu, liste d'outils gelée, historique projeté) et
//! ajoute la consigne de résumé en dernier message : le fournisseur relit son cache.
//!
//! ```text
//!  conversation   [outils][système][h1 … hn]
//!  résumé         [outils][système][h1 … hn][consigne]     ← même préfixe, cache lu
//! ```
//!
//! Sans préfixe chaud, sans liste gelée, sur un modèle de l'abonnement ChatGPT, si la
//! projection ne tient pas, ou si `auto` juge le résumeur moins cher, rien n'est tenté ;
//! un échec se dit dans le bilan et le résumeur classique prend la suite.

use super::summarise::{Attempt, call_summarizer, summary_max_tokens};
use super::*;
use penelope_context::engine::SUMMARIZER_PROMPT;
use penelope_kernel::config::PrefixCompaction;
use penelope_llm::types::{ChatMessage, ToolChoice};

/// Sortie attendue du résumé, en tokens, pour comparer les coûts.
const EXPECTED_OUTPUT_TOKENS: u64 = 4_000;

/// Le résumé du lot fait sur le préfixe de la conversation, et le modèle qui l'a fait ;
/// `None` si l'appel n'a pas lieu ou échoue (le résumeur classique prend la suite).
pub(super) async fn summarise(
    d: &Context,
    job: &SummaryJob,
    at: &Attempt<'_>,
    report: &mut Report,
) -> Option<(Value, String)> {
    let request = request(d, job).await?;
    let model = request.model.clone();
    let provider = d.provider_for(&model).await.ok()?;
    match call_summarizer(d, provider.as_ref(), request, job, at.turn_id).await {
        Ok((summary, cost)) => {
            report.cost_usd += cost;
            Some((summary, model))
        }
        Err(f) => {
            report.recovered.push(format!(
                "résumé sur le préfixe de la conversation écarté ({}) : repli sur le résumeur",
                f.message
            ));
            None
        }
    }
}

/// La dernière requête de la conversation, plus la consigne de résumé.
async fn request(d: &Context, job: &SummaryJob) -> Option<ChatRequest> {
    let s = &d.services;
    let sid = job.session_id.as_str();
    let cfg = s.config.config();
    let mode = cfg.context.compaction_on_prefix;
    if mode == PrefixCompaction::Never {
        return None;
    }
    let tiers = crate::prefix::held_prefix(s, sid).await.ok()??;
    let tools = penelope_app::frozen_tools::frozen(s, sid).await?;
    let previous = s.budget.previous_call(sid).await.ok()??;
    // Le modèle de la dernière requête : l'abonnement ChatGPT ne sert que les tours du
    // propriétaire (#142).
    let model = previous.model.clone();
    if penelope_app::codex_scope::is_codex(&model) {
        return None;
    }
    let bare = strip_provider(&model);
    let params = CompactionParams::from_config(&cfg, s.catalog.window_of(&model), &model);
    let entries = s.context.projected_entries(sid).await.ok()?;
    let ctx = s.context.build_from_entries(
        &entries,
        &tiers,
        &params,
        &model,
        bare.starts_with("anthropic/"),
    );
    if !ctx.fits {
        return None;
    }
    let landmark = entries
        .iter()
        .find(|e| e.seq == job.to_seq)
        .map(|e| e.message.text())?;
    let consigne = instruction(job, &landmark);
    if mode == PrefixCompaction::Auto && !cheaper(d, job, &model, ctx.tokens, &consigne) {
        return None;
    }
    let mut messages = ctx.messages;
    messages.push(ChatMessage::user(consigne));
    let now = s.clock.now_ms();
    Some(ChatRequest {
        pinned_upstream: penelope_llm::cache::sticky_upstream(Some(&previous), &model, now),
        model,
        messages,
        // Comme la conversation : un autre choix d'outil invaliderait le cache des
        // messages chez Anthropic.
        tool_choice: (!tools.is_empty()).then_some(ToolChoice::Auto),
        tools,
        stream: true,
        max_tokens: Some(summary_max_tokens(None)),
        session_id: Some(job.session_id.clone()),
        ..Default::default()
    })
}

/// La consigne de résumé, en dernier message : les règles du résumeur, et la portée du
/// lot repérée par le début de son dernier message, sans son bloc de contexte volatil.
fn instruction(job: &SummaryJob, landmark: &str) -> String {
    let text = match landmark.split_once("</contexte>") {
        Some((_, rest)) if landmark.starts_with("<contexte>") => rest,
        _ => landmark,
    };
    let excerpt: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();
    let previous = if job.previous_summary.is_some() {
        "le résumé de la conversation antérieure, en tête, compris : mets-le à jour"
    } else {
        "sans résumé antérieur"
    };
    format!(
        "<consigne-de-compaction>\nCe message vient du harnais, pas du propriétaire : ne lui \
         réponds pas et n'appelle aucun outil.\n\n{SUMMARIZER_PROMPT}\n\nPortée : la \
         conversation ci-dessus depuis son début ({previous}), jusqu'au message qui commence \
         par « {excerpt} » inclus. Ce qui suit ce message reste en clair : ne le résume \
         pas.\n</consigne-de-compaction>"
    )
}

/// `auto` : l'appel sur le préfixe coûte-t-il au plus autant que le résumeur ? Le préfixe
/// est compté au prix du cache (au prix plein si le catalogue n'en donne pas), le lot du
/// résumeur au prix plein ; la même sortie des deux côtés. Sans prix connu d'un côté, non.
fn cheaper(d: &Context, job: &SummaryJob, model: &str, prefix_tokens: u64, consigne: &str) -> bool {
    let s = &d.services;
    let cfg = s.config.config();
    let summarizer = cfg
        .alias_model(&cfg.role_alias("compaction"))
        .unwrap_or_default()
        .to_string();
    let (Some(conv), Some(summ)) = (s.catalog.get(model), s.catalog.get(&summarizer)) else {
        return false;
    };
    let estimator = &s.context.estimator;
    let cached = if conv.price_cached_read > 0.0 {
        conv.price_cached_read
    } else {
        conv.price_prompt
    };
    let on_prefix = prefix_tokens as f64 * cached
        + estimator.text_tokens(model, consigne) as f64 * conv.price_prompt
        + EXPECTED_OUTPUT_TOKENS as f64 * conv.price_completion;
    let previous = job
        .previous_summary
        .as_deref()
        .map(|p| estimator.text_tokens(&summarizer, p))
        .unwrap_or(0);
    let classic = (job.tokens_src + previous + 2_000) as f64 * summ.price_prompt
        + EXPECTED_OUTPUT_TOKENS as f64 * summ.price_completion;
    on_prefix <= classic
}
