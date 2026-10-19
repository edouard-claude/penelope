//! La section `[plugins]` (#393) : la politique du propriétaire sur ce que les plugins
//! déposés apportent. Jamais lue dans un plugin.

use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugins {
    /// Serveurs MCP de plugins autorisés à démarrer, par leur nom `<plugin>-<serveur>`,
    /// ou `<plugin>-*` pour tous ceux d'un plugin. Vide : aucun ; un serveur non autorisé
    /// reste visible, en erreur, avec la raison.
    pub allowed_mcp_servers: Vec<String>,
}

impl Plugins {
    /// Vrai si le serveur `<plugin>-<serveur>` du plugin `plugin` est autorisé.
    pub fn allows_mcp(&self, plugin: &str, server: &str) -> bool {
        self.allowed_mcp_servers
            .iter()
            .any(|a| a == server || a.strip_suffix("-*") == Some(plugin))
    }
}
