//! La section `[plugins]` (#393) : la politique du propriétaire sur ce que les plugins
//! déposés apportent. Jamais lue dans un plugin.

use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugins {
    /// Serveurs MCP de plugins autorisés à démarrer : `<plugin>/<serveur>`, ou
    /// `<plugin>/*` pour tous ceux d'un plugin (le plugin est nommé par son dossier sous
    /// `{data}/plugins`). Vide : aucun ; un serveur non autorisé reste visible, en erreur,
    /// avec la raison.
    pub allowed_mcp_servers: Vec<String>,
    /// Hooks de plugins autorisés : `<plugin>/<événement>` (`post_tool`, `pre_tool`,
    /// `prompt_submit`, `session_start`, `turn_end`) ou `<plugin>/*`. Vide : aucun.
    pub allowed_hooks: Vec<String>,
}

impl Plugins {
    /// Vrai si le serveur `server` (sa clé dans `.mcp.json`) du plugin `plugin` est
    /// autorisé. Le `/` ne peut figurer dans aucun des deux noms : la paire est sans
    /// ambiguïté.
    pub fn allows_mcp(&self, plugin: &str, server: &str) -> bool {
        self.allowed_mcp_servers.iter().any(|a| {
            a.split_once('/')
                .is_some_and(|(p, s)| p == plugin && (s == server || s == "*"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_names_a_plugin_and_a_server_without_ambiguity() {
        let p = Plugins {
            allowed_mcp_servers: vec!["notes/index".into(), "outils/*".into(), "a-b".into()],
            ..Default::default()
        };
        assert!(p.allows_mcp("notes", "index"));
        assert!(!p.allows_mcp("notes", "web"));
        assert!(p.allows_mcp("outils", "n_importe"));
        assert!(!p.allows_mcp("a", "b"));
        assert!(!p.allows_mcp("notes-index", ""));
    }
}
