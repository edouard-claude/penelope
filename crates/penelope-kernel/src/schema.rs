//! Validateur JSON Schema 2020-12 (sous-ensemble) avec résolution `$ref` **bornée**.
//!
//! Le PRD (§3.2, §8.4) impose la validation des `inputSchema` / `outputSchema` MCP, du
//! `structuredContent`, des paramètres de workflow et des formulaires Telegram (§14.7).
//!
//! Décision d'architecture : implémentation locale plutôt qu'une bibliothèque externe
//! (voir `docs/decisions/0003-validateur-json-schema-local.md`). Deux raisons : la
//! résolution `$ref` doit être **bornée** (un schéma MCP est du contenu non fiable, §13.3)
//! et le validateur doit exposer les `default` et les `enumNames` au moteur de formulaires.
//!
//! Mots-clés couverts : `type`, `enum`, `const`, `properties`, `required`,
//! `additionalProperties`, `patternProperties`, `items`, `prefixItems`, `minItems`,
//! `maxItems`, `uniqueItems`, `minimum`, `maximum`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `multipleOf`, `minLength`, `maxLength`, `pattern`, `allOf`,
//! `anyOf`, `oneOf`, `not`, `$ref` (pointeur local), `$defs`, `definitions`.
//! Les autres mots-clés sont ignorés (annotations).

use serde_json::Value;

const MAX_REF_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Pointeur JSON de l'emplacement fautif, par exemple `/items/2/name`.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };
        write!(f, "{p} : {}", self.message)
    }
}

/// Valide `instance` contre `schema`. Renvoie toutes les erreurs trouvées (bornées à 50).
pub fn validate(schema: &Value, instance: &Value) -> Vec<ValidationError> {
    let mut errs = Vec::new();
    let mut ctx = Ctx {
        root: schema,
        depth: 0,
        errors: &mut errs,
    };
    check(&mut ctx, schema, instance, "");
    errs
}

/// Variante « tout ou rien », pratique aux frontières.
pub fn validate_ok(schema: &Value, instance: &Value) -> Result<(), String> {
    let errs = validate(schema, instance);
    if errs.is_empty() {
        return Ok(());
    }
    Err(errs
        .iter()
        .take(5)
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join(" ; "))
}

struct Ctx<'a> {
    root: &'a Value,
    depth: usize,
    errors: &'a mut Vec<ValidationError>,
}

impl Ctx<'_> {
    fn err(&mut self, path: &str, msg: impl Into<String>) {
        if self.errors.len() < 50 {
            self.errors.push(ValidationError {
                path: path.to_string(),
                message: msg.into(),
            });
        }
    }
}

/// Résout un `$ref` local (`#/$defs/x`). Les `$ref` externes ne sont pas suivis :
/// un schéma MCP ne doit jamais provoquer de requête réseau à la validation.
fn resolve<'a>(root: &'a Value, schema: &'a Value, depth: usize) -> Option<&'a Value> {
    if depth > MAX_REF_DEPTH {
        return None;
    }
    let r = schema.get("$ref")?.as_str()?;
    if !r.starts_with('#') {
        return None;
    }
    let pointer = r.trim_start_matches('#');
    let target = if pointer.is_empty() {
        Some(root)
    } else {
        root.pointer(&decode_pointer(pointer))
    }?;
    if target.get("$ref").is_some() {
        resolve(root, target, depth + 1)
    } else {
        Some(target)
    }
}

fn decode_pointer(p: &str) -> String {
    p.replace("~1", "/").replace("~0", "~")
}

