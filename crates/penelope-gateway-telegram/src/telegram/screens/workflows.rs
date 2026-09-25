//! Écrans des workflows : `wf`, `wf.detail`, `runs`, `run.detail`, `schedules`.

use super::*;

impl TelegramGateway {
    /// Écran `wf`.
    pub(super) async fn screen_wf(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let here = back_of(name, args);
        let screen: Screen = {
            let list = rpc.call(m::WF_LIST, json!({})).await?;
            let list = list.as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                return Ok(Screen::new("Aucun workflow enregistré."));
            }
            let page = page_of(args);
            let mut sc = Screen::new(format!(
                "**Workflows** ({})\n▶️ lance (les paramètres sont demandés un par un), ℹ️ \
                 détaille les étapes.\n",
                list.len()
            ));
            for w in list.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let id = w["id"].as_str().unwrap_or("?");
                let name = w["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
                sc.text.push_str(&format!(
                    "\n- **{name}** (`{id}`) · {} étape(s){}",
                    super::shown(&w["steps"]),
                    if w["runsHere"].as_bool() == Some(false) {
                        " · pas sur cette machine"
                    } else {
                        ""
                    }
                ));
                sc.rows.push(vec![
                    self.op(
                        &format!("▶️ {}", trunc(name, 32)),
                        "wf.run",
                        json!({"id": id}),
                        here.clone(),
                    )
                    .await?,
                    self.nav("ℹ️", "wf.detail", json!({"id": id})).await?,
                ]);
            }
            self.pager(&mut sc, name, args, list.len()).await?;
            sc
        };
        Ok(screen)
    }

    /// Écran `wf.detail`.
    pub(super) async fn screen_wf_detail(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let here = back_of(name, args);
        let screen: Screen = {
            let id = args["id"].as_str().unwrap_or_default();
            let entry = s
                .workflows
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
            let w = &entry;
            let mut t = format!(
                "**{}** (`{id}`)\n{}\n",
                if w.metadata.name.is_empty() {
                    id
                } else {
                    &w.metadata.name
                },
                w.metadata.description
            );
            if !w.metadata.parameters.is_empty() {
                t.push_str("\n**Paramètres**\n");
                for p in &w.metadata.parameters {
                    t.push_str(&format!(
                        "- {} (`{}`){}{}\n",
                        if p.label.is_empty() { &p.id } else { &p.label },
                        p.id,
                        if p.required { " *" } else { "" },
                        if p.description.is_empty() {
                            String::new()
                        } else {
                            format!(" : {}", trunc(&p.description, 120))
                        }
                    ));
                }
            }
            let steps: Vec<&str> = w.steps.iter().map(|st| st.id.as_str()).collect();
            t.push_str(&format!(
                "\n**Étapes** ({}) : {}",
                steps.len(),
                trunc(&steps.join(" → "), 600)
            ));
            let mut sc = Screen::new(t);
            sc.rows.push(vec![
                self.op("▶️ Lancer", "wf.run", json!({"id": id}), here.clone())
                    .await?,
                self.nav("« Workflows", "wf", json!({})).await?,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `runs`.
    pub(super) async fn screen_runs(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let here = back_of(name, args);
        let screen: Screen = {
            let stuck = args["filter"].as_str() == Some("stuck");
            let runs: Vec<_> = s
                .runs
                .list(None, 50)
                .await?
                .into_iter()
                .filter(|r| {
                    !stuck
                        || matches!(
                            r.state,
                            penelope_workflow::RunState::Paused
                                | penelope_workflow::RunState::Blocked
                        )
                })
                .collect();
            let mut sc = Screen::new(if stuck {
                format!("**Runs en pause ou bloqués** ({})\n", runs.len())
            } else {
                format!("**Runs** ({})\n", runs.len())
            });
            if runs.is_empty() {
                sc.text.push_str(if stuck {
                    "\nAucun run à reprendre."
                } else {
                    "\nAucun run."
                });
            }
            let page = page_of(args);
            for r in runs.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let state = r.state.as_str();
                sc.text.push_str(&format!(
                    "\n{} **{}** · {state}{} · `{}`",
                    run_icon(state),
                    r.workflow_id,
                    r.current_step
                        .as_deref()
                        .map(|st| format!(" · étape `{st}`"))
                        .unwrap_or_default(),
                    r.id
                ));
                let mut row = vec![
                    self.nav(
                        &format!("🔎 {}", trunc(&r.workflow_id, 24)),
                        "run.detail",
                        json!({"run": r.id}),
                    )
                    .await?,
                ];
                let question = format!("Arrêter le run `{}` ({}) ?", r.id, r.workflow_id);
                let stop = |label: &'static str| {
                    self.guarded(
                        label,
                        "run.cancel",
                        json!({"run": r.id}),
                        &question,
                        here.clone(),
                    )
                };
                match r.state {
                    penelope_workflow::RunState::Running => {
                        row.push(
                            self.op("⏸", "run.pause", json!({"run": r.id}), here.clone())
                                .await?,
                        );
                        row.push(stop("⏹").await?);
                    }
                    penelope_workflow::RunState::Paused | penelope_workflow::RunState::Blocked => {
                        row.push(
                            self.op("▶️", "run.resume", json!({"run": r.id}), here.clone())
                                .await?,
                        );
                        row.push(stop("⏹").await?);
                    }
                    _ => {}
                }
                sc.rows.push(row);
            }
            self.pager(&mut sc, name, args, runs.len()).await?;
            if stuck {
                sc.rows
                    .push(vec![self.nav("Tous les runs", "runs", json!({})).await?]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `run.detail`.
    pub(super) async fn screen_run_detail(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let here = back_of(name, args);
        let screen: Screen = {
            let id = args["run"].as_str().unwrap_or_default();
            let r = s
                .runs
                .get(id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("run `{id}` introuvable"))?;
            let state = r.state.as_str();
            let mut t = format!(
                "{} **{}** · {state}\n`{}`\nÉtape : `{}` · itérations {}/{} · {:.2} $\nDémarré : {}",
                run_icon(state),
                r.workflow_id,
                r.id,
                r.current_step.as_deref().unwrap_or("-"),
                r.iterations,
                r.max_iterations,
                r.spent_usd,
                r.started_at.get(..16).unwrap_or(&r.started_at)
            );
            if r.params.as_object().is_some_and(|o| !o.is_empty()) {
                t.push_str(&format!(
                    "\nParamètres : `{}`",
                    trunc(&r.params.to_string(), 300)
                ));
            }
            if let Some(e) = &r.error {
                t.push_str(&format!("\nErreur : {}", trunc(e, 400)));
            }
            if let Some(res) = &r.result {
                t.push_str(&format!("\nRésultat : {}", trunc(res, 400)));
            }
            let mut sc = Screen::new(t);
            let mut row = Vec::new();
            match r.state {
                penelope_workflow::RunState::Running => {
                    row.push(
                        self.op("⏸ Pause", "run.pause", json!({"run": id}), here.clone())
                            .await?,
                    );
                }
                penelope_workflow::RunState::Paused | penelope_workflow::RunState::Blocked => {
                    row.push(
                        self.op(
                            "▶️ Reprendre",
                            "run.resume",
                            json!({"run": id}),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                _ => {}
            }
            if matches!(
                r.state,
                penelope_workflow::RunState::Running
                    | penelope_workflow::RunState::Paused
                    | penelope_workflow::RunState::Blocked
            ) {
                row.push(
                    self.guarded(
                        "⏹ Arrêter",
                        "run.cancel",
                        json!({"run": id}),
                        &format!("Arrêter le run `{id}` ?"),
                        here.clone(),
                    )
                    .await?,
                );
            }
            sc.rows.push(row);
            sc.rows
                .push(vec![self.nav("« Runs", "runs", json!({})).await?]);
            sc
        };
        Ok(screen)
    }

    /// Écran `schedules`.
    pub(super) async fn screen_schedules(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let here = back_of(name, args);
        let screen: Screen = {
            let v = rpc.call(m::SCHEDULE_LIST, json!({})).await?;
            let list = v.as_array().cloned().unwrap_or_default();
            let mut sc = Screen::new(schedules_text(&v));
            if !list.is_empty() {
                sc.text.push_str(
                    "\n⚡ déclenche maintenant, ⏸/▶️ suspend ou reprend, 📍 livre ici, \
                     🗑 supprime.",
                );
            }
            let page = page_of(args);
            for sched in list.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let id = sched["id"].as_str().unwrap_or("?");
                let label = format!(
                    "{} {}",
                    sched["kind"].as_str().unwrap_or("?"),
                    &id[id.len().saturating_sub(6)..]
                );
                let paused = sched["state"].as_str() == Some("paused");
                sc.rows.push(vec![
                    self.op(
                        &format!("⚡ {label}"),
                        "schedule.run",
                        json!({"id": id}),
                        here.clone(),
                    )
                    .await?,
                    if paused {
                        self.op("▶️", "schedule.resume", json!({"id": id}), here.clone())
                            .await?
                    } else {
                        self.op("⏸", "schedule.pause", json!({"id": id}), here.clone())
                            .await?
                    },
                    self.op("📍", "schedule.here", json!({"id": id}), here.clone())
                        .await?,
                    self.guarded(
                        "🗑",
                        "schedule.rm",
                        json!({"id": id}),
                        &format!("Supprimer le déclencheur `{id}` ?"),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            self.pager(&mut sc, name, args, list.len()).await?;
            sc
        };
        Ok(screen)
    }
}
