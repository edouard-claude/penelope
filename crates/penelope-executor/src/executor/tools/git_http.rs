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
            "git_clone" => {
                let dest = self.path_arg(args, "dest")?;
                penelope_tools::git::clone_in(
                    &str_arg(args, "url")?,
                    &dest,
                    &self.workspaces(),
                    u_arg(args, "depth").map(|d| d as u32),
                )
                .await?
            }
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
