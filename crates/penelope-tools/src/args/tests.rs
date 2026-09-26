use super::*;

/// #106 : seul `shell_exec` avec `network: true` demande le réseau.
#[test]
fn only_shell_exec_with_network_true_wants_the_network() {
    assert!(wants_network("shell_exec", &json!({"network": true})));
    assert!(!wants_network("shell_exec", &json!({"network": false})));
    assert!(!wants_network("shell_exec", &json!({"network": "true"})));
    assert!(!wants_network("shell_exec", &json!({})));
    assert!(!wants_network("http_fetch", &json!({"network": true})));
}

/// #110 : l'objet `args` gagne quand il est rempli ; vide, `args_json` le remplace.
#[test]
fn call_arguments_prefer_the_object_then_the_json_string() {
    let full = json!({"args": {"a": 1}, "args_json": "{\"a\": 2}"});
    assert_eq!(call_arguments(&full).unwrap(), json!({"a": 1}));

    let emptied = json!({"args": {}, "args_json": "{\"a\": 2}"});
    assert_eq!(call_arguments(&emptied).unwrap(), json!({"a": 2}));

    // Des arguments rendus en chaîne à la place de l'objet.
    let stringly = json!({"args": "{\"q\": \"x\"}"});
    assert_eq!(call_arguments(&stringly).unwrap(), json!({"q": "x"}));

    // Rien du tout, ou une chaîne blanche : un objet vide, pas une erreur.
    assert_eq!(call_arguments(&json!({})).unwrap(), json!({}));
    assert_eq!(call_arguments(&json!({"args": null})).unwrap(), json!({}));
    assert_eq!(
        call_arguments(&json!({"args_json": "   "})).unwrap(),
        json!({})
    );
}

/// #110 : une chaîne qui n'est pas un objet JSON est refusée avec un message pour le
/// modèle, au lieu d'appeler l'outil avec n'importe quoi.
#[test]
fn call_arguments_refuse_a_string_that_is_not_an_object() {
    for bad in ["[1, 2]", "pas du json", "42"] {
        let err = call_arguments(&json!({"args_json": bad})).unwrap_err();
        assert!(
            err.to_string()
                .contains("`args_json` n'est pas un objet JSON"),
            "{bad} : {err}"
        );
    }
}

/// La politique juge les arguments de l'outil visé par `tool_call`, et ceux de l'appel
/// pour tout autre outil ; des arguments illisibles valent un objet vide.
#[test]
fn effective_arguments_unwrap_tool_call_only() {
    let call = json!({"name": "fs_write", "args": {"path": "/tmp/x"}});
    assert_eq!(
        effective_arguments("tool_call", &call),
        json!({"path": "/tmp/x"})
    );
    assert_eq!(effective_arguments("fs_read", &call), call);
    assert_eq!(
        effective_arguments("tool_call", &json!({"args_json": "[]"})),
        json!({})
    );
}

/// L'intention est retirée, le reste de l'appel est intact.
#[test]
fn the_intention_is_removed_and_nothing_else() {
    let args = json!({"path": "a", crate::WHY_FIELD: "parce que"});
    assert_eq!(without_intention(&args), json!({"path": "a"}));
    assert_eq!(without_intention(&json!("brut")), json!("brut"));
}
