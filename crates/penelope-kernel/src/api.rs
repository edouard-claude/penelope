//! Contrat RPC entre la CLI et le daemon (§2.7, §15).
//!
//! JSON-RPC 2.0 sur socket locale, une requête par ligne (framing NDJSON). Les canaux
//! (`telegram`, `cli`) ne dépendent que de ce module, jamais de l'intérieur du daemon
//! (§3.1).

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const JSONRPC: &str = "2.0";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Jeton de session du daemon (`{state}/rpc.token`, issue #91) : sans lui, la socket
    /// refuse tout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
}

impl RpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        RpcRequest {
            jsonrpc: JSONRPC.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params: Some(params),
            auth: None,
        }
    }
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        RpcRequest {
            jsonrpc: JSONRPC.into(),
            id: None,
            method: method.into(),
            params: Some(params),
            auth: None,
        }
    }
    /// Joint le jeton de session du daemon.
    pub fn with_auth(mut self, token: Option<String>) -> Self {
        self.auth = token;
        self
    }
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        RpcResponse {
            jsonrpc: JSONRPC.into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        RpcResponse {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// Codes JSON-RPC standard.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
// Codes propres à Pénélope.
pub const NOT_FOUND: i32 = -32000;
pub const CONFLICT: i32 = -32001;
pub const DENIED: i32 = -32003;

/// Méthodes exposées par le daemon. La table de correspondance
/// Telegram ↔ CLI du CA 15 est dérivée de cette liste.
pub mod method {
    pub const STATUS: &str = "status";
    pub const DOCTOR: &str = "doctor";
    pub const SHUTDOWN: &str = "shutdown";
    pub const RESTART: &str = "restart";
    pub const PATHS: &str = "paths";

    pub const CHAT_SEND: &str = "chat.send";
    pub const CHAT_STREAM: &str = "chat.stream";
    pub const CHAT_STOP: &str = "chat.stop";

    pub const SESSION_LIST: &str = "session.list";
    pub const SESSION_NEW: &str = "session.new";
    pub const SESSION_SWITCH: &str = "session.switch";
    pub const SESSION_CLOSE: &str = "session.close";
    pub const SESSION_TITLE: &str = "session.title";
    pub const SESSION_FORK: &str = "session.fork";
    pub const SESSION_REWIND: &str = "session.rewind";
    pub const SESSION_COMPACT: &str = "session.compact";
    pub const SESSION_EXPORT: &str = "session.export";
    /// Purge RGPD : efface le contenu d'une session, garde la chaîne d'audit (issue #46).
    pub const SESSION_PURGE: &str = "session.purge";
    /// Modèle d'une session : lecture, épinglage d'un alias, retour à l'automatique.
    pub const SESSION_MODEL: &str = "session.model";
    pub const SESSION_BUDGET: &str = "session.budget";

    pub const CONFIG_GET: &str = "config.get";
    pub const CONFIG_SET: &str = "config.set";
    pub const CONFIG_STATUS: &str = "config.status";
    pub const CONFIG_RELOAD: &str = "config.reload";

    pub const SECRET_SET: &str = "secret.set";
    pub const SECRET_LIST: &str = "secret.list";
    pub const SECRET_RM: &str = "secret.rm";
    pub const SECRET_BACKEND: &str = "secret.backend";

    pub const MODEL_LIST: &str = "model.list";
    pub const MODEL_SET: &str = "model.set";
    pub const MODEL_ROUTE_TEST: &str = "model.route_test";

    pub const MCP_LIST: &str = "mcp.list";
    pub const MCP_SHOW: &str = "mcp.show";
    pub const MCP_ADD: &str = "mcp.add";
    pub const MCP_EDIT: &str = "mcp.edit";
    pub const MCP_RM: &str = "mcp.rm";
    pub const MCP_ENABLE: &str = "mcp.enable";
    pub const MCP_DISABLE: &str = "mcp.disable";
    pub const MCP_RESTART: &str = "mcp.restart";
    pub const MCP_TEST: &str = "mcp.test";
    pub const MCP_AUTH: &str = "mcp.auth";
    pub const MCP_LOGS: &str = "mcp.logs";

    pub const SKILL_LIST: &str = "skill.list";
    pub const SKILL_SHOW: &str = "skill.show";
    pub const SKILL_ROLLBACK: &str = "skill.rollback";
    /// Relit les dossiers de skills tout de suite (issue #63).
    pub const SKILL_RELOAD: &str = "skill.reload";

    pub const WF_LIST: &str = "wf.list";
    pub const WF_SHOW: &str = "wf.show";
    pub const WF_VALIDATE: &str = "wf.validate";
    pub const WF_RUN: &str = "wf.run";
    pub const WF_RUNS: &str = "wf.runs";
    pub const WF_TRACE: &str = "wf.trace";
    pub const WF_CONTROL: &str = "wf.control";

    pub const SCHEDULE_LIST: &str = "schedule.list";
    pub const SCHEDULE_ADD: &str = "schedule.add";
    pub const SCHEDULE_RM: &str = "schedule.rm";
    pub const SCHEDULE_PAUSE: &str = "schedule.pause";
    pub const SCHEDULE_RESUME: &str = "schedule.resume";
    pub const SCHEDULE_RUN_NOW: &str = "schedule.run_now";

    pub const APPROVALS: &str = "approvals";
    pub const APPROVE: &str = "approve";
    pub const DENY: &str = "deny";
    pub const POLICIES: &str = "policies";
    pub const POLICY_REVOKE: &str = "policy.revoke";
    pub const QUIET: &str = "quiet";

    pub const MEM_SEARCH: &str = "mem.search";
    pub const MEM_SHOW: &str = "mem.show";
    pub const MEM_HISTORY: &str = "mem.history";
    pub const MEM_RESTORE: &str = "mem.restore";
    pub const MEM_REINDEX: &str = "mem.reindex";
    pub const MEM_FORGET: &str = "mem.forget";
    pub const MEM_CANDIDATES: &str = "mem.candidates";
    pub const MEM_DREAM: &str = "mem.dream";
    pub const MEM_LEARNED: &str = "mem.learned";
    /// Audit noté sur 100, historisé (issue #23).
    pub const MEM_AUDIT: &str = "mem.audit";
    /// Règles rejetées pour leur seule origine, remises à consolider (issue #24).
    pub const MEM_RETRY_REJECTED: &str = "mem.retry_rejected";
    pub const MEM_DIFF: &str = "mem.diff";
    pub const INTENT_LIST: &str = "intent.list";
    pub const INTENT_CANCEL: &str = "intent.cancel";
    pub const VAULT_SYNC: &str = "vault.sync";
    pub const VAULT_CHECK: &str = "vault.check";
    pub const VAULT_LINT: &str = "vault.lint";
    /// Entretien d'accueil (issue #21) : question suivante, réponse, écriture validée.
    pub const ONBOARD_NEXT: &str = "onboard.next";
    pub const ONBOARD_ANSWER: &str = "onboard.answer";
    pub const ONBOARD_WRITE: &str = "onboard.write";

    pub const IMPORT_HERMES: &str = "import.hermes";
    pub const EXPORT: &str = "export";
    pub const BACKUP: &str = "backup";
    pub const RESTORE: &str = "restore";
    pub const AUDIT_VERIFY: &str = "audit.verify";
    pub const STORE_REBUILD: &str = "store.rebuild";
    pub const USAGE: &str = "usage";
    pub const TAIL: &str = "tail";
    pub const EVAL_RUN: &str = "eval.run";
    pub const UPGRADE: &str = "upgrade";

    /// Toutes les méthodes, pour le test de couverture Telegram ↔ CLI (CA 15).
    pub const ALL: &[&str] = &[
        STATUS,
        DOCTOR,
        SHUTDOWN,
        RESTART,
        PATHS,
        CHAT_SEND,
        CHAT_STREAM,
        CHAT_STOP,
        SESSION_LIST,
        SESSION_NEW,
        SESSION_SWITCH,
        SESSION_CLOSE,
        SESSION_TITLE,
        SESSION_FORK,
        SESSION_REWIND,
        SESSION_COMPACT,
        SESSION_EXPORT,
        SESSION_PURGE,
        SESSION_MODEL,
        SESSION_BUDGET,
        CONFIG_GET,
        CONFIG_SET,
        CONFIG_STATUS,
        CONFIG_RELOAD,
        SECRET_SET,
        SECRET_LIST,
        SECRET_RM,
        SECRET_BACKEND,
        MODEL_LIST,
        MODEL_SET,
        MODEL_ROUTE_TEST,
        MCP_LIST,
        MCP_SHOW,
        MCP_ADD,
        MCP_EDIT,
        MCP_RM,
        MCP_ENABLE,
        MCP_DISABLE,
        MCP_RESTART,
        MCP_TEST,
        MCP_AUTH,
        MCP_LOGS,
        SKILL_LIST,
        SKILL_SHOW,
        SKILL_ROLLBACK,
        SKILL_RELOAD,
        WF_LIST,
        WF_SHOW,
        WF_VALIDATE,
        WF_RUN,
        WF_RUNS,
        WF_TRACE,
        WF_CONTROL,
        SCHEDULE_LIST,
        SCHEDULE_ADD,
        SCHEDULE_RM,
        SCHEDULE_PAUSE,
        SCHEDULE_RESUME,
        SCHEDULE_RUN_NOW,
        APPROVALS,
        APPROVE,
        DENY,
        POLICIES,
        POLICY_REVOKE,
        QUIET,
        MEM_SEARCH,
        MEM_SHOW,
        MEM_HISTORY,
        MEM_RESTORE,
        MEM_REINDEX,
        MEM_FORGET,
        MEM_CANDIDATES,
        MEM_DREAM,
        MEM_LEARNED,
        MEM_AUDIT,
        MEM_RETRY_REJECTED,
        MEM_DIFF,
        INTENT_LIST,
        INTENT_CANCEL,
        VAULT_SYNC,
        VAULT_CHECK,
        VAULT_LINT,
        ONBOARD_NEXT,
        ONBOARD_ANSWER,
        ONBOARD_WRITE,
        IMPORT_HERMES,
        EXPORT,
        BACKUP,
        RESTORE,
        AUDIT_VERIFY,
        STORE_REBUILD,
        USAGE,
        TAIL,
        EVAL_RUN,
        UPGRADE,
    ];
}

/// Événement poussé par le daemon vers un client abonné (`penelope tail`, `chat`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Delta {
        session_id: String,
        text: String,
    },
    Reasoning {
        session_id: String,
        text: String,
    },
    ToolCall {
        session_id: String,
        name: String,
        args: Value,
    },
    ToolResult {
        session_id: String,
        name: String,
        ok: bool,
        preview: String,
    },
    Approval {
        id: String,
        kind: String,
        subject: String,
        risk: String,
    },
    Usage {
        session_id: String,
        prompt: u64,
        completion: u64,
        cost_usd: f64,
    },
    Done {
        session_id: String,
        text: String,
    },
    Error {
        session_id: Option<String>,
        message: String,
    },
    Log {
        level: String,
        target: String,
        message: String,
    },
    RunUpdate {
        run_id: String,
        step: String,
        state: String,
    },
}

