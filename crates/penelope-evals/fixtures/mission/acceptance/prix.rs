//! Recette de la story « prix » : les corrections demandées par la revue.
use atelier::parse_price;

#[test]
fn a_comma_is_a_decimal_separator() {
    assert_eq!(parse_price("12,50"), Ok(1250));
    assert_eq!(parse_price("12.50"), Ok(1250));
}

#[test]
fn one_decimal_digit_means_tens_of_cents() {
    assert_eq!(parse_price("12.5"), Ok(1250));
    assert_eq!(parse_price("0,5"), Ok(50));
}

#[test]
fn more_than_two_decimals_are_refused() {
    assert!(parse_price("12.505").is_err());
}

#[test]
fn surrounding_spaces_are_accepted() {
    assert_eq!(parse_price(" 3 "), Ok(300));
    assert_eq!(parse_price(" 3,20 "), Ok(320));
}

#[test]
fn errors_quote_the_input() {
    let err = parse_price("12,5a").unwrap_err();
    assert!(err.contains("12,5a"), "{err}");
    assert!(parse_price("-3").is_err());
    assert!(parse_price("").is_err());
}

#[test]
fn the_old_behaviour_still_holds() {
    assert_eq!(parse_price("3"), Ok(300));
    assert!(parse_price("abc").is_err());
}
