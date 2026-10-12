//! Suivre un run de loin (#346) : son état lisible d'un coup, sa destination, son
//! autonomie.
//!
//! Le 12/10, un point demandé « toutes les demi-heures » rendait « ?/53 » pendant que
//! la story 1.1 attendait depuis deux heures une approbation partie dans le fil général :
//! `workflow_status` rendait le run brut, sans liste ni attente.

use super::*;
use penelope_workflow::foreach::ItemState;

/// Le run et ses sous-runs encore actifs, lui en tête.
async fn family(s: &Services, root: &Run) -> anyhow::Result<Vec<Run>> {
    let all = s.runs.list(None, 500).await?;
    let mut out = vec![root.clone()];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].id.clone();
        out.extend(
            all.iter()
                .filter(|r| r.parent_run.as_deref() == Some(parent.as_str()))
                .filter(|r| !r.state.is_terminal())
                .cloned(),
        );
        i += 1;
    }
    Ok(out)
}

/// « 1 h 40 », « 12 min », « 40 s ».
fn since(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {:02}", s / 3600, (s % 3600) / 60),
    }
}

/// Ce qui retient le run ou un de ses sous-runs : approbation en attente, run en pause ou
/// bloqué, avec la durée. `None` : il avance.
async fn waiting(s: &Services, runs: &[Run]) -> anyhow::Result<Option<String>> {
    let now = s.clock.now_ms();
    for a in s.approvals.pending(500).await? {
        let Some(run) = a
            .run_id
            .as_deref()
            .and_then(|id| runs.iter().find(|r| r.id == id))
        else {
            continue;
        };
        let at = chrono::DateTime::parse_from_rfc3339(&a.created_at)
            .map(|t| t.timestamp_millis())
            .unwrap_or(now);
        return Ok(Some(format!(
            "bloqué : en attente d'approbation `{}` depuis {} (run {}, approbation {})",
            a.subject,
            since(now - at),
            run.id,
            a.id
        )));
    }
    for r in runs {
        if matches!(r.state, RunState::Paused | RunState::Blocked) {
            return Ok(Some(format!(
                "{} : {} (run {})",
                if r.state == RunState::Paused {
                    "en pause"
                } else {
                    "bloqué"
                },
                r.error.as_deref().unwrap_or("raison non dite"),
                r.id
            )));
        }
    }
    Ok(None)
}

/// L'état d'un run pour `workflow_status` : le run, sa consommation (`budget`), sa place
/// dans sa liste (`progress`), le décompte de la liste en cours (`list`), son autonomie
/// et ce qui le retient (`waiting`).
pub async fn run_status(s: &Services, run_id: &str) -> anyhow::Result<Value> {
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    let mut v = serde_json::to_value(&run)?;
    v["budget"] = json!(budget::budget_view(s, &run).await);
    v["progress"] = json!(foreach::position_of(s, &run).await);
    let items = foreach::items_of(s)
        .latest(&run.id)
        .await
        .unwrap_or_default();
    if !items.is_empty() {
        let mut list = foreach::summary(&items);
        list["todo"] = json!(
            items
                .iter()
                .filter(|i| matches!(i.state, ItemState::Todo | ItemState::Running))
                .count()
        );
        if let Some(all) = list.as_object_mut() {
            all.remove("items");
        }
        v["list"] = list;
    }
    v["autonomous"] = json!(penelope_app::autonomy::is_autonomous(s, &run.id).await);
    let runs = family(s, &run).await?;
    v["waiting"] = json!(waiting(s, &runs).await?);
    Ok(v)
}

/// Nouvelle destination d'un run et de ses sous-runs actifs (#346) : un run lancé par la
/// CLI ou la socket parle sinon dans le fil privé, sans sujet.
pub async fn retarget(s: &Services, run_id: &str, to: &Origin) -> anyhow::Result<Vec<String>> {
    let run = s
        .runs
        .get(run_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("run {run_id} introuvable"))?;
    let mut moved = Vec::new();
    for r in family(s, &run).await? {
        s.kv_set(&origin_key(&r.id), &to.to_value().to_string())
            .await?;
        moved.push(r.id);
    }
    Ok(moved)
}

/// Les approbations en attente des runs autonomes : celles posées avant que le run le
/// devienne, à trancher d'office.
pub async fn pending_of_autonomous(s: &Services) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for a in s.approvals.pending(500).await? {
        if let Some(run) = a.run_id.as_deref()
            && penelope_app::autonomy::is_autonomous(s, run).await
        {
            out.push(a.id.to_string());
        }
    }
    Ok(out)
}
