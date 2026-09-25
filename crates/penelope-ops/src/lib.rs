//! `penelope-ops` : l'exploitation de l'instance (épopée #208, T28).
//!
//! Diagnostic (`doctor`), mise à jour et retour arrière (`upgrade`), sauvegarde, import
//! d'une instance Hermes, connexion et quota de l'abonnement Codex, installation de skills
//! tierces et de leurs dépendances, purge et rétention, opérations sur les sessions (fork,
//! retour arrière, export, reconstruction). Au-dessus de `penelope-app` et de `penelope-vault`,
//! sous le daemon, qui compose la méthode RPC `doctor` avec ses propres contrôles
//! (`rpc/methods/doctor.rs`) ; la crate ne connaît ni le daemon ni l'hôte MCP.

#![forbid(unsafe_code)]

pub mod backup;
pub mod codex_auth;
pub mod codex_quota;
pub mod doctor;
pub mod hermes;
pub mod purge;
pub mod session_ops;
pub mod skill_deps;
pub mod skill_install;
pub mod upgrade;

// Modules du socle et du vault, sous les chemins que les fichiers déplacés du daemon
// nomment encore (`crate::helpers`…).
pub(crate) use penelope_app::{bus, codex_scope, helpers, machine, ports};
pub(crate) use penelope_vault::{
    embeddings, episodes, mem_split, session_notes, vault_inventory, vault_ops,
};

/// Version de l'instance : celle du workspace, comme `penelope_daemon::VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
