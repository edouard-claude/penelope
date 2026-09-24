//! Sortie structurée du classifieur de routage.

use super::*;

/// Extrait la classification d'une réponse, même entourée de texte.
/// `response_format` du classifieur : schéma strict (toutes les propriétés requises,
/// aucune autre admise), comme l'exigent les providers à sortie structurée stricte.
pub(super) fn classification_schema() -> Value {
    json!({
        "type": "json_schema",
        "json_schema": {
            "name": "classification",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": {
                    "complexity": {"type": "string", "enum": ["low", "medium", "high"]},
                    "needs_tools": {"type": "boolean"},
                    "domain": {"type": "string"}
                },
                "required": ["complexity", "needs_tools", "domain"],
                "additionalProperties": false
            }
        }
    })
}

pub fn parse_classification(text: &str) -> Option<Classification> {
    let start = text.find('{')?;
    // Une réponse coupée peut porter une `}` avant sa première `{` : `text[30..=10]`
    // paniquerait (« slice index starts at 30 but ends at 11 », issue #130).
    let end = text.rfind('}').filter(|e| *e > start)?;
    let mut v: Value = serde_json::from_str(&text[start..=end]).ok()?;
    // Un domaine trop bavard ne doit pas invalider la complexité.
    if let Some(d) = v.get("domain").and_then(|d| d.as_str())
        && d.chars().count() > 40
    {
        let short: String = d.chars().take(40).collect();
        v["domain"] = json!(short);
    }
    let errors = penelope_kernel::schema::validate(&penelope_llm::router::classifier_schema(), &v);
    if !errors.is_empty() {
        return None;
    }
    serde_json::from_value(v).ok()
}
