use super::*;
use serde_json::json;

mod refusals;

fn known() -> Known {
    Known {
        workflow_ids: ["deploy-generic".to_string()].into_iter().collect(),
        model_aliases: ["main", "fast", "reasoning", "code"]
            .into_iter()
            .map(String::from)
            .collect(),
        native_tools: ["fs_read", "shell_exec", "git_push"]
            .into_iter()
            .map(String::from)
            .collect(),
        mcp_tools: ["mcp__redmine__get_issue".to_string()]
            .into_iter()
            .collect(),
        templates: ["question", "plan_proposal", "deploy_gate"]
            .into_iter()
            .map(String::from)
            .collect(),
        max_depth: 3,
    }
}

fn base() -> Workflow {
    Workflow {
        metadata: Metadata {
            id: "demo".into(),
            name: "Démo".into(),
            parameters: vec![Parameter {
                id: "cible".into(),
                label: "Cible".into(),
                kind: "string".into(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        entry_step: "un".into(),
        settings: Settings::default(),
        start_condition: json!({"type":"always"}),
        steps: vec![
            Step {
                id: "un".into(),
                kind: "shell".into(),
                command: json!("echo {{cible}}"),
                transitions: vec![Transition::always("deux")],
                ..Default::default()
            },
            Step {
                id: "deux".into(),
                kind: "tool".into(),
                tool: "fs_read".into(),
                args: json!({"path":"{{workdir}}/a.rs"}),
                transitions: vec![Transition::always(DONE)],
                ..Default::default()
            },
        ],
    }
}

/// Sous-groupes : une boucle ne se quitte que par une transition taguée.
#[test]
fn a_subgroup_is_left_only_through_a_tagged_transition() {
    let mut w = base();
    w.steps[0].sub_group = "boucle".into();
    w.steps[1].sub_group = "boucle".into();
    w.steps[1].transitions = vec![
        Transition {
            tag: "vert".into(),
            ..Transition::on_result(DONE, "success")
        },
        Transition::always("un"),
    ];
    let r = validate(&w, Some("demo"), &known());
    assert!(r.is_valid(), "{}", r.render());
    assert_eq!(escape_tag(&w, &w.steps[1], DONE), Some("vert"));
    assert_eq!(
        escape_tag(&w, &w.steps[1], "un"),
        None,
        "reste dans la boucle"
    );

    let mut untagged = w.clone();
    untagged.steps[1].transitions[0].tag.clear();
    let r = validate(&untagged, Some("demo"), &known());
    let text = r.render();
    assert!(text.contains("sans `tag`"), "{text}");
    assert!(text.contains("aucune sortie taguée"), "{text}");

    let mut useless = w.clone();
    useless.steps[1].transitions[1].tag = "encore".into();
    let r = validate(&useless, Some("demo"), &known());
    assert!(r.is_valid(), "{}", r.render());
    assert!(r.render().contains("sans effet"));

    // `$blocked` reste une issue implicite, sans tag.
    let mut blocked = w.clone();
    blocked.steps[0].transitions = vec![
        Transition::on_result(BLOCKED, "failure"),
        Transition::always("deux"),
    ];
    assert!(validate(&blocked, Some("demo"), &known()).is_valid());
}

#[test]
fn a_valid_workflow_passes() {
    let r = validate(&base(), Some("demo"), &known());
    assert!(r.is_valid(), "{}", r.render());
}

#[test]
fn id_must_equal_the_file_stem() {
    let r = validate(&base(), Some("autre-nom"), &known());
    assert!(!r.is_valid());
    assert!(r.render().contains("nom du fichier"));
    // Le suffixe `.workflow` est toléré.
    assert!(validate(&base(), Some("demo.workflow"), &known()).is_valid());
}

#[test]
fn duplicate_ids_are_rejected() {
    let mut w = base();
    w.steps[1].id = "un".into();
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("dupliqué"));
}

#[test]
fn unknown_goto_is_rejected() {
    let mut w = base();
    w.steps[0].transitions = vec![Transition::always("nexistepas")];
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("cible inconnue"));
}

#[test]
fn missing_entry_step() {
    let mut w = base();
    w.entry_step = "absent".into();
    assert!(
        validate(&w, Some("demo"), &known())
            .render()
            .contains("entryStep")
    );
}

#[test]
fn done_must_be_reachable() {
    let mut w = base();
    w.steps[1].transitions = vec![Transition::always("un")];
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("$done"), "{}", r.render());
}

#[test]
fn always_must_be_last() {
    let mut w = base();
    w.steps[0].transitions = vec![
        Transition::always("deux"),
        Transition::on_result("un", "failure"),
    ];
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("dernière"), "{}", r.render());
}

