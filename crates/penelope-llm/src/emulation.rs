//! Émulation d'outils pour les modèles sans tool calling natif (§10.1).
//!
//! Activée automatiquement selon le catalogue. Le principe : décrire les outils dans le
//! prompt système, demander un bloc JSON, puis le récupérer avec un parseur **tolérant**
//! (clôtures de balises manquantes, texte autour, virgules traînantes).

use crate::types::{ToolCall, ToolDef};
use serde_json::Value;

/// Bloc d'instructions ajouté au prompt système quand l'émulation est active.
pub fn emulation_preamble(tools: &[ToolDef]) -> String {
    let mut s = String::from(
        "Tu disposes des outils suivants. Pour en appeler un, réponds UNIQUEMENT par un \
         bloc JSON de la forme :\n\
         ```json\n{\"tool\": \"<nom>\", \"arguments\": { … }}\n```\n\
         N'ajoute aucun texte autour du bloc quand tu appelles un outil. Pour répondre \
         normalement, écris du texte sans bloc JSON.\n\nOutils :\n",
    );
    for t in tools {
        s.push_str(&format!(
            "- `{}` : {}\n  paramètres : {}\n",
            t.name,
            t.description,
            serde_json::to_string(&t.parameters).unwrap_or_else(|_| "{}".into())
        ));
    }
    s
}

/// Extrait un appel d'outil émulé d'une réponse texte.
///
/// Renvoie aussi le texte restant, débarrassé du bloc : un modèle bavard qui ajoute une
/// phrase avant le JSON ne doit pas faire perdre son appel.
pub fn parse_emulated(text: &str, known_tools: &[String]) -> (Option<ToolCall>, String) {
    let Some(v) = extract_json(text) else {
        return (None, text.to_string());
    };
    let name = v
        .get("tool")
        .or_else(|| v.get("name"))
        .or_else(|| v.get("function"))
        .and_then(|x| x.as_str());
    let Some(name) = name else {
        return (None, text.to_string());
    };
    if !known_tools.is_empty() && !known_tools.iter().any(|t| t == name) {
        return (None, text.to_string());
    }
    let arguments = v
        .get("arguments")
        .or_else(|| v.get("args"))
        .or_else(|| v.get("parameters"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let remaining = strip_json_block(text);
    (
        Some(ToolCall {
            id: format!("emul_{}", short_hash(name, &arguments)),
            name: name.to_string(),
            arguments,
        }),
        remaining,
    )
}

/// Trouve le premier objet JSON équilibré dans un texte, éventuellement enveloppé dans
/// une clôture Markdown.
pub fn extract_json(text: &str) -> Option<Value> {
    // 1. Bloc ```json … ``` (même non refermé).
    if let Some(start) = find_fence(text) {
        let after = &text[start..];
        let body = match after.find("```") {
            Some(end) => &after[..end],
            None => after,
        };
        if let Some(v) = parse_tolerant(body) {
            return Some(v);
        }
    }
    // 2. Premier objet équilibré du texte brut.
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(end) = balanced_end(text, i) {
                if let Some(v) = parse_tolerant(&text[i..=end]) {
                    return Some(v);
                }
            }
        }
        i += 1;
    }
    None
}

fn find_fence(text: &str) -> Option<usize> {
    for tag in ["```json\n", "```JSON\n", "```json\r\n", "```\n"] {
        if let Some(i) = text.find(tag) {
            return Some(i + tag.len());
        }
    }
    None
}

/// Index de l'accolade fermante correspondant à celle en `start`, en ignorant les
/// accolades dans les chaînes.
fn balanced_end(text: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, ch) in b.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if *ch == b'\\' {
                escaped = true;
            } else if *ch == b'"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Analyse tolérante : retire les virgules traînantes avant de réessayer.
fn parse_tolerant(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        return Some(v);
    }
    let cleaned = remove_trailing_commas(s);
    serde_json::from_str::<Value>(&cleaned).ok()
}

