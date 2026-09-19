//! Conditions de transition (§12.4) et templates de variables (§12.5).
//!
//! Règles :
//! - les transitions sont évaluées **dans l'ordre**, la première vraie gagne ;
//! - aucune vraie donne `$blocked` ;
//! - une liste vide pour `metadata_*` est **vraie** (vacuité), avec un avertissement au
//!   chargement si la clé n'est jamais écrite.

use crate::model::{BLOCKED, StepResult, Transition};
use serde_json::Value;

/// Contexte d'évaluation d'une transition.
pub struct EvalContext<'a> {
    pub step_result: &'a StepResult,
    /// `stepOutput` de l'étape qui vient de finir.
    pub step_output: &'a Value,
    /// `session_metadata` courant.
    pub metadata: &'a Value,
}

/// Évalue une condition.
pub fn evaluate(cond: &Value, ctx: &EvalContext<'_>) -> bool {
    let Some(kind) = cond.get("type").and_then(|t| t.as_str()) else {
        return false;
    };
    match kind {
        "always" => true,
        "step_result" => {
            let expected = cond.get("result").and_then(|r| r.as_str()).unwrap_or("");
            ctx.step_result.as_str() == expected
        }
        "metadata_all_match" => metadata_all(cond, ctx, |actual, expected| actual == expected),
        "metadata_any_match" => {
            let (key, field, expected) = match triple(cond) {
                Some(t) => t,
                None => return false,
            };
            let items = list(ctx.metadata, &key);
            items
                .iter()
                .any(|i| field_of(i, &field).as_deref() == Some(expected.as_str()))
        }
        "metadata_all_in" => {
            let Some(key) = cond.get("key").and_then(|k| k.as_str()) else {
                return false;
            };
            let Some(field) = cond.get("field").and_then(|f| f.as_str()) else {
                return false;
            };
            let allowed: Vec<String> = cond
                .get("values")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let items = list(ctx.metadata, key);
            // Vacuité : une liste vide satisfait un « tous ».
            items.iter().all(|i| {
                field_of(i, field)
                    .map(|v| allowed.iter().any(|a| a == &v))
                    .unwrap_or(false)
            })
        }
        "output_match" => output_match(cond, ctx.step_output),
        "all" => cond
            .get("of")
            .and_then(|o| o.as_array())
            .map(|a| a.iter().all(|c| evaluate(c, ctx)))
            .unwrap_or(false),
        "any" => cond
            .get("of")
            .and_then(|o| o.as_array())
            .map(|a| a.iter().any(|c| evaluate(c, ctx)))
            .unwrap_or(false),
        "not" => cond.get("cond").map(|c| !evaluate(c, ctx)).unwrap_or(false),
        _ => false,
    }
}

fn metadata_all(cond: &Value, ctx: &EvalContext<'_>, pred: impl Fn(&str, &str) -> bool) -> bool {
    let Some((key, field, expected)) = triple(cond) else {
        return false;
    };
    let items = list(ctx.metadata, &key);
    // Vacuité assumée : « tous » sur une liste vide est vrai (§12.4).
    items.iter().all(|i| {
        field_of(i, &field)
            .map(|v| pred(&v, &expected))
            .unwrap_or(false)
    })
}

fn triple(cond: &Value) -> Option<(String, String, String)> {
    Some((
        cond.get("key")?.as_str()?.to_string(),
        cond.get("field")?.as_str()?.to_string(),
        cond.get("value")?.as_str()?.to_string(),
    ))
}

