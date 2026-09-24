//! Commandes de mémoire : `/accueil`, `/dream`, `/appris`, `/pratique`, `/retiens`,
//! `/oublie`, `/forget`, `/recall`.

use super::*;

impl TelegramGateway {
    /// `/accueil`.
    pub(super) async fn cmd_accueil(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let text: String = {
            let part = crate::onboarding::Part::parse(args);
            if !args.is_empty() && part.is_none() {
                "Partie inconnue : profil, outils, style ou limites.".to_string()
            } else {
                return self.onboarding_next(chat_id, topic_id, part).await;
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/dream`.
    pub(super) async fn cmd_dream(
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
        // Une passe peut prendre une minute : le bilan arrive quand il est prêt.
        self.react(chat_id, message_id, reaction::RECEIVED);
        let (daemon, messenger) = (d.clone(), d.hooks.messenger());
        let dry_run = args.contains("dry");
        tokio::spawn(async move {
            let text = match crate::dream::run(&daemon, dry_run).await {
                Ok(o) => format!(
                    "🌙 {}{}",
                    if o.dry_run { "(à blanc) " } else { "" },
                    o.report.render()
                ),
                Err(e) => format!("❌ {e}"),
            };
            if let Some(m) = messenger {
                let _ = m.send_text(&origin, &text).await;
            }
        });
        Ok(())
    }

    /// `/appris`.
    pub(super) async fn cmd_appris(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let days = args.trim().parse::<i64>().unwrap_or(7);
        return self
            .show_screen(
                chat_id,
                topic_id,
                reply_to,
                "learned",
                &json!({"days": days}),
                None,
            )
            .await;
    }

    /// `/pratique`.
    pub(super) async fn cmd_pratique(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let reply_to = Some(message_id);
        let (screen, screen_args) = if args.is_empty() {
            ("practices", json!({}))
        } else {
            (
                "practice",
                json!({"slug": penelope_platform::slugify(args)}),
            )
        };
        return self
            .show_screen(chat_id, topic_id, reply_to, screen, &screen_args, None)
            .await;
    }

    /// `/retiens`.
    pub(super) async fn cmd_retiens(
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
            if args.is_empty() {
                return self
                    .send_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        Self::typed_screen(
                            "🧠 **Retenir** : `/retiens` suivi de ce qu'il faut garder en \
                             mémoire de fond (une ligne, sans secret).",
                            "✏️ Écrire",
                            "/retiens ",
                        ),
                        None,
                    )
                    .await;
            } else {
                let vault = crate::helpers::vault_dir(s);
                let session = d.chat_session_for(&origin).await?;
                match crate::vault_ops::remember(
                    s,
                    &vault,
                    penelope_memory::Level::Coeur,
                    args,
                    &session,
                )
                .await
                {
                    Ok(uid) => format!("🧠 Retenu (`{uid}`)."),
                    Err(e) => format!("❌ {e}"),
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/oublie`.
    pub(super) async fn cmd_oublie(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        if !args.is_empty()
            && let Some(e) = s.memory.get(args).await?
        {
            let confirm = json!({
                "op": "mem.forget", "params": {"uid": args},
                "question": format!("Oublier « {} » ?", e.text),
                "back": {"screen": "forget", "args": {}},
            });
            return self
                .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                .await;
        }
        return self
            .show_screen(
                chat_id,
                topic_id,
                reply_to,
                "forget",
                &json!({"query": args}),
                None,
            )
            .await;
    }

    /// `/forget`.
    pub(super) async fn cmd_forget(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let reply_to = Some(message_id);
        let text: String = {
            if args.is_empty() {
                return self
                    .show_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        "forget.sessions",
                        &json!({}),
                        None,
                    )
                    .await;
            }
            match crate::session_ops::resolve(s, args).await {
                Err(e) => format!("❌ {e}"),
                Ok(sess) => {
                    let confirm = json!({
                        "op": "session.forget", "params": {"session": sess.id.to_string()},
                        "question": format!(
                            "Oublier tout ce que la mémoire a retenu de la session « {} » ?",
                            crate::titles::label(&sess)
                        ),
                        "back": {"screen": "forget.sessions", "args": {}},
                    });
                    return self
                        .show_screen(chat_id, topic_id, reply_to, "confirm", &confirm, None)
                        .await;
                }
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }

    /// `/recall`.
    pub(super) async fn cmd_recall(
        self: &Arc<Self>,
        chat_id: i64,
        topic_id: Option<i64>,
        message_id: i64,
        _command: &str,
        args: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let reply_to = Some(message_id);
        let text: String = {
            if args.is_empty() {
                return self
                    .send_screen(
                        chat_id,
                        topic_id,
                        reply_to,
                        Self::typed_screen(
                            "🔎 **Rechercher en mémoire** : `/recall` suivi des mots-clés.",
                            "🔎 Chercher",
                            "/recall ",
                        ),
                        None,
                    )
                    .await;
            }
            let hits = rpc.call(m::MEM_SEARCH, json!({"query": args})).await?;
            let hits = hits.as_array().cloned().unwrap_or_default();
            if hits.is_empty() {
                format!("🔎 Rien en mémoire pour « {args} ».")
            } else {
                let mut t = format!(
                    "🔎 **Mémoire** : {} résultat(s) pour « {args} »\n",
                    hits.len()
                );
                for h in hits.iter().take(10) {
                    t.push_str(&format!(
                        "\n- {} _({})_",
                        h["text"].as_str().unwrap_or_default(),
                        h["file"].as_str().unwrap_or("?")
                    ));
                }
                t
            }
        };
        self.reply(chat_id, topic_id, reply_to, &text).await
    }
}
