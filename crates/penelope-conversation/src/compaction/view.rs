use super::*;

/// Taille du contexte d'une session (issue #18) : prompt du dernier appel de conversation,
/// part en cache, seuils de compaction et fenêtre du modèle.
pub async fn context_view(
    s: &Services,
    session_id: &str,
    model_id: Option<&str>,
) -> anyhow::Result<Value> {
    use penelope_store::rusqlite::OptionalExtension;
    let sid = session_id.to_string();
    /// Prompt, cache, modèle, cause du raté, fournisseur amont.
    type LastCall = (i64, i64, String, Option<String>, Option<String>);
    let last: Option<LastCall> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT prompt, cached, model, miss_cause, upstream FROM usage
                 WHERE session_id = ?1 AND COALESCE(role, 'chat') = 'chat'
                 ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?)
        })
        .await?;
    let model = model_id
        .map(String::from)
        .or_else(|| last.as_ref().map(|l| l.2.clone()))
        .unwrap_or_default();
    let cfg = s.config.config();
    let params =
        CompactionParams::from_config(&cfg, s.catalog.window_of(strip_provider(&model)), &model);
    let sid = session_id.to_string();
    let last_compaction: Option<String> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT ts FROM events WHERE session_id = ?1 AND kind = 'context.compacted'
                 ORDER BY id DESC LIMIT 1",
                [sid],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten();
    // Une session qui ne se résume plus se voit, avec son coût par tour (issue #131).
    let key = cooldown_key(session_id);
    let cooldown: Cooldown = s
        .store
        .read(move |c| penelope_store::kv_get(c, &key))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let per_turn = if cooldown.failures > 0 {
        cost_per_turn(s, session_id).await
    } else {
        None
    };
    Ok(json!({
        "compaction_failures": cooldown.failures,
        "cost_per_turn_usd": per_turn,
        "last_compaction": last_compaction,
        "last_prompt_tokens": last.as_ref().map(|l| l.0),
        "last_cached_tokens": last.as_ref().map(|l| l.1),
        "last_upstream": last.as_ref().and_then(|l| l.4.clone()),
        "last_cache_miss": last.as_ref().and_then(|l| l.3.as_deref()).map(|m| {
            json!({"cause": m, "label": penelope_kernel::budget::miss_label(m)})
        }),
        "model": model,
        "window": params.window,
        "max_prompt_tokens": params.max_prompt_tokens,
        "background_compaction_at": params.background_threshold_tokens(params.background_margin),
        "compaction_at": params.threshold_tokens(),
    }))
}
