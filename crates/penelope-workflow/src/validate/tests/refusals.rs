//! Un refus par règle de `validate` (§12.4) : la valeur fautive seule, le chemin JSON et
//! le message qui la désignent.

use super::*;

type Mutation = fn(&mut Workflow);

/// Remplace la seconde étape (un outil qui mène à `$done`) par `s`, transitions
/// comprises si `s` n'en a pas.
fn second(w: &mut Workflow, mut s: Step) {
    if s.transitions.is_empty() {
        s.transitions = vec![Transition::always(DONE)];
    }
    s.id = "deux".into();
    w.steps[1] = s;
}

fn step(kind: &str) -> Step {
    Step {
        kind: kind.into(),
        ..Default::default()
    }
}

#[test]
fn each_rule_names_its_path_and_its_fault() {
    let cases: &[(&str, &str, Mutation)] = &[
        ("/metadata/id", "identifiant obligatoire", |w| {
            w.metadata.id.clear()
        }),
        ("/metadata/id", "doit être un slug", |w| {
            w.metadata.id = "Démo Majuscule".into()
        }),
        ("/metadata/parameters/0/id", "identifiant vide", |w| {
            w.metadata.parameters[0].id.clear()
        }),
        (
            "/metadata/parameters/0/type",
            "type inconnu : `date`",
            |w| w.metadata.parameters[0].kind = "date".into(),
        ),
        (
            "/metadata/platforms/0",
            "plateforme inconnue : `beos`",
            |w| w.metadata.platforms = vec!["beos".into()],
        ),
        ("/settings/concurrency/admission", "valeur inconnue", |w| {
            w.settings.concurrency.admission = "queue".into()
        }),
        ("/settings/workspace", "persistent:<nom>", |w| {
            w.settings.workspace = "partagé".into()
        }),
        ("/steps/1/id", "identifiant dupliqué : `un`", |w| {
            w.steps[1].id = "un".into()
        }),
        ("/steps/1/id", "identifiant d'étape vide", |w| {
            w.steps[1].id.clear()
        }),
        ("/steps/1/id", "commencer par `$`", |w| {
            w.steps[1].id = "$deux".into()
        }),
        ("/entryStep", "ne correspond à aucune étape", |w| {
            w.entry_step = "zéro".into()
        }),
        ("/steps/1/type", "type inconnu : `robot`", |w| {
            w.steps[1].kind = "robot".into()
        }),
        ("/steps/1/prompt", "prompt vide", |w| {
            second(w, step("agent"))
        }),
        ("/steps/1/outputSchema", "schéma sans `type`", |w| {
            second(
                w,
                Step {
                    prompt: "résume".into(),
                    output_schema: Some(json!({"properties": {}})),
                    ..step("sub_agent")
                },
            )
        }),
        ("/steps/1/command", "commande absente", |w| {
            second(w, step("shell"))
        }),
        ("/steps/0/command", "aucune commande pour `windows`", |w| {
            w.metadata.platforms = vec!["windows".into()];
            w.steps[0].command = json!({"macos": "ls", "linux": "ls"});
        }),
        ("/steps/1/tool", "outil absent", |w| w.steps[1].tool.clear()),
        ("/steps/1/tool", "outil natif inconnu : `fs_brule`", |w| {
            w.steps[1].tool = "fs_brule".into()
        }),
        ("/steps/1/template", "template absent", |w| {
            second(
                w,
                Step {
                    choices: vec!["oui".into()],
                    ..step("user")
                },
            )
        }),
        ("/steps/1/template", "template inconnu : `devis`", |w| {
            second(
                w,
                Step {
                    template: "devis".into(),
                    choices: vec!["oui".into()],
                    ..step("user")
                },
            )
        }),
        ("/steps/1/choices", "doit offrir des choix", |w| {
            second(
                w,
                Step {
                    template: "question".into(),
                    ..step("user")
                },
            )
        }),
        ("/steps/1/input", "formulaire `fiche` absent", |w| {
            second(
                w,
                Step {
                    template: "question".into(),
                    choices: vec!["oui".into()],
                    input: "form:fiche".into(),
                    ..step("user")
                },
            )
        }),
        ("/settings/forms/fiche", "au moins une propriété", |w| {
            w.settings
                .forms
                .insert("fiche".into(), json!({"type": "object", "properties": {}}));
            second(
                w,
                Step {
                    template: "question".into(),
                    choices: vec!["oui".into()],
                    input: "form:fiche".into(),
                    ..step("user")
                },
            )
        }),
        ("/steps/1/input", "attendu `none`, `text`", |w| {
            second(
                w,
                Step {
                    template: "question".into(),
                    choices: vec!["oui".into()],
                    input: "voix".into(),
                    ..step("user")
                },
            )
        }),
        ("/steps/1/children", "aucun enfant", |w| {
            second(w, step("parallel"))
        }),
        ("/steps/1/workflowId", "workflow inconnu : `absent`", |w| {
            second(
                w,
                Step {
                    workflow_id: "absent".into(),
                    ..step("workflow")
                },
            )
        }),
        ("/steps", "s'appelle lui-même", |w| {
            second(
                w,
                Step {
                    workflow_id: "demo".into(),
                    ..step("workflow")
                },
            )
        }),
        ("/steps/1/on", "attendu `event`, `cron`", |w| {
            second(w, step("wait"))
        }),
        ("/steps/1/on/cron", "", |w| {
            second(
                w,
                Step {
                    on: json!({"cron": "pas un cron"}),
                    ..step("wait")
                },
            )
        }),
        (
            "/steps/1/verifier",
            "vérificateur ou des contrôles",
            |w| second(w, step("verify")),
        ),
        (
            "/steps/1/model",
            "alias de modèle inconnu : `géant`",
            |w| w.steps[1].model = "géant".into(),
        ),
        ("/steps/1/transitions", "aucune transition", |w| {
            w.steps[1].transitions.clear()
        }),
        (
            "/steps/1/transitions/0/goto",
            "cible inconnue : `nulle-part`",
            |w| w.steps[1].transitions = vec![Transition::always("nulle-part")],
        ),
        (
            "/steps/1/transitions/0/condition",
            "condition sans `type`",
            |w| w.steps[1].transitions[0].condition = json!({"key": "x"}),
        ),
    ];
    for (path, want, mutate) in cases {
        let mut w = base();
        mutate(&mut w);
        let r = validate(&w, None, &known());
        assert!(
            r.errors()
                .iter()
                .any(|i| i.path == *path && i.message.contains(want)),
            "attendu {path} « {want} », reçu : {:?}",
            r.errors().iter().map(|i| i.to_string()).collect::<Vec<_>>()
        );
    }
}

