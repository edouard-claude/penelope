//! Budget d'un run : dépense relevée au ledger, plafonds effectifs, bornes et affichage.

use super::*;
use penelope_workflow::model::Budget;

/// Coût du run d'après le ledger d'usage, pour les bornes de budget. Les tokens sont
/// ceux **facturés** : l'entrée hors cache plus la sortie. Un préfixe servi par le cache
/// (décision 0008, #40) est l'économie voulue, pas une dépense (issue #136) : il est
/// compté à part, avec son plafond optionnel (#337).
pub(super) async fn refresh_spent(s: &Services, mut run: Run) -> anyhow::Result<Run> {
    let id = run.id.clone();
    let (usd, tokens, cached): (f64, i64, i64) = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(cost_usd), 0),
                        COALESCE(SUM(MAX(prompt - cached, 0) + completion), 0),
                        COALESCE(SUM(MIN(cached, prompt)), 0)
                 FROM usage WHERE run_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await?;
    if (usd - run.spent_usd).abs() > f64::EPSILON
        || tokens as u64 != run.spent_tokens
        || cached as u64 != run.spent_cached_tokens
    {
        s.runs
            .set_spent(&run.id, usd, tokens as u64, cached as u64)
            .await?;
        run.spent_usd = usd;
        run.spent_tokens = tokens as u64;
        run.spent_cached_tokens = cached as u64;
    }
    Ok(run)
}

fn budget_key(run_id: &str) -> String {
    format!("run.budget.{run_id}")
}

/// Plafonds d'un run : ceux du workflow, relevés au besoin pour ce run seul par
/// `wf control <run> budget` (issue #136).
pub async fn effective_budget(s: &Services, run: &Run, declared: &Budget) -> Budget {
    let mut b = *declared;
    if let Ok(Some(raw)) = s.kv_get(&budget_key(&run.id)).await
        && let Ok(v) = serde_json::from_str::<Value>(&raw)
    {
        if let Some(usd) = v["max_usd"].as_f64() {
            b.max_usd = usd;
        }
        if let Some(tokens) = v["max_tokens"].as_u64() {
            b.max_tokens = tokens;
        }
        if let Some(cached) = v["max_cached_tokens"].as_u64() {
            b.max_cached_tokens = cached;
        }
        if let Some(wall) = v["max_wall_ms"].as_u64() {
            b.max_wall_ms = wall;
        }
    }
    b
}

/// Plafonds propres à un run, posés à son départ : `itemBudget` d'un élément de
/// `foreach` (#338). Seuls les plafonds non nuls remplacent ceux du workflow.
pub(super) async fn set_caps(s: &Services, run_id: &str, b: &Budget) -> anyhow::Result<()> {
    let mut caps = serde_json::Map::new();
    if b.max_usd > 0.0 {
        caps.insert("max_usd".into(), json!(b.max_usd));
    }
    for (k, v) in [
        ("max_tokens", b.max_tokens),
        ("max_cached_tokens", b.max_cached_tokens),
        ("max_wall_ms", b.max_wall_ms),
    ] {
        if v > 0 {
            caps.insert(k.into(), json!(v));
        }
    }
    s.kv_set(&budget_key(run_id), &Value::Object(caps).to_string())
        .await
}

/// Borne atteinte, avec ses chiffres et la commande qui la relève (issue #136, #337).
pub(super) fn limit_reason(limit: &Limit, run: &Run, b: &Budget) -> String {
    let raise = |what: &str| format!("`penelope wf control {} budget {what}`", run.id);
    match limit {
        Limit::IterationsExhausted => format!(
            "itérations épuisées ({} sur {}) : {}",
            run.iterations,
            run.max_iterations,
            raise("--iterations <nombre>")
        ),
        Limit::BudgetUsd => format!(
            "budget de {:.2} $ atteint ({:.2} $ dépensés) : {}",
            b.max_usd,
            run.spent_usd,
            raise("--usd <montant>")
        ),
        Limit::BudgetTokens => format!(
            "budget de tokens atteint ({} tokens facturés sur {}) : {}",
            run.spent_tokens,
            b.max_tokens,
            raise("--tokens <nombre>")
        ),
        Limit::BudgetCachedTokens => format!(
            "budget de tokens servis par le cache atteint ({} sur {}) : {}",
            run.spent_cached_tokens,
            b.max_cached_tokens,
            raise("--cached-tokens <nombre>")
        ),
        Limit::WallClock => format!(
            "durée maximale atteinte ({} min de travail, arrêts et attentes exclus) : {}",
            b.max_wall_ms / 60_000,
            raise("--minutes <durée>")
        ),
        Limit::Ok => String::new(),
    }
}

