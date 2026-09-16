//! Rendu des sorties : lisible par défaut, `--json` partout (§15).

use serde_json::Value;

/// Affiche une valeur, en JSON ou en texte lisible.
pub fn print(value: &Value, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        );
        return;
    }
    println!("{}", render(value));
}

/// Rendu lisible d'une valeur quelconque.
pub fn render(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "oui" } else { "non" }.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => {
            if a.is_empty() {
                return "(vide)".into();
            }
            // Tableau d'objets homogènes : on rend un tableau texte.
            if a.iter().all(|x| x.is_object()) {
                return table(a);
            }
            a.iter()
                .map(|x| format!("- {}", render(x)))
                .collect::<Vec<_>>()
                .join("\n")
        }
        Value::Object(o) => o
            .iter()
            .map(|(k, val)| format!("{k} : {}", inline(val)))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn inline(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(|x| x.is_string()) => a
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) | Value::Array(_) => v.to_string(),
        other => other.to_string(),
    }
}

/// Tableau texte à colonnes alignées.
pub fn table(rows: &[Value]) -> String {
    let mut columns: Vec<String> = Vec::new();
    for r in rows {
        if let Some(o) = r.as_object() {
            for k in o.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    if columns.is_empty() {
        return "(vide)".into();
    }
    // Limite de largeur : au-delà, on passe en liste verticale.
    if columns.len() > 8 {
        return rows.iter().map(render).collect::<Vec<_>>().join("\n---\n");
    }

    let cell = |r: &Value, c: &str| -> String { r.get(c).map(inline).unwrap_or_default() };
    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for r in rows {
        for (i, c) in columns.iter().enumerate() {
            widths[i] = widths[i].max(cell(r, c).chars().count()).min(40);
        }
    }

    let line = |cells: Vec<String>| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let t: String = s.chars().take(widths[i]).collect();
                format!("{t:<width$}", width = widths[i])
            })
            .collect::<Vec<_>>()
            .join("  ")
    };

    let mut out = line(columns.clone());
    out.push('\n');
    out.push_str(
        &widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    for r in rows {
        out.push('\n');
        out.push_str(&line(columns.iter().map(|c| cell(r, c)).collect()));
    }
    out
}

/// Message de succès court.
pub fn ok(message: &str, json: bool) {
    if json {
        println!("{}", serde_json::json!({"ok": true, "message": message}));
    } else {
        println!("✅ {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn objects_render_as_key_value_lines() {
        let out = render(&json!({"version":"1.0.0","uptime_s":42}));
        assert!(out.contains("version : 1.0.0"));
        assert!(out.contains("uptime_s : 42"));
    }

    #[test]
    fn arrays_of_objects_render_as_a_table() {
        let out = table(&[
            json!({"id":"a","state":"ready"}),
            json!({"id":"bbbb","state":"failed"}),
        ]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("id"));
        assert!(lines[1].starts_with("---"));
        assert!(lines[2].contains("ready"));
        // Les colonnes sont alignées sur la valeur la plus longue.
        assert!(lines[2].starts_with("a    "), "{:?}", lines[2]);
    }

    #[test]
    fn wide_objects_fall_back_to_vertical_lists() {
        let row: serde_json::Map<String, Value> =
            (0..12).map(|i| (format!("col{i}"), json!(i))).collect();
        let out = table(&[Value::Object(row)]);
        assert!(out.contains("col0 : 0"));
    }

    #[test]
    fn empty_arrays_are_explicit() {
        assert_eq!(render(&json!([])), "(vide)");
    }

    #[test]
    fn booleans_are_readable_in_french() {
        assert_eq!(render(&json!(true)), "oui");
        assert_eq!(render(&json!(false)), "non");
    }

    #[test]
    fn string_arrays_are_joined_inline() {
        let out = render(&json!({"tags":["a","b","c"]}));
        assert!(out.contains("tags : a, b, c"));
    }
}
