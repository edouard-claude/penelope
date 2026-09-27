//! Section `[approval]` : le juge d'approbation (issue #203).

use super::Config;
use serde::{Deserialize, Serialize};

/// Approbation des appels d'outils.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Approval {
    /// Juge des lignes `shell_exec` sans motif possible (issue #203), sous les planchers
    /// déterministes : `off` (aucun appel), `explain` (la carte dit ce que la ligne fait
    /// réellement et propose une règle sur les pouvoirs reconnus ; rien n'est autorisé
    /// seul), `auto_read` (comme `explain`, et une lecture pure dans les workspaces, sans
    /// réseau ni écriture ni processus détaché, passe sans carte). Modèle : rôle
    /// `approval_judge`.
    pub judge: JudgeMode,
}

/// Mode du juge d'approbation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeMode {
    Off,
    /// Mesure faite sur l'instance le 26/09/2026 (T21) : go, `explain` par défaut.
    #[default]
    Explain,
    AutoRead,
}

impl JudgeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            JudgeMode::Off => "off",
            JudgeMode::Explain => "explain",
            JudgeMode::AutoRead => "auto_read",
        }
    }
}

/// Rôle du modèle du juge.
pub const APPROVAL_JUDGE_ROLE: &str = "approval_judge";

impl Config {
    /// Alias du modèle du juge : le rôle `approval_judge`, sinon `fast`, sinon celui du
    /// classifieur. Jamais le modèle de conversation par défaut : une configuration
    /// écrite avant le rôle ne doit pas payer un grand modèle pour chaque carte.
    pub fn judge_alias(&self) -> String {
        if let Some(alias) = self.models.roles.get(APPROVAL_JUDGE_ROLE) {
            return alias.clone();
        }
        if self.models.aliases.contains_key("fast") {
            return "fast".into();
        }
        self.role_alias("classifier")
    }
}
