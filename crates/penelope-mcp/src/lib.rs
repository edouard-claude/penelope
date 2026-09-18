//! `penelope-mcp` : client MCP complet, toutes versions, HITL, ajout à chaud (§8).
//!
//! Priorité absolue du PRD. Ce crate couvre la négociation multi-versions, les trois
//! transports, toutes les primitives du tableau §8.4, l'autorisation OAuth 2.1, le
//! registre paresseux et la supervision.

#![forbid(unsafe_code)]

pub mod client;
pub mod config;
pub mod error;
pub mod oauth;
pub mod protocol;
pub mod registry;
pub mod supervisor;
pub mod tasks;
pub mod transport;

pub use client::{McpClient, Negotiated, negotiate};
pub use config::{ServerConfig, load_dir, write_server};
pub use error::{McpError, Result};
pub use oauth::{AuthRequest, Pkce, RedirectMode, Tokens};
pub use protocol::{ContentBlock, ProtocolVersion, ServerCapabilities, ToolDescriptor, ToolResult};
pub use registry::{RegisteredTool, ReplaceReport, ToolRegistry, qualified_name};
pub use supervisor::{Backoff, ServerState, ServerStatus};
pub use transport::{HttpTransport, LoopbackTransport, StdioTransport, Transport};

/// Rendu d'une ligne de description compacte pour le tier T1 (§5.2).
///
/// Une ligne par serveur, **jamais** les schémas.
pub fn server_summary_line(status: &ServerStatus) -> String {
    let proto = status.protocol.as_deref().unwrap_or("—");
    format!(
        "{} — {} outils — {} — protocole {}{}",
        status.name,
        status.tool_count,
        status.state.as_str(),
        proto,
        if status.lazy {
            " — démarrage différé"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_line_has_no_schema() {
        let s = ServerStatus {
            name: "redmine".into(),
            state: ServerState::Ready,
            transport: "stdio".into(),
            protocol: Some("2026-07-28".into()),
            tool_count: 12,
            failures: 0,
            last_error: None,
            last_ok: None,
            p50_ms: 30.0,
            p95_ms: 120.0,
            calls: 42,
            errors: 0,
            running: true,
            lazy: true,
            keychain: false,
        };
        let line = server_summary_line(&s);
        assert!(line.contains("redmine"));
        assert!(line.contains("12 outils"));
        assert!(line.contains("2026-07-28"));
        assert!(!line.contains("inputSchema"));
        assert!(line.lines().count() == 1, "exactement une ligne");
    }
}
