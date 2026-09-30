#[test]
fn a_world_difference_names_its_first_differing_field() {
    let e = vec![
        serde_json::json!({"type": "outcome", "result": [{"id": "a", "ok": true}, {"id": "b", "ok": true}]}),
    ];
    let a = vec![
        serde_json::json!({"type": "outcome", "result": [{"id": "a", "ok": true}, {"id": "b", "ok": false}]}),
    ];
    let err = compare_world("x", &e, &a).unwrap_err();
    assert!(
        err.contains("premier écart en .result[1].ok : attendu true, obtenu false"),
        "{err}"
    );
}

/// Un long texte (prompt système rendu) est cadré autour du premier caractère qui
/// diffère, avec sa position : le début commun ne cache plus l'écart.
#[test]
fn a_long_text_difference_is_framed_around_the_first_differing_char() {
    let common = "x".repeat(900);
    let e = vec![json!({"type": "event", "rendered": format!("{common}Machine : A")})];
    let a = vec![json!({"type": "event", "rendered": format!("{common}Machine : B")})];
    let err = compare_world("x", &e, &a).unwrap_err();
    assert!(err.contains("[car. 910]"), "{err}");
    assert!(
        err.contains("Machine : A") && err.contains("Machine : B"),
        "{err}"
    );
    let framed = err.split("premier écart en").nth(1).expect("premier écart");
    assert!(
        !framed.contains(&"x".repeat(100)),
        "le début commun ne doit pas être recopié dans l'écart : {framed}"
    );
}
use super::*;
use serde_json::json;

#[test]
fn script_lines_round_trip_and_map_to_scripted() {
    let lines = vec![
        ScriptLine::Text("bonjour".into()),
        ScriptLine::ToolCalls {
            text: String::new(),
            calls: vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path": "a.txt"}),
            }],
        },
        ScriptLine::Error {
            kind: "transient".into(),
            message: "503".into(),
        },
        ScriptLine::ContextOverflow,
        ScriptLine::MidStreamError {
            text: "début".into(),
            message: "coupure".into(),
        },
    ];
    for l in &lines {
        let s = serde_json::to_string(l).unwrap();
        let back: ScriptLine = serde_json::from_str(&s).unwrap();
        assert_eq!(&back, l);
    }
    assert!(matches!(lines[0].to_scripted(), Scripted::Text(t) if t == "bonjour"));
    assert!(matches!(lines[1].to_scripted(), Scripted::ToolCalls(_, c) if c.len() == 1));
    assert!(matches!(
        lines[2].to_scripted(),
        Scripted::Error(LlmErrorKind::Transient, _)
    ));
    assert!(matches!(lines[3].to_scripted(), Scripted::ContextOverflow));
    assert_eq!(error_kind("inconnu"), LlmErrorKind::Other);
}

#[test]
fn the_world_diff_names_the_group_and_the_line() {
    let expected = vec![
        json!({"type": "session", "id": "{{session:1}}"}),
        json!({"type": "effect", "tool": "schedule_create", "state": "completed"}),
    ];
    let actual = vec![json!({"type": "session", "id": "{{session:1}}"})];
    let err = compare_world("reminders-daily", &expected, &actual).unwrap_err();
    assert!(err.starts_with("scénario reminders-daily : expected.jsonl diffère (effects n°1"));
    assert!(err.contains("schedule_create"), "{err}");
    assert!(err.contains("obtenu [absent]"), "{err}");
    assert!(err.contains("UPDATE_SCENARIOS=1"), "{err}");
    assert!(compare_world("x", &expected, &expected).is_ok());
}

#[test]
fn the_surface_diff_names_the_call_and_the_message() {
    let expected = vec![json!({"call": 1, "model": "m", "messages": [
        {"role": "system", "text": "S"}, {"role": "user", "text": "bonjour"}]})];
    let mut actual = expected.clone();
    actual[0]["messages"][1]["text"] = json!("salut");
    let err = compare_surface("tour-simple", &expected, &actual).unwrap_err();
    assert!(err.contains("appel 1, message 2"), "{err}");
    assert!(err.contains("bonjour") && err.contains("salut"), "{err}");
    let mut other = expected.clone();
    other[0]["model"] = json!("autre");
    let err = compare_surface("tour-simple", &expected, &other).unwrap_err();
    assert!(err.contains("appel 1, champ model"), "{err}");
    let err = compare_surface("tour-simple", &expected, &[]).unwrap_err();
    assert!(err.contains("1 appel(s) attendu(s), 0 obtenu(s)"), "{err}");
}

#[test]
fn the_spec_format_is_read_as_documented() {
    let spec: Spec = toml::from_str(
        r#"
name = "exemple"
description = "un exemple"
pin_model = "main"
messenger = true

[config]
"context.large_payload_tokens" = 300

[[files]]
path = "notes.txt"
content = "ligne\n"
repeat = 3

[[mcp_tools]]
server = "banc"
name = "lire"
description = "Lit un fichier distant."
read_only = true
params = ["path"]
result = "contenu"

[[steps]]
kind = "message"
text = "bonjour"

[[steps]]
kind = "message"
text = "lis le fichier"
crash = "during_tool"

[[steps]]
kind = "restart"

[[steps]]
kind = "command"
command = "/compact"

[[steps]]
kind = "advance_clock"
by = "10m"

[[steps]]
kind = "seed"
exchanges = 2
user = "question {i}"
assistant = "réponse {i}"
"#,
    )
    .unwrap();
    assert_eq!(spec.steps.len(), 6);
    assert!(matches!(
        spec.steps[1],
        Step::Message {
            crash: Some(Crash::DuringTool),
            ..
        }
    ));
    assert_eq!(spec.steps[2].label(), "redémarrage");
    assert_eq!(spec.files[0].repeat, 3);
    assert!(spec.mcp_tools[0].read_only);
    assert!(spec.messenger);
    assert_eq!(spec.config.len(), 1);
    assert!(toml::from_str::<Spec>("name = \"x\"\n[[steps]]\nkind = \"inconnu\"\n").is_err());
}

#[test]
fn modes_follow_the_environment_variables() {
    // Les variables ne sont pas posées par cette suite : le défaut est le rejeu.
    if std::env::var("UPDATE_SCENARIOS").is_err() && std::env::var("RECORD_SCENARIO").is_err() {
        assert_eq!(mode_for("x"), Mode::Replay);
    }
}
