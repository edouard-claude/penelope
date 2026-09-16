//! `schemas/workflow.schema.json` est la référence publiée du §12.2 : ce test garantit
//! qu'elle décrit bien ce que le code accepte, et refuse ce qu'il refuse.

use penelope_kernel::schema;
use serde_json::{Value, json};

fn workflow_schema() -> Value {
    let path = penelope_evals::ca_matrix::repo_root()
        .join("schemas")
        .join("workflow.schema.json");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} : {e}", path.display()));
    serde_json::from_str(&raw).expect("le schéma doit être du JSON valide")
}

#[test]
fn every_bundled_workflow_satisfies_the_published_schema() {
    let s = workflow_schema();
    for w in penelope_workflow::bundled::all() {
        let instance: Value = serde_json::from_str(&w.to_json()).unwrap();
        let errors = schema::validate(&s, &instance);
        assert!(
            errors.is_empty(),
            "`{}` ne respecte pas schemas/workflow.schema.json : {:#?}",
            w.metadata.id,
            errors
        );
    }
}

#[test]
fn the_schema_rejects_what_the_validator_rejects() {
    let s = workflow_schema();

    // Métadonnées absentes.
    assert!(!schema::validate(&s, &json!({"entryStep": "a", "steps": []})).is_empty());

    // Identifiant qui n'est pas un slug.
    let bad_id = json!({
        "metadata": {"id": "Pas Un Slug", "name": "n", "description": "d"},
        "entryStep": "a",
        "steps": [{"id": "a", "type": "agent", "prompt": "p"}]
    });
    assert!(!schema::validate(&s, &bad_id).is_empty());

    // Type d'étape inconnu.
    let bad_kind = json!({
        "metadata": {"id": "x", "name": "n", "description": "d"},
        "entryStep": "a",
        "steps": [{"id": "a", "type": "telepathie"}]
    });
    assert!(!schema::validate(&s, &bad_kind).is_empty());

    // Champ inconnu : le modèle est fermé, comme `deny_unknown_fields` côté Rust.
    let unknown_field = json!({
        "metadata": {"id": "x", "name": "n", "description": "d"},
        "entryStep": "a",
        "steps": [{"id": "a", "type": "agent", "prompt": "p", "inventé": true}]
    });
    assert!(!schema::validate(&s, &unknown_field).is_empty());

    // `workspace` hors des deux formes admises.
    let bad_workspace = json!({
        "metadata": {"id": "x", "name": "n", "description": "d"},
        "entryStep": "a",
        "settings": {"workspace": "quelque-part"},
        "steps": [{"id": "a", "type": "agent", "prompt": "p"}]
    });
    assert!(!schema::validate(&s, &bad_workspace).is_empty());

    // Condition de transition d'un type inconnu.
    let bad_condition = json!({
        "metadata": {"id": "x", "name": "n", "description": "d"},
        "entryStep": "a",
        "steps": [{
            "id": "a", "type": "agent", "prompt": "p",
            "transitions": [{"goto": "$done", "condition": {"type": "si_la_lune_est_pleine"}}]
        }]
    });
    assert!(!schema::validate(&s, &bad_condition).is_empty());
}

#[test]
fn the_schema_accepts_the_shapes_the_prd_describes() {
    let s = workflow_schema();

    // Commande shell par OS (§2.10), enfants parallèles, sous-workflow, formulaire.
    let w = json!({
        "metadata": {
            "id": "complet", "name": "Complet", "description": "Toutes les formes.",
            "platforms": ["macos", "linux"],
            "parameters": [{"id": "ticket", "type": "integer", "required": true}]
        },
        "entryStep": "verifs",
        "settings": {
            "maxIterations": 10,
            "budget": {"maxUsd": 2.0},
            "concurrency": {"maxConcurrent": 2, "admission": "coalesce"},
            "workspace": "persistent:penelope"
        },
        "startCondition": {"type": "always"},
        "steps": [
            {
                "id": "verifs", "name": "Vérifications", "type": "parallel", "phase": "verification",
                "maxConcurrency": 2,
                "children": [
                    {"id": "lint", "type": "shell",
                     "command": {"unix": "cargo clippy", "windows": "cargo clippy"}},
                    {"id": "tests", "type": "shell", "command": "cargo test",
                     "successExitCodes": [0]},
                    {"id": "relecture", "type": "sub_agent", "prompt": "Relis le diff.",
                     "model": "reasoning",
                     "outputSchema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}}
                ],
                "transitions": [
                    {"goto": "demander",
                     "condition": {"type": "metadata_all_in", "key": "criteria",
                                   "field": "status", "values": ["completed", "passed"]}},
                    {"goto": "$blocked"}
                ]
            },
            {
                "id": "demander", "type": "user", "phase": "waiting",
                "template": "tool_approval",
                "choices": ["Déployer", "Annuler"],
                "input": "form:deploiement",
                "transitions": [
                    {"goto": "deployer", "condition": {"type": "step_result", "result": "Déployer"}},
                    {"goto": "$done"}
                ]
            },
            {
                "id": "deployer", "type": "workflow", "phase": "deploy",
                "workflowId": "deploy-generic",
                "params": {"ticket": "{{ticket}}"},
                "retry": {"max": 2, "backoffMs": 5000},
                "timeoutMs": 900000,
                "transitions": [{"goto": "$done"}]
            }
        ]
    });
    let errors = schema::validate(&s, &w);
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn the_schema_itself_is_well_formed() {
    let s = workflow_schema();
    assert_eq!(s["$schema"], "https://json-schema.org/draft/2020-12/schema");
    // Toutes les références internes se résolvent : le validateur local ne va jamais
    // chercher un schéma sur le réseau (§13).
    let raw = s.to_string();
    for needle in raw.match_indices("\"$ref\":\"#/$defs/") {
        let rest = &raw[needle.0 + 16..];
        let name: String = rest.chars().take_while(|c| *c != '"').collect();
        assert!(
            s["$defs"].get(&name).is_some(),
            "référence cassée : #/$defs/{name}"
        );
    }
    assert!(!raw.contains("\"$ref\":\"http"), "aucune référence externe");
}
