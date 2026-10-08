//! `backlog` et `tache` (#338) : dérouler une liste de tâches, dans l'ordre, une tâche
//! après l'autre, chacune de la spécification au statut du tracker.
//!
//! Le tracker est un paramètre (ClickUp, Redmine, issues GitHub ou fichier) : la première
//! étape lit la liste par les outils du tracker (MCP) ou dans le fichier, sans code propre
//! à aucun d'eux, et la rend en JSON ; l'étape `foreach` la fige puis appelle `tache` pour
//! chaque élément.

use super::step;
use crate::model::*;
use serde_json::json;

fn param(id: &str, label: &str, required: bool, default: Option<&str>) -> Parameter {
    Parameter {
        id: id.into(),
        label: label.into(),
        kind: "string".into(),
        required,
        default: default.map(|d| json!(d)),
        ..Default::default()
    }
}

/// `backlog` : lit la liste, la fige, la déroule avec `tache`, point d'arrêt à chaque fin
/// d'épique.
pub fn backlog() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "backlog".into(),
            name: "Dérouler un backlog".into(),
            description: "Lit une liste de tâches (ClickUp, Redmine, GitHub, fichier) et la \
                          déroule dans l'ordre avec `tache`, point d'arrêt à chaque fin \
                          d'épique."
                .into(),
            parameters: vec![
                param("tracker", "Tracker", false, Some("fichier")),
                param("source", "Liste, requête ou fichier", true, None),
                param("filtre", "Filtre", false, Some("tâches à faire")),
                param("depot", "Dépôt", false, Some(".")),
            ],
            ..Default::default()
        },
        entry_step: "lister".into(),
        settings: Settings {
            max_iterations: 10,
            budget: Budget {
                max_usd: 200.0,
                max_tokens: 0,
                max_wall_ms: 0,
                max_cached_tokens: 0,
            },
            workspace: "persistent:backlog".into(),
            ..Settings::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                prompt: "Lis la liste de tâches à dérouler dans {{tracker}} : {{source}} \
                         ({{filtre}}). Avec `fichier`, c'est un fichier du dépôt {{depot}} ; \
                         sinon, passe par les outils du tracker (`tool_search`). Ne modifie \
                         rien. Garde l'ordre de travail (épiques puis stories, par priorité). \
                         Rends la liste par `return_value(result=\"success\", content=[{\"id\": \
                         …, \"title\": …, \"url\": …, \"epic\": …}, …])`, puis `step_done()`."
                    .into(),
                context: "fresh".into(),
                max_calls: Some(40),
                transitions: vec![Transition::always("derouler")],
                ..step("lister", "agent", Phase::Plan)
            },
            Step {
                items: json!({"step": "lister", "path": "content"}),
                workflow_id: "tache".into(),
                params: json!({"tracker": "{{tracker}}", "source": "{{source}}",
                               "depot": "{{depot}}"}),
                on_error: "stop".into(),
                pause_after: json!({"changes": "epic"}),
                item_noun: "tâche".into(),
                item_label: "{{item.title}}".into(),
                transitions: vec![
                    Transition::on_result(DONE, "success"),
                    Transition::on_result(DONE, "partial"),
                    Transition {
                        goto: BLOCKED.into(),
                        condition: json!({"type":"always"}),
                        tag: "une tâche en échec arrête la liste".into(),
                    },
                ],
                ..step("derouler", "foreach", Phase::Build)
            },
        ],
    }
}

const ITEM: &str = "Tâche {{item_index}}/{{item_total}} : {{item.title}} ({{item.url}}, \
                    identifiant {{item.id}}, tracker {{tracker}}). Dépôt : {{depot}}.";

