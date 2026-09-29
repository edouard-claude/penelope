//! Livraison d'un plan approuvé en dev (T4 de #185, issue #192).
//!
//! Quand la dernière revue ou vérification d'un plan qui écrit du code l'accepte, le run
//! ne s'arrête plus : il ouvre la PR vers la branche de développement, attend la CI du
//! projet, puis vérifie l'environnement de dev depuis l'extérieur. Trois étapes
//! `delivery`, chacune suivie de sa carte « livraison bloquée » :
//!
//! ```text
//!  juge ─passed─► livraison-pr ─passed─► livraison-ci ─passed─► livraison-e2e ─passed─► fin
//!                    │                      │                      │
//!                    └─► carte ◄────────────┴──────────────────────┘  (échec, information
//!                        Réessayer ─► la même étape                     manquante)
//!                        Arrêter   ─► $blocked
//! ```
//!
//! Ce module est la logique pure : la configuration du projet et ce qui y manque
//! ([`config`]), les verdicts de CI et d'E2E ([`verdicts`]), les étapes ajoutées au plan
//! compilé. Les appels au forgeur, à git et à l'environnement de dev vivent dans
//! l'orchestrateur. Rien n'y est supposé : un forgeur, une branche de dev, une CI ou une
//! URL que ni la configuration ni le dépôt ne donnent devient une demande ciblée au
//! propriétaire, sur la carte.

pub mod config;
pub mod verdicts;

use crate::model::{BLOCKED, Phase as RunPhase, Step, Transition};

/// Actions d'une étape `delivery`.
pub const PULL_REQUEST: &str = "pull_request";
pub const CI: &str = "ci";
pub const E2E: &str = "e2e";
pub const ACTIONS: &[&str] = &[PULL_REQUEST, CI, E2E];

/// Première étape de la livraison, où va la dernière revue acceptée.
pub const ENTRY: &str = "livraison-pr";

/// Choix de la carte « livraison bloquée ».
pub const RETRY: &str = "Réessayer";
pub const STOP: &str = "Arrêter";

/// Résultat d'une étape `delivery` qui laisse avancer ; tout autre (`blocked` pour une
/// information manquante, `failed` pour une CI ou un E2E rouge) mène à la carte.
pub const PASSED: &str = "passed";

struct Stage {
    action: &'static str,
    id: &'static str,
    name: &'static str,
}

const STAGES: [Stage; 3] = [
    Stage {
        action: PULL_REQUEST,
        id: ENTRY,
        name: "Livraison · PR dev",
    },
    Stage {
        action: CI,
        id: "livraison-ci",
        name: "Livraison · CI",
    },
    Stage {
        action: E2E,
        id: "livraison-e2e",
        name: "Livraison · E2E dev",
    },
];

/// Les étapes de la livraison, dans l'ordre, la dernière menant à `end`. Elles n'ont pas
/// de variante « laisse filer » : ce sont des gates, pas des cartes d'OK ordinaires.
pub fn tail(end: &str) -> Vec<Step> {
    let mut out = Vec::new();
    for (i, stage) in STAGES.iter().enumerate() {
        let next = STAGES.get(i + 1).map_or(end, |s| s.id);
        let card = format!("{}-bloquee", stage.id);
        out.push(Step {
            id: stage.id.into(),
            name: stage.name.into(),
            kind: "delivery".into(),
            delivery: stage.action.into(),
            phase: RunPhase::Deploy,
            transitions: vec![
                Transition::on_result(next, PASSED),
                Transition::always(&card),
            ],
            ..Default::default()
        });
        out.push(Step {
            id: card,
            name: format!("{} bloquée", stage.name),
            kind: "user".into(),
            phase: RunPhase::Waiting,
            template: "question".into(),
            choices: vec![RETRY.into(), STOP.into()],
            transitions: vec![
                Transition::on_result(stage.id, RETRY),
                Transition {
                    tag: "livraison arrêtée par le propriétaire".into(),
                    ..Transition::on_result(BLOCKED, STOP)
                },
            ],
            ..Default::default()
        });
    }
    out
}

#[cfg(test)]
mod tests;
