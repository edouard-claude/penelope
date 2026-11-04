//! `backlog` et `story` (#338, #425) : dérouler une liste de tâches, dans l'ordre, une
//! tâche après l'autre.
//!
//! Le tracker est un paramètre (ClickUp, Redmine, issues GitHub ou fichier) : la première
//! étape lit la liste par les outils du tracker (MCP) ou dans le fichier, sans code propre
//! à aucun d'eux, et la rend en JSON ; l'étape `foreach` la fige puis appelle `story` pour
//! chaque élément. `story` confie la tâche à une **mission** (décision 0025) : le workflow
//! garde l'extérieur (la liste, l'ordre, les arrêts de fin d'épique), la mission tient la
//! boucle du code jusqu'à son critère de fin. `tache`, l'ancienne suite d'étapes `agent`
//! qui tournait entre développement, tests et revue (#348, #354, #362), n'est plus livrée.

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

/// `backlog` : lit la liste, la fige, la déroule avec `story` (une mission par tâche),
/// point d'arrêt à chaque fin d'épique.
pub fn backlog() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "backlog".into(),
            name: "Dérouler un backlog".into(),
            description: "Lit une liste de tâches (ClickUp, Redmine, GitHub, fichier) et la \
                          déroule dans l'ordre, une mission par tâche, point d'arrêt à chaque \
                          fin d'épique."
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
            // Lancé le matin pour la semaine : il ne s'arrête qu'aux fins d'épique, sur
            // un élément en échec ou faute de crédits, jamais sur une approbation (#346).
            autonomous: true,
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
                workflow_id: "story".into(),
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
                    identifiant {{item.id}}, tracker {{tracker}}).";

/// Budget d'une mission par tâche, en dollars.
const STORY_USD: f64 = 20.0;

/// `story` : une tâche du backlog confiée à une mission, dans le dépôt.
pub fn story() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "story".into(),
            name: "Tâche du backlog, en mission".into(),
            description: "Confie une tâche du backlog à une mission dans le dépôt : code, \
                          tests, commit et push, statut du tracker ; la mission tient la \
                          boucle jusqu'à sa fin vérifiée."
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
        entry_step: "mission".into(),
        settings: Settings {
            max_iterations: 4,
            budget: Budget {
                max_usd: STORY_USD,
                max_tokens: 0,
                max_wall_ms: 0,
                max_cached_tokens: 0,
            },
            ..Settings::default()
        },
        start_condition: json!({"type":"always"}),
        steps: vec![Step {
            prompt: format!(
                "{ITEM}\n\nRéalise cette tâche dans le dépôt : lis ses règles (CLAUDE.md, \
                 AGENTS.md), écris le code et ses tests, fais passer les tests du dépôt, \
                 commite sur une branche de travail et pousse si le dépôt a un remote. Mets \
                 ensuite son statut à jour dans {{{{tracker}}}} (ses outils, ou le fichier \
                 {{{{source}}}}). Ne touche à aucune autre tâche de la liste."
            ),
            cwd: "{{depot}}".into(),
            budget: Some(Budget {
                max_usd: STORY_USD,
                ..Budget::default()
            }),
            transitions: vec![
                Transition::on_result(DONE, "success"),
                Transition {
                    goto: BLOCKED.into(),
                    condition: json!({"type":"always"}),
                    tag: "la mission n'a pas fini : la liste s'arrête".into(),
                },
            ],
            ..step("mission", "mission", Phase::Build)
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #425 : `backlog` délègue chaque tâche à `story`, qui la confie à une mission dans
    /// le dépôt, avec son budget ; une mission qui ne finit pas arrête la liste.
    #[test]
    fn each_task_of_the_backlog_is_a_mission() {
        let b = backlog();
        let each = b.steps.iter().find(|s| s.kind == "foreach").unwrap();
        assert_eq!(each.workflow_id, "story");
        let w = story();
        assert_eq!(w.steps.len(), 1);
        let m = &w.steps[0];
        assert_eq!(m.kind, "mission");
        assert_eq!(m.cwd, "{{depot}}");
        assert!(m.prompt.contains("{{item.title}}") && m.prompt.contains("{{tracker}}"));
        assert_eq!(m.budget.as_ref().unwrap().max_usd, STORY_USD);
        assert_eq!(m.transitions[0].goto, DONE);
        assert_eq!(m.transitions[1].goto, BLOCKED);
    }
}