fn check(ctx: &mut Ctx<'_>, schema: &Value, inst: &Value, path: &str) {
    // `true` accepte tout, `false` refuse tout (JSON Schema booléen).
    match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            ctx.err(path, "aucune valeur n'est acceptée ici (schéma `false`)");
            return;
        }
        _ => {}
    }
    let Some(obj) = schema.as_object() else {
        return;
    };

    if obj.contains_key("$ref") {
        let depth = ctx.depth;
        match resolve(ctx.root, schema, depth) {
            Some(target) => {
                if ctx.depth >= MAX_REF_DEPTH {
                    ctx.err(path, "profondeur de $ref dépassée");
                    return;
                }
                let target = target.clone();
                ctx.depth += 1;
                check(ctx, &target, inst, path);
                ctx.depth -= 1;
            }
            None => ctx.err(path, "$ref non résoluble (externe ou cyclique)"),
        }
        // 2020-12 : $ref peut cohabiter avec d'autres mots-clés, on continue.
    }

    if let Some(t) = obj.get("type") {
        check_type(ctx, t, inst, path);
    }
    if let Some(Value::Array(vals)) = obj.get("enum") {
        if !vals.iter().any(|v| v == inst) {
            ctx.err(
                path,
                format!(
                    "valeur hors énumération ; attendu l'un de {}",
                    vals.iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
        }
    }
    if let Some(c) = obj.get("const") {
        if c != inst {
            ctx.err(path, format!("attendu la constante {c}"));
        }
    }

    match inst {
        Value::Object(_) => check_object(ctx, obj, inst, path),
        Value::Array(_) => check_array(ctx, obj, inst, path),
        Value::String(s) => check_string(ctx, obj, s, path),
        Value::Number(_) => check_number(ctx, obj, inst, path),
        _ => {}
    }

    check_combinators(ctx, obj, inst, path);
}

fn check_type(ctx: &mut Ctx<'_>, t: &Value, inst: &Value, path: &str) {
    let ok = match t {
        Value::String(s) => type_matches(s, inst),
        Value::Array(a) => a
            .iter()
            .filter_map(|v| v.as_str())
            .any(|s| type_matches(s, inst)),
        _ => true,
    };
    if !ok {
        ctx.err(
            path,
            format!("type attendu {t}, reçu {}", json_type_name(inst)),
        );
    }
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || is_integral_float(v),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "null" => v.is_null(),
        _ => true,
    }
}

fn is_integral_float(v: &Value) -> bool {
    v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn child_path(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn check_object(ctx: &mut Ctx<'_>, obj: &serde_json::Map<String, Value>, inst: &Value, path: &str) {
    let map = inst.as_object().expect("appelé sur un objet");

    if let Some(Value::Array(req)) = obj.get("required") {
        for r in req.iter().filter_map(|v| v.as_str()) {
            if !map.contains_key(r) {
                ctx.err(path, format!("propriété requise manquante : `{r}`"));
            }
        }
    }

    let props = obj.get("properties").and_then(|v| v.as_object());
    let pattern_props = obj.get("patternProperties").and_then(|v| v.as_object());

    for (k, v) in map {
        let cp = child_path(path, k);
        let mut matched = false;
        if let Some(p) = props.and_then(|p| p.get(k)) {
            matched = true;
            check(ctx, p, v, &cp);
        }
        if let Some(pp) = pattern_props {
            for (re, sub) in pp {
                if regex::Regex::new(re)
                    .map(|r| r.is_match(k))
                    .unwrap_or(false)
                {
                    matched = true;
                    check(ctx, sub, v, &cp);
                }
            }
        }
        if !matched {
            match obj.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    ctx.err(path, format!("propriété non autorisée : `{k}`"));
                }
                Some(sub @ Value::Object(_)) => check(ctx, sub, v, &cp),
                _ => {}
            }
        }
    }

    if let Some(n) = obj.get("minProperties").and_then(|v| v.as_u64()) {
        if (map.len() as u64) < n {
            ctx.err(path, format!("au moins {n} propriétés attendues"));
        }
    }
    if let Some(n) = obj.get("maxProperties").and_then(|v| v.as_u64()) {
        if (map.len() as u64) > n {
            ctx.err(path, format!("au plus {n} propriétés attendues"));
        }
    }
}

fn check_array(ctx: &mut Ctx<'_>, obj: &serde_json::Map<String, Value>, inst: &Value, path: &str) {
    let arr = inst.as_array().expect("appelé sur un tableau");

    let prefix = obj.get("prefixItems").and_then(|v| v.as_array());
    if let Some(pre) = prefix {
        for (i, s) in pre.iter().enumerate() {
            if let Some(v) = arr.get(i) {
                check(ctx, s, v, &format!("{path}/{i}"));
            }
        }
    }
    if let Some(items) = obj.get("items") {
        let start = prefix.map(|p| p.len()).unwrap_or(0);
        for (i, v) in arr.iter().enumerate().skip(start) {
            check(ctx, items, v, &format!("{path}/{i}"));
        }
    }
    if let Some(n) = obj.get("minItems").and_then(|v| v.as_u64()) {
        if (arr.len() as u64) < n {
            ctx.err(path, format!("au moins {n} éléments attendus"));
        }
    }
    if let Some(n) = obj.get("maxItems").and_then(|v| v.as_u64()) {
        if (arr.len() as u64) > n {
            ctx.err(path, format!("au plus {n} éléments attendus"));
        }
    }
    if obj.get("uniqueItems").and_then(|v| v.as_bool()) == Some(true) {
        for i in 0..arr.len() {
            for j in (i + 1)..arr.len() {
                if arr[i] == arr[j] {
                    ctx.err(path, "les éléments doivent être uniques");
                    return;
                }
            }
        }
    }
}

