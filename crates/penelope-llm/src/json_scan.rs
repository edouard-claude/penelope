//! Lecture tolérante d'un objet JSON dans du texte.
//!
//! Un modèle qui renvoie des arguments d'outil mal formés (clôture Markdown, texte autour,
//! virgule traînante) ne doit pas faire échouer le tour : le harnais récupère ce qui est
//! lisible et laisse le modèle se corriger. L'émulation d'outils qui vivait ici a été
//! retirée (issue #54, décision 0009) : un modèle sans tool calling est refusé, pas émulé.

use serde_json::Value;

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
        if bytes[i] == b'{'
            && let Some(end) = balanced_end(text, i)
            && let Some(v) = parse_tolerant(&text[i..=end])
        {
            return Some(v);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_a_fenced_block() {
        let t = "```json\n{\"path\":\"a.rs\"}\n```";
        assert_eq!(extract_json(t), Some(json!({"path":"a.rs"})));
    }

    #[test]
    fn tolerates_an_unclosed_fence() {
        assert!(extract_json("```json\n{\"a\":1}").is_some());
    }

    #[test]
    fn tolerates_trailing_commas() {
        assert_eq!(extract_json("{\"path\":\"a\",}"), Some(json!({"path":"a"})));
    }

    #[test]
    fn braces_inside_strings_do_not_break_balance() {
        let t = r#"{"command":"echo \"{}\" | cat"}"#;
        assert_eq!(
            extract_json(t),
            Some(json!({"command":"echo \"{}\" | cat"}))
        );
    }

    #[test]
    fn reads_an_object_surrounded_by_prose() {
        let t = "Je vais lire le fichier.\n{\"path\":\"b.rs\"}\nVoilà.";
        assert_eq!(extract_json(t), Some(json!({"path":"b.rs"})));
    }

    #[test]
    fn plain_text_has_no_object() {
        assert!(extract_json("Voici l'explication, sans JSON.").is_none());
    }
}
