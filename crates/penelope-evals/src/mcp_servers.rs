//! Serveurs MCP simulés, un par version du protocole (§8, CA 8).
//!
//! Chaque serveur répond comme la version qu'il annonce : un serveur 2024-11-05 ne connaît
//! pas `server/discover`, un serveur 2025-06-18 refuse le batching, un serveur 2026-07-28
//! exige `_meta`. C'est ce qui permet de tester la **négociation** et pas seulement le
//! chemin nominal.

use penelope_mcp::error::McpError;
use penelope_mcp::protocol::{METHOD_NOT_FOUND, ProtocolVersion, UNSUPPORTED_PROTOCOL_VERSION};
use penelope_mcp::transport::LoopbackTransport;
use serde_json::{Value, json};
use std::sync::Arc;

/// Capacités par défaut d'un serveur simulé.
fn capabilities(v: ProtocolVersion) -> Value {
    let mut c = json!({
        "tools": {"listChanged": true},
        "resources": {"subscribe": true, "listChanged": true},
        "prompts": {"listChanged": true},
        "logging": {}
    });
    if v.has_structured_output() {
        c["completions"] = json!({});
    }
    if v.has_tasks() {
        c["experimental"] = json!({"io.modelcontextprotocol/tasks": {"version": "1"}});
    }
    c
}

fn tools(v: ProtocolVersion) -> Value {
    let mut read = json!({
        "name": "get_issue",
        "title": "Lire un ticket",
        "description": "Lit un ticket du tracker",
        "inputSchema": {
            "type":"object",
            "properties":{"id":{"type":"integer"}},
            "required":["id"]
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false}
    });
    if v.has_structured_output() {
        read["outputSchema"] = json!({
            "type":"object",
            "properties":{"id":{"type":"integer"},"subject":{"type":"string"}},
            "required":["id","subject"]
        });
    }
    if v.has_icons() {
        read["icons"] = json!([{"src":"data:image/png;base64,AA","sizes":"16x16"}]);
    }
    json!([
        read,
        {
            "name": "create_issue",
            "description": "Crée un ticket",
            "inputSchema": {"type":"object","properties":{"subject":{"type":"string"}},
                            "required":["subject"]},
            "annotations": {"readOnlyHint": false, "openWorldHint": true}
        },
        {
            "name": "delete_project",
            "description": "Supprime un projet",
            "inputSchema": {"type":"object","properties":{"id":{"type":"integer"}}},
            "annotations": {"destructiveHint": true}
        }
    ])
}

/// Construit un serveur simulé pour une version et un transport donnés.
pub fn server(version: ProtocolVersion, transport_kind: &'static str) -> Arc<LoopbackTransport> {
    LoopbackTransport::new(transport_kind, move |method, params| {
        handle(version, method, params)
    })
}

