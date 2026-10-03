//! Outils git et réseau.

use super::*;

impl NativeToolExecutor {
    /// Git.
    pub(super) async fn git_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let v = match name {
            "git_status" => penelope_tools::git::status(&self.cwd_arg(args)?).await?,
            "git_diff" => {
                penelope_tools::git::diff(
                    &self.cwd_arg(args)?,
                    args.get("against").and_then(|v| v.as_str()),
                    b_arg(args, "staged").unwrap_or(false),
                )
                .await?
            }
            "git_branch" => {
                penelope_tools::git::branch(
                    &self.cwd_arg(args)?,
                    &str_arg(args, "name")?,
                    b_arg(args, "create").unwrap_or(true),
                )
                .await?
            }
            "git_commit" => {
                penelope_tools::git::commit(
                    &self.cwd_arg(args)?,
                    &str_arg(args, "message")?,
                    b_arg(args, "all").unwrap_or(true),
                )
                .await?
            }
            "git_clone" => return self.git_clone(args).await,
            "git_push" => {
                penelope_tools::git::push(
                    &self.cwd_arg(args)?,
                    args.get("remote")
                        .and_then(|v| v.as_str())
                        .unwrap_or("origin"),
                    &str_arg(args, "branch")?,
                )
                .await?
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// `git_clone` (#305) : sur une source `https://` dont le client de forge est connecté
    /// d'après l'inventaire, git emprunte ses identifiants par l'assistant du client. Un
    /// échec avec cet assistant refait une fois la sonde de connexion et le dit, sans
    /// nouvel essai.
    async fn git_clone(&self, args: &Value) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let dest = self.path_arg(args, "dest")?;
        let url = str_arg(args, "url")?;
        let mut auth = penelope_tools::git::CloneAuth::default();
        if let Some(host) = penelope_tools::git::normalize_clone_url(&url)
            .ok()
            .and_then(|u| penelope_tools::git::https_host(&u))
            && let Some(inv) = penelope_app::machine::cached(s).await
            && let Some(program) = inv.credential_helper(&host)
        {
            auth = penelope_tools::git::CloneAuth::forge_helper(program, &host);
        }
        auth.config.extend(s.platform.git_config.iter().cloned());
        let cloned = penelope_tools::git::clone_in(
            &url,
            &dest,
            &self.workspaces(),
            u_arg(args, "depth").map(|d| d as u32),
            &auth,
        )
        .await;
        match (cloned, auth.helper) {
            (Ok(v), _) => Ok(ToolOutcome::ok(v)),
            (Err(ToolError::Other(why)), Some(program)) => {
                let now = match penelope_app::machine::recheck_login(s, &program).await {
                    Some(account) => format!(
                        "`{program}` est toujours connecté ({account}) : l'échec ne vient pas \
                         de sa connexion (droits sur le dépôt, adresse)"
                    ),
                    None => format!(
                        "`{program}` n'est plus connecté, l'inventaire est corrigé : au \
                         propriétaire de lancer `{program} auth login`, ne réessaie pas"
                    ),
                };
                Err(ToolError::Other(format!(
                    "{why}\nIdentifiants empruntés à `{program}` ; nouvelle sonde : {now}."
                )))
            }
            (Err(e), _) => Err(e),
        }
    }

    /// Réseau.
    pub(super) async fn http_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        match name {
            "http_fetch" => {
                let headers: Vec<(String, String)> = args
                    .get("headers")
                    .and_then(|h| h.as_object())
                    .map(|o| {
                        o.iter()
                            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let v = penelope_tools::http::fetch(
                    &self.http,
                    &str_arg(args, "url")?,
                    args.get("method").and_then(|v| v.as_str()).unwrap_or("GET"),
                    &headers,
                    args.get("body").and_then(|v| v.as_str()),
                    &cfg.tools.http_allowlist,
                    u_arg(args, "max_bytes").unwrap_or(512 * 1024),
                    cfg.tools.http_block_private_ips,
                )
                .await?;
                return self
                    .fetched_page(v, &str_arg(args, "url")?)
                    .await
                    .map(ToolOutcome::eager);
            }

            other => Err(ToolError::Unknown(other.to_string())),
        }
    }
}
