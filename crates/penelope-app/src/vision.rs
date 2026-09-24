//! Ce qu'on attend du modèle de vision (issue #125) : nommé par le port `Orchestrator`
//! (`inspect_image`), descendu de `penelope-daemon/src/vision.rs` (épopée #208, T21).

/// Ce qu'on attend du modèle de vision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Describe,
    Read,
    Locate,
}

impl Task {
    pub fn parse(s: &str) -> Option<Task> {
        Some(match s.trim() {
            "describe" | "decrire" | "décrire" => Task::Describe,
            "read" | "text" | "lire" | "recopier" => Task::Read,
            "locate" | "localiser" | "pointer" => Task::Locate,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Task::Describe => "describe",
            Task::Read => "read",
            Task::Locate => "locate",
        }
    }

    /// Rôle de modèle : un modèle de description et un modèle de pointage n'ont pas les
    /// mêmes forces.
    pub fn role(&self) -> &'static str {
        match self {
            Task::Locate => "image_locate",
            _ => "image_describe",
        }
    }

    /// Plafond de la réponse du modèle de vision pour cette tâche.
    pub fn max_tokens(&self) -> u32 {
        match self {
            Task::Describe => 2_000,
            Task::Read => 4_000,
            Task::Locate => 1_000,
        }
    }
}
