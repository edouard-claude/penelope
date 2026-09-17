//! Moteur de formulaires piloté par JSON Schema (§14.7).
//!
//! Un champ par écran : boutons pour les énumérations et les booléens, saisie en réponse
//! pour les chaînes et les nombres, récapitulatif final éditable avant envoi.
//!
//! Sert pour : elicitation MCP, paramètres de workflow, arguments de prompts MCP,
//! assistant `/mcp add`.

use crate::error::{TgError, TgResult};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    /// Énumération : un bouton par option.
    Enum {
        options: Vec<EnumOption>,
        multi: bool,
    },
    Boolean,
    Text {
        multiline: bool,
    },
    Number {
        integer: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumOption {
    pub value: Value,
    /// Titre lisible (`enumNames`), sinon la valeur.
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub title: String,
    pub description: String,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
}

impl Field {
    /// Valide une valeur saisie pour ce champ.
    pub fn coerce(&self, raw: &str) -> TgResult<Value> {
        match &self.kind {
            FieldKind::Boolean => match raw.trim().to_lowercase().as_str() {
                "oui" | "true" | "1" | "o" => Ok(Value::Bool(true)),
                "non" | "false" | "0" | "n" => Ok(Value::Bool(false)),
                other => Err(TgError::Form(format!(
                    "`{other}` n'est pas un booléen : réponds oui ou non"
                ))),
            },
            FieldKind::Number { integer } => {
                let t = raw.trim().replace(',', ".");
                if *integer {
                    t.parse::<i64>()
                        .map(|n| json!(n))
                        .map_err(|_| TgError::Form(format!("`{raw}` n'est pas un entier")))
                } else {
                    t.parse::<f64>()
                        .map(|n| json!(n))
                        .map_err(|_| TgError::Form(format!("`{raw}` n'est pas un nombre")))
                }
            }
            FieldKind::Enum { options, multi } => {
                let picks: Vec<&str> = if *multi {
                    raw.split(',').map(|s| s.trim()).collect()
                } else {
                    vec![raw.trim()]
                };
                let mut chosen = Vec::new();
                for p in picks {
                    let found = options
                        .iter()
                        .find(|o| o.title.eq_ignore_ascii_case(p) || value_str(&o.value) == p);
                    match found {
                        Some(o) => chosen.push(o.value.clone()),
                        None => {
                            return Err(TgError::Form(format!(
                                "`{p}` n'est pas une option : {}",
                                options
                                    .iter()
                                    .map(|o| o.title.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                    }
                }
                Ok(if *multi {
                    Value::Array(chosen)
                } else {
                    chosen.into_iter().next().unwrap_or(Value::Null)
                })
            }
            FieldKind::Text { .. } => {
                if raw.trim().is_empty() && self.required {
                    return Err(TgError::Form(format!("`{}` est obligatoire", self.title)));
                }
                Ok(Value::String(raw.to_string()))
            }
        }
    }

    /// Libellés des boutons proposés pour ce champ (vide = saisie libre).
    pub fn button_labels(&self) -> Vec<String> {
        match &self.kind {
            FieldKind::Enum { options, .. } => options.iter().map(|o| o.title.clone()).collect(),
            FieldKind::Boolean => vec!["Oui".into(), "Non".into()],
            _ => Vec::new(),
        }
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Options titrées `[{"const": …, "title": …}]` (enum titré des élicitations MCP).
fn titled_options(list: Option<&Value>) -> Option<Vec<EnumOption>> {
    let options: Vec<EnumOption> = list?
        .as_array()?
        .iter()
        .filter_map(|o| {
            let value = o.get("const")?.clone();
            Some(EnumOption {
                title: o
                    .get("title")
                    .and_then(|t| t.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| value_str(&value)),
                value,
            })
        })
        .collect();
    (!options.is_empty()).then_some(options)
}

/// Compile un JSON Schema en champs de formulaire.
pub fn fields_from_schema(schema: &Value) -> TgResult<Vec<Field>> {
    let props = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .ok_or_else(|| TgError::Form("schéma sans `properties`".into()))?;
    let required: Vec<String> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for (name, p) in props {
        let ty = p.get("type").and_then(|t| t.as_str()).unwrap_or("string");
        let enum_values = p.get("enum").and_then(|e| e.as_array());
        let enum_names: Vec<String> = p
            .get("enumNames")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().map(value_str).collect())
            .unwrap_or_default();

        let kind = if let Some(values) = enum_values {
            FieldKind::Enum {
                options: values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| EnumOption {
                        title: enum_names.get(i).cloned().unwrap_or_else(|| value_str(v)),
                        value: v.clone(),
                    })
                    .collect(),
                multi: false,
            }
        } else if let Some(options) = titled_options(p.get("oneOf")) {
            FieldKind::Enum {
                options,
                multi: false,
            }
        } else if ty == "array" {
            let item = p.get("items").cloned().unwrap_or(json!({}));
            let titled = titled_options(item.get("anyOf").or_else(|| item.get("oneOf")));
            match (item.get("enum").and_then(|e| e.as_array()), titled) {
                (Some(values), _) => FieldKind::Enum {
                    options: values
                        .iter()
                        .map(|v| EnumOption {
                            title: value_str(v),
                            value: v.clone(),
                        })
                        .collect(),
                    multi: true,
                },
                (None, Some(options)) => FieldKind::Enum {
                    options,
                    multi: true,
                },
                (None, None) => FieldKind::Text { multiline: true },
            }
        } else {
            match ty {
                "boolean" => FieldKind::Boolean,
                "integer" => FieldKind::Number { integer: true },
                "number" => FieldKind::Number { integer: false },
                _ => FieldKind::Text {
                    multiline: p
                        .get("maxLength")
                        .and_then(|m| m.as_u64())
                        .map(|m| m > 200)
                        .unwrap_or(false),
                },
            }
        };

        out.push(Field {
            name: name.clone(),
            title: p
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or(name)
                .to_string(),
            description: p
                .get("description")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            kind,
            required: required.contains(name),
            default: p.get("default").cloned(),
        });
    }
    // Ordre déterministe et naturel : les champs obligatoires **dans l'ordre déclaré par
    // `required`**, puis les optionnels par ordre alphabétique.
    let rank = |f: &Field| -> (usize, String) {
        match required.iter().position(|r| r == &f.name) {
            Some(i) => (i, String::new()),
            None => (usize::MAX, f.name.clone()),
        }
    };
    out.sort_by_key(|a| rank(a));
    Ok(out)
}

/// État d'un formulaire en cours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormState {
    pub id: String,
    pub schema: Value,
    pub fields: Vec<Field>,
    pub cursor: usize,
    pub values: Map<String, Value>,
    pub done: bool,
    pub declined: bool,
}

impl FormState {
    pub fn new(id: &str, schema: Value) -> TgResult<FormState> {
        let fields = fields_from_schema(&schema)?;
        let mut values = Map::new();
        for f in &fields {
            if let Some(d) = &f.default {
                values.insert(f.name.clone(), d.clone());
            }
        }
        Ok(FormState {
            id: id.to_string(),
            schema,
            fields,
            cursor: 0,
            values,
            done: false,
            declined: false,
        })
    }

    pub fn current(&self) -> Option<&Field> {
        self.fields.get(self.cursor)
    }

    pub fn progress(&self) -> String {
        format!(
            "{}/{}",
            (self.cursor + 1).min(self.fields.len()),
            self.fields.len()
        )
    }

    /// Enregistre une réponse et avance.
    pub fn answer(&mut self, raw: &str) -> TgResult<()> {
        let Some(f) = self.fields.get(self.cursor).cloned() else {
            return Err(TgError::Form("formulaire terminé".into()));
        };
        let v = f.coerce(raw)?;
        self.values.insert(f.name.clone(), v);
        self.next();
        Ok(())
    }

    /// Passe le champ courant (interdit s'il est obligatoire et sans défaut).
    pub fn skip(&mut self) -> TgResult<()> {
        let Some(f) = self.fields.get(self.cursor) else {
            return Ok(());
        };
        if f.required && !self.values.contains_key(&f.name) {
            return Err(TgError::Form(format!(
                "`{}` est obligatoire : impossible de passer",
                f.title
            )));
        }
        self.next();
        Ok(())
    }

    pub fn next(&mut self) {
        if self.cursor + 1 >= self.fields.len() {
            self.cursor = self.fields.len().saturating_sub(1);
            self.done = true;
        } else {
            self.cursor += 1;
        }
    }

    pub fn prev(&mut self) {
        self.done = false;
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Revient sur un champ nommé (récapitulatif éditable).
    pub fn goto(&mut self, name: &str) -> TgResult<()> {
        match self.fields.iter().position(|f| f.name == name) {
            Some(i) => {
                self.cursor = i;
                self.done = false;
                Ok(())
            }
            None => Err(TgError::Form(format!("champ inconnu : `{name}`"))),
        }
    }

    pub fn decline(&mut self) {
        self.declined = true;
        self.done = true;
    }

    /// Valeurs finales, validées contre le schéma d'origine.
    pub fn submit(&self) -> TgResult<Value> {
        let v = Value::Object(self.values.clone());
        penelope_kernel::schema::validate_ok(&self.schema, &v).map_err(TgError::Form)?;
        Ok(v)
    }

    /// Récapitulatif affiché avant envoi.
    pub fn summary(&self) -> String {
        let mut s = String::from("Récapitulatif :\n");
        for f in &self.fields {
            let v = self
                .values
                .get(&f.name)
                .map(value_str)
                .unwrap_or_else(|| "—".into());
            s.push_str(&format!("- {} : {v}\n", f.title));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "environnement": {
                    "type": "string",
                    "title": "Environnement",
                    "enum": ["prod", "staging"],
                    "enumNames": ["Production", "Pré-production"]
                },
                "confirmer": {"type": "boolean", "title": "Confirmer", "default": false},
                "replicas": {"type": "integer", "title": "Replicas", "default": 2},
                "note": {"type": "string", "title": "Note", "maxLength": 500}
            },
            "required": ["environnement", "confirmer"]
        })
    }

    #[test]
    fn schema_compiles_to_typed_fields() {
        let f = fields_from_schema(&schema()).unwrap();
        assert_eq!(f.len(), 4);
        // Les obligatoires d'abord, dans l'ordre déclaré par `required`.
        assert!(f[0].required && f[1].required);
        assert_eq!(f[0].name, "environnement");
        assert_eq!(f[1].name, "confirmer");
        let env = f.iter().find(|x| x.name == "environnement").unwrap();
        match &env.kind {
            FieldKind::Enum { options, multi } => {
                assert!(!multi);
                assert_eq!(options[0].title, "Production");
                assert_eq!(options[0].value, "prod");
            }
            other => panic!("{other:?}"),
        }
        let note = f.iter().find(|x| x.name == "note").unwrap();
        assert!(matches!(note.kind, FieldKind::Text { multiline: true }));
    }

    #[test]
    fn enum_without_titles_falls_back_to_values() {
        let f = fields_from_schema(&json!({
            "type":"object",
            "properties":{"x":{"type":"string","enum":["a","b"]}}
        }))
        .unwrap();
        match &f[0].kind {
            FieldKind::Enum { options, .. } => assert_eq!(options[0].title, "a"),
            other => panic!("{other:?}"),
        }
    }

    /// Enums titrés des élicitations MCP (2025-11-25) : `oneOf` et `items.anyOf`.
    #[test]
    fn titled_enums_use_const_and_title() {
        let schema = json!({
            "type": "object",
            "properties": {
                "couleur": {"type": "string", "oneOf": [
                    {"const": "#FF0000", "title": "Rouge"},
                    {"const": "#00FF00", "title": "Vert"}
                ]},
                "tags": {"type": "array", "items": {"anyOf": [
                    {"const": "a", "title": "Alpha"},
                    {"const": "b", "title": "Bêta"}
                ]}}
            },
            "required": ["couleur"]
        });
        let mut form = FormState::new("e1", schema).unwrap();
        match &form.fields[0].kind {
            FieldKind::Enum { options, multi } => {
                assert!(!multi);
                assert_eq!(options[1].title, "Vert");
            }
            other => panic!("{other:?}"),
        }
        form.answer("Vert").unwrap();
        form.answer("Alpha, Bêta").unwrap();
        assert_eq!(
            form.submit().unwrap(),
            json!({"couleur": "#00FF00", "tags": ["a", "b"]})
        );
    }

    #[test]
    fn multi_select_arrays() {
        let f = fields_from_schema(&json!({
            "type":"object",
            "properties":{"tags":{"type":"array","items":{"enum":["a","b","c"]}}}
        }))
        .unwrap();
        match &f[0].kind {
            FieldKind::Enum { multi, options } => {
                assert!(multi);
                assert_eq!(options.len(), 3);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn defaults_are_prefilled() {
        let s = FormState::new("f1", schema()).unwrap();
        assert_eq!(s.values["replicas"], 2);
        assert_eq!(s.values["confirmer"], false);
    }

    #[test]
    fn one_field_per_screen_with_progress() {
        let mut s = FormState::new("f1", schema()).unwrap();
        assert_eq!(s.progress(), "1/4");
        assert!(s.current().unwrap().required);
        s.answer("Production").unwrap();
        assert_eq!(s.progress(), "2/4");
        assert_eq!(s.values["environnement"], "prod");
    }

    #[test]
    fn coercion_and_error_messages() {
        let s = FormState::new("f1", schema()).unwrap();
        let env = &s.fields[0];
        assert!(env.coerce("Production").is_ok());
        let e = env.coerce("recette").unwrap_err();
        assert!(e.to_string().contains("Production"), "{e}");

        let b = s.fields.iter().find(|f| f.name == "confirmer").unwrap();
        assert_eq!(b.coerce("oui").unwrap(), json!(true));
        assert_eq!(b.coerce("Non").unwrap(), json!(false));
        assert!(b.coerce("peut-être").is_err());

        let n = s.fields.iter().find(|f| f.name == "replicas").unwrap();
        assert_eq!(n.coerce("3").unwrap(), json!(3));
        assert!(n.coerce("beaucoup").is_err());
    }

    #[test]
    fn required_fields_cannot_be_skipped() {
        let mut s = FormState::new("f1", schema()).unwrap();
        let e = s.skip().unwrap_err();
        assert!(e.to_string().contains("obligatoire"));
        // Un champ obligatoire avec défaut peut être passé.
        s.answer("prod").unwrap();
        assert_eq!(s.current().unwrap().name, "confirmer");
        s.skip().unwrap();
    }

    #[test]
    fn navigation_back_and_goto() {
        let mut s = FormState::new("f1", schema()).unwrap();
        s.answer("prod").unwrap();
        s.answer("oui").unwrap();
        s.prev();
        assert_eq!(s.current().unwrap().name, "confirmer");
        s.goto("environnement").unwrap();
        assert_eq!(s.current().unwrap().name, "environnement");
        assert!(s.goto("inexistant").is_err());
    }

    #[test]
    fn submit_validates_against_the_original_schema() {
        let mut s = FormState::new("f1", schema()).unwrap();
        s.answer("prod").unwrap();
        s.answer("oui").unwrap();
        let v = s.submit().unwrap();
        assert_eq!(v["environnement"], "prod");
        assert_eq!(v["confirmer"], true);

        // Sans le champ obligatoire, l'envoi est refusé.
        let mut vide = FormState::new("f2", schema()).unwrap();
        vide.values.remove("confirmer");
        assert!(vide.submit().is_err());
    }

    #[test]
    fn summary_lists_every_field() {
        let mut s = FormState::new("f1", schema()).unwrap();
        s.answer("prod").unwrap();
        let sum = s.summary();
        assert!(sum.contains("Environnement : prod"));
        assert!(sum.contains("Note : —"));
    }

    #[test]
    fn decline_marks_the_form_done() {
        let mut s = FormState::new("f1", schema()).unwrap();
        s.decline();
        assert!(s.done && s.declined);
    }

    #[test]
    fn buttons_are_offered_for_enums_and_booleans() {
        let s = FormState::new("f1", schema()).unwrap();
        let env = s.fields.iter().find(|f| f.name == "environnement").unwrap();
        assert_eq!(env.button_labels(), vec!["Production", "Pré-production"]);
        let b = s.fields.iter().find(|f| f.name == "confirmer").unwrap();
        assert_eq!(b.button_labels(), vec!["Oui", "Non"]);
        let n = s.fields.iter().find(|f| f.name == "note").unwrap();
        assert!(n.button_labels().is_empty(), "saisie libre pour le texte");
    }

    #[test]
    fn schema_without_properties_is_refused() {
        assert!(fields_from_schema(&json!({"type":"string"})).is_err());
    }

    #[test]
    fn the_last_answer_marks_the_form_done() {
        let mut s = FormState::new("f1", schema()).unwrap();
        for _ in 0..4 {
            let f = s.current().unwrap().clone();
            let raw = match &f.kind {
                FieldKind::Enum { options, .. } => options[0].title.clone(),
                FieldKind::Boolean => "oui".into(),
                FieldKind::Number { .. } => "1".into(),
                FieldKind::Text { .. } => "texte".into(),
            };
            s.answer(&raw).unwrap();
        }
        assert!(s.done);
    }
}
