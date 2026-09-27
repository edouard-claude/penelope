//! Appel du résumeur et ses reprises (issue #131), sorti de `compaction.rs`.

use super::*;

/// Délai du résumeur, proportionné au lot : 120 s, plus une seconde par millier de tokens
/// de source, 420 s au plus. Au-delà, le lot est en échec (issue #131).
fn summary_timeout(tokens_src: u64) -> Duration {
    Duration::from_secs((120 + tokens_src / 1_000).min(420))
}

/// Fait résumer un lot par le modèle du rôle `compaction` et valide sa sortie.
/// Échec d'un résumé : passager (délai, 5xx, 429), ou réponse inutilisable.
pub(super) struct SummaryFailure {
    pub(super) message: String,
    pub(super) passing: bool,
}

impl SummaryFailure {
    fn passing(message: String) -> Self {
        SummaryFailure {
            message,
            passing: true,
        }
    }
}

/// Ce qu'il faut pour préparer un lot plus court.
pub(super) struct Attempt<'a> {
    pub(super) session_id: &'a str,
    pub(super) params: &'a CompactionParams,
    pub(super) window: u64,
    pub(super) force: bool,
    pub(super) turn_id: Option<&'a str>,
}

/// Modèle inscrit sur un résumé fait sans modèle.
pub const MECHANICAL_MODEL: &str = "sans modèle";

/// Un lot résumé, avec ses reprises (issue #131) : le lot tel quel ; sur un échec
/// passager, le début du même lot en trois fois plus court ; puis, s'il est déclaré,
/// l'alias de repli du résumeur (`models.routing.fallback`), si la réserve des résumés
/// couvre son coût estimé. Rend le lot effectivement résumé et le modèle qui l'a fait,
/// ou le dernier lot essayé et la raison de l'échec.
pub(super) async fn summarise_or_recover(
    d: &Context,
    provider: &dyn Provider,
    model: &str,
    job: SummaryJob,
    at: &Attempt<'_>,
    report: &mut Report,
) -> Result<(SummaryJob, Value, String), Box<(SummaryJob, SummaryFailure)>> {
    let s = &d.services;
    let first = match summarise(d, provider, model, &job, at.turn_id).await {
        Ok((summary, cost)) => {
            report.cost_usd += cost;
            return Ok((job, summary, model.to_string()));
        }
        Err(f) => f,
    };
    if !first.passing {
        return Err(Box::new((job, first)));
    }
    let mut job = job;
    let mut failure = first;
    if let Ok(Some(short)) = s
        .context
        .prepare_summary_capped(
            at.session_id,
            at.params,
            at.window,
            model,
            at.force,
            Some((job.tokens_src / 3).max(1)),
        )
        .await
        && short.to_seq < job.to_seq
    {
        report.recovered.push(format!(
            "demande plus courte ({} tokens au lieu de {}) après : {}",
            short.tokens_src, job.tokens_src, failure.message
        ));
        job = short;
        match summarise(d, provider, model, &job, at.turn_id).await {
            Ok((summary, cost)) => {
                report.cost_usd += cost;
                return Ok((job, summary, model.to_string()));
            }
            Err(f) => failure = f,
        }
    }
    if !failure.passing {
        return Err(Box::new((job, failure)));
    }
    let cfg = s.config.config();
    let Some(fallback) = cfg
        .models
        .routing
        .fallback
        .get(&cfg.role_alias("compaction"))
        .and_then(|v| v.first())
        .and_then(|a| cfg.alias_model(a))
        .map(String::from)
        .filter(|m| m != model)
    else {
        return Err(Box::new((job, failure)));
    };
    // Un repli coûte plus cher que le résumeur : il reste dans la réserve des résumés.
    let estimated = s
        .catalog
        .get(strip_provider(&fallback))
        .map(|i| (job.tokens_src + 2_000) as f64 * i.price_prompt + 8_000.0 * i.price_completion)
        .unwrap_or(f64::INFINITY);
    let spent = compaction_spent_today(s).await;
    if spent + estimated > cfg.budget.compaction_reserve_usd {
        report.recovered.push(format!(
            "repli `{fallback}` écarté : ~{estimated:.2} $ estimés, {spent:.2} $ déjà dépensés \
             sur {:.2} $ de réserve (`budget.compaction_reserve_usd`)",
            cfg.budget.compaction_reserve_usd
        ));
        return Err(Box::new((job, failure)));
    }
    let fallback =
        penelope_app::codex_scope::background(&d.services, &fallback, "compaction").await;
    let Ok(fb) = d.provider_for(&fallback).await else {
        return Err(Box::new((job, failure)));
    };
    report.recovered.push(format!(
        "repli sur `{fallback}` après : {}",
        failure.message
    ));
    match summarise(d, fb.as_ref(), &fallback, &job, at.turn_id).await {
        Ok((summary, cost)) => {
            report.cost_usd += cost;
            Ok((job, summary, fallback))
        }
        Err(f) => Err(Box::new((job, f))),
    }
}

