//! Sérialisation JSON canonique.
//!
//! Utilisée par la chaîne de hachage de l'event log (§4.1) et par le calcul des clés
//! d'idempotence (§4.2) : deux valeurs sémantiquement égales DOIVENT produire exactement
//! les mêmes octets, quel que soit l'ordre d'insertion des clés.

use serde_json::Value;
use std::fmt::Write as _;

/// Rend une valeur JSON sous forme canonique : clés triées, aucun espace, nombres
/// normalisés, chaînes échappées en JSON strict.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

/// Hash SHA-256 (hex minuscule) de la forme canonique.
pub fn canonical_hash(v: &Value) -> String {
    sha256_hex(canonical_json(v).as_bytes())
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => write_number(out, n),
        Value::String(s) => write_string(out, s),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(out, k);
                out.push(':');
                write_value(out, &map[*k]);
            }
            out.push('}');
        }
    }
}

fn write_number(out: &mut String, n: &serde_json::Number) {
    if let Some(i) = n.as_i64() {
        let _ = write!(out, "{i}");
    } else if let Some(u) = n.as_u64() {
        let _ = write!(out, "{u}");
    } else if let Some(f) = n.as_f64() {
        if f == f.trunc() && f.abs() < 1e15 {
            let _ = write!(out, "{}", f as i64);
        } else {
            let _ = write!(out, "{f}");
        }
    } else {
        out.push_str("null");
    }
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) == 0x08 => out.push_str("\\b"),
            c if (c as u32) == 0x0c => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_matter() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn nested_objects_are_sorted() {
        let v = json!({"z": {"y": 1, "x": [ {"b":1,"a":2} ]}, "a": null});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":null,"z":{"x":[{"a":2,"b":1}],"y":1}}"#
        );
    }

    #[test]
    fn strings_are_escaped_strictly() {
        let v = json!({ "k": format!("a\"b\\c\nd{}", '\u{1}') });
        let expected = concat!(
            "{\"k\":\"a",
            "\\\"", // guillemet échappé
            "b",
            "\\\\", // antislash échappé
            "c",
            "\\n", // saut de ligne échappé
            "d",
            "\\u0001", // caractère de contrôle en \uXXXX
            "\"}"
        );
        assert_eq!(canonical_json(&v), expected);
    }

    #[test]
    fn form_feed_and_backspace_use_short_escapes() {
        let v = json!({ "k": format!("{}{}", '\u{8}', '\u{c}') });
        assert_eq!(canonical_json(&v), "{\"k\":\"\\b\\f\"}");
    }

    #[test]
    fn hash_is_stable() {
        let a = json!({"x":1,"y":[1,2,3]});
        let b = json!({"y":[1,2,3],"x":1});
        assert_eq!(canonical_hash(&a), canonical_hash(&b));
        assert_ne!(
            canonical_hash(&a),
            canonical_hash(&json!({"x":2,"y":[1,2,3]}))
        );
    }
}
