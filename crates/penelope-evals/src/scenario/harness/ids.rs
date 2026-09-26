//! Identifiants d'exécution cités par le script : `{{id:<préfixe>:<n>}}` dans une ligne de
//! `model.jsonl` devient le n-ième identifiant distinct à ce préfixe (`art`, `n`, `i`,
//! `sch`, `tj`, `r`… ; vide pour un ULID nu, un uid de mémoire) apparu dans les résultats
//! d'outils de la requête reçue, dans l'ordre. Les ULID étant aléatoires, c'est la seule
//! façon pour un modèle scripté de rappeler ce qu'un outil vient de créer ; le
//! normaliseur remet ensuite les jetons habituels dans les attendus.
//!
//! Un jeton sans identifiant correspondant est une erreur : le rejeu échoue en le disant,
//! jamais de remplacement silencieux.

use super::super::ScriptLine;
use penelope_llm::types::ChatRequest;
use regex::Regex;
use std::sync::OnceLock;

const ULID: &str = "[0-9A-HJKMNP-TV-Z]{26}";

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\{\{id:([a-z]{0,4}):(\d+)\}\}").expect("regex jeton"))
}

/// Identifiants distincts à ce préfixe, dans l'ordre d'apparition dans les résultats
/// d'outils de la requête.
fn candidates(req: &ChatRequest, prefix: &str) -> Vec<String> {
    let pattern = if prefix.is_empty() {
        format!(r"(?:^|[^A-Za-z0-9_])({ULID})\b")
    } else {
        format!(r"\b({prefix}_{ULID})\b")
    };
    let re = Regex::new(&pattern).expect("regex identifiant");
    let mut out: Vec<String> = Vec::new();
    for m in req.messages.iter().filter(|m| m.role.as_str() == "tool") {
        for caps in re.captures_iter(&m.text()) {
            let id = caps[1].to_string();
            if !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// Remplace les jetons `{{id:…}}` d'une ligne de script ; rend la ligne telle quelle
/// s'il n'y en a aucun.
pub(super) fn resolve(line: &ScriptLine, req: &ChatRequest) -> Result<ScriptLine, String> {
    let raw = serde_json::to_string(line).map_err(|e| e.to_string())?;
    if !token_re().is_match(&raw) {
        return Ok(line.clone());
    }
    let mut missing = None;
    let replaced = token_re().replace_all(&raw, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let n: usize = caps[2].parse().unwrap_or(0);
        let found = candidates(req, prefix);
        match n.checked_sub(1).and_then(|i| found.get(i)) {
            Some(id) => id.clone(),
            None => {
                missing.get_or_insert_with(|| {
                    format!(
                        "`{}` : {} identifiant(s) à ce préfixe dans les résultats d'outils de \
                         la requête",
                        &caps[0],
                        found.len()
                    )
                });
                caps[0].to_string()
            }
        }
    });
    if let Some(m) = missing {
        return Err(m);
    }
    serde_json::from_str(&replaced).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_llm::types::{ChatMessage, ToolCall};
    use serde_json::json;

    fn request(results: &[&str]) -> ChatRequest {
        let mut messages = vec![ChatMessage::user("bonjour")];
        for (i, r) in results.iter().enumerate() {
            messages.push(ChatMessage::tool_result(format!("c{i}"), "outil", *r));
        }
        ChatRequest {
            messages,
            ..Default::default()
        }
    }

    fn call(args: serde_json::Value) -> ScriptLine {
        ScriptLine::ToolCalls {
            text: String::new(),
            calls: vec![ToolCall {
                id: "c9".into(),
                name: "intent_cancel".into(),
                arguments: args,
            }],
        }
    }

    #[test]
    fn a_token_takes_the_nth_distinct_identifier_of_its_prefix() {
        let req = request(&[
            r#"{"id": "i_01JAAAAAAAAAAAAAAAAAAAAAAA", "uid": "01JBBBBBBBBBBBBBBBBBBBBBBB"}"#,
            r#"[{"id": "i_01JAAAAAAAAAAAAAAAAAAAAAAA"}, {"id": "i_01JCCCCCCCCCCCCCCCCCCCCCCC"}]"#,
        ]);
        let line = call(json!({"id": "{{id:i:2}}", "uid": "{{id::1}}"}));
        let ScriptLine::ToolCalls { calls, .. } = resolve(&line, &req).unwrap() else {
            panic!("ligne d'appels attendue");
        };
        assert_eq!(calls[0].arguments["id"], "i_01JCCCCCCCCCCCCCCCCCCCCCCC");
        assert_eq!(calls[0].arguments["uid"], "01JBBBBBBBBBBBBBBBBBBBBBBB");
        let plain = ScriptLine::Text("rien à citer".into());
        assert_eq!(resolve(&plain, &req).unwrap(), plain);
    }

    #[test]
    fn a_token_without_identifier_is_an_error_not_a_silent_copy() {
        let req = request(&[r#"{"id": "i_01JAAAAAAAAAAAAAAAAAAAAAAA"}"#]);
        let err = resolve(&call(json!({"id": "{{id:i:2}}"})), &req).unwrap_err();
        assert!(
            err.contains("{{id:i:2}}") && err.contains("1 identifiant"),
            "{err}"
        );
        let err = resolve(&call(json!({"id": "{{id:sch:1}}"})), &req).unwrap_err();
        assert!(err.contains("0 identifiant"), "{err}");
    }
}