/// Résumé fait sans modèle, quand le résumeur a échoué trop souvent : il ne prétend rien
/// comprendre. Les derniers messages du propriétaire du lot (dans le budget habituel) et
/// les ancres sont gardés tels quels par le rendu du nœud ; le résumé précédent est
/// repris.
pub(super) fn mechanical_summary(job: &SummaryJob) -> Value {
    let previous = job
        .previous_summary
        .as_deref()
        .map(penelope_context::compaction::summary_sections_only)
        .unwrap_or_default();
    json!({
        "objectif": "Compaction sans modèle : le résumeur n'a pas répondu à temps plusieurs \
                     fois de suite. Ce qui suit n'est pas un résumé : les derniers messages du \
                     propriétaire du passage compacté et ses ancres (chemins, identifiants) \
                     sont gardés tels quels, le reste se relit avec `history_expand`.",
        "fait": previous.chars().take(3_500).collect::<String>(),
        "en_cours": format!(
            "{} messages compactés sans être relus (séquences {} à {}) : les relire au besoin \
             avec `history_expand`.",
            job.messages(),
            job.chunk_from_seq,
            job.to_seq
        ),
        "prochaines_etapes": "",
    })
}

async fn summarise(
    d: &Context,
    provider: &dyn Provider,
    model: &str,
    job: &SummaryJob,
    turn_id: Option<&str>,
) -> Result<(Value, f64), SummaryFailure> {
    let s = &d.services;
    let info = s.catalog.get(strip_provider(model));
    let effort = info.as_ref().and_then(|i| i.lightest_effort());
    let structured = info
        .as_ref()
        .map(|i| i.supports_structured_output())
        .unwrap_or(false);
    let request = ChatRequest {
        model: model.to_string(),
        messages: job.summarizer_messages(),
        stream: true,
        // Neuf sections de 4 000 caractères au plus, plus le raisonnement éventuel.
        max_tokens: Some(if effort.as_deref() == Some("none") {
            8_000
        } else {
            16_000
        }),
        reasoning_effort: effort,
        response_format: structured.then(penelope_context::compaction::summary_response_format),
        session_id: Some(job.session_id.clone()),
        ..Default::default()
    };
    let failed = |e: penelope_llm::types::LlmError| SummaryFailure {
        passing: e.kind.is_retryable(),
        message: e.to_string(),
    };
    let call = async {
        let rx = provider
            .chat_stream(request, CancelToken::new())
            .await
            .map_err(failed)?;
        collect_stream(rx, model, provider.name(), &s.catalog)
            .await
            .map_err(failed)
    };
    let timeout = summary_timeout(job.tokens_src);
    let response = tokio::time::timeout(timeout, call).await.map_err(|_| {
        SummaryFailure::passing(format!(
            "le résumeur n'a pas répondu en {} s",
            timeout.as_secs()
        ))
    })??;
    let _ = s
        .budget
        .record(penelope_kernel::budget::UsageRecord {
            session_id: Some(job.session_id.clone()),
            turn_id: turn_id.map(String::from),
            model: response.model.clone(),
            provider: response.provider.clone(),
            role: Some("compaction".into()),
            generation_id: (!response.id.is_empty()).then(|| response.id.clone()),
            upstream: response.upstream.clone(),
            finish: Some(format!("{:?}", response.finish).to_lowercase()),
            prompt: response.usage.prompt,
            completion: response.usage.completion,
            cached: response.usage.cached,
            cache_write: response.usage.cache_write,
            reasoning: response.usage.reasoning,
            cost_usd: response.cost_usd,
            estimated: response.cost_estimated,
            ..Default::default()
        })
        .await;
    let summary = penelope_context::compaction::validate_summary(&response.message.text())
        .map_err(|message| SummaryFailure {
            message,
            passing: false,
        })?;
    Ok((summary, response.cost_usd))
}
