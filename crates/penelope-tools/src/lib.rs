//! `penelope-tools` : outils natifs (§11).
//!
//! Chaque appel d'outil non `readOnly` passe par le ledger d'effets **avant** exécution
//! (§4.2) : c'est le daemon qui orchestre, ce crate fournit les implémentations et les
//! gardes (workspace, commandes interdites, SSRF, détection de boucles).

#![forbid(unsafe_code)]

pub mod args;
pub mod error;
pub mod fs;
pub mod git;
pub mod html;
pub mod http;
pub mod loops;
pub mod shell;
pub mod spec;
pub mod test_output;

pub use error::{ToolError, ToolResult};
pub use loops::{LoopDetector, LoopVerdict};
pub use spec::{
    ON_DEMAND, ToolSpec, WHY_FIELD, all as all_tools, always_exposed, core_exposed,
    get as tool_spec, is_on_demand, search_on_demand,
};

use penelope_kernel::risk::RiskClass;
use serde_json::Value;

/// Contexte d'exécution d'un appel d'outil.
pub struct ToolContext {
    pub session_id: String,
    pub run_id: Option<String>,
    pub workspaces: Vec<std::path::PathBuf>,
    pub sandbox_profile: String,
    pub http_allowlist: Vec<String>,
    pub block_private_ips: bool,
    pub max_output_bytes: usize,
    pub shell_timeout: std::time::Duration,
    pub in_workflow: bool,
}

impl ToolContext {
    pub fn workspace(&self) -> std::path::PathBuf {
        self.workspaces
            .first()
            .cloned()
            .unwrap_or_else(std::env::temp_dir)
    }
}

/// Résultat normalisé d'un appel d'outil.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub value: Value,
    /// Erreur d'exécution renvoyée au modèle plutôt qu'au harnais.
    pub is_error: bool,
    /// Texte prêt à insérer dans le transcript.
    pub text: String,
    /// Le résultat est volatil : candidat à la micro-compaction (§5.4 niveau 0).
    pub eager: bool,
}

impl ToolOutcome {
    pub fn ok(value: Value) -> Self {
        let text = render(&value);
        ToolOutcome {
            value,
            is_error: false,
            text,
            eager: false,
        }
    }
    pub fn eager(mut self) -> Self {
        self.eager = true;
        self
    }
    pub fn error(e: &ToolError) -> Self {
        ToolOutcome {
            value: serde_json::json!({"error": e.to_string()}),
            is_error: true,
            text: e.for_model(),
            eager: false,
        }
    }
}

/// Rendu lisible d'un résultat d'outil.
pub fn render(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            // Cas fréquents : on privilégie le champ le plus parlant.
            for k in ["content", "stdout", "body", "text"] {
                if let Some(Value::String(s)) = o.get(k)
                    && !s.is_empty()
                {
                    return s.clone();
                }
            }
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        }
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

/// Classe de risque effective : surcharge de configuration d'abord, spécification ensuite.
pub fn effective_risk(
    tool: &str,
    override_map: &std::collections::BTreeMap<String, String>,
) -> RiskClass {
    if let Some(r) = override_map.get(tool).and_then(|s| RiskClass::parse(s)) {
        return r;
    }
    spec::get(tool)
        .map(|s| s.risk)
        .unwrap_or(RiskClass::Unknown)
}

/// Un outil est-il sur la liste blanche d'une étape de workflow ou d'une skill ?
pub fn is_allowed(tool: &str, allow: &[String]) -> bool {
    if allow.is_empty() {
        return true;
    }
    allow
        .iter()
        .any(|a| a == tool || (a.ends_with('*') && tool.starts_with(a.trim_end_matches('*'))))
}

/// Valide les arguments contre le schéma de l'outil.
pub fn validate_args(tool: &str, args: &Value) -> ToolResult<()> {
    let Some(s) = spec::get(tool) else {
        return Err(ToolError::NoSuchTool {
            name: tool.to_string(),
            close: close_names(tool, spec::all().iter().map(|s| s.name), 5),
        });
    };
    penelope_kernel::schema::validate_ok(&s.schema, args).map_err(|reason| {
        ToolError::BadArguments {
            tool: tool.to_string(),
            reason,
            expected: expected_args(&s.schema, EXPECTED_ARGS_MAX_CHARS),
        }
    })
}