fn remove_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut escaped = false;
    let chars: Vec<char> = s.chars().collect();
    for i in 0..chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
            out.push(c);
            continue;
        }
        if c == ',' {
            // Regarde le prochain caractère non blanc.
            let next = chars[i + 1..].iter().find(|x| !x.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn strip_json_block(text: &str) -> String {
    let mut out = text.to_string();
    if let Some(i) = out.find("```") {
        let rest = &out[i + 3..];
        if let Some(j) = rest.find("```") {
            out = format!("{}{}", &out[..i], &rest[j + 3..]);
            return out.trim().to_string();
        }
        return out[..i].trim().to_string();
    }
    if let Some(start) = out.find('{') {
        if let Some(end) = balanced_end(&out, start) {
            let before = out[..start].to_string();
            let after = out[end + 1..].to_string();
            return format!("{before}{after}").trim().to_string();
        }
    }
    out.trim().to_string()
}

fn short_hash(name: &str, args: &Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    args.to_string().hash(&mut h);
    format!("{:x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<String> {
        vec!["fs_read".into(), "shell_exec".into()]
    }

    #[test]
    fn parses_fenced_block() {
        let t = "```json\n{\"tool\":\"fs_read\",\"arguments\":{\"path\":\"a.rs\"}}\n```";
        let (call, rest) = parse_emulated(t, &tools());
        let call = call.unwrap();
        assert_eq!(call.name, "fs_read");
        assert_eq!(call.arguments, json!({"path":"a.rs"}));
        assert!(rest.is_empty());
    }

    #[test]
    fn parses_unfenced_block_with_surrounding_text() {
        let t =
            "Je vais lire le fichier.\n{\"tool\":\"fs_read\",\"args\":{\"path\":\"b.rs\"}}\nVoilà.";
        let (call, rest) = parse_emulated(t, &tools());
        assert_eq!(call.unwrap().name, "fs_read");
        assert!(rest.contains("Je vais lire"), "{rest}");
        assert!(!rest.contains('{'));
    }

    #[test]
    fn tolerates_unclosed_fence() {
        let t = "```json\n{\"tool\":\"fs_read\",\"arguments\":{}}";
        assert!(parse_emulated(t, &tools()).0.is_some());
    }

    #[test]
    fn tolerates_trailing_commas() {
        let t = "{\"tool\":\"fs_read\",\"arguments\":{\"path\":\"a\",},}";
        let (call, _) = parse_emulated(t, &tools());
        assert_eq!(call.unwrap().arguments, json!({"path":"a"}));
    }

    #[test]
    fn braces_inside_strings_do_not_break_balance() {
        let t = r#"{"tool":"shell_exec","arguments":{"command":"echo \"{}\" | cat"}}"#;
        let (call, _) = parse_emulated(t, &tools());
        assert_eq!(
            call.unwrap().arguments,
            json!({"command":"echo \"{}\" | cat"})
        );
    }

    #[test]
    fn unknown_tool_is_not_an_emulated_call() {
        let t = r#"{"tool":"inconnu","arguments":{}}"#;
        assert!(parse_emulated(t, &tools()).0.is_none());
    }

    #[test]
    fn plain_text_is_left_alone() {
        let t = "Voici l'explication, sans appel d'outil.";
        let (call, rest) = parse_emulated(t, &tools());
        assert!(call.is_none());
        assert_eq!(rest, t);
    }

    #[test]
    fn json_in_prose_that_is_not_a_tool_call() {
        let t = "La configuration attendue est {\"a\": 1}.";
        assert!(parse_emulated(t, &tools()).0.is_none());
    }

    #[test]
    fn preamble_lists_every_tool() {
        let p = emulation_preamble(&[
            ToolDef::new("fs_read", "lire un fichier", json!({"type":"object"})),
            ToolDef::new(
                "shell_exec",
                "lancer une commande",
                json!({"type":"object"}),
            ),
        ]);
        assert!(p.contains("`fs_read`"));
        assert!(p.contains("`shell_exec`"));
        assert!(p.contains("\"tool\""));
    }
}
