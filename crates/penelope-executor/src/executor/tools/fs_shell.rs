//! Outils de fichiers et shell.

use super::*;

impl NativeToolExecutor {
    /// Fichiers.
    pub(super) async fn fs_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

        let v = match name {
            "fs_read" => {
                let p = self.path_arg(args, "path")?;
                let offset = u_arg(args, "offset").unwrap_or(0);
                let limit = u_arg(args, "limit").unwrap_or(400);
                return Ok(ToolOutcome::ok(penelope_tools::fs::read(&p, offset, limit)?).eager());
            }
            "fs_list" => {
                let p = self.path_arg(args, "path")?;
                let rec = b_arg(args, "recursive").unwrap_or(false);
                let max = u_arg(args, "max_entries").unwrap_or(500);
                let v = penelope_tools::fs::list(&p, rec, max)?;
                let listing = render_listing(&p, &v);
                let mut o = ToolOutcome::ok(v.clone()).eager();
                // Une longue liste part en artefact : le modèle garde un résumé (issue #8).
                o.text = if listing.chars().count() > FS_LIST_INLINE_CHARS {
                    let art = s
                        .context
                        .history
                        .put_artifact(
                            Some(&self.env.session_id),
                            self.env.run_id.as_deref(),
                            "listing",
                            None,
                            &listing,
                        )
                        .await?;
                    summarise_listing(&p, &v, max, &listing, &art.id)
                } else {
                    listing
                };
                return Ok(o);
            }
            "fs_search" => {
                let root = match args.get("path").and_then(|v| v.as_str()) {
                    Some(p) if !p.is_empty() => penelope_tools::fs::resolve(p, &self.workspaces())?,
                    _ => self.workspace(),
                };
                let pattern = str_arg(args, "pattern")?;
                let glob = args.get("glob").and_then(|v| v.as_str());
                let max = u_arg(args, "max_results").unwrap_or(100);
                return Ok(ToolOutcome::ok(penelope_tools::fs::search(
                    &root, &pattern, glob, max,
                )?)
                .eager());
            }
            "fs_write" => {
                let p = self.path_arg(args, "path")?;
                let lock = self.locks.for_path(&p);
                let _g = lock.lock().await;
                penelope_tools::fs::write(&p, &str_arg(args, "content")?)?
            }
            "fs_edit" => {
                let p = self.path_arg(args, "path")?;
                let lock = self.locks.for_path(&p);
                let _g = lock.lock().await;
                penelope_tools::fs::edit(
                    &p,
                    &str_arg(args, "old")?,
                    &str_arg(args, "new")?,
                    b_arg(args, "replace_all").unwrap_or(false),
                )?
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Shell.
    #[allow(clippy::needless_return)] // bras déplacé tel quel de `dispatch`, où il n'était pas en fin
    pub(super) async fn shell_tools(
        &self,
        name: &str,
        args: &Value,
        cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        match name {
            "shell_exec" => {
                let command = str_arg(args, "command")?;
                let cwd = self.cwd_arg(args)?;
                let default_timeout =
                    penelope_kernel::config::parse_duration(&cfg.tools.shell_timeout)
                        .unwrap_or(std::time::Duration::from_secs(120));
                let timeout = u_arg(args, "timeout_ms")
                    .map(|ms| std::time::Duration::from_millis(ms as u64))
                    .unwrap_or(default_timeout)
                    .min(std::time::Duration::from_secs(1800));
                // Réseau accordé par appel (#106) : la carte d'approbation l'a dit.
                let network = cfg.sandbox.shell_network || b_arg(args, "network").unwrap_or(false);
                let profile = penelope_tools::shell::profile_with_denied_reads(
                    &cfg.sandbox.default_profile,
                    &cwd,
                    network,
                    &denied_reads(s),
                );
                let shell = shell_override(&cfg.tools.shell);
                let out = penelope_tools::shell::exec(
                    &s.platform.processes,
                    &command,
                    penelope_tools::shell::ExecOptions {
                        profile: Some(&profile),
                        cwd: Some(&cwd),
                        timeout,
                        max_output_bytes: cfg.tools.max_output_bytes,
                        shell,
                        cancel: Some(cancel),
                    },
                )
                .await?;
                let offline = !network
                    && cfg.sandbox.default_profile != "full"
                    && penelope_tools::shell::looks_like_network_failure(
                        &command,
                        out.exit_code,
                        &out.stdout,
                        &out.stderr,
                    );
                let mut value = out.to_json();
                if offline {
                    value["network"] = json!(false);
                    value["note"] = json!(penelope_tools::shell::NETWORK_OFF_NOTE);
                }
                let mut o = ToolOutcome::ok(value);
                // Suites de tests et longues sorties en échec : résumé et échecs pour le
                // modèle, sortie complète en artefact (issue #32).
                let full = args.get("output").and_then(|v| v.as_str()) == Some("full");
                if !full
                    && let Some(body) = penelope_tools::test_output::digest(
                        &command,
                        out.exit_code,
                        &out.stdout,
                        &out.stderr,
                    )
                {
                    let log = format!(
                        "$ {command}\nCode de sortie {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        out.exit_code, out.stdout, out.stderr
                    );
                    let art = s
                        .context
                        .history
                        .put_artifact(
                            Some(&self.env.session_id),
                            self.env.run_id.as_deref(),
                            "log",
                            Some("shell.log"),
                            &log,
                        )
                        .await?;
                    o.text = format!(
                        "{body}\n\nSortie complète : artifact_read(\"{}\"){}",
                        art.id,
                        if out.truncated {
                            " (déjà tronquée au plafond de sortie)"
                        } else {
                            ""
                        }
                    );
                    if let Some(hint) = crate::jobs::background_hint(&cfg, name, args) {
                        o.text.push_str(&hint);
                    }
                    return Ok(o.eager());
                }
                if out.exit_code != 0 {
                    o.text = format!(
                        "Code de sortie {}.\n--- stdout ---\n{}\n--- stderr ---\n{}",
                        out.exit_code, out.stdout, out.stderr
                    );
                    if offline {
                        o.text =
                            format!("{}\n\n{}", penelope_tools::shell::NETWORK_OFF_NOTE, o.text);
                    }
                }
                // Un délai demandé au-delà de `tools.background_after` a immobilisé le
                // tour : la prochaine fois, proposer l'arrière-plan (issue #204).
                if let Some(hint) = crate::jobs::background_hint(&cfg, name, args) {
                    o.text.push_str(&hint);
                }
                return Ok(o.eager());
            }

            other => Err(ToolError::Unknown(other.to_string())),
        }
    }
}
