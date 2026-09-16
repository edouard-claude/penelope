//! Protocole MCP : enveloppes JSON-RPC, versions, capacités, codes d'erreur (§8.1, §8.4).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ------------------------------------------------------------------ versions

/// Versions du protocole supportées, de la plus ancienne à la préférée (§8.1).
pub const VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    "2025-11-25",
    "2026-07-28",
];

pub const PREFERRED: &str = "2026-07-28";

/// Version du protocole négociée avec un serveur.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash, Default,
)]
pub enum ProtocolVersion {
    V20241105,
    V20250326,
    V20250618,
    V20251125,
    #[default]
    V20260728,
}

impl ProtocolVersion {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProtocolVersion::V20241105 => "2024-11-05",
            ProtocolVersion::V20250326 => "2025-03-26",
            ProtocolVersion::V20250618 => "2025-06-18",
            ProtocolVersion::V20251125 => "2025-11-25",
            ProtocolVersion::V20260728 => "2026-07-28",
        }
    }

    pub fn parse(s: &str) -> Option<ProtocolVersion> {
        Some(match s {
            "2024-11-05" => ProtocolVersion::V20241105,
            "2025-03-26" => ProtocolVersion::V20250326,
            "2025-06-18" => ProtocolVersion::V20250618,
            "2025-11-25" => ProtocolVersion::V20251125,
            "2026-07-28" => ProtocolVersion::V20260728,
            _ => return None,
        })
    }

    /// Cœur sans état, `server/discover`, `subscriptions/listen`, MRTR.
    pub fn is_stateless_core(&self) -> bool {
        *self >= ProtocolVersion::V20260728
    }
    /// Sortie structurée, elicitation, liens de ressources, RFC 8707.
    pub fn has_structured_output(&self) -> bool {
        *self >= ProtocolVersion::V20250618
    }
    pub fn has_elicitation(&self) -> bool {
        *self >= ProtocolVersion::V20250618
    }
    /// Icônes, elicitation URL, sampling avec outils, tâches expérimentales.
    pub fn has_icons(&self) -> bool {
        *self >= ProtocolVersion::V20251125
    }
    pub fn has_tasks(&self) -> bool {
        *self >= ProtocolVersion::V20251125
    }
    /// Streamable HTTP et OAuth 2.1.
    pub fn has_streamable_http(&self) -> bool {
        *self >= ProtocolVersion::V20250326
    }
    /// Le batching JSON-RPC a été retiré en 2025-06-18.
    pub fn allows_batching(&self) -> bool {
        *self < ProtocolVersion::V20250618
    }
    /// `logging/setLevel` (ancien) contre `_meta.logLevel` (2026-07-28).
    pub fn logging_via_meta(&self) -> bool {
        self.is_stateless_core()
    }
    /// `Mcp-Session-Id` et reprise `Last-Event-ID` : versions antérieures uniquement.
    pub fn has_session_resume(&self) -> bool {
        self.has_streamable_http() && !self.is_stateless_core()
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ------------------------------------------------------------------ erreurs

/// Codes d'erreur JSON-RPC standard.
pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// Codes MCP historiques.
pub const RESOURCE_NOT_FOUND_LEGACY: i32 = -32002;

/// Codes 2026-07-28 (§8.4).
pub const HEADER_MISMATCH: i32 = -32020;
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i32 = -32021;
pub const UNSUPPORTED_PROTOCOL_VERSION: i32 = -32022;

/// Vrai si le code signale une ressource introuvable, dans l'une ou l'autre version.
pub fn is_resource_not_found(code: i32) -> bool {
    code == RESOURCE_NOT_FOUND_LEGACY || code == INVALID_PARAMS
}

// ------------------------------------------------------------------ enveloppes

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Request {
            jsonrpc: "2.0".into(),
            id: Value::from(id),
            method: method.into(),
            params: Some(params),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Notification {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: Some(params),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Message entrant : réponse, notification ou requête du serveur (sampling, elicitation,
/// roots).
#[derive(Debug, Clone)]
pub enum Incoming {
    Response(Response),
    Notification(Notification),
    ServerRequest(Request),
}

/// Décode une ligne JSON-RPC.
pub fn decode(line: &str) -> Option<Incoming> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method").is_some() {
        if v.get("id").is_some() {
            return serde_json::from_value::<Request>(v)
                .ok()
                .map(Incoming::ServerRequest);
        }
        return serde_json::from_value::<Notification>(v)
            .ok()
            .map(Incoming::Notification);
    }
    serde_json::from_value::<Response>(v)
        .ok()
        .map(Incoming::Response)
}

// ------------------------------------------------------------------ capacités

/// Ce que Pénélope annonce au serveur.
pub fn client_capabilities(version: ProtocolVersion) -> Value {
    let mut caps = json!({
        "roots": {"listChanged": true},
        "sampling": {}
    });
    if version.has_elicitation() {
        caps["elicitation"] = json!({});
    }
    if version.has_tasks() {
        caps["experimental"] = json!({"io.modelcontextprotocol/tasks": {}});
    }
    caps
}

pub fn client_info() -> Value {
    json!({
        "name": "penelope",
        "title": "Pénélope",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Capacités annoncées par le serveur, telles que Pénélope les exploite.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerCapabilities {
    pub tools: bool,
    pub tools_list_changed: bool,
    pub resources: bool,
    pub resources_list_changed: bool,
    pub resources_subscribe: bool,
    pub prompts: bool,
    pub prompts_list_changed: bool,
    pub logging: bool,
    pub completions: bool,
    pub tasks: bool,
    /// Champ `extensions` lu et stocké tel quel (§8.4).
    pub extensions: Value,
    pub raw: Value,
}

impl ServerCapabilities {
    pub fn parse(v: &Value) -> Self {
        let b = |path: &[&str]| -> bool {
            let mut cur = v;
            for p in path {
                match cur.get(p) {
                    Some(next) => cur = next,
                    None => return false,
                }
            }
            !cur.is_null()
        };
        let flag = |path: &[&str]| -> bool {
            let mut cur = v;
            for p in path {
                match cur.get(p) {
                    Some(next) => cur = next,
                    None => return false,
                }
            }
            cur.as_bool().unwrap_or(false)
        };
        ServerCapabilities {
            tools: b(&["tools"]),
            tools_list_changed: flag(&["tools", "listChanged"]),
            resources: b(&["resources"]),
            resources_list_changed: flag(&["resources", "listChanged"]),
            resources_subscribe: flag(&["resources", "subscribe"]),
            prompts: b(&["prompts"]),
            prompts_list_changed: flag(&["prompts", "listChanged"]),
            logging: b(&["logging"]),
            completions: b(&["completions"]),
            tasks: b(&["experimental", "io.modelcontextprotocol/tasks"])
                || b(&["extensions", "io.modelcontextprotocol/tasks"]),
            extensions: v.get("extensions").cloned().unwrap_or(Value::Null),
            raw: v.clone(),
        }
    }
}

// ------------------------------------------------------------------ résultats

/// Résultat d'un `tools/call` (§8.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolResult {
    pub content: Vec<ContentBlock>,
    /// `structuredContent`, validé contre `outputSchema` quand il existe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// Erreur d'exécution : **renvoyée au modèle** pour auto-correction.
    #[serde(default)]
    pub is_error: bool,
    /// `CacheableResult` : durée de vie et portée.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_scope: Option<String>,
    /// MRTR : le serveur demande une saisie avant de pouvoir répondre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_requests: Option<Value>,
    /// `complete` | `input_required` | `incomplete` ; absent vaut `complete` (§8.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    /// Référence de tâche quand l'appel devient une tâche longue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<String>,
}

impl ToolResult {
    pub fn parse(v: &Value) -> ToolResult {
        let content = v
            .get("content")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().map(ContentBlock::parse).collect())
            .unwrap_or_default();
        ToolResult {
            content,
            structured: v.get("structuredContent").cloned(),
            is_error: v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false),
            cache_ttl_ms: v
                .get("cacheable")
                .and_then(|c| c.get("ttlMs"))
                .and_then(|t| t.as_u64())
                .or_else(|| v.get("ttlMs").and_then(|t| t.as_u64())),
            cache_scope: v
                .get("cacheable")
                .and_then(|c| c.get("cacheScope"))
                .and_then(|s| s.as_str())
                .or_else(|| v.get("cacheScope").and_then(|s| s.as_str()))
                .map(String::from),
            input_requests: v.get("inputRequests").cloned(),
            result_type: v
                .get("resultType")
                .and_then(|s| s.as_str())
                .map(String::from),
            task_ref: v
                .get("task")
                .and_then(|t| t.get("taskId").or_else(|| t.get("id")))
                .and_then(|s| s.as_str())
                .map(String::from),
        }
    }

    /// `resultType` absent est traité comme `complete` (§8.4).
    pub fn is_complete(&self) -> bool {
        matches!(self.result_type.as_deref(), None | Some("complete"))
    }
    pub fn needs_input(&self) -> bool {
        self.result_type.as_deref() == Some("input_required") || self.input_requests.is_some()
    }

    /// Rend le résultat en texte pour le modèle.
    pub fn render_text(&self) -> String {
        let mut parts: Vec<String> = self
            .content
            .iter()
            .map(|c| c.render())
            .filter(|s| !s.is_empty())
            .collect();
        if let Some(s) = &self.structured {
            parts.push(format!(
                "Données structurées :\n{}",
                serde_json::to_string_pretty(s).unwrap_or_else(|_| s.to_string())
            ));
        }
        if parts.is_empty() {
            "(aucun contenu)".into()
        } else {
            parts.join("\n\n")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
    },
    Audio {
        data: String,
        mime_type: String,
    },
    /// `resource_link` (2025-06-18+) : un pointeur, pas le contenu.
    ResourceLink {
        uri: String,
        name: Option<String>,
        description: Option<String>,
    },
    /// Ressource embarquée.
    Resource {
        uri: String,
        text: Option<String>,
        blob: Option<String>,
        mime_type: Option<String>,
    },
    Other(Value),
}

impl ContentBlock {
    pub fn parse(v: &Value) -> ContentBlock {
        match v.get("type").and_then(|t| t.as_str()) {
            Some("text") => ContentBlock::Text {
                text: v
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string(),
            },
            Some("image") => ContentBlock::Image {
                data: str_at(v, "data"),
                mime_type: str_or(v, "mimeType", "image/png"),
            },
            Some("audio") => ContentBlock::Audio {
                data: str_at(v, "data"),
                mime_type: str_or(v, "mimeType", "audio/wav"),
            },
            Some("resource_link") => ContentBlock::ResourceLink {
                uri: str_at(v, "uri"),
                name: v.get("name").and_then(|s| s.as_str()).map(String::from),
                description: v
                    .get("description")
                    .and_then(|s| s.as_str())
                    .map(String::from),
            },
            Some("resource") => {
                let r = v.get("resource").unwrap_or(v);
                ContentBlock::Resource {
                    uri: str_at(r, "uri"),
                    text: r.get("text").and_then(|s| s.as_str()).map(String::from),
                    blob: r.get("blob").and_then(|s| s.as_str()).map(String::from),
                    mime_type: r.get("mimeType").and_then(|s| s.as_str()).map(String::from),
                }
            }
            // `resources/read` renvoie des entrées **sans** champ `type` : un `uri` avec
            // un `text` ou un `blob` est une ressource, quelle que soit la version.
            None if v.get("uri").is_some() => ContentBlock::Resource {
                uri: str_at(v, "uri"),
                text: v.get("text").and_then(|s| s.as_str()).map(String::from),
                blob: v.get("blob").and_then(|s| s.as_str()).map(String::from),
                mime_type: v.get("mimeType").and_then(|s| s.as_str()).map(String::from),
            },
            _ => ContentBlock::Other(v.clone()),
        }
    }

    pub fn render(&self) -> String {
        match self {
            ContentBlock::Text { text } => text.clone(),
            ContentBlock::Image { mime_type, data } => {
                format!("[image {mime_type}, {} octets encodés]", data.len())
            }
            ContentBlock::Audio { mime_type, data } => {
                format!("[audio {mime_type}, {} octets encodés]", data.len())
            }
            ContentBlock::ResourceLink { uri, name, .. } => format!(
                "[ressource {} — lire avec mcp_resource_read(\"{uri}\")]",
                name.clone().unwrap_or_else(|| uri.clone())
            ),
            ContentBlock::Resource { uri, text, .. } => match text {
                Some(t) => format!("[{uri}]\n{t}"),
                None => format!("[{uri} — contenu binaire stocké en artefact]"),
            },
            ContentBlock::Other(v) => v.to_string(),
        }
    }
}

fn str_at(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_string()
}

fn str_or(v: &Value, k: &str, d: &str) -> String {
    v.get(k).and_then(|s| s.as_str()).unwrap_or(d).to_string()
}

/// Descripteur d'outil tel que renvoyé par `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    pub annotations: Value,
    pub icons: Option<Value>,
}

impl ToolDescriptor {
    pub fn parse(v: &Value) -> Option<ToolDescriptor> {
        Some(ToolDescriptor {
            name: v.get("name")?.as_str()?.to_string(),
            title: v.get("title").and_then(|s| s.as_str()).map(String::from),
            description: v
                .get("description")
                .and_then(|s| s.as_str())
                .unwrap_or_default()
                .to_string(),
            input_schema: v
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object"})),
            output_schema: v.get("outputSchema").cloned(),
            annotations: v.get("annotations").cloned().unwrap_or_else(|| json!({})),
            icons: v.get("icons").cloned(),
        })
    }
}

/// Construit le bloc `_meta` d'une requête 2026-07-28 (§8.2).
pub fn request_meta(
    version: ProtocolVersion,
    log_level: Option<&str>,
    traceparent: Option<&str>,
) -> Value {
    let mut meta = json!({
        "io.modelcontextprotocol/protocolVersion": version.as_str(),
        "clientCapabilities": client_capabilities(version),
        "clientInfo": client_info(),
    });
    if let Some(l) = log_level {
        meta["logLevel"] = json!(l);
    }
    if let Some(tp) = traceparent {
        meta["traceparent"] = json!(tp);
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_ordered_and_parseable() {
        let mut prev: Option<ProtocolVersion> = None;
        for v in VERSIONS {
            let p = ProtocolVersion::parse(v).expect(v);
            if let Some(prev) = prev {
                assert!(p > prev, "{v} doit être postérieure");
            }
            prev = Some(p);
        }
        assert_eq!(ProtocolVersion::default().as_str(), PREFERRED);
        assert!(ProtocolVersion::parse("1999-01-01").is_none());
    }

    #[test]
    fn capability_gates_follow_the_table() {
        let v = |s: &str| ProtocolVersion::parse(s).unwrap();
        assert!(!v("2024-11-05").has_streamable_http());
        assert!(v("2025-03-26").has_streamable_http());
        assert!(v("2024-11-05").allows_batching());
        assert!(!v("2025-06-18").allows_batching());
        assert!(v("2025-06-18").has_structured_output());
        assert!(!v("2025-03-26").has_structured_output());
        assert!(v("2025-11-25").has_icons());
        assert!(v("2026-07-28").is_stateless_core());
        assert!(!v("2025-11-25").is_stateless_core());
        assert!(v("2025-06-18").has_session_resume());
        assert!(
            !v("2026-07-28").has_session_resume(),
            "en 2026-07-28, un flux coupé impose de ré-émettre la requête"
        );
        assert!(v("2026-07-28").logging_via_meta());
        assert!(!v("2025-06-18").logging_via_meta());
    }

    #[test]
    fn decode_distinguishes_message_kinds() {
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
            Some(Incoming::Response(_))
        ));
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{}}"#),
            Some(Incoming::Notification(_))
        ));
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","id":7,"method":"sampling/createMessage","params":{}}"#),
            Some(Incoming::ServerRequest(_))
        ));
        assert!(decode("pas du json").is_none());
    }

    #[test]
    fn capabilities_are_parsed() {
        let c = ServerCapabilities::parse(&json!({
            "tools": {"listChanged": true},
            "resources": {"subscribe": true, "listChanged": false},
            "prompts": {},
            "completions": {},
            "logging": {},
            "extensions": {"io.modelcontextprotocol/tasks": {"version":"1"}}
        }));
        assert!(c.tools && c.tools_list_changed);
        assert!(c.resources && c.resources_subscribe && !c.resources_list_changed);
        assert!(c.prompts && c.completions && c.logging);
        assert!(c.tasks, "l'extension Tasks doit être détectée");
        assert!(!c.extensions.is_null());
    }

    #[test]
    fn absent_capabilities_are_false() {
        let c = ServerCapabilities::parse(&json!({}));
        assert!(!c.tools && !c.resources && !c.prompts && !c.tasks);
    }

    #[test]
    fn tool_result_parses_every_content_kind() {
        let r = ToolResult::parse(&json!({
            "content": [
                {"type":"text","text":"bonjour"},
                {"type":"image","data":"AAAA","mimeType":"image/png"},
                {"type":"audio","data":"BBBB","mimeType":"audio/wav"},
                {"type":"resource_link","uri":"file:///a.txt","name":"a.txt"},
                {"type":"resource","resource":{"uri":"file:///b.txt","text":"contenu"}}
            ],
            "structuredContent": {"ok": true}
        }));
        assert_eq!(r.content.len(), 5);
        assert!(!r.is_error);
        assert!(r.is_complete());
        let text = r.render_text();
        assert!(text.contains("bonjour"));
        assert!(text.contains("[image image/png"));
        assert!(text.contains("mcp_resource_read"));
        assert!(text.contains("Données structurées"));
    }

    #[test]
    fn is_error_is_preserved_for_self_correction() {
        let r = ToolResult::parse(&json!({
            "content":[{"type":"text","text":"fichier introuvable"}],
            "isError": true
        }));
        assert!(r.is_error);
        assert!(r.render_text().contains("introuvable"));
    }

    #[test]
    fn cacheable_result_is_read() {
        let r = ToolResult::parse(&json!({
            "content": [],
            "cacheable": {"ttlMs": 60000, "cacheScope": "session"}
        }));
        assert_eq!(r.cache_ttl_ms, Some(60_000));
        assert_eq!(r.cache_scope.as_deref(), Some("session"));
    }

    #[test]
    fn missing_result_type_means_complete() {
        let r = ToolResult::parse(&json!({"content": []}));
        assert!(r.is_complete());
        assert!(!r.needs_input());
        let r = ToolResult::parse(&json!({"content": [], "resultType": "input_required"}));
        assert!(!r.is_complete());
        assert!(r.needs_input());
    }

    #[test]
    fn resource_not_found_accepts_both_codes() {
        assert!(is_resource_not_found(-32002));
        assert!(is_resource_not_found(-32602));
        assert!(!is_resource_not_found(-32601));
    }

    #[test]
    fn client_capabilities_grow_with_the_version() {
        let old = client_capabilities(ProtocolVersion::V20241105);
        assert!(old.get("elicitation").is_none());
        let new = client_capabilities(ProtocolVersion::V20260728);
        assert!(new.get("elicitation").is_some());
        assert!(new["experimental"]["io.modelcontextprotocol/tasks"].is_object());
    }

    #[test]
    fn request_meta_carries_version_and_trace() {
        let m = request_meta(
            ProtocolVersion::V20260728,
            Some("debug"),
            Some("00-abc-def-01"),
        );
        assert_eq!(m["io.modelcontextprotocol/protocolVersion"], "2026-07-28");
        assert_eq!(m["logLevel"], "debug");
        assert_eq!(m["traceparent"], "00-abc-def-01");
        assert_eq!(m["clientInfo"]["name"], "penelope");
    }

    #[test]
    fn tool_descriptor_parsing_defaults() {
        let d = ToolDescriptor::parse(&json!({"name":"x"})).unwrap();
        assert_eq!(d.input_schema, json!({"type":"object"}));
        assert!(d.description.is_empty());
        assert!(ToolDescriptor::parse(&json!({})).is_none());
    }
}
