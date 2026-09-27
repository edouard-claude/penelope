//! Commandes de modèle et de coût : `/model`, `/models`, `/budget`, `/usage`, `/audit`.

use super::*;

impl TelegramGateway {
    /// `/model`.
    pub(super) async fn cmd_model(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let session = d.chat_session_for(&origin).await?;
            match parts.as_slice() {
                // Sans argument : l'état de la session et un bouton par modèle.
                [] => {
                    return self
                        .send_model_menu(chat_id, topic_id, reply_to, &session)
                        .await;
                }
                ["auto", switch] => {
                    let on = match *switch {
                        "on" | "oui" => Some(true),
                        "off" | "non" => Some(false),
                        _ => None,
                    };
                    match on {
                        None => "Usage : `/model auto on` ou `/model auto off`".into(),
                        Some(on) => {
                            rpc.call(
                                m::CONFIG_SET,
                                json!({"path": "models.routing.classifier", "value": on}),
                            )
                            .await?;
                            if on {
                                "🔀 Routage adaptatif activé : le classifieur choisit l'alias à chaque message.".into()
                            } else {
                                "📌 Routage fixe : les sessions non épinglées passent par `main`."
                                    .into()
                            }
                        }
                    }
                }
                // Connexion d'un fournisseur à compte (#142) : le code s'affiche
                // ici, et Pénélope confirme dès qu'il est saisi. Ni le code ni les
                // jetons ne passent par une carte ni par une demande (#134).
                ["auth", rest @ ..] => {
                    let provider = rest
                        .iter()
                        .find(|w| !w.starts_with('-') && **w != "status" && **w != "logout")
                        .copied()
                        .unwrap_or("codex")
                        .to_string();
                    let action = if rest.iter().any(|w| w.trim_start_matches('-') == "logout") {
                        "logout"
                    } else if rest.iter().any(|w| w.trim_start_matches('-') == "status") {
                        "status"
                    } else {
                        "start"
                    };
                    let params = json!({"provider": provider, "action": action});
                    match (action, rpc.call(m::MODEL_AUTH, params).await) {
                        (_, Err(e)) => format!("❌ {e}"),
                        ("logout", Ok(_)) => {
                            format!("🔌 `{provider}` déconnecté : jeton révoqué et oublié.")
                        }
                        ("status", Ok(v)) => codex_status_text(&v),
                        (_, Ok(v)) => {
                            // L'attente se fait en fond : le tour Telegram ne reste
                            // pas suspendu un quart d'heure.
                            let g = self.clone();
                            let daemon = d.clone();
                            tokio::spawn(async move {
                                let text = match penelope_daemon::rpc::Rpc::new(daemon)
                                    .call(
                                        m::MODEL_AUTH,
                                        json!({"provider": "codex", "action": "wait"}),
                                    )
                                    .await
                                {
                                    Ok(v) => format!(
                                        "✅ Connecté : plan {}, compte {}.\nDonner un \
                                         alias : `/model code codex:gpt-6-astra`",
                                        shown(&v["plan"]),
                                        shown(&v["account"])
                                    ),
                                    Err(e) => format!("❌ Connexion abandonnée : {e}"),
                                };
                                if let Err(e) = g.reply(chat_id, topic_id, None, &text).await {
                                    tracing::warn!(error = %e, "confirmation de connexion non envoyée");
                                }
                            });
                            format!(
                                "🔐 Ouvrir {}\net saisir le code : `{}`\n\nJe confirme ici \
                                 dès que c'est validé (quinze minutes).",
                                shown(&v["url"]),
                                shown(&v["user_code"])
                            )
                        }
                    }
                }
                // Un alias seul : l'épingler sur la session, `auto` pour revenir.
                [alias] => match rpc
                    .call(
                        m::SESSION_MODEL,
                        json!({"session": session, "alias": alias}),
                    )
                    .await
                {
                    Ok(v) => model_pin_notice(&v),
                    Err(e) => format!("❌ {e}"),
                },
                // Un alias et un modèle : changer ce que vise l'alias, partout.
                [alias, model, ..] => {
                    let model = normalise_model_id(model);
                    match rpc
                        .call(m::MODEL_SET, json!({"alias": alias, "model": model}))
                        .await
                    {
                        Ok(v) => format!(
                            "✅ `{alias}` → `{model}` (génération {}).",
                            shown(&v["generation"])
                        ),
                        Err(e) => format!("❌ {e}"),
                    }
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/models`.
    pub(super) async fn cmd_models(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        return self
            .show_screen(
                chat_id,
                topic_id,
                reply_to,
                "models",
                &json!({"filter": args}),
                None,
            )
            .await;
    }

    /// `/budget`.
    pub(super) async fn cmd_budget(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let reply_to = Some(message_id);
        let text: String = {
            let session = d.chat_session_for(&origin).await?;
            match args.split_whitespace().collect::<Vec<_>>().as_slice() {
                // Plafond propre à la session de travail (issue #32).
                ["session", amount] => {
                    let usd = match *amount {
                        "off" | "défaut" | "defaut" | "0" => None,
                        raw => match raw.trim_end_matches('$').replace(',', ".").parse::<f64>() {
                            Ok(v) if v > 0.0 => Some(v),
                            _ => {
                                return self
                                    .send_screen(
                                        chat_id,
                                        topic_id,
                                        reply_to,
                                        Self::typed_screen(
                                            &format!("Montant illisible : `{raw}`. Un nombre de dollars, ou `off`."),
                                            "✏️ Écrire le plafond",
                                            "/budget session ",
                                        ),
                                        None,
                                    )
                                    .await;
                            }
                        },
                    };
                    s.sessions.set_budget(&session, usd).await?;
                    let cfg = s.config.config();
                    let (daily, limit, _) =
                        s.budget.limits(&cfg.budget, Some(&session), None).await?;
                    format!(
                        "💰 Plafond de cette session : {} $ ({}). Le jour reste plafonné à {} $.",
                        fmt_usd(limit),
                        if usd.is_some() {
                            "propre à la session"
                        } else {
                            "celui de la configuration"
                        },
                        fmt_usd(daily)
                    )
                }
                ["session"] => {
                    return self
                        .send_screen(
                            chat_id,
                            topic_id,
                            reply_to,
                            Self::typed_screen(
                                "💰 **Plafond de la session** : `/budget session` suivi d'un montant en dollars (`off` pour revenir à la configuration).",
                                "✏️ Écrire le plafond",
                                "/budget session ",
                            ),
                            None,
                        )
                        .await;
                }
                _ => self.budget_text(&session, args).await?,
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/usage`.
    pub(super) async fn cmd_usage(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: Some(message_id),
        };
        let reply_to = Some(message_id);
        let text: String = {
            let session = d.chat_session_for(&origin).await?;
            self.usage_text(&session, args).await?
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/audit`.
    pub(super) async fn cmd_audit(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        _args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let reply_to = Some(message_id);
        // L'audit relit toute la mémoire : détaché (issue #69).
        let (me, d2) = (self.clone(), d.clone());
        tokio::spawn(async move {
            let note = match penelope_vault::mem_audit::run(&d2.services, d2.hooks.mcp_supervisor())
                .await
            {
                Ok(a) => penelope_vault::mem_audit::summary(&a),
                Err(e) => format!("❌ {e}"),
            };
            let _ = me.reply(chat_id, topic_id, reply_to, &note).await;
        });
        Ok(())
    }
}
