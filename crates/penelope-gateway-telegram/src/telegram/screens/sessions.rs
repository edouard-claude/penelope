//! Écrans de session et de réglages : `help`, `confirm`, `status`, `rewind`, `prompts`,
//! `config`, `logs`, `quiet`.

use super::*;

impl TelegramGateway {
    /// Écran `help`.
    pub(super) async fn screen_help(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let screen: Screen = {
            let all = penelope_telegram::commands::all();
            match args["cat"].as_str() {
                None => {
                    let mut sc = Screen::new(
                        "**Commandes**\nChoisis une famille : chaque bouton exécute la \
                         commande, ou ouvre son menu quand elle attend un argument.",
                    );
                    let mut row = Vec::new();
                    for cat in penelope_telegram::commands::categories() {
                        let n = all.iter().filter(|c| c.category == cat).count();
                        row.push(
                            self.nav(&format!("📂 {cat} ({n})"), "help", json!({"cat": cat}))
                                .await?,
                        );
                        if row.len() == 2 {
                            sc.rows.push(std::mem::take(&mut row));
                        }
                    }
                    sc.rows.push(row);
                    sc
                }
                Some(cat) => {
                    let mut sc = Screen::new(format!("**{cat}**\n"));
                    let mut row = Vec::new();
                    for c in all.iter().filter(|c| c.category == cat) {
                        sc.text
                            .push_str(&format!("\n- /{} : {}", c.name, c.description));
                        row.push(self.command_button(&format!("/{}", c.name), c.name).await?);
                        if row.len() == 3 {
                            sc.rows.push(std::mem::take(&mut row));
                        }
                    }
                    sc.rows.push(row);
                    sc.rows
                        .push(vec![self.nav("« Familles", "help", json!({})).await?]);
                    sc
                }
            }
        };
        Ok(screen)
    }