/// Réponse de `status`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatusReport {
    pub version: String,
    pub uptime_s: u64,
    pub config_generation: u64,
    pub sessions_active: u64,
    pub runs_active: u64,
    pub approvals_pending: u64,
    pub mcp_ready: u64,
    pub mcp_total: u64,
    pub turns_queued: u64,
    pub rss_mb: f64,
    pub spent_today_usd: f64,
    pub telegram: String,
    /// Runners vivants sur `runners.count` configurés (issue #84).
    #[serde(default)]
    pub runners_alive: u64,
    #[serde(default)]
    pub runners_expected: u64,
}

/// Une ligne du rapport `doctor` (§2.11).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub label: String,
    pub ok: bool,
    pub detail: String,
    /// Commande corrective **proposée, jamais exécutée automatiquement**.
    pub fix: Option<String>,
    pub severity: String,
}

impl DoctorCheck {
    pub fn ok(id: &str, label: &str, detail: impl Into<String>) -> Self {
        DoctorCheck {
            id: id.into(),
            label: label.into(),
            ok: true,
            detail: detail.into(),
            fix: None,
            severity: "info".into(),
        }
    }
    pub fn fail(id: &str, label: &str, detail: impl Into<String>, fix: Option<String>) -> Self {
        DoctorCheck {
            id: id.into(),
            label: label.into(),
            ok: false,
            detail: detail.into(),
            fix,
            severity: "warn".into(),
        }
    }
    pub fn critical(mut self) -> Self {
        self.severity = "error".into();
        self
    }
}