/// Balisage d'appel d'outil qu'un modèle laisse parfois dans une valeur au lieu de
/// structurer ses arguments (issue #117) : `<arg_key>objectif</arg_key> <arg_value>…`.
const CALL_MARKUP: &[&str] = &[
    "<arg_key>",
    "</arg_key>",
    "<arg_value>",
    "</arg_value>",
    "<tool_call>",
    "</tool_call>",
    "<function=",
    "<parameter=",
    "<|tool_call",
    "<|python_tag|>",
];

/// Premier champ dont la valeur contient du balisage d'appel d'outil : son chemin et la
/// balise trouvée.
pub fn call_markup(args: &Value) -> Option<(String, &'static str)> {
    fn walk(v: &Value, path: &str) -> Option<(String, &'static str)> {
        match v {
            Value::String(s) => CALL_MARKUP
                .iter()
                .find(|m| s.contains(*m))
                .map(|m| (path.to_string(), *m)),
            Value::Array(a) => a
                .iter()
                .enumerate()
                .find_map(|(i, x)| walk(x, &format!("{path}[{i}]"))),
            Value::Object(o) => o.iter().find_map(|(k, x)| {
                walk(
                    x,
                    &if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    },
                )
            }),
            _ => None,
        }
    }
    walk(args, "")
}

/// Plafond des paramètres rendus avec une erreur d'arguments : un gros schéma ne fait pas
/// exploser le tour (issue #110).
pub const EXPECTED_ARGS_MAX_CHARS: usize = 1_500;

/// Paramètres d'un schéma d'arguments, lisibles par le modèle : les requis d'abord, avec
/// leur type, leurs valeurs permises et leur description, sous `max_chars` ; ce qui ne
/// tient pas est compté, `tool_describe` donne le reste.
pub fn expected_args(schema: &Value, max_chars: usize) -> String {
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let Some(props) = schema["properties"].as_object() else {
        return if required.is_empty() {
            "- aucun paramètre déclaré".into()
        } else {
            required
                .iter()
                .map(|r| format!("- `{r}` (requis)"))
                .collect::<Vec<_>>()
                .join("\n")
        };
    };
    let mut names: Vec<&String> = props.keys().collect();
    names.sort_by_key(|n| (!required.contains(&n.as_str()), n.to_string()));
    let mut out = String::new();
    let mut shown = 0;
    for name in &names {
        let p = &props[name.as_str()];
        let ty = match &p["type"] {
            Value::String(t) => t.clone(),
            Value::Array(ts) => ts
                .iter()
                .filter_map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join("|"),
            _ => "valeur".into(),
        };
        let mut attrs = vec![ty];
        if required.contains(&name.as_str()) {
            attrs.push("requis".into());
        }
        if let Some(values) = p["enum"].as_array() {
            let list: Vec<String> = values
                .iter()
                .take(8)
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect();
            attrs.push(format!("une de : {}", list.join(", ")));
        }
        let mut line = format!("- `{name}` ({})", attrs.join(", "));
        if let Some(d) = p["description"].as_str().filter(|d| !d.trim().is_empty()) {
            let d = d.replace('\n', " ");
            let d: String = if d.chars().count() > 160 {
                format!("{}…", d.chars().take(159).collect::<String>())
            } else {
                d
            };
            line.push_str(&format!(" : {d}"));
        }
        if out.chars().count() + line.chars().count() + 1 > max_chars && shown > 0 {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&line);
        shown += 1;
    }
    let rest = names.len() - shown;
    if rest > 0 {
        out.push_str(&format!(
            "\n- … {rest} autre(s) paramètre(s) facultatif(s) : `tool_describe` les donne"
        ));
    }
    out
}

