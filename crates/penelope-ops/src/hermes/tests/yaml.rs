//! Le sous-ensemble YAML lu dans la configuration Hermes : cas limites.

use super::super::yaml::{Node, parse};

fn s(v: &str) -> Node {
    Node::Scalar(v.into())
}

#[test]
fn empty_documents_and_accessors() {
    assert_eq!(parse(""), Node::Null);
    assert_eq!(parse("---\n# seul un commentaire\n...\n"), Node::Null);
    let list = parse("- a\n");
    assert_eq!(list.get("a"), None, "une liste n'a pas de clé");
    assert!(list.entries().is_empty());
    assert_eq!(Node::Null.as_str(), None);
    for (v, b) in [
        ("yes", Some(true)),
        ("On", Some(true)),
        ("off", Some(false)),
        ("peut-être", None),
    ] {
        assert_eq!(s(v).as_bool(), b, "{v}");
    }
}

#[test]
fn quoted_keys_and_escaped_scalars() {
    let doc = parse(concat!(
        "\"clé: avec deux-points\": 1\n",
        "'simple': 'l''apostrophe'\n",
        "\"collée\":x\n",
        "echappe: \"a\\nb\\tc\\\"d\\\\e\\/f\\u00e9\\uZZZZ\\q\\\"\n",
        "vide:\n",
        "tilde: ~\n",
        "nul: null\n",
        ": sans clé\n",
        "  orpheline: ignorée\n",
    ));
    assert_eq!(doc.get("clé: avec deux-points"), Some(&s("1")));
    assert_eq!(doc.get("simple"), Some(&s("l'apostrophe")));
    assert_eq!(doc.get("collée"), None, "`:` doit être suivi d'un blanc");
    assert_eq!(
        doc.get("echappe").and_then(|v| v.as_str()),
        Some("a\nb\tc\"d\\e/fé\\uZZZZ\\q\\")
    );
    assert_eq!(doc.get("vide"), Some(&Node::Null));
    assert_eq!(doc.get("tilde"), Some(&Node::Null));
    assert_eq!(doc.get("nul"), Some(&Node::Null));
    assert_eq!(doc.get("orpheline"), None);
}

#[test]
fn flow_collections_nest_and_span_lines() {
    let doc = parse(concat!(
        "env: {A: \"1, 2\", 'B': [x, y], C: {D: e}}\n",
        "vide: []\n",
        "liste:\n",
        "  - [a, [b, c]]\n",
        "  - {k: v,\n",
        "     l: w}\n",
        "  -\n",
        "    - imbriqué\n",
        "  -\n",
        "  - \"échappé \\\"là\\\"\"\n",
    ));
    let env = doc.get("env").unwrap();
    assert_eq!(env.get("A"), Some(&s("1, 2")));
    assert_eq!(env.get("B"), Some(&Node::List(vec![s("x"), s("y")])));
    assert_eq!(env.get("C").and_then(|c| c.get("D")), Some(&s("e")));
    assert_eq!(doc.get("vide"), Some(&Node::List(vec![])));
    assert_eq!(
        doc.get("liste"),
        Some(&Node::List(vec![
            Node::List(vec![s("a"), Node::List(vec![s("b"), s("c")])]),
            Node::Map(vec![("k".into(), s("v")), ("l".into(), s("w"))]),
            Node::List(vec![s("imbriqué")]),
            Node::Null,
            s("échappé \"là\""),
        ]))
    );
}

#[test]
fn block_scalars_keep_or_fold_their_lines() {
    let doc = parse("a: |-\n  un\n\n  deux\n\nb: >+\n  trois\n  quatre\nc: fin\n");
    assert_eq!(doc.get("a").and_then(|v| v.as_str()), Some("un\n\ndeux"));
    assert_eq!(doc.get("b").and_then(|v| v.as_str()), Some("trois quatre"));
    assert_eq!(doc.get("c"), Some(&s("fin")));
    let items = parse("clé:\n- a\n- b\nautre: 1\n");
    assert_eq!(items.get("clé"), Some(&Node::List(vec![s("a"), s("b")])));
    assert_eq!(items.get("autre"), Some(&s("1")));
}
