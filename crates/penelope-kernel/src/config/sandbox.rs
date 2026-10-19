//! La section `[sandbox]` : profils, dossiers accordés, lectures refusées et chemins
//! masqués.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sandbox {
    /// Profil du bac à sable de `shell_exec` : `read-only`, `workspace-write` ou `full`.
    pub default_profile: String,
    /// Serveurs MCP autorisés à tourner avec le profil `full` (sans bac à sable).
    pub allow_full_for: Vec<String>,
    /// Serveurs MCP stdio qui gardent leur bac à sable mais joignent le trousseau macOS :
    /// ceux dont le métier est de lire leurs propres identifiants. Le trousseau reste fermé
    /// aux autres, qui y verraient « introuvable » ce qui y est rangé.
    pub allow_keychain_for: Vec<String>,
    /// Répertoires de travail des outils de fichiers et du shell, en plus du défaut.
    pub workspaces: Vec<String>,
    /// Réseau pour **toutes** les commandes de `shell_exec` et des étapes `shell`. Faux :
    /// une commande n'a le réseau que si son appel le demande (`network: true`, carte
    /// d'approbation qui le dit, « Toujours » borné à la famille de commandes) ou si son
    /// étape de workflow le déclare. Une configuration qui porte `true` le garde.
    pub shell_network: bool,
    /// Chemins dont la lecture est refusée aux commandes sous bac à sable, même quand le
    /// profil lit le disque : clés, jetons, base de Pénélope, secrets, configuration.
    /// `{data}`, `{config}`, `{state}` et `~` sont développés.
    pub deny_read: Vec<String>,
    /// Motifs masqués dans les dossiers accordés (workspaces, vault) : `**/.env`,
    /// `.git/config`, `secrets/`. Refusés aux outils de fichiers et, sous macOS, aux
    /// commandes du shell, comme `deny_read` (#394).
    pub hide: Vec<String>,
}

/// Motifs masqués par défaut : fichiers d'environnement, identifiants, clés (#394).
pub fn default_hide() -> Vec<String> {
    [
        "**/.env",
        "**/.env.*",
        "**/.netrc",
        "**/.npmrc",
        "**/.pypirc",
        "**/id_rsa",
        "**/id_ed25519",
        "**/*.pem",
        "**/*.key",
    ]
    .map(String::from)
    .to_vec()
}

/// Lectures refusées par défaut : ce qu'une consigne cachée dans un résultat d'outil
/// chercherait à exfiltrer (issue #68).
pub fn default_deny_read() -> Vec<String> {
    [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.config/gh",
        "~/.netrc",
        "~/.kube",
        "~/.docker/config.json",
        "{data}/penelope.db",
        "{data}/secrets.enc",
        "{data}/mcp.d",
        "{data}/hooks.d",
        "{config}",
        "{state}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox {
            default_profile: "workspace-write".into(),
            allow_full_for: Vec::new(),
            allow_keychain_for: Vec::new(),
            workspaces: Vec::new(),
            shell_network: false,
            deny_read: default_deny_read(),
            hide: default_hide(),
        }
    }
}
