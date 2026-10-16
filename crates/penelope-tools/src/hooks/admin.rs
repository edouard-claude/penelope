//! Édition d'un hook de `hooks.d/` (#383) : `penelope hooks edit <nom> <champ> <valeur>`,
//! avec les règles de `mcp edit` (une valeur seule pour `args` vaut une liste d'un
//! élément). Le fichier n'est réécrit que si la déclaration modifiée reste valide.

use super::{Hook, parse};
use std::path::Path;

/// Champs qu'on peut modifier ; `name` vient du nom du fichier.
pub const EDITABLE: &[&str] = &[
    "on",
    "match",
    "command",
    "args",
    "cwd",
    "timeout",
    "sandbox",
    "on_failure",
    "deliver",
    "env",
];

/// Modifie un champ de `hooks.d/<nom>.toml` et rend la déclaration relue.
pub fn edit(dir: &Path, name: &str, field: &str, value: serde_json::Value) -> Result<Hook, String> {
    if !EDITABLE.contains(&field) {
        return Err(format!(
            "champ `{field}` inconnu ou non modifiable ({})",
            EDITABLE.join(", ")
        ));
    }
    let path = dir.join(format!("{name}.toml"));
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "aucun hook `{name}` : créer d'abord {} (voir docs/hooks.md)",
            path.display()
        )
    })?;
    let mut table: toml::Table = toml::from_str(&raw).map_err(|e| e.to_string())?;
    let value = match (field, value) {
        ("args", serde_json::Value::String(s)) => serde_json::Value::Array(vec![s.into()]),
        (_, v) => v,
    };
    let as_toml: toml::Value =
        serde_json::from_value(value).map_err(|e| format!("valeur de `{field}` : {e}"))?;
    table.insert(field.to_string(), as_toml);
    let text = toml::to_string_pretty(&table).map_err(|e| e.to_string())?;
    let hook = parse(name, &text)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(hook)
}
