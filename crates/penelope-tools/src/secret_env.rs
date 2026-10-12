//! Secrets collés injectés dans une commande (#352) : `shell_exec` reçoit
//! `secrets: {"PGPASSWORD": "ci.mentaleco.dev.postgres_password"}`, le daemon passe la
//! valeur en variable d'environnement, la commande n'écrit que `$PGPASSWORD`.
//!
//! Une règle générale pour toute commande (`glab`, `psql`, `docker login`, `aws`…) plutôt
//! qu'un outil par commande. Seuls les secrets que le propriétaire a collés
//! (`/secrets`, `secret import`, préfixe `ci.`) sont injectables : jamais un secret interne
//! de Pénélope.

use crate::error::{ToolError, ToolResult};
use serde_json::Value;

/// Variables qu'un secret ne remplace jamais : elles changeraient ce que la commande
/// exécute ou où elle lit.
const RESERVED: &[&str] = &[
    "PATH", "HOME", "SHELL", "IFS", "ENV", "BASH_ENV", "PWD", "USER",
];

/// Seuls les secrets collés par le propriétaire (`ci.…`) sont injectables.
pub fn check_secret_name(name: &str) -> ToolResult<()> {
    let prefix = penelope_platform::secrets::import::PREFIX;
    if !name.starts_with(prefix) || penelope_platform::validate_secret_name(name).is_err() {
        return Err(ToolError::Denied(format!(
            "`{name}` : seuls les secrets collés par le propriétaire (`/secrets <projet>`, \
             noms `{prefix}…`) s'injectent dans une commande"
        )));
    }
    Ok(())
}

fn check_var(var: &str) -> ToolResult<()> {
    let ok = var
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && var
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let reserved = RESERVED.contains(&var) || var.starts_with("LD_") || var.starts_with("DYLD_");
    if !ok || reserved {
        return Err(ToolError::Invalid(format!(
            "variable `{var}` : `[A-Z_][A-Z0-9_]*`, hors PATH, HOME, SHELL, LD_*, DYLD_*…"
        )));
    }
    Ok(())
}

/// Lit `secrets` des arguments : (variable, nom du secret), validés.
pub fn requested(args: &Value) -> ToolResult<Vec<(String, String)>> {
    let Some(map) = args.get("secrets") else {
        return Ok(Vec::new());
    };
    let Some(map) = map.as_object() else {
        return Err(ToolError::Invalid(
            "`secrets` : objet {\"VARIABLE\": \"nom du secret\"}".into(),
        ));
    };
    let mut out = Vec::new();
    for (var, name) in map {
        let name = name.as_str().ok_or_else(|| {
            ToolError::Invalid(format!("`secrets.{var}` : nom de secret attendu"))
        })?;
        check_var(var)?;
        check_secret_name(name)?;
        out.push((var.clone(), name.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_pasted_secrets_go_into_plain_variables() {
        assert_eq!(
            requested(&json!({"secrets": {"PGPASSWORD": "ci.mentaleco.dev.postgres_password"}}))
                .unwrap(),
            [(
                "PGPASSWORD".to_string(),
                "ci.mentaleco.dev.postgres_password".to_string()
            )]
        );
        assert!(requested(&json!({})).unwrap().is_empty());
        for bad in [
            json!({"secrets": {"K": "openrouter_api_key"}}),
            json!({"secrets": {"K": "codex.oauth"}}),
            json!({"secrets": {"PATH": "ci.a.b"}}),
            json!({"secrets": {"LD_PRELOAD": "ci.a.b"}}),
            json!({"secrets": {"pg password": "ci.a.b"}}),
            json!({"secrets": "ci.a.b"}),
        ] {
            assert!(requested(&bad).is_err(), "{bad}");
        }
    }
}