    /// Écran `confirm`.
    pub(super) async fn screen_confirm(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let screen: Screen = {
            let back = args["back"].clone();
            let mut sc = Screen::new(format!(
                "⚠️ {}",
                args["question"].as_str().unwrap_or("Confirmer ?")
            ));
            let op = args["op"].as_str().unwrap_or_default();
            let cancel = match back["screen"].as_str() {
                Some(screen) => self.nav("✖️ Annuler", screen, back["args"].clone()).await?,
                // Sans écran d'origine : « Annuler » retire simplement les boutons.
                None => {
                    self.op("✖️ Annuler", "noop", json!({}), Value::Null)
                        .await?
                }
            };
            sc.rows.push(vec![
                self.op("✅ Confirmer", op, args["params"].clone(), back.clone())
                    .await?,
                cancel,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `status`.
    pub(super) async fn screen_status(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let screen: Screen = {
            let st = d.status().await?;
            let origin = Origin::Telegram {
                chat_id,
                topic_id,
                message_id: None,
            };
            let session_line = match d.chat_session_for(&origin).await {
                Ok(sid) => {
                    let cfg = s.config.config();
                    let (_, limit, _) = s.budget.limits(&cfg.budget, Some(&sid), None).await?;
                    let view =
                        penelope_conversation::compaction::context_view(s, &sid, None).await?;
                    format!(
                        "\n- Session : {:.2} $ sur {:.2} $\n- {}",
                        s.budget.spent_session(&sid).await?,
                        limit,
                        super::context_line(&view)
                    )
                }
                Err(_) => String::new(),
            };
            let up = st.uptime_s;
            let mut sc = Screen::new(format!(
                "**Pénélope** · version {} · en route depuis {} h {:02} min\n\
                 - Sessions actives : {}\n- Tours en file : {}\n- Runs actifs : {}\n\
                 - Demandes en attente : {}\n- Serveurs MCP prêts : {}/{}\n\
                 - Dépense du jour : {:.2} $\n- Mémoire : {:.0} Mo\n- Telegram : {}",
                st.version,
                up / 3600,
                (up % 3600) / 60,
                st.sessions_active,
                st.turns_queued,
                st.runs_active,
                st.approvals_pending,
                st.mcp_ready,
                st.mcp_total,
                st.spent_today_usd,
                st.rss_mb,
                st.telegram
            ));
            sc.text.push_str(&session_line);
            if st.outbox_failed > 0 {
                sc.text.push_str(&format!(
                    "\n- Messages non envoyés : {} (journal du daemon)",
                    st.outbox_failed
                ));
            }
            let mut row = Vec::new();
            if st.approvals_pending > 0 {
                row.push(
                    self.command_button(
                        &format!("📋 Demandes ({})", st.approvals_pending),
                        "approvals",
                    )
                    .await?,
                );
            }
            if st.mcp_ready < st.mcp_total {
                row.push(
                    self.nav(
                        &format!("🔌 MCP ({}/{})", st.mcp_ready, st.mcp_total),
                        "mcp",
                        json!({}),
                    )
                    .await?,
                );
            }
            if st.runs_active > 0 {
                row.push(self.nav("🏃 Runs", "runs", json!({})).await?);
            }
            sc.rows.push(row);
            sc.rows.push(vec![
                self.command_button("💶 Dépenses", "usage").await?,
                self.nav("🩺 Diagnostic", "doctor", json!({})).await?,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `config`.
    pub(super) async fn screen_config(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let screen: Screen = {
            let v = rpc.call(m::CONFIG_STATUS, json!({})).await?;
            let mut t = format!(
                "⚙️ **Configuration** · génération {}\n`{}`\n\n**Sous-systèmes**",
                super::shown(&v["generation"]),
                v["path"].as_str().unwrap_or("?")
            );
            match v["subsystems"].as_object() {
                Some(subs) if !subs.is_empty() => {
                    for (sub, r) in subs {
                        let ok = r["ok"]
                            .as_bool()
                            .or_else(|| r["error"].is_null().then_some(true));
                        let detail = r["message"]
                            .as_str()
                            .or(r["error"].as_str())
                            .unwrap_or_default();
                        t.push_str(&format!(
                            "\n{} {sub}{}",
                            if ok == Some(false) { "❌" } else { "✅" },
                            if detail.is_empty() {
                                String::new()
                            } else {
                                format!(" : {}", trunc(detail, 160))
                            }
                        ));
                    }
                }
                _ => t.push_str("\nAucune génération appliquée depuis le démarrage."),
            }
            let mut sc = Screen::new(t);
            sc.rows
                .push(vec![self.nav("🩺 Diagnostic", "doctor", json!({})).await?]);
            sc
        };
        Ok(screen)
    }

    /// Écran `logs`.
    pub(super) async fn screen_logs(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let screen: Screen = {
            let component = args["component"].as_str().unwrap_or_default();
            let n = args["n"].as_u64().unwrap_or(20).clamp(5, 200) as usize;
            let lines = super::recent_log_lines(&s.platform.dirs.logs(), component, n);
            let mut sc = Screen::new(if lines.is_empty() {
                format!(
                    "📜 Aucune ligne de journal{}.",
                    if component.is_empty() {
                        String::new()
                    } else {
                        format!(" pour `{component}`")
                    }
                )
            } else {
                format!(
                    "📜 **Journal**{} · {} ligne(s)\n```\n{}\n```",
                    if component.is_empty() {
                        String::new()
                    } else {
                        format!(" `{component}`")
                    },
                    lines.len(),
                    lines.join("\n").replace("```", "ʼʼʼ")
                )
            });
            let mut row = Vec::new();
            for c in ["", "telegram", "mcp", "llm", "workflow", "memory"] {
                if c == component {
                    continue;
                }
                row.push(
                    self.nav(
                        if c.is_empty() { "Tout" } else { c },
                        "logs",
                        json!({"component": c}),
                    )
                    .await?,
                );
                if row.len() == 3 {
                    sc.rows.push(std::mem::take(&mut row));
                }
            }
            sc.rows.push(row);
            sc.rows.push(vec![
                self.nav("Plus", "logs", json!({"component": component, "n": n * 2}))
                    .await?,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `quiet`.
    pub(super) async fn screen_quiet(
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
            let v = rpc.call(m::QUIET, json!({})).await?;
            let range = v["quiet_hours"].as_str().unwrap_or_default();
            let mut sc = Screen::new(if range.is_empty() {
                "🌙 Heures silencieuses : **désactivées**.".to_string()
            } else {
                format!(
                    "🌙 Heures silencieuses : **{range}**. Les messages non urgents attendent \
                     la fin de la plage."
                )
            });
            let mut row = Vec::new();
            for preset in ["22:00-07:00", "23:00-08:00"] {
                if preset != range {
                    row.push(
                        self.op(preset, "quiet.set", json!({"range": preset}), here.clone())
                            .await?,
                    );
                }
            }
            if !range.is_empty() {
                row.push(
                    self.op(
                        "Désactiver",
                        "quiet.set",
                        json!({"range": ""}),
                        here.clone(),
                    )
                    .await?,
                );
            }
            sc.rows.push(row);
            sc.rows
                .push(vec![ButtonSpec::copy_text("✏️ Autre plage", "/quiet ")]);
            sc
        };
        Ok(screen)
    }

    /// Écran `rewind`.
    pub(super) async fn screen_rewind(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let here = back_of(name, args);
        let screen: Screen = {
            let mut sc = Screen::new(
                "⏪ **Revenir en arrière** : combien d'échanges défaire ? Les messages \
                 retirés sont mis de côté, pas effacés.",
            );
            let mut row = Vec::new();
            for n in [1, 2, 3, 5] {
                row.push(
                    self.guarded(
                        &n.to_string(),
                        "session.rewind",
                        json!({"turns": n}),
                        &format!("Défaire les {n} dernier(s) échange(s) ?"),
                        here.clone(),
                    )
                    .await?,
                );
            }
            sc.rows.push(row);
            sc
        };
        Ok(screen)
    }

    /// Écran `prompts`.
    pub(super) async fn screen_prompts(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let here = back_of(name, args);
        let screen: Screen = {
            let Some(sup) = d.hooks.mcp_supervisor() else {
                return Ok(Screen::new("Aucun serveur MCP chargé."));
            };
            match args["server"].as_str() {
                None => {
                    let statuses = sup.statuses().await;
                    let mut sc = Screen::new(if statuses.is_empty() {
                        "Aucun serveur MCP déclaré.".to_string()
                    } else {
                        "💬 **Prompts MCP** : choisis un serveur.".to_string()
                    });
                    for st in statuses {
                        sc.rows.push(vec![
                            self.nav(
                                &format!(
                                    "{} {}",
                                    super::mcp_state_icon(st.state.as_str()),
                                    st.name
                                ),
                                "prompts",
                                json!({"server": st.name}),
                            )
                            .await?,
                        ]);
                    }
                    sc
                }
                Some(server) => {
                    let prompts = sup.prompts(server).await.map_err(anyhow::Error::msg)?;
                    let mut sc = Screen::new(if prompts.is_empty() {
                        format!("`{server}` ne propose aucun prompt.")
                    } else {
                        format!("💬 **Prompts de `{server}`** ({})\n", prompts.len())
                    });
                    for p in prompts.iter().take(30) {
                        let pname = p["name"].as_str().unwrap_or("?");
                        sc.text.push_str(&format!(
                            "\n- `{pname}` : {}",
                            trunc(p["description"].as_str().unwrap_or_default(), 120)
                        ));
                        sc.rows.push(vec![
                            self.op(
                                &format!("▶️ {}", trunc(pname, 36)),
                                "prompt.run",
                                json!({"server": server, "prompt": pname}),
                                here.clone(),
                            )
                            .await?,
                        ]);
                    }
                    sc.rows
                        .push(vec![self.nav("« Serveurs", "prompts", json!({})).await?]);
                    sc
                }
            }
        };
        Ok(screen)
    }
}
