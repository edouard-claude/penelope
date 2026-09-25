//! Observation de la fidélité d'un résumé (issue #179), sortie de `compaction.rs`.

use super::*;

/// Observation prudente des éléments explicites de messages utilisateur.
/// Une paraphrase peut être signalée comme absente : ce n'est pas un verdict
/// sémantique, et ce contrôle ne bloque jamais la compaction (issue #179).
#[derive(Debug, Serialize)]
pub(super) struct FidelityObservation {
    pub(super) actions_total: usize,
    pub(super) actions_missing: usize,
    pub(super) action_samples: Vec<String>,
    pub(super) identifiers_total: usize,
    pub(super) identifiers_missing: usize,
    pub(super) identifier_samples: Vec<String>,
}

pub(super) fn observe_fidelity(job: &SummaryJob, summary: &Value) -> FidelityObservation {
    let rendered = penelope_context::compaction::render_summary(
        summary,
        &penelope_context::anchors::render(&job.anchors),
        &job.verbatim_users,
    );
    let normalised_rendered = normalise_evidence(&rendered);
    let mut user_text = String::new();
    let mut actions = BTreeMap::<String, String>::new();
    let mut in_user = false;
    for raw in job.source_text.lines() {
        let text = if raw.starts_with("[UTILISATEUR #") {
            in_user = true;
            raw.split_once("] ").map(|(_, text)| text).unwrap_or("")
        } else if ["[ASSISTANT #", "[OUTIL #", "[SYSTÈME #"]
            .iter()
            .any(|prefix| raw.starts_with(prefix))
        {
            in_user = false;
            ""
        } else {
            raw
        };
        if !in_user {
            continue;
        }
        user_text.push_str(text);
        user_text.push('\n');
        let trimmed = text
            .trim_start()
            .trim_start_matches(['-', '*', ' '])
            .trim_start();
        let action = if let Some(rest) = trimmed.strip_prefix("[ ]") {
            Some(rest)
        } else {
            let lower = trimmed.to_lowercase();
            ["todo:", "à faire:", "a faire:"]
                .iter()
                .find_map(|prefix| lower.starts_with(prefix).then(|| &trimmed[prefix.len()..]))
        };
        if let Some(action) = action {
            let action = action.trim();
            if !action.is_empty() {
                actions
                    .entry(normalise_evidence(action))
                    .or_insert_with(|| action.to_string());
            }
        }
    }
    let missing_actions: Vec<&String> = actions
        .iter()
        .filter_map(|(normalised, original)| {
            (!normalised_rendered.contains(normalised)).then_some(original)
        })
        .collect();
    let identifiers: BTreeSet<String> = penelope_context::anchors::extract(&user_text)
        .into_iter()
        .filter(|a| a.kind != penelope_context::AnchorKind::Error)
        .map(|a| a.value)
        .collect();
    let missing_identifiers: Vec<&String> = identifiers
        .iter()
        .filter(|id| !rendered.contains(id.as_str()))
        .collect();
    FidelityObservation {
        actions_total: actions.len(),
        actions_missing: missing_actions.len(),
        action_samples: missing_actions
            .into_iter()
            .take(3)
            .map(|s| s.chars().take(80).collect())
            .collect(),
        identifiers_total: identifiers.len(),
        identifiers_missing: missing_identifiers.len(),
        identifier_samples: missing_identifiers
            .into_iter()
            .take(3)
            .map(|s| s.chars().take(80).collect())
            .collect(),
    }
}

fn normalise_evidence(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