/// `tache` : une tâche du backlog, de la spécification au statut du tracker.
pub fn task() -> Workflow {
    let agent = |id: &str, phase: Phase, prompt: String, next: Vec<Transition>| Step {
        prompt,
        transitions: next,
        ..step(id, "agent", phase)
    };
    let passed_or =
        |ok: &str, ko: &str| vec![Transition::on_result(ok, "passed"), Transition::always(ko)];
    Workflow {
        metadata: Metadata {
            id: "tache".into(),
            name: "Tâche du backlog".into(),
            description: "Spécification, tests, développement, tests locaux, revue adverse, \
                          commit et push, statut du tracker."
                .into(),
            parameters: vec![
                Parameter {
                    id: "item".into(),
                    label: "Tâche".into(),
                    kind: "object".into(),
                    required: true,
                    ..Default::default()
                },
                param("item_index", "Rang", false, Some("1")),
                param("item_total", "Total", false, Some("1")),
                param("tracker", "Tracker", false, Some("fichier")),
                param("source", "Liste, requête ou fichier", false, Some("")),
                param("depot", "Dépôt", false, Some(".")),
            ],
            ..Default::default()
        },
        entry_step: "spec".into(),
        settings: Settings {
            max_iterations: 30,
            budget: Budget {
                max_usd: 10.0,
                max_tokens: 4_000_000,
                max_wall_ms: 0,
                max_cached_tokens: 0,
            },
            ..Settings::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![
            agent(
                "spec",
                Phase::Plan,
                format!(
                    "{ITEM}\nLis la tâche dans son tracker et le code concerné. Écris une \
                     spécification courte : comportement attendu, critères d'acceptation \
                     vérifiables, fichiers touchés. Pose les critères : `session_metadata` \
                     op=set key=criteria entry=[{{\"id\": \"…\", \"text\": \"…\", \"status\": \
                     \"pending\"}}, …]. Puis `step_done()`."
                ),
                vec![Transition::always("tests")],
            ),
            agent(
                "tests",
                Phase::Build,
                "Écris d'abord les tests qui traduisent les critères, sans toucher au code \
                 produit, et lance-les : ils doivent échouer pour la bonne raison. Puis \
                 `step_done()`."
                    .into(),
                vec![Transition::always("dev")],
            ),
            Step {
                max_calls: Some(120),
                max_turns: Some(3),
                ..agent(
                    "dev",
                    Phase::Build,
                    "Implémente jusqu'à ce que les tests passent. Coche chaque critère rempli : \
                     `session_metadata` op=update key=criteria entry={\"id\": \"<id>\", \
                     \"status\": \"completed\"}. {{pendingCount}} critère(s) restant(s) :\n\
                     {{criteriaList}}\nCe que la dernière vérification a refusé, s'il y en a : \
                     {{reason}}. Puis `step_done()`."
                        .into(),
                    vec![Transition::always("tests-locaux")],
                )
            },
            agent(
                "tests-locaux",
                Phase::Verification,
                "Lance dans {{depot}} ce que la CI lancera : format, lint, tests complets. Tout \
                 vert : `return_value(result=\"passed\")`. Sinon `return_value(result=\
                 \"failed\", content=<l'échec, la commande et sa sortie utile>)`. Puis \
                 `step_done()`."
                    .into(),
                passed_or("revue", "dev"),
            ),
            Step {
                context: "fresh".into(),
                ..agent(
                    "revue",
                    Phase::Verification,
                    format!(
                        "{ITEM}\nTu fais la revue adverse de ce changement : tu ne l'as pas \
                         écrit et tu cherches ce qui casse. Lis le diff (`git diff` depuis la \
                         branche de départ), les tests ajoutés et les critères. Bugs, \
                         régressions, failles, cas limites non testés : rends \
                         `return_value(result=\"failed\", content=<les corrections, chacune \
                         avec fichier et raison>)`. Rien de bloquant : \
                         `return_value(result=\"passed\")`. Puis `step_done()`."
                    ),
                    passed_or("commit", "dev"),
                )
            },
            agent(
                "commit",
                Phase::Deploy,
                "Commite le changement (message : ce qui change et pourquoi, avec \
                 l'identifiant {{item.id}}) et pousse la branche. Rends \
                 `return_value(result=\"success\", content=<SHA court>)`, puis `step_done()`."
                    .into(),
                vec![Transition::always("statut")],
            ),
            agent(
                "statut",
                Phase::Done,
                "Passe la tâche {{item.id}} à l'état « fait » dans {{tracker}}, avec un \
                 commentaire court : ce qui a été fait et le commit {{steps.commit.content}}. \
                 Avec `fichier`, coche sa case dans {{source}}. Puis `step_done()`."
                    .into(),
                vec![Transition::always(DONE)],
            ),
        ],
    }
}
