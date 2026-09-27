//! Les valeurs JSON libres d'une réponse, gardées octet pour octet (épopée #208, T14).
//!
//! Le journal est haché sous sa forme canonique : les clés de chaque objet y sont
//! rangées, les flottants entiers perdent leur `.0`. Les arguments d'un appel d'outil et
//! les `reasoning_details` sont des objets libres, dont la V0 garde l'ordre du
//! fournisseur (`preserve_order`) : relus du journal, `{"path", "content"}` devenait
//! `{"content", "path"}`, et la requête suivante changeait d'octets avant son dernier
//! message (décision 0008, `source-de-verite.md` §7, risque 1).
//!
//! Quand la forme canonique d'une de ces valeurs ne se relit pas à l'identique, le
//! payload `conv.assistant` en porte aussi le texte JSON exact, sous `verbatim` : une
//! chaîne, que la forme canonique ne touche pas. La relecture la préfère à la valeur
//! rangée. Une valeur déjà canonique (un seul argument, clés dans l'ordre) n'est pas
//! doublée.

use penelope_kernel::canonical::canonical_json;
use penelope_llm::types::ChatMessage;
use serde_json::Value;
use std::collections::BTreeMap;

/// Clé de `reasoning_details` dans `verbatim`.
const REASONING_DETAILS: &str = "reasoning_details";

/// Clé des arguments de l'appel d'indice `i`.
fn arguments_key(i: usize) -> String {
    format!("tool_calls.{i}.arguments")
}

/// Le texte exact de `v`, s'il ne survit pas à la forme canonique du journal.
fn exact(v: &Value) -> Option<String> {
    let text = serde_json::to_string(v).ok()?;
    let relu: Value = serde_json::from_str(&canonical_json(v)).ok()?;
    (serde_json::to_string(&relu).ok()? != text).then_some(text)
}

/// Les valeurs libres d'un message que la forme canonique changerait.
pub fn verbatim_of(m: &ChatMessage) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (i, call) in m.tool_calls.iter().enumerate() {
        if let Some(text) = exact(&call.arguments) {
            out.insert(arguments_key(i), text);
        }
    }
    if let Some(text) = m.reasoning_details.as_ref().and_then(exact) {
        out.insert(REASONING_DETAILS.to_string(), text);
    }
    out
}

/// Rend au message relu du journal les valeurs gardées telles quelles.
pub fn restore_verbatim(m: &mut ChatMessage, verbatim: &BTreeMap<String, String>) {
    let parse = |k: &str| {
        verbatim
            .get(k)
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
    };
    for (i, call) in m.tool_calls.iter_mut().enumerate() {
        if let Some(v) = parse(&arguments_key(i)) {
            call.arguments = v;
        }
    }
    if let Some(v) = parse(REASONING_DETAILS) {
        m.reasoning_details = Some(v);
    }
}
