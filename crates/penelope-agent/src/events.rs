//! Les événements que la boucle écrit dans le journal, nommés par un type (épopée #208,
//! lot K, tâche T26).
//!
//! Avant, chaque kind était un littéral à l'endroit de l'écriture : un nom mal tapé
//! partait tel quel dans le journal et le flux runtime. Chaque variante est documentée
//! dans `docs/runtime-events.md` (test `every_turn_event_kind_is_documented`). Les
//! contenus (`conv.*`) n'en sont pas : ils passent par `Conversation` et `AttemptSink`.

use penelope_kernel::event::EventDraft;
use penelope_kernel::journal::{KIND_TURN_FINISHED, KIND_TURN_STARTED};
use serde_json::Value;

/// Un kind d'événement écrit par la boucle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnEventKind {
    /// Ouverture d'un tour (`turn_log`).
    Started,
    /// Fermeture d'un tour, sur chaque sortie (`turn_log`).
    Finished,
    /// Messages du propriétaire réclamés pendant le tour (`steering`).
    Merged,
    /// Réponse sans texte ni appel d'outil.
    EmptyAnswer,
    /// Le détecteur de boucles a arrêté les outils du tour.
    LoopAborted,
    /// Un appel d'outil a rendu son résultat.
    ToolResult,
    /// Nouvel essai du même modèle après une erreur d'avant flux.
    LlmRetried,
    /// Réponse servie par un autre modèle que celui demandé (repli d'OpenRouter).
    LlmFallbackUsed,
    /// Une approbation, ou un effet incertain, est tranché.
    ApprovalDecided,
    /// Le juge d'approbation a rendu un avis, ou n'a pas pu (#203).
    ApprovalJudged,
}

impl TurnEventKind {
    /// Toutes les variantes, pour les tests.
    pub const ALL: [TurnEventKind; 10] = [
        TurnEventKind::Started,
        TurnEventKind::Finished,
        TurnEventKind::Merged,
        TurnEventKind::EmptyAnswer,
        TurnEventKind::LoopAborted,
        TurnEventKind::ToolResult,
        TurnEventKind::LlmRetried,
        TurnEventKind::LlmFallbackUsed,
        TurnEventKind::ApprovalDecided,
        TurnEventKind::ApprovalJudged,
    ];

    /// Le kind écrit dans le journal.
    pub fn as_str(self) -> &'static str {
        match self {
            TurnEventKind::Started => KIND_TURN_STARTED,
            TurnEventKind::Finished => KIND_TURN_FINISHED,
            TurnEventKind::Merged => "turn.merged",
            TurnEventKind::EmptyAnswer => "turn.empty_answer",
            TurnEventKind::LoopAborted => "turn.loop_aborted",
            TurnEventKind::ToolResult => "tool.result",
            TurnEventKind::LlmRetried => "llm.retried",
            TurnEventKind::LlmFallbackUsed => "llm.fallback_used",
            TurnEventKind::ApprovalDecided => "approval.decided",
            TurnEventKind::ApprovalJudged => "approval.judged",
        }
    }

    /// Un événement de ce kind, à attacher à sa session par l'appelant.
    pub fn draft(self, payload: Value) -> EventDraft {
        EventDraft::new(self.as_str(), payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chaque kind de la boucle est dans la documentation du journal, entre accents
    /// graves : un kind ajouté sans sa ligne ne passe pas.
    #[test]
    fn every_turn_event_kind_is_documented() {
        let doc = include_str!("../../../docs/runtime-events.md");
        let missing: Vec<&str> = TurnEventKind::ALL
            .iter()
            .map(|k| k.as_str())
            .filter(|k| !doc.contains(&format!("`{k}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "kinds de la boucle absents de docs/runtime-events.md : {missing:?}"
        );
    }

    #[test]
    fn kinds_are_distinct_and_the_bounds_are_the_journal_ones() {
        let names: std::collections::BTreeSet<_> =
            TurnEventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), TurnEventKind::ALL.len());
        assert_eq!(TurnEventKind::Started.as_str(), "turn.started");
        assert_eq!(TurnEventKind::Finished.as_str(), "turn.finished");
    }
}