/// Noms proches d'un nom d'outil inconnu : même nom à quelques fautes près, ou mots en
/// commun (`schedule_add` rapproche `schedule_create`). Les plus proches d'abord.
pub fn close_names<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
    max: usize,
) -> Vec<String> {
    fn words(s: &str) -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 3 && *w != "mcp")
            .map(String::from)
            .collect()
    }
    fn distance(a: &str, b: &str) -> usize {
        let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for (i, ca) in a.iter().enumerate() {
            let mut cur = vec![i + 1; b.len() + 1];
            for (j, cb) in b.iter().enumerate() {
                cur[j + 1] = (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1);
            }
            prev = cur;
        }
        prev[b.len()]
    }
    let wanted = name.to_lowercase();
    let wanted_words = words(name);
    let mut scored: Vec<(usize, usize, String)> = candidates
        .into_iter()
        .filter(|c| *c != name)
        .filter_map(|c| {
            let d = distance(&wanted, &c.to_lowercase());
            let shared = words(c).iter().filter(|w| wanted_words.contains(w)).count();
            (d <= (wanted.chars().count() / 3).max(2) || shared > 0)
                .then(|| (usize::MAX - shared, d, c.to_string()))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(max).map(|(_, _, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #110 : les paramètres attendus, requis d'abord, sous le plafond.
    #[test]
    fn expected_arguments_are_readable_and_bounded() {
        let schema = json!({
            "type": "object",
            "properties": {
                "b_opt": {"type": "string", "enum": ["x", "y"]},
                "a_req": {"type": "integer", "description": "Identifiant"},
            },
            "required": ["a_req"]
        });
        let t = expected_args(&schema, 1_000);
        assert_eq!(
            t,
            "- `a_req` (integer, requis) : Identifiant\n- `b_opt` (string, une de : x, y)"
        );
        let mut props = serde_json::Map::new();
        for i in 0..50 {
            props.insert(
                format!("p{i:02}"),
                json!({"type": "string", "description": "d".repeat(300)}),
            );
        }
        let big = expected_args(&json!({"properties": props}), EXPECTED_ARGS_MAX_CHARS);
        assert!(
            big.chars().count() <= EXPECTED_ARGS_MAX_CHARS + 100,
            "{}",
            big.len()
        );
        assert!(big.contains("autre(s) paramètre(s)"));
        let e = validate_args("fs_read", &json!({})).unwrap_err();
        assert!(
            e.for_model().contains("`path` (string, requis)"),
            "{}",
            e.for_model()
        );
    }

    /// #110 : un nom inconnu rapproche les noms à quelques fautes près ou aux mots
    /// communs, pas n'importe lequel.
    #[test]
    fn unknown_names_get_close_suggestions() {
        let names = [
            "schedule_create",
            "schedule_list",
            "fs_read",
            "mcp__redmine__get_issue",
        ];
        assert_eq!(close_names("fs_raed", names, 3), vec!["fs_read"]);
        let s = close_names("schedule_add", names, 3);
        assert!(s.contains(&"schedule_create".to_string()), "{s:?}");
        assert!(!s.contains(&"fs_read".to_string()));
        assert_eq!(
            close_names("mcp__redmine__getissue", names, 3)[0],
            "mcp__redmine__get_issue"
        );
        assert!(close_names("zzzzzz", names, 3).is_empty());
        let e = validate_args("fs_raed", &json!({})).unwrap_err();
        assert!(e.for_model().contains("`fs_read`"), "{}", e.for_model());
    }

    #[test]
    fn render_prefers_the_readable_field() {
        assert_eq!(render(&json!({"content":"bonjour","lines":3})), "bonjour");
        assert_eq!(render(&json!({"stdout":"sortie","exitCode":0})), "sortie");
        assert!(render(&json!({"a":1})).contains("\"a\": 1"));
        assert_eq!(render(&json!("texte brut")), "texte brut");
    }

    #[test]
    fn empty_readable_field_falls_back_to_json() {
        let r = render(&json!({"stdout":"","exitCode":1}));
        assert!(r.contains("exitCode"));
    }

    #[test]
    fn risk_overrides_win_over_annotations() {
        let mut o = std::collections::BTreeMap::new();
        assert_eq!(effective_risk("fs_read", &o), RiskClass::Read);
        o.insert("fs_read".to_string(), "destructive".to_string());
        assert_eq!(effective_risk("fs_read", &o), RiskClass::Destructive);
        assert_eq!(effective_risk("inconnu", &o), RiskClass::Unknown);
    }

    #[test]
    fn allowlists_support_prefixes() {
        assert!(is_allowed("fs_read", &[]));
        assert!(is_allowed("fs_read", &["fs_read".to_string()]));
        assert!(is_allowed("fs_read", &["fs_*".to_string()]));
        assert!(!is_allowed("shell_exec", &["fs_*".to_string()]));
    }

    #[test]
    fn arguments_are_validated_against_the_spec() {
        validate_args("fs_read", &json!({"path":"a.rs"})).unwrap();
        assert!(validate_args("fs_read", &json!({})).is_err());
        assert!(validate_args("outil_inexistant", &json!({})).is_err());
    }

    #[test]
    fn outcome_carries_error_text_for_the_model() {
        let o = ToolOutcome::error(&ToolError::Denied("hors workspace".into()));
        assert!(o.is_error);
        assert!(o.text.contains("Ne réessaie pas"));
    }
}