fn handle(v: ProtocolVersion, method: &str, params: &Value) -> Result<Value, McpError> {
    match method {
        // Seule la famille 2026-07-28 connaît `server/discover`.
        "server/discover" => {
            if !v.is_stateless_core() {
                return Err(McpError::Rpc {
                    code: METHOD_NOT_FOUND,
                    message: "server/discover inconnu".into(),
                    data: None,
                });
            }
            // En mode sans état, `_meta` est obligatoire.
            if params.get("_meta").is_none() {
                return Err(McpError::Rpc {
                    code: penelope_mcp::protocol::MISSING_REQUIRED_CLIENT_CAPABILITY,
                    message: "_meta manquant".into(),
                    data: None,
                });
            }
            Ok(json!({
                "protocolVersion": v.as_str(),
                "capabilities": capabilities(v),
                "serverInfo": {"name":"tracker-mock","version":"1.0"},
                "instructions": "utilise get_issue avant create_issue"
            }))
        }
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(|x| x.as_str())
                .and_then(ProtocolVersion::parse);
            match asked {
                Some(a) if a <= v => Ok(json!({
                    "protocolVersion": v.min(a).as_str(),
                    "capabilities": capabilities(v),
                    "serverInfo": {"name":"tracker-mock","version":"1.0"}
                })),
                _ => Err(McpError::Rpc {
                    code: UNSUPPORTED_PROTOCOL_VERSION,
                    message: "version trop récente".into(),
                    data: Some(json!({"supported": [v.as_str()]})),
                }),
            }
        }
        "tools/list" => {
            // Pagination : deux pages, pour forcer le client à suivre `nextCursor`.
            match params.get("cursor").and_then(|c| c.as_str()) {
                None => Ok(json!({"tools": tools(v), "nextCursor": "page2"})),
                Some("page2") => Ok(json!({"tools": [{
                    "name":"search_issues",
                    "description":"Cherche des tickets",
                    "inputSchema":{"type":"object","properties":{"q":{"type":"string"}}},
                    "annotations":{"readOnlyHint": true}
                }]})),
                Some(_) => Ok(json!({"tools": []})),
            }
        }
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "get_issue" => {
                    let id = args.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
                    let mut r = json!({
                        "content":[{"type":"text","text":format!("Ticket {id} : TVA incorrecte")}]
                    });
                    if v.has_structured_output() {
                        r["structuredContent"] = json!({"id": id, "subject": "TVA incorrecte"});
                    }
                    if v.is_stateless_core() {
                        r["cacheable"] = json!({"ttlMs": 60000, "cacheScope": "session"});
                        r["resultType"] = json!("complete");
                    }
                    Ok(r)
                }
                "create_issue" => Ok(json!({
                    "content":[{"type":"text","text":"Ticket 5000 créé"}],
                    "structuredContent":{"id":5000}
                })),
                "long_task" if v.has_tasks() => Ok(json!({
                    "content": [],
                    "resultType": "incomplete",
                    "task": {"taskId": "task-1"}
                })),
                "needs_input" if v.is_stateless_core() => Ok(json!({
                    "content": [],
                    "resultType": "input_required",
                    "inputRequests": [{
                        "id":"i1",
                        "schema":{"type":"object","properties":{"token":{"type":"string"}},
                                  "required":["token"]}
                    }]
                })),
                "failing" => Ok(json!({
                    "content":[{"type":"text","text":"le ticket n'existe pas"}],
                    "isError": true
                })),
                other => Err(McpError::Rpc {
                    code: penelope_mcp::protocol::INVALID_PARAMS,
                    message: format!("outil inconnu : {other}"),
                    data: None,
                }),
            }
        }
        "resources/list" => Ok(json!({"resources":[
            {"uri":"tracker://projets","name":"Projets","mimeType":"application/json"}
        ]})),
        "resources/templates/list" => Ok(json!({"resourceTemplates":[
            {"uriTemplate":"tracker://ticket/{id}","name":"Ticket"}
        ]})),
        "resources/read" => Ok(json!({"contents":[
            {"uri":"tracker://projets","text":"[{\"id\":1,\"nom\":\"Pénélope\"}]",
             "mimeType":"application/json"}
        ]})),
        "resources/subscribe" => Ok(json!({})),
        "prompts/list" => Ok(json!({"prompts":[
            {"name":"resume_ticket","description":"Résume un ticket",
             "arguments":[{"name":"id","required":true}]}
        ]})),
        "prompts/get" => Ok(json!({
            "description":"Résumé de ticket",
            "messages":[{"role":"user","content":{"type":"text","text":"Résume le ticket 1."}}]
        })),
        "completion/complete" => {
            if !v.has_streamable_http() {
                return Err(McpError::Rpc {
                    code: METHOD_NOT_FOUND,
                    message: "completions inconnues".into(),
                    data: None,
                });
            }
            Ok(json!({"completion":{"values":["4312","4313"],"hasMore":false}}))
        }
        "subscriptions/listen" => {
            if v.is_stateless_core() {
                Ok(json!({"ok": true}))
            } else {
                Err(McpError::Rpc {
                    code: METHOD_NOT_FOUND,
                    message: "subscriptions/listen inconnu".into(),
                    data: None,
                })
            }
        }
        "logging/setLevel" => {
            if v.logging_via_meta() {
                Err(McpError::Rpc {
                    code: METHOD_NOT_FOUND,
                    message: "utiliser _meta.logLevel".into(),
                    data: None,
                })
            } else {
                Ok(json!({}))
            }
        }
        "ping" => Ok(json!({})),
        "tasks/get" => {
            if !v.has_tasks() {
                return Err(McpError::Rpc {
                    code: METHOD_NOT_FOUND,
                    message: "tasks inconnues".into(),
                    data: None,
                });
            }
            Ok(json!({
                "taskId": params.get("taskId").cloned().unwrap_or(json!("task-1")),
                "status": "completed",
                "result": {"content":[{"type":"text","text":"terminé"}]}
            }))
        }
        "notifications/initialized" | "notifications/cancelled" => Ok(json!({})),
        other => Err(McpError::Rpc {
            code: METHOD_NOT_FOUND,
            message: format!("méthode inconnue : {other}"),
            data: None,
        }),
    }
}

