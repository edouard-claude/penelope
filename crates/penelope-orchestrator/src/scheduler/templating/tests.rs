use super::*;

fn item(id: &str, value: Value) -> PolledItem {
    PolledItem {
        id: id.into(),
        fingerprint: format!("fp-{id}"),
        value,
    }
}

#[test]
fn parameters_are_templated_at_every_depth() {
    let vars = BTreeMap::from([("id".to_string(), "42".to_string())]);
    let it = item("42", json!({"title": "Panne"}));
    let params = json!({
        "issue": "#{{id}}",
        "whole": " {{item}} ",
        "list": ["{{id}}", 3, {"deep": "x{{id}}{{absent}}"}],
        "flag": true,
    });
    assert_eq!(
        template_params(&params, &vars, Some(&it)),
        json!({
            "issue": "#42",
            "whole": {"title": "Panne"},
            "list": ["42", 3, {"deep": "x42{{absent}}"}],
            "flag": true,
        }),
        "une variable inconnue reste telle quelle"
    );
    assert_eq!(
        template_params(&json!("{{item}}"), &vars, None),
        Value::Null,
        "sans élément, `{{{{item}}}}` ne vaut rien"
    );
}

#[test]
fn each_item_is_listed_with_its_usual_label() {
    let items = [
        item("1", json!({"subject": "Facture"})),
        item("2", json!({"titre": "Réunion", "name": "ignoré"})),
        item("3", json!({"title": 7})),
    ];
    assert_eq!(
        items_lines(&items),
        "- 1 : Facture\n- 2 : ignoré\n- 3",
        "l'ordre des clés usuelles fait foi ; un libellé non textuel est ignoré"
    );
    assert_eq!(items_lines(&[]), "");
}

#[test]
fn a_tool_payload_prefers_structured_content_then_json_text() {
    assert_eq!(
        tool_payload(&json!({"structuredContent": {"n": 1}, "content": []})),
        json!({"n": 1})
    );
    assert_eq!(
        tool_payload(&json!({
            "structuredContent": null,
            "content": [{"type": "image"}, {"type": "text", "text": "[1, 2]"}]
        })),
        json!([1, 2])
    );
    assert_eq!(
        tool_payload(&json!({"content": [{"type": "text", "text": "pas du JSON"}]})),
        json!("pas du JSON")
    );
    assert_eq!(tool_payload(&json!({})), json!(""), "réponse vide");
}