/// Codes de sortie documentés de la CLI (§15).
pub mod exit_code {
    pub const OK: i32 = 0;
    pub const USAGE: i32 = 2;
    pub const DAEMON_UNREACHABLE: i32 = 3;
    pub const VALIDATION_FAILED: i32 = 4;
    pub const DENIED: i32 = 5;
    pub const NOT_FOUND: i32 = 6;
    /// Le daemon accepte la connexion mais ne répond pas dans le délai (issue #99).
    pub const DAEMON_UNRESPONSIVE: i32 = 7;
    pub const INTERNAL: i32 = 70;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serialises_as_jsonrpc() {
        let r = RpcRequest::new(1, method::STATUS, json!({}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        let back: RpcRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.method, "status");
        assert!(!back.is_notification());
    }

    #[test]
    fn notification_has_no_id() {
        let n = RpcRequest::notification("tail", json!({}));
        assert!(n.is_notification());
        let s = serde_json::to_string(&n).unwrap();
        assert!(s.contains("\"id\":null"));
    }

    #[test]
    fn stream_events_roundtrip() {
        let e = StreamEvent::Delta {
            session_id: "s".into(),
            text: "bonjour".into(),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains("\"type\":\"delta\""));
        let _: StreamEvent = serde_json::from_str(&s).unwrap();
    }

    #[test]
    fn method_list_has_no_duplicates() {
        let mut v = method::ALL.to_vec();
        let n = v.len();
        v.sort_unstable();
        v.dedup();
        assert_eq!(v.len(), n, "doublon dans method::ALL");
    }
}