fn check_string(ctx: &mut Ctx<'_>, obj: &serde_json::Map<String, Value>, s: &str, path: &str) {
    let len = s.chars().count() as u64;
    if let Some(n) = obj.get("minLength").and_then(|v| v.as_u64()) {
        if len < n {
            ctx.err(path, format!("longueur minimale {n} (reçu {len})"));
        }
    }
    if let Some(n) = obj.get("maxLength").and_then(|v| v.as_u64()) {
        if len > n {
            ctx.err(path, format!("longueur maximale {n} (reçu {len})"));
        }
    }
    if let Some(p) = obj.get("pattern").and_then(|v| v.as_str()) {
        match regex::Regex::new(p) {
            Ok(re) => {
                if !re.is_match(s) {
                    ctx.err(path, format!("ne correspond pas au motif `{p}`"));
                }
            }
            Err(_) => ctx.err(path, format!("motif `pattern` invalide : `{p}`")),
        }
    }
}

fn check_number(ctx: &mut Ctx<'_>, obj: &serde_json::Map<String, Value>, inst: &Value, path: &str) {
    let Some(x) = inst.as_f64() else { return };
    if let Some(m) = obj.get("minimum").and_then(|v| v.as_f64()) {
        if x < m {
            ctx.err(path, format!("minimum {m}"));
        }
    }
    if let Some(m) = obj.get("maximum").and_then(|v| v.as_f64()) {
        if x > m {
            ctx.err(path, format!("maximum {m}"));
        }
    }
    if let Some(m) = obj.get("exclusiveMinimum").and_then(|v| v.as_f64()) {
        if x <= m {
            ctx.err(path, format!("strictement supérieur à {m}"));
        }
    }
    if let Some(m) = obj.get("exclusiveMaximum").and_then(|v| v.as_f64()) {
        if x >= m {
            ctx.err(path, format!("strictement inférieur à {m}"));
        }
    }
    if let Some(m) = obj.get("multipleOf").and_then(|v| v.as_f64()) {
        if m > 0.0 && (x / m).fract().abs() > 1e-9 {
            ctx.err(path, format!("doit être un multiple de {m}"));
        }
    }
}

fn check_combinators(
    ctx: &mut Ctx<'_>,
    obj: &serde_json::Map<String, Value>,
    inst: &Value,
    path: &str,
) {
    if let Some(Value::Array(all)) = obj.get("allOf") {
        for s in all {
            check(ctx, s, inst, path);
        }
    }
    if let Some(Value::Array(any)) = obj.get("anyOf") {
        let ok = any.iter().any(|s| sub_valid(ctx.root, s, inst));
        if !ok {
            ctx.err(path, "ne satisfait aucune branche de `anyOf`");
        }
    }
    if let Some(Value::Array(one)) = obj.get("oneOf") {
        let n = one.iter().filter(|s| sub_valid(ctx.root, s, inst)).count();
        if n != 1 {
            ctx.err(
                path,
                format!("doit satisfaire exactement une branche de `oneOf` (satisfaites : {n})"),
            );
        }
    }
    if let Some(not) = obj.get("not") {
        if sub_valid(ctx.root, not, inst) {
            ctx.err(path, "ne doit pas satisfaire le schéma `not`");
        }
    }
}

fn sub_valid(root: &Value, schema: &Value, inst: &Value) -> bool {
    let mut errs = Vec::new();
    let mut ctx = Ctx {
        root,
        depth: 0,
        errors: &mut errs,
    };
    check(&mut ctx, schema, inst, "");
    errs.is_empty()
}

/// Extrait la valeur `default` d'une propriété (moteur de formulaires §14.7).
pub fn property_default<'a>(schema: &'a Value, prop: &str) -> Option<&'a Value> {
    schema
        .get("properties")
        .and_then(|p| p.get(prop))
        .and_then(|p| p.get("default"))
}

