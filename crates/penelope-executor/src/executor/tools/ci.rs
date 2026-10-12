//! `ci_variable_set` (#350) : une variable CI posée depuis un secret rangé, par son nom.

use super::*;

impl NativeToolExecutor {
    /// Lit le secret dans le magasin, l'ajoute au caviardage, le passe à la forge sur
    /// l'entrée standard. Le résultat ne porte que la clé, la portée et l'issue.
    pub(super) async fn ci_tools(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let name = str_arg(args, "secret")?;
        penelope_platform::validate_secret_name(&name)
            .map_err(|e| ToolError::Invalid(e.to_string()))?;
        penelope_tools::ci::check_secret_name(&name)?;
        let req = penelope_tools::ci::Request::from_args(args)?;
        let cwd = self.cwd_arg(args)?;
        let allowed = self.services.config.config().tools.ci_hosts.clone();
        let target = penelope_tools::ci::resolve(&req, &cwd, &allowed).await?;
        let platform = self.services.platform.clone();
        let wanted = name.clone();
        let value = tokio::task::spawn_blocking(move || platform.secrets.get(&wanted))
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?
            .map_err(|e| ToolError::Other(format!("magasin de secrets : {e}")))?
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                ToolError::Invalid(format!(
                    "secret `{name}` absent du magasin : le propriétaire le range avec \
                     `/secrets <projet>` ou `penelope secret import <projet>`"
                ))
            })?;
        penelope_observe::register_secret(&value);
        let out = penelope_tools::ci::set_variable(&req, &target, &value).await?;
        let stderr = penelope_observe::redact(out.stderr.trim());
        if out.code != 0 {
            return Err(ToolError::Other(format!(
                "{} {} : code {} : {stderr}",
                req.forge, req.key, out.code
            )));
        }
        Ok(ToolOutcome::ok(json!({
            "ok": true,
            "forge": req.forge,
            "key": req.key,
            "scope": req.scope.unwrap_or_else(|| "*".into()),
            "host": target.host,
            "repo": target.repo,
            "from_secret": name,
            "updated": out.updated,
        })))
    }
}
