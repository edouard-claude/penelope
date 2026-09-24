//! Le superviseur vu de l'exécuteur d'outils.

use super::*;

#[async_trait::async_trait]
impl crate::executor::McpGateway for McpSupervisor {
    async fn call_tool(
        &self,
        qualified: &str,
        args: &Value,
        from: crate::elicitation::Destination,
    ) -> Result<Value, String> {
        self.call(qualified, args, from).await
    }

    /// Une ligne par serveur qui a des outils, sans état volatil : le préfixe du prompt
    /// ne doit pas bouger à chaque reconnexion.
    async fn server_lines(&self) -> Vec<String> {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        slots
            .iter()
            .filter(|s| s.config().enabled)
            .filter_map(|s| {
                s.info(|i| {
                    (i.tool_count > 0).then(|| match i.state {
                        ServerState::Failed | ServerState::AuthRequired => {
                            format!("{} : {} outils (indisponible)", s.name, i.tool_count)
                        }
                        _ => format!("{} : {} outils", s.name, i.tool_count),
                    })
                })
            })
            .collect()
    }

    async fn tool_policy(&self, qualified: &str) -> Option<String> {
        let tool = self
            .services
            .mcp_tools
            .get(qualified)
            .await
            .ok()
            .flatten()?;
        let slot = self.slot(&tool.server).await?;
        slot.config().tool_policy.get(&tool.name).cloned()
    }

    /// Outils des serveurs `eager_schemas`, exposés directement au modèle.
    async fn eager_tools(&self) -> Vec<penelope_llm::ToolDef> {
        let slots: Vec<Arc<Slot>> = self.slots.read().await.values().cloned().collect();
        let names: Vec<String> = slots
            .iter()
            .filter(|s| {
                let c = s.config();
                c.enabled && c.eager_schemas
            })
            .map(|s| s.name.clone())
            .collect();
        if names.is_empty() {
            return Vec::new();
        }
        self.services
            .mcp_tools
            .eager_schemas(&names)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|d| {
                Some(penelope_llm::ToolDef::new(
                    d.get("name")?.as_str()?,
                    d.get("description").and_then(|x| x.as_str()).unwrap_or(""),
                    d.get("inputSchema")
                        .cloned()
                        .unwrap_or(json!({"type": "object"})),
                ))
            })
            .collect()
    }
}
