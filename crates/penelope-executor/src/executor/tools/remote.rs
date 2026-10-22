//! Les outils d'une mission `--host` (#395) : fichiers et shell sur l'hôte distant. La
//! politique et l'approbation ont déjà tourné ici ; l'hôte n'exécute que l'outil.

use super::*;
use penelope_tools::shell::ShellOutput;

impl NativeToolExecutor {
    /// L'hôte et la racine de la mission en cours dans cette session, s'il y en a un.
    pub(super) async fn mission_host(&self) -> Option<(String, String)> {
        let m = penelope_app::mission::load(&self.services, &self.env.session_id).await?;
        if m.state != penelope_app::mission::State::Running {
            return None;
        }
        Some((m.host?, m.cwd?))
    }

    /// Un outil de fichiers sur l'hôte.
    pub(super) async fn remote_fs(
        &self,
        host: &str,
        root: &str,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let req = penelope_app::mission::remote_request(
            &self.services,
            host,
            root,
            name,
            args.clone(),
            std::time::Duration::from_secs(120),
        );
        let v = penelope_app::child::remote::call(&self.services, host, &req, cancel)
            .await
            .map_err(ToolError::Other)?;
        let o = ToolOutcome::ok(v);
        Ok(if matches!(name, "fs_read" | "fs_list" | "fs_search") {
            o.eager()
        } else {
            o
        })
    }

    /// Une commande sur l'hôte ; la sortie revient au traitement local (masquage des
    /// secrets, digest des tests, artefacts).
    pub(super) async fn remote_shell(
        &self,
        (host, root): &(String, String),
        args: &Value,
        command: &str,
        env: Vec<(String, String)>,
        timeout: std::time::Duration,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ShellOutput> {
        let mut args = args.clone();
        args["command"] = json!(command);
        let mut req = penelope_app::mission::remote_request(
            &self.services,
            host,
            root,
            "shell_exec",
            args,
            timeout,
        );
        req.env = env;
        let v = penelope_app::child::remote::call(&self.services, host, &req, cancel)
            .await
            .map_err(ToolError::Other)?;
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Ok(ShellOutput {
            exit_code: v["exitCode"].as_i64().unwrap_or(-1) as i32,
            stdout: text("stdout"),
            stderr: text("stderr"),
            truncated: v["truncated"].as_bool().unwrap_or(false),
            duration_ms: v["durationMs"].as_u64().unwrap_or(0),
        })
    }
}
