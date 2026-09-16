//! Trajectoires rejouables (§16) : `penelope export run <id>` produit un JSONL de
//! messages, appels, résultats, décisions et compactions, relu par `penelope eval replay`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrajectoryRecord {
    Meta {
        run_id: Option<String>,
        session_id: Option<String>,
        started_at: String,
        penelope_version: String,
    },
    Message {
        seq: i64,
        role: String,
        content: Value,
        ts: String,
    },
    LlmRequest {
        id: String,
        model: String,
        provider: String,
        body_hash: String,
        ts: String,
    },
    LlmResponse {
        id: String,
        prompt: u64,
        completion: u64,
        cached: u64,
        cost_usd: f64,
        finish: String,
        ts: String,
    },
    ToolCall {
        effect_id: String,
        name: String,
        args: Value,
        ts: String,
    },
    ToolResult {
        effect_id: String,
        ok: bool,
        result: Value,
        ts: String,
    },
    Approval {
        id: String,
        request_kind: String,
        decision: String,
        via: String,
        ts: String,
    },
    Compaction {
        level: u8,
        before_tokens: u64,
        after_tokens: u64,
        node_id: Option<String>,
        ts: String,
    },
    WorkflowStep {
        run_id: String,
        step_id: String,
        result: String,
        ts: String,
    },
    Error {
        message: String,
        ts: String,
    },
}

/// Sérialise une trajectoire en JSONL, avec redaction appliquée.
pub fn to_jsonl(records: &[TrajectoryRecord]) -> String {
    let mut out = String::new();
    for r in records {
        if let Ok(v) = serde_json::to_value(r) {
            let cleaned = crate::redact::redact_json(&v);
            if let Ok(s) = serde_json::to_string(&cleaned) {
                out.push_str(&s);
                out.push('\n');
            }
        }
    }
    out
}

/// Relit une trajectoire JSONL. Les lignes illisibles sont ignorées avec un
/// avertissement : un export tronqué reste partiellement rejouable.
pub fn from_jsonl(s: &str) -> Vec<TrajectoryRecord> {
    let mut out = Vec::new();
    for (i, line) in s.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TrajectoryRecord>(line) {
            Ok(r) => out.push(r),
            Err(e) => tracing::warn!(line = i + 1, error = %e, "ligne de trajectoire illisible"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_preserves_order() {
        let recs = vec![
            TrajectoryRecord::Meta {
                run_id: Some("r1".into()),
                session_id: Some("s1".into()),
                started_at: "2026-09-16T00:00:00Z".into(),
                penelope_version: "1.0.0".into(),
            },
            TrajectoryRecord::Message {
                seq: 1,
                role: "user".into(),
                content: json!("salut"),
                ts: "2026-09-16T00:00:01Z".into(),
            },
            TrajectoryRecord::ToolCall {
                effect_id: "e1".into(),
                name: "fs_read".into(),
                args: json!({"path":"a.rs"}),
                ts: "2026-09-16T00:00:02Z".into(),
            },
        ];
        let s = to_jsonl(&recs);
        assert_eq!(s.lines().count(), 3);
        let back = from_jsonl(&s);
        assert_eq!(back.len(), 3);
        assert!(matches!(back[2], TrajectoryRecord::ToolCall { .. }));
    }

    #[test]
    fn secrets_are_redacted_in_export() {
        let recs = vec![TrajectoryRecord::ToolCall {
            effect_id: "e1".into(),
            name: "http_fetch".into(),
            args: json!({"headers": {"Authorization": "Bearer abcdefghijklmnop1234"}}),
            ts: "t".into(),
        }];
        let s = to_jsonl(&recs);
        assert!(!s.contains("abcdefghijklmnop"), "{s}");
    }

    #[test]
    fn truncated_file_is_partially_readable() {
        let good = r#"{"kind":"error","message":"x","ts":"t"}"#;
        let s = format!("{good}\n{{ceci n'est pas du json\n{good}\n");
        assert_eq!(from_jsonl(&s).len(), 2);
    }
}
