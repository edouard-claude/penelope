//! Les bornes d'un tour : `turn.started` et `turn.finished` (§2.2, épopée #208, T4).
//!
//! Des événements d'observation, hors versionnage : le pliage ne les lit que pour
//! effacer la note de fusion et la tentative en cours. Ce module en fixe la forme ;
//! la boucle du daemon décide quand les écrire.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Pourquoi un tour s'est fermé. `Interrupted` (reprise après crash, §2.7) et `Forked`
/// ne sont jamais écrits par la boucle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnReason {
    Answered,
    AwaitingApproval,
    Cancelled,
    Failed,
    BudgetExceeded,
    LoopAborted,
    CallsExhausted,
    Interrupted,
    Forked,
}

impl TurnReason {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnReason::Answered => "answered",
            TurnReason::AwaitingApproval => "awaiting_approval",
            TurnReason::Cancelled => "cancelled",
            TurnReason::Failed => "failed",
            TurnReason::BudgetExceeded => "budget_exceeded",
            TurnReason::LoopAborted => "loop_aborted",
            TurnReason::CallsExhausted => "calls_exhausted",
            TurnReason::Interrupted => "interrupted",
            TurnReason::Forked => "forked",
        }
    }
}

/// Une sortie de tour et ce qu'elle dit d'elle-même.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    Answered {
        iterations: u32,
        cost_usd: f64,
    },
    AwaitingApproval {
        approval_id: String,
    },
    Cancelled,
    Failed {
        error: String,
    },
    CallsExhausted {
        error: String,
    },
    BudgetExceeded {
        scope: String,
        spent_usd: f64,
        limit_usd: f64,
    },
    LoopAborted {
        report: String,
    },
}

impl TurnEnd {
    pub fn reason(&self) -> TurnReason {
        match self {
            TurnEnd::Answered { .. } => TurnReason::Answered,
            TurnEnd::AwaitingApproval { .. } => TurnReason::AwaitingApproval,
            TurnEnd::Cancelled => TurnReason::Cancelled,
            TurnEnd::Failed { .. } => TurnReason::Failed,
            TurnEnd::CallsExhausted { .. } => TurnReason::CallsExhausted,
            TurnEnd::BudgetExceeded { .. } => TurnReason::BudgetExceeded,
            TurnEnd::LoopAborted { .. } => TurnReason::LoopAborted,
        }
    }
}

/// Identité d'un tour de la file (`turn_queue`) ; un sous-agent ou une étape de
/// workflow n'en ont pas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnIdentity {
    pub turn_id: String,
    /// `message`, `trigger`, `resume`, `nudge`.
    pub kind: String,
    /// Monte quand un tour est rejoué après crash.
    pub attempt: i64,
}

/// Ce que le modèle va lire, nommé à l'ouverture (#205).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnCall {
    pub model: String,
    pub system_hash: Option<String>,
    pub tools_hash: String,
}

/// Payload de `turn.started`. Sans appel (tour tombé avant sa boucle), seulement
/// l'identité.
pub fn started_payload(
    call: Option<&TurnCall>,
    origin_turn: Option<&str>,
    id: Option<&TurnIdentity>,
) -> Value {
    let mut p = match call {
        Some(c) => json!({
            "model": c.model,
            "system_hash": c.system_hash,
            "tools_hash": c.tools_hash,
        }),
        None => json!({}),
    };
    add_identity(&mut p, origin_turn, id);
    p
}

/// Payload de `turn.finished` : la raison, et ce que la sortie dit d'elle-même.
pub fn finished_payload(
    end: &TurnEnd,
    origin_turn: Option<&str>,
    id: Option<&TurnIdentity>,
) -> Value {
    let mut p = json!({"reason": end.reason().as_str()});
    match end {
        TurnEnd::Answered {
            iterations,
            cost_usd,
        } => {
            p["iterations"] = json!(iterations);
            p["cost_usd"] = json!(cost_usd);
        }
        TurnEnd::AwaitingApproval { approval_id } => p["approval_id"] = json!(approval_id),
        TurnEnd::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        } => {
            p["scope"] = json!(scope);
            p["spent_usd"] = json!(spent_usd);
            p["limit_usd"] = json!(limit_usd);
        }
        TurnEnd::LoopAborted { report } => p["report"] = json!(report),
        TurnEnd::Failed { error } | TurnEnd::CallsExhausted { error } => p["error"] = json!(error),
        TurnEnd::Cancelled => {}
    }
    add_identity(&mut p, origin_turn, id);
    p
}

/// `turn_id` (la ligne de la file), `origin_turn` (la requête du propriétaire, qu'une
/// reprise après approbation partage), `kind`, `attempt` : omis hors de la file.
fn add_identity(p: &mut Value, origin_turn: Option<&str>, id: Option<&TurnIdentity>) {
    if let Some(m) = id {
        p["turn_id"] = json!(m.turn_id);
        p["kind"] = json!(m.kind);
        p["attempt"] = json!(m.attempt);
    }
    if let Some(origin) = origin_turn {
        p["origin_turn"] = json!(origin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> TurnIdentity {
        TurnIdentity {
            turn_id: "q_1".into(),
            kind: "resume".into(),
            attempt: 2,
        }
    }

    #[test]
    fn the_reason_names_every_exit() {
        for (end, reason) in [
            (TurnEnd::Cancelled, "cancelled"),
            (TurnEnd::Failed { error: "x".into() }, "failed"),
            (
                TurnEnd::CallsExhausted { error: "x".into() },
                "calls_exhausted",
            ),
            (TurnEnd::LoopAborted { report: "r".into() }, "loop_aborted"),
        ] {
            assert_eq!(finished_payload(&end, None, None)["reason"], reason);
            assert_eq!(
                serde_json::to_value(end.reason()).unwrap(),
                json!(end.reason().as_str())
            );
        }
    }

    #[test]
    fn the_bounds_carry_the_identity_of_the_queued_turn() {
        let call = TurnCall {
            model: "m".into(),
            system_hash: None,
            tools_hash: "h".into(),
        };
        let started = started_payload(Some(&call), Some("q_0"), Some(&id()));
        assert_eq!(
            started,
            json!({"model": "m", "system_hash": null, "tools_hash": "h",
                   "turn_id": "q_1", "kind": "resume", "attempt": 2, "origin_turn": "q_0"})
        );
        let end = TurnEnd::Answered {
            iterations: 3,
            cost_usd: 0.5,
        };
        let finished = finished_payload(&end, Some("q_0"), Some(&id()));
        assert_eq!(finished["reason"], "answered");
        assert_eq!(finished["iterations"], 3);
        assert_eq!(finished["turn_id"], "q_1");
        // Hors de la file, ni identité ni origine.
        assert_eq!(started_payload(None, None, None), json!({}));
    }
}