/// Bornes d'un run maintenant : dépense relue, plafonds effectifs, attente du
/// propriétaire retirée de la durée.
pub(super) async fn limits_now(
    s: &Services,
    run: Run,
    wf: &Workflow,
) -> anyhow::Result<(Run, Budget, Limit)> {
    let run = refresh_spent(s, run).await?;
    let budget = effective_budget(s, &run, &wf.settings.budget).await;
    let now = s.clock.now_ms() - owner_wait_ms(s, &run, wf).await;
    let limit = check_limits(&run, &budget, now);
    Ok((run, budget, limit))
}

/// Relève les plafonds d'un run, pour lui seul et avec trace (issue #136) : l'équivalent
/// de `session budget` pour un run. Un run bloqué par la borne relevée redevient
/// reprenable ; la reprise repart de l'étape courante, sans rejouer les effets faits.
/// `p` porte `usd`, `tokens`, `cached_tokens`, `minutes` et `iterations` (#337).
pub async fn raise_budget(d: &Context, run_id: &str, p: &Value) -> anyhow::Result<Value> {
    let s = &d.services;
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    let usd = p.get("usd").and_then(Value::as_f64);
    let tokens = p.get("tokens").and_then(Value::as_u64);
    let cached = p.get("cached_tokens").and_then(Value::as_u64);
    let minutes = p.get("minutes").and_then(Value::as_u64);
    let iterations = p.get("iterations").and_then(Value::as_u64);
    if [usd.map(|_| 0u64), tokens, cached, minutes, iterations]
        .iter()
        .all(Option::is_none)
    {
        anyhow::bail!(
            "rien à relever : `--usd <montant>`, `--tokens <nombre>`, `--cached-tokens \
             <nombre>`, `--minutes <durée>` ou `--iterations <nombre>`"
        );
    }
    let wf = workflow_of(s, &run)
        .await
        .ok_or_else(|| anyhow::anyhow!("workflow retiré du registre"))?;
    let mut b = effective_budget(s, &run, &wf.settings.budget).await;
    if let Some(u) = usd {
        b.max_usd = u;
    }
    if let Some(t) = tokens {
        b.max_tokens = t;
    }
    if let Some(c) = cached {
        b.max_cached_tokens = c;
    }
    if let Some(m) = minutes {
        b.max_wall_ms = m.saturating_mul(60_000);
    }
    if let Some(n) = iterations {
        s.runs.set_max_iterations(run_id, n as u32).await?;
    }
    let caps = json!({
        "max_usd": b.max_usd,
        "max_tokens": b.max_tokens,
        "max_cached_tokens": b.max_cached_tokens,
        "max_wall_ms": b.max_wall_ms,
    });
    s.kv_set(&budget_key(run_id), &caps.to_string()).await?;
    let mut traced = caps.clone();
    traced["run"] = json!(run_id);
    traced["max_iterations"] = json!(iterations);
    let _ = s
        .events
        .append(EventDraft::new("workflow.budget_raised", traced).session(&run.session_id))
        .await;
    let run = s.runs.get(run_id).await?.unwrap_or(run);
    let (run, b, limit) = limits_now(s, run, &wf).await?;
    Ok(json!({
        "run": run_id,
        "max_usd": b.max_usd,
        "max_tokens": b.max_tokens,
        "max_cached_tokens": b.max_cached_tokens,
        "max_wall_ms": b.max_wall_ms,
        "max_iterations": run.max_iterations,
        "spent_usd": run.spent_usd,
        "spent_tokens": run.spent_tokens,
        "spent_cached_tokens": run.spent_cached_tokens,
        "still_blocked": (limit != Limit::Ok).then(|| limit_reason(&limit, &run, &b)),
    }))
}

/// Consommation d'un run face à ses plafonds, en une ligne (#337) ; vide si son workflow
/// a quitté le registre.
pub async fn budget_view(s: &Services, run: &Run) -> String {
    let Some(wf) = workflow_of(s, run).await else {
        return String::new();
    };
    let budget = effective_budget(s, run, &wf.settings.budget).await;
    let waited = owner_wait_ms(s, run, &wf).await;
    penelope_workflow::runs::budget_line(run, &budget, s.clock.now_ms(), waited)
}

/// Les runs récents pour `penelope wf runs` : chaque ligne porte sa consommation
/// (`budget`) face à ses plafonds (#337) et sa place dans sa liste (`progress`, #338).
pub async fn runs_listing(s: &Services, limit: i64) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for run in s.runs.list(None, limit).await? {
        let line = budget_view(s, &run).await;
        let position = super::foreach::position_of(s, &run).await;
        let mut v = serde_json::to_value(&run)?;
        v["budget"] = json!(line);
        v["progress"] = json!(position);
        out.push(v);
    }
    Ok(Value::Array(out))
}
