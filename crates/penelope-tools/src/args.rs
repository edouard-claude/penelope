//! Arguments d'un appel tels que la politique les lit (sortis de l'exécuteur natif,
//! épopée #208, T09) : la boucle et l'exécuteur en ont besoin sans se connaître (T24).

use crate::{ToolError, ToolResult};
use serde_json::{Value, json};

/// Vrai quand un appel `shell_exec` demande le réseau (issue #106).
pub fn wants_network(tool: &str, args: &Value) -> bool {
    tool == "shell_exec" && args.get("network").and_then(|v| v.as_bool()) == Some(true)
}

/// Arguments d'un `tool_call` : l'objet `args`, ou `args_json` quand l'objet arrive vide
/// (certains fournisseurs vident un objet sans propriétés déclarées, issue #110).
pub fn call_arguments(args: &Value) -> ToolResult<Value> {
    let object = args.get("args").filter(|v| !v.is_null());
    if let Some(v) = object.filter(|v| v.as_object().is_some_and(|o| !o.is_empty())) {
        return Ok(v.clone());
    }
    // Des arguments rendus en chaîne, dans `args_json` ou à la place de l'objet.
    let raw = args
        .get("args_json")
        .and_then(|v| v.as_str())
        .or_else(|| object.and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty());
    if let Some(raw) = raw {
        return serde_json::from_str::<Value>(raw)
            .ok()
            .filter(|v| v.is_object())
            .ok_or_else(|| ToolError::Invalid("`args_json` n'est pas un objet JSON".into()));
    }
    Ok(json!({}))
}

/// Arguments sur lesquels portent la politique et la carte d'approbation : ceux de
/// l'outil visé par un `tool_call`, ceux de l'appel sinon.
pub fn effective_arguments(tool: &str, args: &Value) -> Value {
    if tool == "tool_call" {
        call_arguments(args).unwrap_or_else(|_| json!({}))
    } else {
        args.clone()
    }
}

/// Arguments sans l'intention : elle ne change pas l'appel, ni pour la garde de boucle ni
/// pour le serveur qui l'exécute.
pub fn without_intention(args: &Value) -> Value {
    let mut a = args.clone();
    if let Some(o) = a.as_object_mut() {
        o.remove(crate::WHY_FIELD);
    }
    a
}
