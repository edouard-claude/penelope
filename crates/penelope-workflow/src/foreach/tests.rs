use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

#[test]
fn on_error_parses() {
    assert_eq!(OnError::parse(""), Ok(OnError::Stop));
    assert_eq!(OnError::parse("skip"), Ok(OnError::Skip));
    assert_eq!(OnError::parse("retry:2"), Ok(OnError::Retry(2)));
    assert!(OnError::parse("retry:x").is_err());
}

#[test]
fn sources_parse() {
    assert_eq!(
        Source::parse(&json!([1, 2])),
        Ok(Source::Inline(vec![json!(1), json!(2)]))
    );
    assert_eq!(
        Source::parse(&json!({"step": "lister", "path": "content"})),
        Ok(Source::Step {
            step: "lister".into(),
            path: "content".into()
        })
    );
    assert!(matches!(
        Source::parse(
            &json!({"tool": "mcp__clickup__clickup_filter_tasks", "args": {"list_ids": ["9"]}})
        ),
        Ok(Source::Tool { .. })
    ));
    assert!(Source::parse(&json!({"autre": 1})).is_err());
}

/// La liste se trouve où elle est : tableau, chaîne JSON, résultat MCP, objet à liste.
#[test]
fn lists_are_found_where_they_are() {
    let mcp = json!({"data": {"content": [{"type": "text", "text": "x"}]},
                     "text": "{\"tasks\": [{\"id\": \"a\"}, {\"id\": \"b\"}]}"});
    assert_eq!(extract(&mcp, "").unwrap().len(), 2);
    assert_eq!(extract(&mcp, "text").unwrap().len(), 2);
    let structured = json!({"structuredContent": {"tasks": [{"id": 1}]}});
    assert_eq!(extract(&structured, "").unwrap(), vec![json!({"id": 1})]);
    assert_eq!(
        extract(&json!({"content": "[1,2,3]"}), "content")
            .unwrap()
            .len(),
        3
    );
    assert!(extract(&json!({"a": 1}), "a").is_err());
}

#[test]
fn markdown_lists_skip_ticked_boxes() {
    let md =
        "# Sprint\n\n- [x] déjà fait\n- [ ] 1.1 Moteur\n* 1.2 Écran\n3. 1.3 Export\ntexte libre\n";
    let items = parse_file("backlog.md", md, "").unwrap();
    let titles: Vec<&str> = items.iter().map(|i| i["title"].as_str().unwrap()).collect();
    assert_eq!(titles, vec!["1.1 Moteur", "1.2 Écran", "1.3 Export"]);
    let json = parse_file("b.json", r#"{"stories": [{"name": "a"}]}"#, "").unwrap();
    assert_eq!(json.len(), 1);
}

#[test]
fn filter_and_sort_follow_the_spec() {
    let items = vec![
        json!({"id": "c", "status": "à faire", "rank": 3}),
        json!({"id": "a", "status": "fait", "rank": 1}),
        json!({"id": "b", "status": "à faire", "rank": 2}),
    ];
    let kept = refine(
        items.clone(),
        &json!({"filter": {"status": ["à faire"]}, "sortBy": "rank"}),
    );
    let ids: Vec<&str> = kept.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["b", "c"]);
    let desc = refine(items, &json!({"sortBy": "-id"}));
    assert_eq!(desc[0]["id"], "c");
}

#[test]
fn breakpoints_read_the_item_and_the_next() {
    let a = json!({"epic": "1", "title": "1.2"});
    let b = json!({"epic": "2", "title": "2.1"});
    assert!(pause_after(&json!({"changes": "epic"}), &a, Some(&b)));
    assert!(!pause_after(&json!({"changes": "epic"}), &a, Some(&a)));
    assert!(!pause_after(&json!({"changes": "epic"}), &a, None));
    let cond = json!({"type": "output_match", "path": "item.title", "equals": "1.2"});
    assert!(pause_after(&cond, &a, None));
    assert!(!pause_after(&Value::Null, &a, Some(&b)));
    assert_eq!(default_label(&a, 0), "1.2");
    assert_eq!(default_label(&json!(42), 4), "#5");
}

/// La liste figée : une fois, dans l'ordre, chaque élément avec son état et son sous-run.
#[tokio::test]
async fn a_frozen_list_keeps_its_order_and_states() {
    let store = Store::open_memory().unwrap();
    let items = ItemStore::new(store, Arc::new(TestClock::default()));
    let list = |n: usize| -> Vec<(Value, String)> {
        (0..n).map(|i| (json!({"n": i}), format!("e{i}"))).collect()
    };
    assert!(items.freeze("r1", "liste", 3, list(3)).await.unwrap());
    assert!(
        !items.freeze("r1", "liste", 3, list(5)).await.unwrap(),
        "figée"
    );
    let rows = items.list("r1", "liste", 3).await.unwrap();
    assert_eq!(rows.len(), 3);
    items.start(&rows[0], "r_c1").await.unwrap();
    let rows = items.list("r1", "liste", 3).await.unwrap();
    assert_eq!(rows[0].state, ItemState::Running);
    assert_eq!(rows[0].attempts, 0);
    items.start(&rows[0], "r_c2").await.unwrap();
    let again = items.of_child("r_c2").await.unwrap().unwrap();
    assert_eq!(
        (again.idx, again.attempts),
        (0, 1),
        "une nouvelle tentative"
    );
    items
        .finish(&again, ItemState::Skipped, Some("tests rouges"))
        .await
        .unwrap();
    let latest = items.latest("r1").await.unwrap();
    assert_eq!(latest[0].error.as_deref(), Some("tests rouges"));
    let tally = Tally::of(&latest);
    assert_eq!(tally.line(), "0 fait, 0 en échec, 1 sauté sur 3");
}
