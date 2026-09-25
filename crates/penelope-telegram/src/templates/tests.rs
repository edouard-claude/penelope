use super::*;

/// #129 : les variables se cherchent dans le gabarit ; une valeur qui porte `{{…}}`
/// (gabarit Go, nom d'une autre variable) passe telle quelle ; une vraie variable
/// manquante est toujours refusée.
#[test]
fn values_are_never_read_as_variables() {
    let t = Template {
        id: "essai".into(),
        body: "{{intention}}\n\n{{action}}".into(),
        variables: vec!["intention".into(), "action".into()],
        ..Default::default()
    };
    let vars: BTreeMap<String, String> = [
        ("intention".to_string(), "voir {{action}}".to_string()),
        (
            "action".to_string(),
            r#"docker ps --format "{{.Name}}""#.to_string(),
        ),
    ]
    .into_iter()
    .collect();
    let r = t.render(&vars, &BTreeMap::new(), &[]).unwrap();
    assert!(r.html.contains("voir {{action}}"), "{}", r.html);
    assert!(r.html.contains("{{.Name}}"), "{}", r.html);
    let mut partial = vars.clone();
    partial.remove("action");
    let e = t.render(&partial, &BTreeMap::new(), &[]).unwrap_err();
    assert!(e.message.contains("action"), "{}", e.message);
    assert_eq!(
        substitute(
            "a {{ x }} b {{y}}",
            &[("x".to_string(), "{{y}}".to_string())]
                .into_iter()
                .collect()
        ),
        "a {{y}} b {{y}}"
    );
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn the_catalog_is_complete() {
    let r = TemplateRegistry::with_builtins();
    for id in CATALOG {
        assert!(r.get(id).is_some(), "template manquant : {id}");
    }
    assert_eq!(r.len(), CATALOG.len());
}

/// CA 14 : chaque template se rend en blocs riches **et** en HTML de repli.
#[test]
fn ca_14_1_every_template_renders_in_both_forms() {
    let r = TemplateRegistry::with_builtins();
    for id in r.ids() {
        let t = r.get(&id).unwrap();
        t.validate().unwrap_or_else(|e| panic!("{e}"));

        let v: BTreeMap<String, String> = t
            .variables
            .iter()
            .map(|name| (name.clone(), format!("<valeur {name}>")))
            .collect();
        let tokens: BTreeMap<String, String> = t
            .actions()
            .into_iter()
            .map(|a| (a, "a:jeton1234".to_string()))
            .collect();

        let rendered = t.render(&v, &tokens, &[]).unwrap_or_else(|e| panic!("{e}"));
        assert!(!rendered.blocks.is_empty(), "{id} : aucun bloc");
        assert!(!rendered.html.is_empty(), "{id} : HTML vide");
        assert!(
            !rendered.html.contains("{{"),
            "{id} : variable non substituée dans le HTML"
        );
        // Les valeurs injectées sont échappées.
        assert!(
            !rendered.html.contains("<valeur"),
            "{id} : échappement HTML manquant"
        );
    }
}

#[test]
fn missing_variable_is_an_error_not_a_hole() {
    let r = TemplateRegistry::with_builtins();
    let t = r.get("tool_approval").unwrap();
    let e = t.render(&vars(&[("outil", "x")]), &BTreeMap::new(), &[]);
    assert!(e.is_err());
    assert!(e.unwrap_err().message.contains("non fournies"));
}

#[test]
fn undeclared_variable_is_rejected_at_validation() {
    let t = Template {
        id: "x".into(),
        body: "bonjour {{inconnue}}".into(),
        variables: vec![],
        buttons: vec![],
        fallback_html: String::new(),
    };
    assert!(t.validate().unwrap_err().message.contains("inconnue"));
}

#[test]
fn unknown_action_or_style_is_rejected() {
    let mut t = Template {
        id: "x".into(),
        body: "corps".into(),
        variables: vec![],
        buttons: vec![vec![b("Go", "action_inconnue", "")]],
        fallback_html: String::new(),
    };
    assert!(
        t.validate()
            .unwrap_err()
            .message
            .contains("action inconnue")
    );
    t.buttons = vec![vec![b("Go", crate::actions::kind::APPROVE, "violet")]];
    assert!(t.validate().unwrap_err().message.contains("style inconnu"));
}

#[test]
fn disabled_buttons_are_kept_in_place() {
    let r = TemplateRegistry::with_builtins();
    let t = r.get("run_card").unwrap();
    let v: BTreeMap<String, String> = t
        .variables
        .iter()
        .map(|n| (n.clone(), "x".to_string()))
        .collect();
    let tokens: BTreeMap<String, String> = t
        .actions()
        .into_iter()
        .map(|a| (a, "a:t".to_string()))
        .collect();
    let rendered = t
        .render(&v, &tokens, &[crate::actions::kind::RUN_RESUME.to_string()])
        .unwrap();
    let flat: Vec<&ButtonSpec> = rendered.buttons.iter().flatten().collect();
    assert_eq!(flat.len(), 5, "aucun bouton n'est retiré");
    assert_eq!(flat.iter().filter(|b| b.disabled).count(), 1);
}

#[test]
fn url_buttons_substitute_variables() {
    let r = TemplateRegistry::with_builtins();
    let t = r.get("mcp_oauth_required").unwrap();
    let v = vars(&[
        ("serveur", "forge"),
        ("scopes", "repo:write"),
        ("mode", "paste_back"),
        ("url", "https://auth.example/authorize?x=1"),
    ]);
    let tokens: BTreeMap<String, String> = t
        .actions()
        .into_iter()
        .map(|a| (a, "a:t".to_string()))
        .collect();
    let rendered = t.render(&v, &tokens, &[]).unwrap();
    let url_btn = rendered.buttons[0]
        .iter()
        .find(|b| matches!(b.action, crate::render::ButtonAction::Url { .. }))
        .unwrap();
    match &url_btn.action {
        crate::render::ButtonAction::Url { url } => {
            assert_eq!(url, "https://auth.example/authorize?x=1")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn hot_reload_overrides_builtins() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("answer.toml"),
        "id = \"answer\"\nbody = \"Version maison : {{corps}}\"\nvariables = [\"corps\"]\n",
    )
    .unwrap();
    let mut r = TemplateRegistry::with_builtins();
    assert_eq!(r.load_dir(dir.path()), 1);
    let t = r.get("answer").unwrap();
    assert!(t.body.contains("Version maison"));
    assert!(t.buttons.is_empty());
}

#[test]
fn a_broken_template_is_reported_and_the_others_survive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bon.toml"), "body = \"ok\"\n").unwrap();
    std::fs::write(dir.path().join("casse.toml"), "== pas du toml\n").unwrap();
    std::fs::write(dir.path().join("invalide.toml"), "body = \"{{absente}}\"\n").unwrap();
    let mut r = TemplateRegistry::new();
    assert_eq!(r.load_dir(dir.path()), 1);
    assert_eq!(r.errors().len(), 2);
}

#[test]
fn placeholders_are_extracted_once() {
    assert_eq!(
        placeholders("a {{x}} b {{y}} c {{x}}"),
        vec!["x".to_string(), "y".to_string()]
    );
}

#[test]
fn every_button_action_is_known() {
    for t in builtin_templates() {
        for a in t.actions() {
            assert!(
                crate::actions::kind::ALL.contains(&a.as_str()),
                "action inconnue dans `{}` : {a}",
                t.id
            );
        }
    }
}
