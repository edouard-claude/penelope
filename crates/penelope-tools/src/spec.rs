//! Catalogue des outils natifs (§11) : nom, risque, schéma, annotations.
//!
//! La liste est **fixe et triée** : elle est identique d'un tour à l'autre, ce qui est la
//! condition du préfixe stable (§5.2).

use penelope_kernel::risk::RiskClass;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub risk: RiskClass,
    pub description: &'static str,
    pub schema: Value,
    /// Déclaré idempotent : un effet `unknown` peut être relancé sans HITL (§4.2).
    pub idempotent: bool,
    /// Atteint le réseau : contamine la provenance du tour (§6.5).
    pub network: bool,
    /// Disponible uniquement dans un run de workflow.
    pub workflow_only: bool,
}

impl ToolSpec {
    pub const fn new(
        name: &'static str,
        risk: RiskClass,
        description: &'static str,
        schema: Value,
    ) -> Self {
        ToolSpec {
            name,
            risk,
            description,
            schema,
            idempotent: false,
            network: false,
            workflow_only: false,
        }
    }
}

/// Champ d'intention des appels qui peuvent demander l'approbation du propriétaire : une
/// phrase, montrée en tête de sa carte (issue #116).
pub const WHY_FIELD: &str = "pourquoi";

fn spec(
    name: &'static str,
    risk: RiskClass,
    description: &'static str,
    mut schema: Value,
    idempotent: bool,
    network: bool,
    workflow_only: bool,
) -> ToolSpec {
    if risk != RiskClass::Read
        && let Some(props) = schema.get_mut("properties").and_then(|p| p.as_object_mut())
    {
        // Sans description : la règle du harnais l'explique une fois pour tous les outils,
        // les schémas envoyés à chaque appel restent courts (#104).
        props.insert(WHY_FIELD.into(), json!({"type": "string"}));
    }
    ToolSpec {
        name,
        risk,
        description,
        schema,
        idempotent,
        network,
        workflow_only,
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

/// Tous les outils natifs du §11, triés par nom.
pub fn all() -> Vec<ToolSpec> {
    let mut v = [
        system::files(),
        system::shell(),
        system::git(),
        system::network(),
        system::time(),
        conversation::channel(),
        conversation::memory(),
        conversation::jobs(),
        conversation::history(),
        agent::skills_mcp(),
        agent::workflows(),
        agent::agent(),
        agent::images(),
        agent::self_knowledge(),
    ]
    .concat();
    v.sort_by_key(|s| s.name);
    v
}

/// Outils natifs **à la demande** (issue #104) : hors de la liste envoyée à chaque appel
/// de conversation, trouvés par `tool_search`, décrits par `tool_describe`, appelés par
/// `tool_call`, puis exposés directement à la session qui s'en sert. Le noyau restant
/// (17 outils d'usage courant et les trois méta-outils) tient sous 20 définitions.
pub const ON_DEMAND: &[&str] = &[
    "config_set",
    "git_branch",
    "git_clone",
    "git_commit",
    "git_diff",
    "git_push",
    "git_status",
    "history_describe",
    "history_expand",
    "history_expand_query",
    "image_generate",
    "image_inspect",
    "intent_cancel",
    "intent_create",
    "intent_list",
    "job_cancel",
    "job_list",
    "job_status",
    "job_wait",
    "mem_forget",
    "mem_get",
    "mem_neighbors",
    "mem_remember",
    "schedule_create",
    "schedule_delete",
    "schedule_list",
    "schedule_move",
    "self_docs",
    "send_file",
    "send_voice",
    "session_metadata",
    "session_notes",
    "skill_load",
    "skill_patch",
    "skill_propose",
    "skill_search",
    "workflow_author",
    "workflow_control",
    "workflow_describe",
    "workflow_list",
    "workflow_plan",
    "workflow_status",
    "workflow_start",
];

/// Vrai pour un outil natif à la demande.
pub fn is_on_demand(name: &str) -> bool {
    ON_DEMAND.contains(&name)
}

/// Noyau exposé à chaque appel de conversation : ni outils de workflow, ni outils à la
/// demande.
pub fn core_exposed() -> Vec<ToolSpec> {
    all()
        .into_iter()
        .filter(|s| !s.workflow_only && !is_on_demand(s.name))
        .collect()
}

/// Racines de mots d'un texte, pour la recherche lexicale des outils à la demande :
/// minuscules, sans accents, cinq premières lettres des mots d'au moins quatre lettres.
fn stems(text: &str) -> Vec<String> {
    let folded: String = text
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect();
    let mut out: Vec<String> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .map(|w| w.chars().take(5).collect())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Outils à la demande qui correspondent à une requête, du plus pertinent au moins
/// pertinent (nombre de racines communes entre la requête et le nom plus la description).
pub fn search_on_demand(query: &str, limit: usize) -> Vec<ToolSpec> {
    let wanted = stems(query);
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, ToolSpec)> = all()
        .into_iter()
        .filter(|s| is_on_demand(s.name))
        .filter_map(|s| {
            let have = stems(&format!("{} {}", s.name.replace('_', " "), s.description));
            let n = wanted.iter().filter(|w| have.contains(w)).count();
            (n > 0).then_some((n, s))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(b.1.name)));
    scored.into_iter().take(limit).map(|(_, s)| s).collect()
}

/// Les outils exposés **en permanence** au modèle : liste fixe, triée, identique d'un tour
/// à l'autre (§5.2).
pub fn always_exposed(in_workflow: bool) -> Vec<ToolSpec> {
    all()
        .into_iter()
        .filter(|s| in_workflow || !s.workflow_only)
        .collect()
}

pub fn get(name: &str) -> Option<ToolSpec> {
    all().into_iter().find(|s| s.name == name)
}

mod agent;
mod conversation;
mod system;

#[cfg(test)]
mod tests;
