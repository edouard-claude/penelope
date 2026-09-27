use super::*;

/// Paramètres d'un workflow : chaînes `{{…}}` remplacées, `{{item}}` = l'élément entier.
pub(super) fn template_params(
    params: &Value,
    vars: &BTreeMap<String, String>,
    item: Option<&PolledItem>,
) -> Value {
    match params {
        Value::String(s) if s.trim() == "{{item}}" => {
            item.map(|i| i.value.clone()).unwrap_or(Value::Null)
        }
        Value::String(s) => Value::String(substitute(s, vars)),
        Value::Array(a) => Value::Array(a.iter().map(|x| template_params(x, vars, item)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), template_params(v, vars, item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Une ligne par élément : identifiant et libellé usuel s'il existe.
pub(super) fn items_lines(items: &[PolledItem]) -> String {
    items
        .iter()
        .map(|i| {
            let label = ["title", "subject", "name", "summary", "sujet", "titre"]
                .iter()
                .find_map(|k| i.value.get(*k).and_then(|v| v.as_str()));
            match label {
                Some(l) => format!("- {} : {l}", i.id),
                None => format!("- {}", i.id),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn substitute(body: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = body.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Données d'un résultat d'outil MCP : `structuredContent`, sinon le premier bloc de
/// texte lu comme JSON, sinon le texte brut.
pub(super) fn tool_payload(result: &Value) -> Value {
    if let Some(sc) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return sc.clone();
    }
    let text = result["content"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find_map(|b| b.get("text").and_then(|t| t.as_str()))
        })
        .unwrap_or_default();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

#[cfg(test)]
mod tests;