/// Taille du schéma sérialisé, pour les plafonds du registre paresseux (§8.9).
pub fn schema_bytes(schema: &Value) -> usize {
    crate::canonical::canonical_json(schema).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn object_required_and_types() {
        let s = json!({
            "type": "object",
            "properties": {"name": {"type":"string"}, "age": {"type":"integer","minimum":0}},
            "required": ["name"]
        });
        assert!(validate(&s, &json!({"name":"a","age":3})).is_empty());
        let e = validate(&s, &json!({"age":-1}));
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(e.iter().any(|x| x.message.contains("requise")));
        assert!(e.iter().any(|x| x.message.contains("minimum")));
    }

    #[test]
    fn additional_properties_false() {
        let s = json!({"type":"object","properties":{"a":{}},"additionalProperties":false});
        assert!(validate(&s, &json!({"a":1})).is_empty());
        assert_eq!(validate(&s, &json!({"a":1,"b":2})).len(), 1);
    }

    #[test]
    fn local_ref_is_resolved() {
        let s = json!({
            "$defs": {"pos": {"type":"integer","minimum":1}},
            "type":"object",
            "properties": {"n": {"$ref":"#/$defs/pos"}}
        });
        assert!(validate(&s, &json!({"n":5})).is_empty());
        assert_eq!(validate(&s, &json!({"n":0})).len(), 1);
    }

    #[test]
    fn external_ref_is_refused_not_fetched() {
        let s = json!({"$ref":"https://example.com/schema.json"});
        let e = validate(&s, &json!({}));
        assert_eq!(e.len(), 1);
        assert!(e[0].message.contains("non résoluble"));
    }

    #[test]
    fn cyclic_ref_is_bounded() {
        let s = json!({"$defs":{"a":{"$ref":"#/$defs/a"}}, "$ref":"#/$defs/a"});
        let e = validate(&s, &json!(1));
        assert_eq!(e.len(), 1, "un cycle doit être détecté, pas boucler");
    }

    #[test]
    fn arrays_prefix_and_items() {
        let s = json!({
            "type":"array",
            "prefixItems":[{"type":"string"}],
            "items":{"type":"integer"},
            "minItems":2
        });
        assert!(validate(&s, &json!(["a", 1, 2])).is_empty());
        assert_eq!(validate(&s, &json!(["a", "b"])).len(), 1);
        assert_eq!(validate(&s, &json!(["a"])).len(), 1);
    }

    #[test]
    fn one_of_requires_exactly_one() {
        let s = json!({"oneOf":[{"type":"string"},{"type":"integer"},{"type":"number"}]});
        assert!(validate(&s, &json!("x")).is_empty());
        // 1 satisfait integer ET number : deux branches, donc invalide.
        assert_eq!(validate(&s, &json!(1)).len(), 1);
    }

    #[test]
    fn enum_and_const() {
        let s = json!({"enum":["a","b"]});
        assert!(validate(&s, &json!("a")).is_empty());
        assert_eq!(validate(&s, &json!("c")).len(), 1);
        let s = json!({"const": 42});
        assert!(validate(&s, &json!(42)).is_empty());
        assert_eq!(validate(&s, &json!(43)).len(), 1);
    }

    #[test]
    fn pattern_and_lengths() {
        let s = json!({"type":"string","pattern":"^[a-z0-9-]+$","maxLength":5});
        assert!(validate(&s, &json!("ab-1")).is_empty());
        assert_eq!(validate(&s, &json!("AB")).len(), 1, "motif seul");
        assert_eq!(validate(&s, &json!("abcdef")).len(), 1, "longueur seule");
        assert_eq!(validate(&s, &json!("ABCDEF")).len(), 2, "motif + longueur");
    }

    #[test]
    fn boolean_schemas() {
        assert!(validate(&json!(true), &json!({"anything": 1})).is_empty());
        assert_eq!(validate(&json!(false), &json!(null)).len(), 1);
    }

    #[test]
    fn validate_ok_summarises() {
        let s = json!({"type":"object","required":["a"]});
        assert!(validate_ok(&s, &json!({"a":1})).is_ok());
        let e = validate_ok(&s, &json!({})).unwrap_err();
        assert!(e.contains("requise"));
    }

    #[test]
    fn nested_error_paths_point_to_the_offender() {
        let s = json!({
            "type":"object",
            "properties":{"items":{"type":"array","items":{"type":"object",
                "properties":{"n":{"type":"integer"}}}}}
        });
        let e = validate(&s, &json!({"items":[{"n":1},{"n":"x"}]}));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, "/items/1/n");
    }
}