/// Serveur d'autorisation simulé (§8.5, CA 8 : PRM, AS metadata, CIMD, DCR, PKCE).
pub struct MockAuthServer {
    pub issuer: String,
    pub resource: String,
}

impl MockAuthServer {
    pub fn new() -> Self {
        MockAuthServer {
            issuer: "https://auth.example.com".into(),
            resource: "https://api.example.com/mcp".into(),
        }
    }

    pub fn protected_resource_metadata(&self) -> Value {
        json!({
            "resource": self.resource,
            "authorization_servers": [self.issuer],
            "scopes_supported": ["tickets:read", "tickets:write"],
        })
    }

    pub fn as_metadata(&self) -> Value {
        json!({
            "issuer": self.issuer,
            "authorization_endpoint": format!("{}/authorize", self.issuer),
            "token_endpoint": format!("{}/token", self.issuer),
            "registration_endpoint": format!("{}/register", self.issuer),
            "scopes_supported": ["tickets:read","tickets:write"],
            "code_challenge_methods_supported": ["S256"],
            "grant_types_supported": ["authorization_code","refresh_token"],
        })
    }

    pub fn www_authenticate(&self, missing_scope: Option<&str>) -> String {
        let mut s = format!(
            "Bearer realm=\"mcp\", resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            self.resource.trim_end_matches("/mcp")
        );
        if let Some(sc) = missing_scope {
            s.push_str(&format!(", error=\"insufficient_scope\", scope=\"{sc}\""));
        }
        s
    }

    /// Réponse d'échange de code, avec vérification PKCE.
    pub fn token(&self, verifier: &str, challenge: &str) -> Result<Value, String> {
        let expected = penelope_mcp::oauth::Pkce::from_verifier(verifier).challenge;
        if expected != challenge {
            return Err("code_verifier ne correspond pas au code_challenge".into());
        }
        Ok(json!({
            "access_token": "at-123",
            "refresh_token": "rt-456",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "tickets:read"
        }))
    }

    /// Réponse d'enregistrement dynamique (RFC 7591).
    pub fn register(&self, body: &Value) -> Value {
        json!({
            "client_id": "client-dyn-1",
            "client_id_issued_at": 1789516800,
            "redirect_uris": body.get("redirect_uris").cloned().unwrap_or(json!([])),
            "token_endpoint_auth_method": "none",
        })
    }
}

impl Default for MockAuthServer {
    fn default() -> Self {
        Self::new()
    }
}

/// Toutes les combinaisons version × transport de la matrice du CA 8.
pub fn conformance_matrix() -> Vec<(ProtocolVersion, &'static str)> {
    let versions = [
        ProtocolVersion::V20241105,
        ProtocolVersion::V20250326,
        ProtocolVersion::V20250618,
        ProtocolVersion::V20251125,
        ProtocolVersion::V20260728,
    ];
    let transports = ["stdio", "http", "sse"];
    let mut out = Vec::new();
    for v in versions {
        for t in transports {
            // Streamable HTTP n'existe qu'à partir de 2025-03-26 ; avant, c'est stdio ou
            // HTTP+SSE historique.
            if t == "http" && !v.has_streamable_http() {
                continue;
            }
            out.push((v, t));
        }
    }
    out
}
