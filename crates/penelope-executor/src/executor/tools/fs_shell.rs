//! Outils de fichiers et shell.

use super::*;

/// Une écriture `vault:` entre en mémoire : elle passe le filtre des secrets du vault,
/// comme `mem_note` (issue #256).
fn vault_write_filter(args: &Value, text: &str) -> ToolResult<()> {
    let by_prefix = args
        .get("path")
        .and_then(|v| v.as_str())
        .is_some_and(|p| p.starts_with(penelope_tools::fs::VAULT_PREFIX));
    if by_prefix {
        penelope_vault::vault_ops::secret_filter(text).map_err(ToolError::Denied)?;
    }
    Ok(())
}

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
                    Some(p) if !p.is_empty() => self.resolve_path(p)?,
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
                let content = str_arg(args, "content")?;
                vault_write_filter(args, &content)?;
                let mut v = penelope_tools::fs::write(&p, &content)?;
                if let Some(w) = self.outside_vault_warning(&p, &content) {
                    v["avertissement"] = json!(w);
                }
                self.after_edit_hook(&p, &mut v).await;
                v
            }
            "fs_edit" => {
                let p = self.path_arg(args, "path")?;
                let new = str_arg(args, "new")?;
                vault_write_filter(args, &new)?;
                let lock = self.locks.for_path(&p);
                let _g = lock.lock().await;
                let mut v = penelope_tools::fs::edit(
                    &p,
                    &str_arg(args, "old")?,
                    &new,
                    b_arg(args, "replace_all").unwrap_or(false),
                )?;
                self.after_edit_hook(&p, &mut v).await;
                v
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Hook après édition (#366) : la commande `--hook` de la mission en cours, jointe au
    /// résultat sous `hook`. Elle vient du propriétaire, jamais du dépôt (#366, revue de
    /// sécurité) ; elle tourne dans le dépôt de la mission, sous le bac à sable du shell,
    /// et seulement pour un fichier de ce dépôt dont le chemin ne porte aucun caractère
    /// que le shell interprète.
    async fn after_edit_hook(&self, path: &Path, v: &mut Value) {
        let s = &self.services;
        let Some(mission) = penelope_app::mission::load(s, &self.env.session_id).await else {
            return;
        };
        let (Some(command), Some(cwd)) = (&mission.hook, &mission.cwd) else {
            return;
        };
        if mission.state != penelope_app::mission::State::Running {
            return;
        }
        // Revérifié à chaque édition : le dossier reste dans un workspace (TOCTOU).
        let Some(dir) = penelope_app::mission::dir_of(s, cwd) else {
            return;
        };
        let Ok(relative) = path.strip_prefix(&dir) else {
            return;
        };
        let Some(command) =
            penelope_tools::edit_hooks::render(command, &relative.to_string_lossy())
        else {
            v["hook"] =
                json!({"skipped": "chemin avec un caractère spécial du shell : hook non lancé"});
            return;
        };
        let cfg = s.config.config();
        let profile = penelope_tools::shell::profile_with_denied_reads(
            &cfg.sandbox.default_profile,
            &dir,
            false,
            &denied_reads(s),
        );
        let out = penelope_tools::shell::exec(
            &s.platform.processes,
            &command,
            penelope_tools::shell::ExecOptions {
                profile: Some(&profile),
                cwd: Some(&dir),
                timeout: std::time::Duration::from_secs(120),
                max_output_bytes: 64 * 1024,
                shell: shell_override(&cfg.tools.shell),
                ..Default::default()
            },
        )
        .await;
        v["hook"] = match out {
            Ok(o) if o.exit_code == 0 => json!({"command": command, "exitCode": 0}),
            Ok(o) => {
                let text = format!("{}{}", o.stdout, o.stderr);
                let chars: Vec<char> = text.chars().collect();
                let tail: String = chars[chars.len().saturating_sub(4_000)..].iter().collect();
                json!({"command": command, "exitCode": o.exit_code, "output": tail,
                    "note": "le hook échoue après cette édition : corrige avant de continuer"})
            }
            Err(e) => json!({"command": command, "error": e.to_string()}),
        };
    }

    /// Note de forme vault écrite dans un workspace hors du vault (issue #256) : elle ne
    /// sera ni indexée ni versionnée. L'écriture est faite ; le modèle l'apprend ici, avec
    /// le chemin `vault:…` qu'il visait sans doute. Un brouillon ordinaire passe sans mot.
    fn outside_vault_warning(&self, written: &Path, content: &str) -> Option<String> {
        let vault = canonical_workspace(&penelope_app::helpers::vault_dir(&self.services));
        if written.starts_with(&vault) {
            return None;
        }
        let rel = self
            .workspaces()
            .iter()
            .filter_map(|w| written.strip_prefix(w).ok())
            .min_by_key(|r| r.components().count())?
            .to_string_lossy()
            .replace('\\', "/");
        penelope_memory::wiki::vault_shaped(&rel, content).then(|| {
            format!(
                "écrit hors du vault ({}) : ni indexé ni versionné ; pour le vault, \
                 `vault:{rel}`",
                vault.display()
            )
        })
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
                // Secrets collés demandés par l'appel (#352) : lus au magasin, caviardés,
                // passés en variables ; la commande n'en écrit que le nom.
                let wanted = penelope_tools::secret_env::requested(args)?;
                let env = if wanted.is_empty() {
                    Vec::new()
                } else {
                    let platform = s.platform.clone();
                    tokio::task::spawn_blocking(move || {
                        wanted
                            .into_iter()
                            .map(|(var, name)| match platform.secrets.get(&name) {
                                Ok(Some(v)) if !v.is_empty() => {
                                    penelope_observe::register_secret(&v);
                                    Ok((var, v))
                                }
                                Ok(_) => Err(ToolError::Invalid(format!(
                                    "secret `{name}` absent du magasin : le propriétaire le \
                                     range avec `/secrets <projet>`"
                                ))),
                                Err(e) => {
                                    Err(ToolError::Other(format!("magasin de secrets : {e}")))
                                }
                            })
                            .collect::<ToolResult<Vec<_>>>()
                    })
                    .await
                    .map_err(|e| ToolError::Other(e.to_string()))??
                };
                let masked: Vec<String> = env.iter().map(|(_, v)| v.clone()).collect();
                let mut out = penelope_tools::shell::exec(
                    &s.platform.processes,
                    &command,
                    penelope_tools::shell::ExecOptions {
                        profile: Some(&profile),
                        cwd: Some(&cwd),
                        timeout,
                        max_output_bytes: cfg.tools.max_output_bytes,
                        shell,
                        cancel: Some(cancel),
                        env,
                    },
                )
                .await?;
                // Une valeur injectée ne revient jamais au modèle, même affichée par la
                // commande (#352).
                for v in &masked {
                    out.stdout = mask_value(&out.stdout, v);
                    out.stderr = mask_value(&out.stderr, v);
                }
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

/// Masque `value` dans `text`, et le début de `value` que laisserait une sortie tronquée en
/// plein milieu (#352) : `…AKDEV12` d'une clé coupée ne passe pas non plus.
fn mask_value(text: &str, value: &str) -> String {
    let mut out = text.replace(value, "[secret masqué]");
    for k in (4..value.len()).rev() {
        if let Some(head) = value.get(..k)
            && out.ends_with(head)
        {
            out.truncate(out.len() - head.len());
            out.push_str("[secret masqué]");
            break;
        }
    }
    out
}

#[cfg(test)]
pub(crate) fn mask_value_for_tests(text: &str, value: &str) -> String {
    mask_value(text, value)
}
