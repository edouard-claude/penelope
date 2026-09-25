//! Méthodes d'exploitation : état, diagnostic, arrêt, jobs, audit, usage, sauvegarde.

use super::*;
use penelope_app::helpers::round_usd;

impl Rpc {
    /// État, diagnostic, arrêt, jobs, audit, usage et sauvegarde.
    pub(super) async fn ops(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::STATUS => Ok(serde_json::to_value(self.daemon.status().await?)?),
            method::PATHS => Ok(json!({
                "config": s.platform.dirs.config(),
                "data": s.platform.dirs.data(),
                "state": s.platform.dirs.state(),
                "logs": s.platform.dirs.logs(),
                "cache": s.platform.dirs.cache(),
                "db": s.platform.dirs.db_path(),
                "socket": s.platform.dirs.socket_path(),
            })),
            method::DOCTOR => {
                let mut checks = super::doctor::run(s, self.daemon.hooks.mcp_supervisor()).await;
                checks.push(penelope_ops::doctor::embedding_check(&self.daemon.embedder()).await);
                checks.push(penelope_ops::doctor::vault_index_check(s).await);
                checks.extend(penelope_ops::doctor::coherence_checks(s).await);
                checks.push(penelope_ops::doctor::logs_secret_check(s));
                checks.push(penelope_ops::doctor::stored_secret_check(s).await);
                checks.push(penelope_vault::vault_git::doctor_check(s));
                checks.push(penelope_ops::doctor::binary_signature_check(s));
                checks.push(penelope_ops::doctor::install_mode_check());
                checks.push(penelope_ops::doctor::pending_upgrade_check(s));
                checks.push(penelope_ops::doctor::schedules_check(s).await);
                checks.push(
                    penelope_executor::voice::doctor_check(
                        &self.daemon.services,
                        self.daemon.providers.as_ref(),
                    )
                    .await,
                );
                checks.push(penelope_ops::backup::doctor_check(&self.daemon.services).await);
                // Boucles de fond relancées ou mortes (#84).
                checks.push(penelope_app::tasks::doctor_check(
                    &self.daemon.supervision(),
                ));
                Ok(json!(checks))
            }
            method::METRICS => {
                // Les jauges se lisent au moment de la demande ; compteurs et
                // histogrammes s'accumulent pendant la vie du daemon (issue #103).
                use penelope_observe::metrics::gauge_set;
                gauge_set(
                    "penelope_approvals_pending",
                    &[],
                    s.approvals.count_pending().await? as f64,
                );
                gauge_set(
                    "penelope_effects_unknown",
                    &[],
                    s.effects
                        .count_by_state(penelope_kernel::effects::EffectState::Unknown)
                        .await? as f64,
                );
                gauge_set(
                    "penelope_rss_bytes",
                    &[],
                    crate::runtime::rss_mb() * 1024.0 * 1024.0,
                );
                Ok(json!({"text": penelope_observe::metrics::render()}))
            }
            method::SHUTDOWN => {
                self.daemon.handle.shutdown();
                Ok(json!({"ok": true}))
            }
            method::RESTART => {
                self.daemon.handle.request_restart();
                Ok(json!({"ok": true}))
            }
            // Jobs d'outils (issue #204) : ce qui tourne hors des tours.
            method::JOBS => {
                let jobs = penelope_executor::jobs::store(s);
                let all = p.get("all").and_then(|v| v.as_bool()) == Some(true);
                let now = s.clock.now_ms();
                let rows = if all {
                    let mut v = jobs.live().await?;
                    for sess in s.sessions.list(None, 100).await? {
                        v.extend(
                            jobs.of_session(sess.id.as_str(), false)
                                .await?
                                .into_iter()
                                .filter(|j| j.state.is_terminal()),
                        );
                    }
                    v
                } else {
                    jobs.live().await?
                };
                // Une ligne par job, sans son résultat : `penelope jobs` dit ce qui
                // tourne, `penelope logs` dit ce que ça a donné.
                Ok(json!(
                    rows.iter()
                        .map(|j| json!({
                            "job": j.id,
                            "tool": j.tool,
                            "état": j.state.as_str(),
                            "âge_s": j.age_s(now),
                            "session": j.session_id,
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::UPGRADE => {
                penelope_ops::upgrade::rpc(&self.daemon.services, &self.daemon.handle, p).await
            }
            method::IMPORT_HERMES => {
                let d = &self.daemon;
                let (mcp, m) = (d.hooks.mcp_supervisor(), d.hooks.messenger());
                penelope_ops::hermes::rpc(&d.services, mcp, m, p).await
            }
            method::STORE_REBUILD => {
                penelope_ops::session_ops::rebuild(&self.daemon.services).await
            }
            method::RESTORE => anyhow::bail!(
                "une restauration remplace la base : elle se fait daemon arrêté, `penelope stop` \
                 puis `penelope restore <sauvegarde>`"
            ),
            method::EVAL_RUN => anyhow::bail!(
                "les suites d'évaluation tournent depuis les sources : `penelope eval <suite>` \
                 dans le dépôt"
            ),
            method::AUDIT_VERIFY => Ok(serde_json::to_value(s.events.verify().await?)?),
            method::HISTORY_VERIFY => crate::history::verify(s, p).await,
            method::HISTORY_REINDEX => crate::history::reindex(s, p).await,
            // #205 : ce que le modèle avait sous les yeux, reconstitué depuis l'empreinte
            // du prompt et le transcript. Sans `turn`, le dernier tour de la session.
            method::AUDIT_SHOW => {
                let turn = match p.get("turn").and_then(|v| v.as_str()) {
                    Some(t) => t.to_string(),
                    None => {
                        let session = required_str(p, "session")?;
                        crate::audit::last_turn(s, &session)
                            .await?
                            .ok_or_else(|| anyhow::anyhow!("aucun appel pour {session}"))?
                    }
                };
                crate::audit::show(s, &turn).await
            }
            method::USAGE => {
                let by = p.get("by").and_then(|b| b.as_str()).unwrap_or("session");
                if !penelope_kernel::budget::USAGE_AXES.contains(&by) {
                    anyhow::bail!(
                        "axe inconnu `{by}` : {}",
                        penelope_kernel::budget::USAGE_AXES.join(", ")
                    );
                }
                let session = p.get("session").and_then(|v| v.as_str());
                let since = p.get("since").and_then(|v| v.as_str());
                let limit = p
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(20)
                    .clamp(1, 500);
                let rows = s.budget.report(by, session, since, limit).await?;
                Ok(json!(
                    rows.into_iter()
                        .map(|r| json!({
                            "key": r.key,
                            "label": r.label,
                            "costUsd": round_usd(r.cost_usd),
                            "tokens": r.tokens,
                            "calls": r.calls,
                            "estimated": r.estimated,
                            "last": r.last_ts,
                            "promptTokens": r.prompt,
                            "cachedTokens": r.cached,
                            "completionTokens": r.completion,
                            "cacheRatio": (r.cache_ratio() * 1000.0).round() / 1000.0,
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::BACKUP => {
                // Sans `push`, l'ancien comportement : un instantané de la base, local.
                let push = p.get("push").and_then(|v| v.as_bool()).unwrap_or(false);
                let full = push || p.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                if full {
                    let media = p.get("media").and_then(|v| v.as_bool());
                    return penelope_ops::backup::run(&self.daemon.services, push, media).await;
                }
                let dest = s.platform.dirs.data().join("backups").join(format!(
                    "penelope-{}.db",
                    s.clock.now_rfc3339().replace(':', "-")
                ));
                let took = s.store.snapshot_to(dest.clone()).await?;
                Ok(json!({"path": dest, "snapshot_ms": took.as_millis() as u64}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}
