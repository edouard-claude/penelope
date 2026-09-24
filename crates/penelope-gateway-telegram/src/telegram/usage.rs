//! Coût et budget : textes `/usage`, `/budget`, carte de budget.

use super::*;

impl TelegramGateway {
    /// `/budget` : dépense du jour et de la session, requêtes les plus chères.
    /// `/budget sessions|requêtes|modèles|jours` : un regroupement précis.
    /// `/usage [session|turn|model|day|role|upstream]` : tokens d'entrée, part en cache,
    /// sortie et coût (issue #20). `turn` se limite à la session du chat.
    pub(super) async fn usage_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let by = match args.trim() {
            "" | "sessions" => "session",
            "requêtes" | "requetes" | "tours" | "turns" => "turn",
            "modèles" | "modeles" | "models" => "model",
            other => other,
        };
        if !penelope_kernel::budget::USAGE_AXES.contains(&by) {
            return Ok(format!(
                "Regroupement inconnu `{by}`. Choix : {}.",
                penelope_kernel::budget::USAGE_AXES.join(", ")
            ));
        }
        let today = s.budget.today();
        let (scope, since, title) = match by {
            "turn" => (Some(session), None, "Requêtes de la session"),
            "day" => (None, None, "Par jour"),
            _ => (None, Some(today.as_str()), "Aujourd'hui"),
        };
        let rows = s.budget.report(by, scope, since, 10).await?;
        if rows.is_empty() {
            return Ok("Aucune consommation enregistrée.".into());
        }
        let k = |n: i64| {
            if n >= 1_000_000 {
                format!("{:.1} M", n as f64 / 1e6).replace('.', ",")
            } else if n >= 1_000 {
                format!("{} k", n / 1_000)
            } else {
                n.to_string()
            }
        };
        let mut t = format!("**{title}, par {by}**\n");
        for r in &rows {
            let label = r
                .label
                .as_deref()
                .map(|l| format!("« {l} »"))
                .unwrap_or_else(|| format!("`{}`", if r.key.is_empty() { "?" } else { &r.key }));
            t.push_str(&format!(
                "\n- {} · {label} · {} appel(s) · entrée {} (cache {:.0} %) · sortie {}",
                crate::budget_alert::usd(r.cost_usd),
                r.calls,
                k(r.prompt),
                r.cache_ratio() * 100.0,
                k(r.completion)
            ));
        }
        Ok(t)
    }
    pub(super) async fn budget_text(&self, session: &str, args: &str) -> anyhow::Result<String> {
        let s = &self.daemon.services;
        let cfg = s.config.config();
        let usd = |x: f64| format!("{:.4} $", x).replace('.', ",");
        let axis = match args.trim() {
            "" => None,
            "sessions" | "session" => Some(("session", None, "Sessions les plus chères")),
            "requêtes" | "requetes" | "tours" | "turn" => {
                Some(("turn", Some(session), "Requêtes les plus chères (session)"))
            }
            "modèles" | "modeles" | "model" => Some(("model", None, "Par modèle")),
            "jours" | "day" => Some(("day", None, "Par jour")),
            "rôles" | "roles" | "role" => Some(("role", None, "Par usage")),
            other => {
                return Ok(format!(
                    "Regroupement inconnu `{other}`. Choix : sessions, requêtes, modèles, jours, rôles."
                ));
            }
        };

        let row_line = |r: &penelope_kernel::budget::UsageRow| {
            let label = r
                .label
                .as_deref()
                .map(|l| format!(" « {l} »"))
                .unwrap_or_default();
            let key = if r.key.is_empty() {
                "?"
            } else {
                r.key.as_str()
            };
            let est = if r.estimated > 0 { " (estimé)" } else { "" };
            format!(
                "- {}{est} · `{key}`{label} · {} appel(s)\n",
                usd(r.cost_usd),
                r.calls
            )
        };

        if let Some((by, scope, title)) = axis {
            let rows = s.budget.report(by, scope, None, 15).await?;
            if rows.is_empty() {
                return Ok("Aucune consommation enregistrée.".into());
            }
            let mut t = format!("**{title}**\n\n");
            for r in &rows {
                t.push_str(&row_line(r));
            }
            return Ok(t);
        }

        let today = s.budget.spent_today().await?;
        let in_session = s.budget.spent_session(session).await?;
        let (daily_limit, session_limit, _) =
            s.budget.limits(&cfg.budget, Some(session), None).await?;
        let own = s
            .sessions
            .get(session)
            .await?
            .and_then(|x| x.budget_usd)
            .is_some();
        let mut t = format!(
            "💶 Aujourd'hui : {} sur {} · session : {} sur {}{}\n",
            usd(today),
            usd(daily_limit),
            usd(in_session),
            usd(session_limit),
            if own { " (plafond propre)" } else { "" }
        );
        let view = crate::compaction::context_view(s, session, None).await?;
        if let Some(prompt) = view["last_prompt_tokens"].as_i64() {
            let cached = view["last_cached_tokens"].as_i64().unwrap_or(0);
            t.push_str(&format!(
                "📏 {} ({:.0} % en cache)\n",
                context_line(&view),
                if prompt > 0 {
                    cached as f64 * 100.0 / prompt as f64
                } else {
                    0.0
                },
            ));
        }
        let turns = s.budget.report("turn", Some(session), None, 5).await?;
        if !turns.is_empty() {
            t.push_str("\n**Requêtes les plus chères de la session**\n\n");
            for r in &turns {
                t.push_str(&row_line(r));
            }
        }
        let today_day = s.budget.today();
        let models = s.budget.report("model", None, Some(&today_day), 5).await?;
        if !models.is_empty() {
            t.push_str("\n**Par modèle, aujourd'hui**\n\n");
            for r in &models {
                t.push_str(&format!(
                    "- {} · `{}` · {} appel(s)\n",
                    usd(r.cost_usd),
                    r.key,
                    r.calls
                ));
            }
        }
        // L'abonnement ChatGPT ne facture pas l'appel : sa limite est le quota du plan,
        // que les plafonds en dollars ne voient pas (#142).
        if cfg.providers.codex.enabled
            && let Some(q) = crate::codex_quota::snapshot(s).await
        {
            t.push_str(&format!(
                "\n**Abonnement ChatGPT** (hors plafonds en dollars)\n\n- {}\n",
                crate::codex_quota::gauge_line(&q, s.clock.now_ms())
            ));
        }
        t.push_str(
            "\nDétail : `/budget sessions`, `/budget requêtes`, `/budget modèles` · plafond de \
             cette session : `/budget session 20`",
        );
        Ok(t)
    }
    /// Carte « plafond atteint : continuer ? » (issue #32) : +5 $, +20 $ ou arrêter.
    pub(super) async fn send_budget_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let scope = a.payload["scope"].as_str().unwrap_or("session");
        let spent = a.payload["spent"].as_f64().unwrap_or(0.0);
        let limit = a.payload["limit"].as_f64().unwrap_or(0.0);
        let place = match scope {
            "jour" => "aujourd'hui".to_string(),
            "run" => format!(
                "dans le run `{}`",
                a.payload["run_id"]
                    .as_str()
                    .or(a.run_id.as_deref())
                    .unwrap_or("?")
            ),
            _ => match a.session_id.as_deref() {
                Some(sid) => match s.sessions.get(sid).await? {
                    Some(sess) => format!("dans « {} »", crate::titles::label(&sess)),
                    None => "dans cette session".into(),
                },
                None => "dans cette session".into(),
            },
        };
        let hint = match scope {
            "jour" => "\nLe relèvement vaut pour aujourd'hui seulement.",
            "run" => "\nLe run reprend après relèvement.",
            _ => "\nLe tour reprend là où il s'est arrêté.",
        };
        let ttl = 24 * 3_600_000;
        let mut row = Vec::new();
        for (label, action, amount) in [
            ("+5 $", k::BUDGET_RAISE, 5.0),
            ("+20 $", k::BUDGET_RAISE, 20.0),
            ("⏹ Arrêter", k::BUDGET_STOP, 0.0),
        ] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({"amount": amount}), ttl, true)
                .await?;
            row.push(ButtonSpec::callback(label, &t.token, ""));
        }
        let text = format!(
            "💸 {} $ dépensés sur {} $ {place} : continuer ?{hint}",
            fmt_usd(spent),
            fmt_usd(limit)
        );
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&[row]),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Relève le plafond visé par une carte de budget et reprend le tour ou le run, ou
    /// arrête.
    pub(super) async fn budget_clicked(
        &self,
        callback_id: &str,
        action: &Action,
        chat_id: i64,
        clicked_topic: Option<i64>,
        message_id: i64,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let _ = self.bot.edit_markup(chat_id, message_id, None).await;
        let Some(a) = s.approvals.get(&action.target).await? else {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Demande introuvable."), false)
                .await;
            return Ok(());
        };
        let (chat_id, topic_id) = self.approval_destination(&a, chat_id, clicked_topic).await;
        let scope = a.payload["scope"].as_str().unwrap_or("session").to_string();
        let spent = a.payload["spent"].as_f64().unwrap_or(0.0);
        let limit = a.payload["limit"].as_f64().unwrap_or(0.0);
        if action.action == k::BUDGET_STOP {
            let _ = self
                .bot
                .answer_callback(callback_id, Some("Arrêté"), false)
                .await;
            decide_approval(s, a.id.as_str(), &Decision::deny("telegram", None)).await?;
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("⏹ Arrêté : le plafond reste à {} $.", fmt_usd(limit)),
                )
                .await;
        }
        let amount = action.args["amount"].as_f64().unwrap_or(5.0);
        let raised = spent.max(limit) + amount;
        match scope.as_str() {
            "jour" => s.budget.raise_daily(raised).await?,
            "run" => {
                if let Some(run) = a.payload["run_id"].as_str().or(a.run_id.as_deref()) {
                    s.budget.raise_run(run, raised).await?;
                }
            }
            _ => {
                if let Some(sid) = a.session_id.as_deref() {
                    s.sessions.set_budget(sid, Some(raised)).await?;
                }
            }
        }
        let won = decide_approval(
            s,
            a.id.as_str(),
            &Decision {
                choice: format!("+{} $", fmt_usd(amount)),
                ..Decision::approve_once("telegram")
            },
        )
        .await?;
        let _ = self
            .bot
            .answer_callback(
                callback_id,
                Some(&format!("Plafond : {} $", fmt_usd(raised))),
                false,
            )
            .await;
        if !won {
            return self
                .reply(chat_id, topic_id, None, "ℹ️ Déjà tranché.")
                .await;
        }
        let note = match scope.as_str() {
            "jour" => format!(
                "💰 Plafond du jour relevé à {} $ pour aujourd'hui. Renvoie ta demande pour \
                 reprendre.",
                fmt_usd(raised)
            ),
            "run" => {
                if let Some(run) = a.payload["run_id"].as_str().or(a.run_id.as_deref())
                    && let Err(e) =
                        crate::workflow::control(d, run, &penelope_workflow::Control::Resume).await
                {
                    tracing::debug!(error = %e, "reprise du run après relèvement");
                }
                format!(
                    "💰 Plafond du run relevé à {} $ : il reprend.",
                    fmt_usd(raised)
                )
            }
            _ => {
                if let Some(sid) = a.session_id.as_deref() {
                    let origin = Origin::Telegram {
                        chat_id,
                        topic_id,
                        message_id: None,
                    };
                    d.enqueue_resume(sid, a.id.as_str(), &origin).await?;
                }
                format!(
                    "💰 Plafond de la session relevé à {} $ : je reprends.",
                    fmt_usd(raised)
                )
            }
        };
        self.reply(chat_id, topic_id, None, &note).await
    }
}
