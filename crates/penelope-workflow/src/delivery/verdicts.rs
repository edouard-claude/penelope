//! Verdicts de la livraison, lus dans ce que rendent le forgeur et l'environnement de dev.
//!
//! La CI et l'E2E sont deux résultats distincts : une CI verte ne dit rien de
//! l'environnement déployé, un E2E vert ne dit rien des tests du projet.

use super::config::{Check, CheckKind};
use serde_json::Value;

/// Où en est la CI d'un commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiVerdict {
    /// Pas encore de verdict : contrôles en file ou en cours, ou pas encore lancés.
    Pending(String),
    Green(String),
    Red(String),
}

/// Conclusions GitHub qui font échouer un contrôle.
const GITHUB_RED: &[&str] = &[
    "failure",
    "cancelled",
    "timed_out",
    "action_required",
    "startup_failure",
    "stale",
];

/// GitHub : les check runs du commit (`/commits/{sha}/check-runs`) et ses statuts
/// combinés (`/commits/{sha}/status`), les deux façons dont une CI y rend compte.
pub fn github_ci(check_runs: &Value, status: &Value) -> CiVerdict {
    let runs = check_runs["check_runs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let statuses = status["statuses"].as_array().cloned().unwrap_or_default();
    let name = |v: &Value, key: &str| v[key].as_str().unwrap_or("?").to_string();
    let red: Vec<String> = runs
        .iter()
        .filter(|r| {
            r["status"] == "completed"
                && GITHUB_RED.contains(&r["conclusion"].as_str().unwrap_or_default())
        })
        .map(|r| format!("{} ({})", name(r, "name"), name(r, "conclusion")))
        .chain(
            statuses
                .iter()
                .filter(|s| matches!(s["state"].as_str(), Some("failure" | "error")))
                .map(|s| format!("{} ({})", name(s, "context"), name(s, "state"))),
        )
        .collect();
    if !red.is_empty() {
        return CiVerdict::Red(format!("en échec : {}", red.join(", ")));
    }
    let waiting = runs.iter().filter(|r| r["status"] != "completed").count()
        + statuses.iter().filter(|s| s["state"] == "pending").count();
    let total = runs.len() + statuses.len();
    if total == 0 {
        return CiVerdict::Pending("aucun contrôle lancé pour l'instant".into());
    }
    if waiting > 0 {
        return CiVerdict::Pending(format!("{waiting} contrôle(s) sur {total} en cours"));
    }
    CiVerdict::Green(format!("{total} contrôle(s) au vert"))
}

/// GitLab : les pipelines du commit (`/pipelines?sha=`), le plus récent d'abord.
pub fn gitlab_ci(pipelines: &Value) -> CiVerdict {
    let Some(last) = pipelines
        .as_array()
        .and_then(|a| a.iter().max_by_key(|p| p["id"].as_i64().unwrap_or(0)))
    else {
        return CiVerdict::Pending("aucun pipeline lancé pour l'instant".into());
    };
    let id = last["id"].as_i64().unwrap_or(0);
    match last["status"].as_str().unwrap_or_default() {
        "success" => CiVerdict::Green(format!("pipeline {id} au vert")),
        s @ ("failed" | "canceled" | "skipped") => CiVerdict::Red(format!("pipeline {id} : {s}")),
        "manual" => CiVerdict::Pending(format!("pipeline {id} : attend une action manuelle")),
        s => CiVerdict::Pending(format!("pipeline {id} : {s}")),
    }
}

/// Ce que l'environnement de dev a répondu à un contrôle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub status: u16,
    pub body: String,
}

/// Juge une réponse à un contrôle `http` ou `graphql` : `Ok` dit ce qui est prouvé,
/// `Err` ce qui manque.
pub fn judge(check: &Check, seen: &Observed) -> Result<String, String> {
    let status_ok = match check.status {
        Some(expected) => seen.status == expected,
        None => (200..300).contains(&seen.status),
    };
    if !status_ok {
        let expected = check
            .status
            .map_or_else(|| "2xx".to_string(), |s| s.to_string());
        return Err(format!("statut {} (attendu {expected})", seen.status));
    }
    if check.kind == CheckKind::Graphql {
        let v: Value = serde_json::from_str(&seen.body)
            .map_err(|_| "réponse GraphQL qui n'est pas du JSON".to_string())?;
        if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let first = errors[0]["message"].as_str().unwrap_or("?");
            return Err(format!("{} erreur(s) GraphQL : {first}", errors.len()));
        }
        if v["data"].is_null() {
            return Err("réponse GraphQL sans `data`".into());
        }
    }
    if let Some(needle) = check.contains.as_deref().filter(|n| !n.is_empty())
        && !seen.body.contains(needle)
    {
        return Err(format!("la réponse ne contient pas « {needle} »"));
    }
    Ok(format!("statut {}", seen.status))
}

/// Attente avant de relire la CI : 15 s, doublée à chaque lecture sans verdict, au plus
/// 5 min. Une CI de vingt minutes coûte une vingtaine de lectures, pas trois cents.
pub fn ci_backoff_ms(polls: u32) -> u64 {
    (15_000u64 << polls.min(5)).min(300_000)
}

/// Lectures de la CI qui peuvent échouer d'affilée (réseau, 5xx) avant que la livraison
/// ne s'arrête sur « CI indisponible ».
pub const CI_UNAVAILABLE_LIMIT: u32 = 3;