#[test]
fn user_steps_may_omit_always_if_choices_are_exhaustive() {
    let mut w = base();
    w.steps[0] = Step {
        id: "un".into(),
        kind: "user".into(),
        template: "question".into(),
        choices: vec!["Oui".into(), "Non".into()],
        transitions: vec![
            Transition::on_result("deux", "Oui"),
            Transition::on_result(DONE, "Non"),
        ],
        ..Default::default()
    };
    assert!(validate(&w, Some("demo"), &known()).is_valid());

    // Un choix non couvert est une erreur.
    w.steps[0].choices.push("Peut-être".into());
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("Peut-être"));
}

#[test]
fn parallel_children_are_restricted() {
    let mut w = base();
    w.steps[0] = Step {
        id: "un".into(),
        kind: "parallel".into(),
        children: vec![
            Step {
                id: "tests".into(),
                kind: "shell".into(),
                command: json!("cargo test"),
                ..Default::default()
            },
            Step {
                id: "demande".into(),
                kind: "user".into(),
                template: "question".into(),
                choices: vec!["Oui".into()],
                ..Default::default()
            },
        ],
        transitions: vec![Transition::always("deux")],
        ..Default::default()
    };
    let r = validate(&w, Some("demo"), &known());
    assert!(
        r.render().contains("interdit comme enfant"),
        "{}",
        r.render()
    );
}

#[test]
fn unknown_tool_model_or_template() {
    let mut w = base();
    w.steps[1].tool = "fs_inexistant".into();
    assert!(
        validate(&w, Some("demo"), &known())
            .render()
            .contains("outil natif inconnu")
    );

    let mut w = base();
    w.steps[0].model = "alias-inconnu".into();
    assert!(
        validate(&w, Some("demo"), &known())
            .render()
            .contains("alias de modèle")
    );
}

#[test]
fn unavailable_mcp_tool_is_only_a_warning() {
    let mut w = base();
    w.steps[1].tool = "mcp__forge__create_pr".into();
    let r = validate(&w, Some("demo"), &known());
    assert!(r.is_valid(), "{}", r.render());
    assert!(
        r.warnings()
            .iter()
            .any(|i| i.message.contains("indisponible"))
    );
}

#[test]
fn unknown_template_variable_is_an_error() {
    let mut w = base();
    w.steps[0].command = json!("echo {{variable_inventee}}");
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("variable inconnue"), "{}", r.render());
}

#[test]
fn dynamic_variables_are_accepted() {
    let mut w = base();
    w.steps[1].args = json!({"path":"{{stepOutput.path}}"});
    assert!(validate(&w, Some("demo"), &known()).is_valid());
}

#[test]
fn missing_command_for_declared_platform() {
    let mut w = base();
    w.metadata.platforms = vec!["windows".into()];
    w.steps[0].command = json!({"unix":"echo {{cible}}"});
    let r = validate(&w, Some("demo"), &known());
    assert!(
        r.render().contains("aucune commande pour `windows`"),
        "{}",
        r.render()
    );
}

#[test]
fn budget_is_mandatory() {
    let mut w = base();
    w.settings.budget = Budget {
        max_usd: 0.0,
        max_tokens: 0,
        max_wall_ms: 0,
    };
    assert!(
        validate(&w, Some("demo"), &known())
            .render()
            .contains("budget")
    );
}

#[test]
fn self_referencing_subworkflow_is_a_cycle() {
    let mut w = base();
    w.steps[1] = Step {
        id: "deux".into(),
        kind: "workflow".into(),
        workflow_id: "demo".into(),
        transitions: vec![Transition::always(DONE)],
        ..Default::default()
    };
    let r = validate(&w, Some("demo"), &known());
    assert!(r.render().contains("cycle"), "{}", r.render());
}

#[test]
fn metadata_key_never_written_is_a_warning() {
    let mut w = base();
    w.steps[0].transitions = vec![
        Transition {
            goto: "deux".into(),
            condition: json!({
                "type":"metadata_all_in","key":"criteria","field":"status",
                "values":["completed"]
            }),
            tag: String::new(),
        },
        Transition::always("deux"),
    ];
    let r = validate(&w, Some("demo"), &known());
    assert!(r.is_valid(), "{}", r.render());
    assert!(
        r.warnings().iter().any(|i| i.message.contains("vacuité")),
        "{}",
        r.render()
    );
}

#[test]
fn issue_paths_point_to_the_offender() {
    let mut w = base();
    w.steps[1].tool = String::new();
    let r = validate(&w, Some("demo"), &known());
    assert!(r.errors().iter().any(|i| i.path == "/steps/1/tool"));
}

#[test]
fn wait_steps_need_a_trigger() {
    let mut w = base();
    w.steps[1] = Step {
        id: "deux".into(),
        kind: "wait".into(),
        on: json!({}),
        transitions: vec![Transition::always(DONE)],
        ..Default::default()
    };
    assert!(
        validate(&w, Some("demo"), &known())
            .render()
            .contains("attendu `event`")
    );

    w.steps[1].on = json!({"cron":"pas du cron"});
    assert!(!validate(&w, Some("demo"), &known()).is_valid());

    w.steps[1].on = json!({"cron":"*/15 * * * *"});
    assert!(validate(&w, Some("demo"), &known()).is_valid());
}
