use atelier::{Inventory, Item, format_cents, parse_price};

#[test]
fn the_total_counts_quantities() {
    let mut inv = Inventory::default();
    inv.add(Item {
        name: "vis".into(),
        qty: 10,
        price_cents: 5,
    });
    inv.add(Item {
        name: "vis".into(),
        qty: 5,
        price_cents: 5,
    });
    assert_eq!(inv.items().len(), 1);
    assert_eq!(inv.total_cents(), 75);
}

#[test]
fn prices_with_a_dot_are_parsed() {
    assert_eq!(parse_price("12.50"), Ok(1250));
    assert_eq!(parse_price("3"), Ok(300));
    assert!(parse_price("abc").is_err());
}

#[test]
fn cents_are_formatted_in_french() {
    assert_eq!(format_cents(1250), "12,50 €");
    assert_eq!(format_cents(5), "0,05 €");
}