/// Le nom du fichier fait foi pour l'identifiant.
#[test]
fn the_file_name_must_match_the_id() {
    let r = validate(&base(), Some("autre.workflow"), &known());
    assert!(
        r.errors()
            .iter()
            .any(|i| i.message.contains("égal au nom du fichier `autre`")),
        "{:?}",
        r.errors()
    );
    assert!(
        validate(&base(), Some("demo.workflow"), &known())
            .errors()
            .is_empty()
    );
}

/// Un outil MCP absent n'est qu'un avertissement : il peut revenir ; un sous-workflow
/// rappelle la borne d'imbrication.
#[test]
fn missing_mcp_tools_and_subworkflows_only_warn() {
    let mut w = base();
    w.steps[1].tool = "mcp__forge__merge".into();
    let r = validate(&w, None, &known());
    assert!(r.errors().is_empty(), "{:?}", r.errors());
    assert!(
        r.warnings()
            .iter()
            .any(|i| i.message.contains("momentanément indisponible"))
    );

    let mut w = base();
    second(
        &mut w,
        Step {
            workflow_id: "deploy-generic".into(),
            ..step("workflow")
        },
    );
    let r = validate(
        &w,
        None,
        &Known {
            max_depth: 0,
            ..known()
        },
    );
    assert!(
        r.warnings()
            .iter()
            .any(|i| i.message.contains("limitée à 3"))
    );
}

/// Une clé de métadonnées lue sous `all`, `any` ou `not` mais jamais écrite est signalée.
#[test]
fn nested_metadata_keys_never_written_are_signalled() {
    let mut w = base();
    w.steps[1].transitions = vec![Transition {
        goto: DONE.into(),
        condition: json!({"type": "not", "cond": {"type": "any", "of": [
            {"type": "metadata_all_match", "key": "tests_verts", "value": true}
        ]}}),
        ..Transition::always(DONE)
    }];
    let r = validate(&w, None, &known());
    assert!(
        r.warnings()
            .iter()
            .any(|i| i.message.contains("`tests_verts` n'est jamais écrite")),
        "{:?}",
        r.issues
    );
}
