//! Le cœur du daemon comme `Admin` des outils qui parlent de lui (`self_status`,
//! `self_config`, `send_voice`).

use super::*;

#[async_trait::async_trait]
impl penelope_executor::selfknow::Admin for Core {
    fn uptime_s(&self) -> u64 {
        self.handle.uptime_s(self.services.clock.now_ms())
    }

    async fn set_config(&self, path: &str, value: Value) -> Result<u64, String> {
        let g = penelope_app::helpers::set_config_path(&self.services, path, value)
            .map_err(|e| e.to_string())?;
        self.invalidate_providers().await;
        Ok(g)
    }

    async fn backup_status(&self) -> Result<Value, String> {
        Ok(penelope_ops::backup::status(&self.services).await)
    }

    fn rss_mb(&self) -> Option<f64> {
        Some(crate::runtime::rss_mb())
    }

    async fn codex_view(&self) -> Value {
        crate::selfknow::codex_view(&self.services, &self.services.config.config()).await
    }

    async fn context_view(
        &self,
        session_id: &str,
        model_id: Option<&str>,
    ) -> anyhow::Result<Value> {
        compaction::context_view(&self.services, session_id, model_id).await
    }

    async fn send_voice(
        &self,
        session_id: &str,
        origin: &Origin,
        args: &Value,
    ) -> Result<Value, String> {
        let (s, p) = (&self.services, self.providers.as_ref());
        penelope_executor::voice::tool(s, p, self.hooks.messenger(), session_id, origin, args).await
    }

    async fn memory_search(&self) -> Value {
        embeddings::search_mode(&self.embedder())
            .await
            .unwrap_or_else(|e| json!({"error": e.to_string()}))
    }

    async fn mcp_servers(&self) -> Value {
        let Some(sup) = self.hooks.mcp_supervisor() else {
            return json!("superviseur MCP non démarré");
        };
        json!({
            "dir": sup.dir(),
            "servers": sup.statuses().await,
            "invalid": sup.invalid(),
            "admin": "penelope mcp list|show|restart|logs|test ; déclarations dans mcp.d/*.toml, prises en compte à chaud",
        })
    }
}