fn list<'a>(metadata: &'a Value, key: &str) -> Vec<&'a Value> {
    metadata
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn field_of(item: &Value, field: &str) -> Option<String> {
    item.get(field).map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// `output_match` : JSONPath simplifié sur `stepOutput`.
fn output_match(cond: &Value, output: &Value) -> bool {
    let Some(path) = cond.get("path").and_then(|p| p.as_str()) else {
        return false;
    };
    let Some(actual) = json_path(output, path) else {
        return false;
    };
    if let Some(eq) = cond.get("equals") {
        return &actual == eq || as_text(&actual) == as_text(eq);
    }
    if let Some(list) = cond.get("in").and_then(|v| v.as_array()) {
        return list
            .iter()
            .any(|x| x == &actual || as_text(x) == as_text(&actual));
    }
    if let Some(re) = cond.get("regex").and_then(|r| r.as_str()) {
        return regex::Regex::new(re)
            .map(|r| r.is_match(&as_text(&actual)))
            .unwrap_or(false);
    }
    false
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Chemin simplifié : `a.b[0].c`, avec `$.` optionnel en tête.
pub fn json_path(root: &Value, path: &str) -> Option<Value> {
    let p = path.trim_start_matches("$.").trim_start_matches('$');
    let mut cur = root;
    for raw in p.split('.') {
        if raw.is_empty() {
            continue;
        }
        let (name, indices) = split_indices(raw);
        if !name.is_empty() {
            cur = cur.get(name)?;
        }
        for i in indices {
            cur = cur.get(i)?;
        }
    }
    Some(cur.clone())
}

fn split_indices(seg: &str) -> (&str, Vec<usize>) {
    let Some(start) = seg.find('[') else {
        return (seg, Vec::new());
    };
    let name = &seg[..start];
    let mut idx = Vec::new();
    let mut rest = &seg[start..];
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        if let Ok(n) = rest[open + 1..open + close].parse::<usize>() {
            idx.push(n);
        }
        rest = &rest[open + close + 1..];
    }
    (name, idx)
}

/// Choisit la transition. Aucune vraie ⇒ `$blocked` (§12.4).
pub fn choose(transitions: &[Transition], ctx: &EvalContext<'_>) -> String {
    for t in transitions {
        if evaluate(&t.condition, ctx) {
            return t.goto.clone();
        }
    }
    BLOCKED.to_string()
}

// ------------------------------------------------------------------ templates

/// Substitue les variables `{{…}}` (§12.5).
pub fn substitute(template: &str, vars: &TemplateVars<'_>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(template.len());
    let mut unknown = Vec::new();
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return (out, unknown);
        };
        let name = after[..end].trim();
        match vars.resolve(name) {
            Some(v) => out.push_str(&v),
            None => {
                unknown.push(name.to_string());
                // Variable inconnue : chaîne vide, avec avertissement (§12.5).
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    (out, unknown)
}

/// Sources de variables d'un run.
pub struct TemplateVars<'a> {
    pub workdir: &'a str,
    pub reason: &'a str,
    pub run_id: &'a str,
    pub now: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
    pub params: &'a Value,
    /// Sortie de l'étape précédente.
    pub step_output: &'a Value,
    /// Sorties de toutes les étapes déjà exécutées.
    pub steps: &'a Value,
    pub metadata: &'a Value,
    pub criteria_key: &'a str,
    /// Résumé de la conversation qui a lancé le run (issue #35) ; vide sinon.
    pub brief: &'a str,
}

impl TemplateVars<'_> {
    pub fn resolve(&self, name: &str) -> Option<String> {
        match name {
            "workdir" => return Some(self.workdir.to_string()),
            "reason" => return Some(self.reason.to_string()),
            "run.id" => return Some(self.run_id.to_string()),
            "now" => return Some(self.now.to_string()),
            "os" => return Some(self.os.to_string()),
            "arch" => return Some(self.arch.to_string()),
            "brief" => return Some(self.brief.to_string()),
            "criteriaCount" => return Some(self.criteria(|_| true).len().to_string()),
            "pendingCount" => {
                return Some(
                    self.criteria(|c| {
                        !matches!(
                            c.get("status").and_then(|s| s.as_str()),
                            Some("completed") | Some("passed")
                        )
                    })
                    .len()
                    .to_string(),
                );
            }
            "criteriaList" => {
                let items: Vec<String> = self
                    .criteria(|_| true)
                    .iter()
                    .map(|c| {
                        let status = c
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("pending");
                        // `text`, ou `label` : le plan écrivait l'un ou l'autre (#137).
                        let text = c
                            .get("text")
                            .or_else(|| c.get("label"))
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        let mark = if matches!(status, "completed" | "passed") {
                            "x"
                        } else {
                            " "
                        };
                        format!("- [{mark}] {text}")
                    })
                    .collect();
                return Some(items.join("\n"));
            }
            "modifiedFiles" => {
                let files = self
                    .metadata
                    .get("modified_files")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                return Some(files);
            }
            _ => {}
        }

        if let Some(path) = name.strip_prefix("stepOutput.") {
            return json_path(self.step_output, path).map(|v| as_text(&v));
        }
        if let Some(path) = name.strip_prefix("steps.") {
            return json_path(self.steps, path).map(|v| as_text(&v));
        }
        if let Some(path) = name.strip_prefix("params.") {
            return json_path(self.params, path).map(|v| as_text(&v));
        }
        if let Some(path) = name.strip_prefix("metadata.") {
            return json_path(self.metadata, path).map(|v| as_text(&v));
        }
        // Forme courte : `{{ticket_url}}` vaut `{{params.ticket_url}}`.
        json_path(self.params, name).map(|v| as_text(&v))
    }

    fn criteria(&self, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
        self.metadata
            .get(self.criteria_key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter(|c| pred(c)).cloned().collect())
            .unwrap_or_default()
    }
}

