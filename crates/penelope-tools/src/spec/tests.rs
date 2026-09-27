use super::*;

#[test]
fn every_prd_tool_is_present() {
    let names: Vec<&str> = all().iter().map(|s| s.name).collect();
    for expected in [
        "fs_read",
        "fs_list",
        "fs_search",
        "fs_write",
        "fs_edit",
        "shell_exec",
        "git_status",
        "git_diff",
        "git_branch",
        "git_commit",
        "git_clone",
        "git_push",
        "http_fetch",
        "time_now",
        "schedule_create",
        "schedule_list",
        "schedule_delete",
        "schedule_move",
        "send_file",
        "send_message",
        "mem_search",
        "mem_get",
        "mem_neighbors",
        "mem_note",
        "session_notes",
        "mem_remember",
        "mem_forget",
        "intent_create",
        "intent_list",
        "intent_cancel",
        "history_grep",
        "history_describe",
        "history_expand",
        "history_expand_query",
        "artifact_read",
        "skill_search",
        "skill_load",
        "skill_propose",
        "skill_patch",
        "workflow_list",
        "workflow_describe",
        "workflow_plan",
        "workflow_start",
        "workflow_status",
        "workflow_control",
        "workflow_author",
        "sub_agent_spawn",
        "session_metadata",
        "step_done",
        "return_value",
        "ask_user",
        "image_generate",
        "image_inspect",
        "self_status",
        "self_docs",
        "config_set",
        "job_status",
        "job_wait",
        "job_cancel",
        "job_list",
    ] {
        assert!(names.contains(&expected), "outil manquant : {expected}");
    }
}

/// #204 : les deux outils qui peuvent immobiliser un tour savent rendre la main, et
/// les outils de suivi restent à la demande — `job_cancel` seul écrit.
#[test]
fn long_tools_can_be_backgrounded_and_jobs_are_followed_on_demand() {
    for n in ["shell_exec", "sub_agent_spawn"] {
        let s = get(n).unwrap();
        assert_eq!(
            s.schema["properties"]["background"]["type"], "boolean",
            "{n} doit accepter `background`"
        );
    }
    for n in ["job_status", "job_wait", "job_list"] {
        assert_eq!(get(n).unwrap().risk, RiskClass::Read, "{n}");
        assert!(is_on_demand(n), "{n} doit rester à la demande (#104)");
    }
    assert_eq!(get("job_cancel").unwrap().risk, RiskClass::Write);
    assert!(is_on_demand("job_cancel"));
    // `job_wait` est borné : le tour ne s'y perd pas.
    let wait = get("job_wait").unwrap();
    assert_eq!(wait.schema["properties"]["timeout_ms"]["maximum"], 120000);
}

#[test]
fn the_list_is_sorted_and_stable() {
    let a: Vec<&str> = all().iter().map(|s| s.name).collect();
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(
        a, sorted,
        "la liste doit être triée pour stabiliser le préfixe"
    );
    let b: Vec<&str> = all().iter().map(|s| s.name).collect();
    assert_eq!(a, b, "deux appels donnent exactement la même liste");
}

#[test]
fn no_duplicate_names() {
    let mut names: Vec<&str> = all().iter().map(|s| s.name).collect();
    let n = names.len();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), n);
}

#[test]
fn risk_classes_follow_the_prd_table() {
    assert_eq!(get("fs_read").unwrap().risk, RiskClass::Read);
    assert_eq!(get("fs_write").unwrap().risk, RiskClass::Write);
    assert_eq!(get("shell_exec").unwrap().risk, RiskClass::Write);
    assert_eq!(get("git_push").unwrap().risk, RiskClass::External);
    assert_eq!(get("http_fetch").unwrap().risk, RiskClass::External);
    assert_eq!(get("mem_forget").unwrap().risk, RiskClass::Destructive);
}

#[test]
fn network_tools_are_marked_for_contamination() {
    for n in ["http_fetch", "git_clone", "git_push", "image_generate"] {
        assert!(get(n).unwrap().network, "{n} doit être marqué réseau");
    }
    for n in ["fs_read", "shell_exec", "mem_search"] {
        assert!(
            !get(n).unwrap().network,
            "{n} ne doit pas être marqué réseau"
        );
    }
}

/// #104 : le noyau tient sous 20 définitions avec les trois méta-outils, et chaque
/// outil à la demande existe.
#[test]
fn the_core_is_small_and_on_demand_tools_exist() {
    let core = core_exposed();
    assert!(core.len() + 3 <= 20, "{} outils dans le noyau", core.len());
    for n in ON_DEMAND {
        assert!(get(n).is_some(), "{n} n'existe pas");
    }
    assert!(core.iter().any(|s| s.name == "shell_exec"));
    assert!(!core.iter().any(|s| s.name == "schedule_create"));
}

/// #104 : la recherche lexicale trouve l'outil rare par ce qu'il fait.
#[test]
fn rare_tools_are_found_by_what_they_do() {
    let first = |q: &str| search_on_demand(q, 5).first().map(|s| s.name);
    assert_eq!(first("planifier un rappel"), Some("schedule_create"));
    assert!(
        search_on_demand("pousser la branche sur le dépôt distant", 5)
            .iter()
            .any(|s| s.name == "git_push")
    );
    assert!(search_on_demand("zz", 5).is_empty());
}

#[test]
fn read_tools_are_idempotent() {
    for s in all() {
        if s.risk == RiskClass::Read && !matches!(s.name, "ask_user" | "history_expand_query") {
            assert!(s.idempotent, "{} devrait être idempotent", s.name);
        }
    }
}

#[test]
fn workflow_only_tools_are_hidden_outside_runs() {
    let chat: Vec<&str> = always_exposed(false).iter().map(|s| s.name).collect();
    assert!(!chat.contains(&"step_done"));
    assert!(!chat.contains(&"return_value"));
    let run: Vec<&str> = always_exposed(true).iter().map(|s| s.name).collect();
    assert!(run.contains(&"step_done"));
}

#[test]
fn every_schema_is_a_valid_object_schema() {
    for s in all() {
        assert_eq!(s.schema["type"], "object", "{}", s.name);
        // Le schéma doit accepter un objet vide ou refuser proprement, jamais paniquer.
        let _ = penelope_kernel::schema::validate(&s.schema, &json!({}));
        assert!(!s.description.is_empty(), "{} sans description", s.name);
    }
}

#[test]
fn required_fields_are_enforced() {
    let s = get("fs_read").unwrap();
    assert!(penelope_kernel::schema::validate(&s.schema, &json!({"path":"a.rs"})).is_empty());
    assert!(!penelope_kernel::schema::validate(&s.schema, &json!({})).is_empty());
    assert!(
        !penelope_kernel::schema::validate(&s.schema, &json!({"path":"a","inconnu":1})).is_empty(),
        "additionalProperties: false doit rejeter les champs inconnus"
    );
}