/// Variables `{{…}}` présentes dans un texte, dans l'ordre d'apparition, dédupliquées.
pub fn placeholders(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim().to_string();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
        rest = &after[end + 2..];
    }
    out
}

/// Variables statiquement détectables : une variable hors de cette liste et hors
/// `params` déclarés est une **erreur de validation** (§12.5).
pub fn known_static_vars() -> Vec<&'static str> {
    vec![
        "workdir",
        "reason",
        "criteriaCount",
        "pendingCount",
        "criteriaList",
        "modifiedFiles",
        "run.id",
        "now",
        "os",
        "arch",
        "brief",
    ]
}

pub fn is_dynamic_var(name: &str) -> bool {
    name.starts_with("stepOutput.") || name.starts_with("steps.")
}

/// Éléments qui retiennent une condition `metadata_all_in` : `id` (ou texte) et valeur du
/// champ, « absent » s'il manque. Pour dire au journal pourquoi une boucle recommence
/// (issue #137).
pub fn unmet_items(cond: &Value, metadata: &Value) -> Vec<String> {
    if cond.get("type").and_then(|t| t.as_str()) != Some("metadata_all_in") {
        return Vec::new();
    }
    let (Some(key), Some(field)) = (
        cond.get("key").and_then(|k| k.as_str()),
        cond.get("field").and_then(|f| f.as_str()),
    ) else {
        return Vec::new();
    };
    let allowed: Vec<String> = cond
        .get("values")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    list(metadata, key)
        .iter()
        .filter_map(|i| {
            let value = field_of(i, field);
            if value.as_ref().is_some_and(|v| allowed.contains(v)) {
                return None;
            }
            let name = i
                .get("id")
                .or_else(|| i.get("text"))
                .or_else(|| i.get("label"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            Some(format!(
                "{name} ({})",
                value.unwrap_or_else(|| "absent".into())
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #137 : une boucle dit ce qui la retient : les critères sans statut coché, avec
    /// leur statut ou « absent ».
    #[test]
    fn unmet_items_name_what_holds_a_loop() {
        let cond = json!({"type": "metadata_all_in", "key": "criteria", "field": "status",
                          "values": ["completed", "passed"]});
        let meta = json!({"criteria": [
            {"id": "build", "status": "completed"},
            {"id": "tests", "status": "pending"},
            {"id": "doc"},
            {"status": "all_passed", "summary": "7/7"}
        ]});
        assert_eq!(
            unmet_items(&cond, &meta),
            vec!["tests (pending)", "doc (absent)", "? (all_passed)"]
        );
        assert!(unmet_items(&json!({"type": "always"}), &meta).is_empty());
    }
    use serde_json::json;

    fn ctx<'a>(result: &'a StepResult, output: &'a Value, metadata: &'a Value) -> EvalContext<'a> {
        EvalContext {
            step_result: result,
            step_output: output,
            metadata,
        }
    }

    #[test]
    fn always_and_step_result() {
        let r = StepResult::Success;
        let (o, m) = (json!({}), json!({}));
        let c = ctx(&r, &o, &m);
        assert!(evaluate(&json!({"type":"always"}), &c));
        assert!(evaluate(
            &json!({"type":"step_result","result":"success"}),
            &c
        ));
        assert!(!evaluate(
            &json!({"type":"step_result","result":"failure"}),
            &c
        ));
    }

    #[test]
    fn user_choices_are_step_results() {
        let r = StepResult::Choice("Déployer".into());
        let (o, m) = (json!({}), json!({}));
        let c = ctx(&r, &o, &m);
        assert!(evaluate(
            &json!({"type":"step_result","result":"Déployer"}),
            &c
        ));
        assert!(!evaluate(
            &json!({"type":"step_result","result":"Annuler"}),
            &c
        ));
    }

    #[test]
    fn metadata_all_in_with_criteria() {
        let r = StepResult::Completed;
        let o = json!({});
        let m = json!({"criteria":[
            {"id":"c1","status":"completed"},
            {"id":"c2","status":"passed"}
        ]});
        let c = ctx(&r, &o, &m);
        let cond = json!({
            "type":"metadata_all_in","key":"criteria","field":"status",
            "values":["completed","passed"]
        });
        assert!(evaluate(&cond, &c));

        let m2 = json!({"criteria":[{"id":"c1","status":"pending"}]});
        let c2 = ctx(&r, &o, &m2);
        assert!(!evaluate(&cond, &c2));
    }

    /// §12.4 : liste vide pour `metadata_*` ⇒ **vrai** (vacuité).
    #[test]
    fn empty_lists_satisfy_all_conditions() {
        let r = StepResult::Completed;
        let o = json!({});
        let m = json!({"criteria": []});
        let c = ctx(&r, &o, &m);
        assert!(evaluate(
            &json!({"type":"metadata_all_in","key":"criteria","field":"status","values":["x"]}),
            &c
        ));
        assert!(evaluate(
            &json!({"type":"metadata_all_match","key":"criteria","field":"status","value":"x"}),
            &c
        ));
        // Une clé absente se comporte comme une liste vide.
        let m2 = json!({});
        let c2 = ctx(&r, &o, &m2);
        assert!(evaluate(
            &json!({"type":"metadata_all_match","key":"criteria","field":"s","value":"x"}),
            &c2
        ));
        // `any` sur une liste vide est faux.
        assert!(!evaluate(
            &json!({"type":"metadata_any_match","key":"criteria","field":"s","value":"x"}),
            &c2
        ));
    }

    #[test]
    fn output_match_equals_in_regex() {
        let r = StepResult::Success;
        let o = json!({"forge":"github","exitCode":0,"data":{"items":[{"id":42}]}});
        let m = json!({});
        let c = ctx(&r, &o, &m);
        assert!(evaluate(
            &json!({"type":"output_match","path":"forge","equals":"github"}),
            &c
        ));
        assert!(evaluate(
            &json!({"type":"output_match","path":"exitCode","equals":0}),
            &c
        ));
        assert!(evaluate(
            &json!({"type":"output_match","path":"forge","in":["github","gitlab"]}),
            &c
        ));
        assert!(evaluate(
            &json!({"type":"output_match","path":"data.items[0].id","regex":"^4\\d$"}),
            &c
        ));
        assert!(!evaluate(
            &json!({"type":"output_match","path":"absent","equals":"x"}),
            &c
        ));
    }

    #[test]
    fn combinators() {
        let r = StepResult::Success;
        let (o, m) = (json!({"a":1}), json!({}));
        let c = ctx(&r, &o, &m);
        let t = json!({"type":"always"});
        let f = json!({"type":"step_result","result":"failure"});
        assert!(evaluate(&json!({"type":"all","of":[t, t]}), &c));
        assert!(!evaluate(&json!({"type":"all","of":[t, f]}), &c));
        assert!(evaluate(&json!({"type":"any","of":[f, t]}), &c));
        assert!(evaluate(&json!({"type":"not","cond": f}), &c));
    }

    #[test]
    fn unknown_condition_kinds_are_false_not_fatal() {
        let r = StepResult::Success;
        let (o, m) = (json!({}), json!({}));
        assert!(!evaluate(&json!({"type":"inventée"}), &ctx(&r, &o, &m)));
        assert!(!evaluate(&json!({}), &ctx(&r, &o, &m)));
    }

    #[test]
    fn transitions_are_evaluated_in_order_and_default_to_blocked() {
        let r = StepResult::Failure;
        let (o, m) = (json!({}), json!({}));
        let c = ctx(&r, &o, &m);
        let ts = vec![
            Transition::on_result("succes", "success"),
            Transition::on_result("echec", "failure"),
            Transition::always("fin"),
        ];
        assert_eq!(choose(&ts, &c), "echec");

        let only_success = vec![Transition::on_result("succes", "success")];
        assert_eq!(choose(&only_success, &c), BLOCKED);
    }

    #[test]
    fn json_path_handles_arrays_and_prefixes() {
        let v = json!({"a":{"b":[{"c":1},{"c":2}]}});
        assert_eq!(json_path(&v, "a.b[1].c"), Some(json!(2)));
        assert_eq!(json_path(&v, "$.a.b[0].c"), Some(json!(1)));
        assert_eq!(json_path(&v, "a.absent"), None);
    }

    fn vars<'a>(
        params: &'a Value,
        output: &'a Value,
        meta: &'a Value,
        steps: &'a Value,
    ) -> TemplateVars<'a> {
        TemplateVars {
            workdir: "/tmp/run",
            reason: "critères non remplis",
            run_id: "r_1",
            now: "2026-09-16T10:00:00Z",
            os: "macos",
            arch: "aarch64",
            params,
            step_output: output,
            steps,
            metadata: meta,
            criteria_key: "criteria",
            brief: "ticket 4312, cache à vider",
        }
    }

    #[test]
    fn template_substitution() {
        let params = json!({"ticket_url":"https://tracker/4312","repo":"org/projet"});
        let output = json!({"forge":"github","data":{"pr":77}});
        let meta = json!({"criteria":[
            {"id":"c1","text":"tests verts","status":"completed"},
            {"id":"c2","text":"lint propre","status":"pending"}
        ]});
        let steps = json!({"checkout":{"branch":"penelope/4312"}});
        let v = vars(&params, &output, &meta, &steps);

        let (out, unknown) = substitute(
            "Ticket {{ticket_url}} dans {{params.repo}} sur {{workdir}} ({{os}}/{{arch}}).\n\
             Forge : {{stepOutput.forge}} · PR {{stepOutput.data.pr}} · \
             branche {{steps.checkout.branch}}\n\
             {{criteriaCount}} critères, {{pendingCount}} en attente.\n{{criteriaList}}",
            &v,
        );
        assert!(unknown.is_empty(), "{unknown:?}");
        assert!(out.contains("https://tracker/4312"));
        assert!(out.contains("org/projet"));
        assert!(out.contains("/tmp/run"));
        assert!(out.contains("macos/aarch64"));
        assert!(out.contains("github"));
        assert!(out.contains("PR 77"));
        assert!(out.contains("penelope/4312"));
        assert!(out.contains("2 critères, 1 en attente"));
        assert!(out.contains("- [x] tests verts"));
        assert!(out.contains("- [ ] lint propre"));
    }

    #[test]
    fn unknown_variables_become_empty_with_a_warning() {
        let (p, o, m, s) = (json!({}), json!({}), json!({}), json!({}));
        let (out, unknown) = substitute("avant {{inconnue}} après", &vars(&p, &o, &m, &s));
        assert_eq!(out, "avant  après");
        assert_eq!(unknown, vec!["inconnue"]);
    }

    #[test]
    fn unterminated_placeholder_is_left_as_is() {
        let (p, o, m, s) = (json!({}), json!({}), json!({}), json!({}));
        let (out, _) = substitute("texte {{cassé", &vars(&p, &o, &m, &s));
        assert_eq!(out, "texte {{cassé");
    }

    #[test]
    fn dynamic_variable_detection() {
        assert!(is_dynamic_var("stepOutput.x"));
        assert!(is_dynamic_var("steps.a.b"));
        assert!(!is_dynamic_var("workdir"));
        assert!(known_static_vars().contains(&"criteriaList"));
        assert!(known_static_vars().contains(&"brief"));
        let (p, o, m, s) = (json!({}), json!({}), json!({}), json!({}));
        let (out, unknown) = substitute("Brief : {{brief}}", &vars(&p, &o, &m, &s));
        assert_eq!(out, "Brief : ticket 4312, cache à vider");
        assert!(unknown.is_empty());
    }
}
