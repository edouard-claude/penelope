//! Cartes d'approbation, d'effet incertain, de lancement, OAuth, mémoire, confirmation.

use super::*;

impl TelegramGateway {
    // ================================================================ cartes

    pub async fn send_approval_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        // Le clic peut arriver après un redémarrage ; le run technique n'a pas de
        // coordonnées Telegram, mais cette destination survit dans le store (#165).
        self.daemon
            .services
            .kv_set(
                &format!("tg.approval_destination.{}", a.id.as_str()),
                &json!({"chat_id": chat_id, "topic_id": topic_id}).to_string(),
            )
            .await?;
        if a.kind == penelope_hitl::ApprovalKind::BudgetExceeded
            && a.payload["budget"].as_bool() == Some(true)
        {
            return self.send_budget_card(chat_id, topic_id, a).await;
        }
        if a.kind == penelope_hitl::ApprovalKind::MemoryProposal {
            return self.send_memory_card(chat_id, topic_id, a).await;
        }
        if a.subject == "workflow_start" {
            return self.send_launch_card(chat_id, topic_id, a).await;
        }
        if a.kind == penelope_hitl::ApprovalKind::EffectUnknown {
            return self.send_effect_card(chat_id, topic_id, a).await;
        }
        let s = &self.daemon.services;
        // Point de contrôle de coût d'un tour (issue #19) : continuer ou arrêter.
        if a.payload["checkpoint"].as_bool() == Some(true) {
            let ttl = 24 * 3_600_000;
            let mut row = Vec::new();
            for (label, action) in [("▶️ Continuer", k::APPROVE), ("⏹ Arrêter", k::DENY)] {
                let t = s
                    .actions
                    .create(action, a.id.as_str(), json!({}), ttl, true)
                    .await?;
                row.push(ButtonSpec::callback(label, &t.token, ""));
            }
            let text = format!(
                "💸 {}",
                a.payload["reason"]
                    .as_str()
                    .unwrap_or("Ce tour coûte cher, je continue ?")
            );
            return self
                .outbox_push(
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
                .await;
        }
        let args = serde_json::to_string_pretty(&a.payload["arguments"])
            .unwrap_or_default()
            .replace("```", "ʼʼʼ");
        let args: String = if args.chars().count() > 1_500 {
            format!("{}…", args.chars().take(1_500).collect::<String>())
        } else {
            args
        };
        let server = crate::agent::server_of(&a.subject).unwrap_or_else(|| "natif".into());
        let double = a.payload["double"].as_bool().unwrap_or(false);
        let card = approval_card(a);
        let mut vars = BTreeMap::new();
        vars.insert("intention".into(), card.intention);
        vars.insert("action".into(), card.action);
        vars.insert("details".into(), card.details);
        // Anciennes variables, pour un gabarit surchargé écrit avant #116.
        vars.insert("outil".into(), a.subject.clone());
        vars.insert("serveur".into(), server);
        vars.insert("risque".into(), a.risk.as_str().to_string());
        vars.insert("arguments".into(), args);
        vars.insert(
            "raison".into(),
            a.payload["reason"].as_str().unwrap_or("").to_string(),
        );
        let mut alerte = if double {
            "⚠️ Seconde confirmation demandée.".to_string()
        } else {
            String::new()
        };
        // Réseau demandé par une commande (#106) : quatre mots ; le bouton « Toujours » dit
        // sur quoi il porte (#116).
        if crate::executor::wants_network(&a.subject, &a.payload["arguments"]) {
            if !alerte.is_empty() {
                alerte.push('\n');
            }
            alerte.push_str("🌐 Accès au réseau demandé.");
        }
        // Outil MCP dont la description porte une consigne : le propriétaire le voit avant
        // d'accepter (#92).
        if let Some(t) = s.mcp_tools.get(&a.subject).await.ok().flatten() {
            let rules: Vec<String> = t
                .flags()
                .iter()
                .map(|f| f.split(" «").next().unwrap_or(f).to_string())
                .collect();
            if !rules.is_empty() {
                if !alerte.is_empty() {
                    alerte.push('\n');
                }
                alerte.push_str(&format!(
                    "⚠️ La description de cet outil contient une consigne selon le détecteur \
                     local ({}) : le modèle a pu être manipulé.",
                    rules.join(", ")
                ));
            }
        }
        vars.insert("alerte".into(), alerte);

        let ttl = 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [
            k::APPROVE,
            k::APPROVE_RUN,
            k::APPROVE_ALWAYS,
            k::DENY,
            k::DENY_REASON,
        ] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("tool_approval")
            .ok_or_else(|| anyhow::anyhow!("gabarit tool_approval absent"))?;
        let rendered = match tpl.render(&vars, &tokens, &[]) {
            Ok(r) => r,
            // Une carte qui ne se rend pas part quand même, en texte brut : une demande
            // invisible bloque le tour sans que personne le sache (issue #129).
            Err(e) => {
                return self
                    .send_plain_approval(chat_id, topic_id, a, &vars, &tokens, &e.to_string())
                    .await;
            }
        };
        // Le libellé « Pour ce run » du gabarit vaut « pour cette session » en conversation.
        // Sans arguments vus, pas de fenêtre ni de « Toujours » : une règle couvrirait
        // l'outil entier (#83, régression de #67).
        let windows = a.payload.get("arguments").is_some();
        let windowed = [
            tokens.get(k::APPROVE_RUN).cloned().unwrap_or_default(),
            tokens.get(k::APPROVE_ALWAYS).cloned().unwrap_or_default(),
        ];
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|b| {
                        windows
                            || !matches!(&b.action, penelope_telegram::render::ButtonAction::Callback { token } if windowed.contains(token))
                    })
                    // Aucune règle possible : pas de bouton dans la case de « Toujours ».
                    // Une coche verte à sa place se lisait comme un « Toujours » nouvelle
                    // formule, et n'autorisait qu'une fois (issue #150).
                    .filter(|b| {
                        card.always.is_some()
                            || !matches!(&b.action, penelope_telegram::render::ButtonAction::Callback { token } if Some(token) == tokens.get(k::APPROVE_ALWAYS))
                    })
                    .map(|b| {
                        let mut b = b.clone();
                        if b.label.contains("Pour ce run") {
                            b.label = "✅ Pour cette session".into();
                        }
                        // « Toujours » dit sur quoi il porte : la famille de commandes, le
                        // répertoire, l'hôte (#116). Quand il ne peut créer aucune règle, il
                        // le dit plutôt que de laisser croire au contraire (#141).
                        if b.label.contains("Toujours")
                            && let Some(scope) = &card.always
                        {
                            b.label = format!("♾️ Toujours pour {scope}");
                        }
                        b
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Demande d'approbation en texte brut, quand son gabarit ne se rend pas : le
    /// propriétaire la voit, sait qu'elle est simplifiée, et peut répondre ; l'échec
    /// laisse un événement (issue #129).
    pub(super) async fn send_plain_approval(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
        vars: &BTreeMap<String, String>,
        tokens: &BTreeMap<String, String>,
        error: &str,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        tracing::warn!(approval = %a.id.as_str(), error, "carte d'approbation simplifiée");
        let _ = s
            .events
            .append(penelope_kernel::event::EventDraft::new(
                "telegram.card_degraded",
                json!({"approval": a.id.as_str(), "template": "tool_approval", "error": error}),
            ))
            .await;
        let part = |k: &str| vars.get(k).cloned().unwrap_or_default();
        let mut text = format!(
            "⚠️ Carte simplifiée : son gabarit ne se rend pas ({error}).\n\n{}\n\n{}\n\n{}",
            part("intention"),
            part("action"),
            part("details")
        );
        if text.chars().count() > 3_800 {
            text = format!("{}…", text.chars().take(3_800).collect::<String>());
        }
        let row: Vec<ButtonSpec> = [("✅ Approuver", k::APPROVE), ("❌ Refuser", k::DENY)]
            .into_iter()
            .filter_map(|(label, action)| {
                tokens
                    .get(action)
                    .map(|t| ButtonSpec::callback(label, t, ""))
            })
            .collect();
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": text,
                "reply_markup": inline_keyboard(&[row]),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Carte d'un effet resté incertain après un arrêt brutal (#83) : « C'est fait »,
    /// « Relancer » ou « Ignorer », jamais de fenêtre ni de règle.
    async fn send_effect_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let request = serde_json::to_string_pretty(&a.payload["request"])
            .unwrap_or_default()
            .replace("```", "ʼʼʼ");
        let request: String = if request.chars().count() > 1_500 {
            format!("{}…", request.chars().take(1_500).collect::<String>())
        } else {
            request
        };
        let tz = s.config.config().owner.timezone.clone();
        let when = chrono::DateTime::parse_from_rfc3339(&a.created_at)
            .ok()
            .map(|t| match tz.parse::<chrono_tz::Tz>() {
                Ok(tz) => t.with_timezone(&tz).format("%d/%m à %H:%M").to_string(),
                Err(_) => t.format("%d/%m à %H:%M UTC").to_string(),
            })
            .unwrap_or_default();
        let mut vars = BTreeMap::new();
        vars.insert("effet".into(), a.subject.clone());
        vars.insert("horodatage".into(), when);
        vars.insert("requete".into(), request);
        let ttl = 7 * 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [k::EFFECT_VERIFY, k::EFFECT_RETRY, k::EFFECT_IGNORE] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("effect_unknown")
            .ok_or_else(|| anyhow::anyhow!("gabarit effect_unknown absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rendered.buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Carte de lancement d'un workflow proposé en conversation (issue #35) : workflow,
    /// paramètres complétés et brief ; « Lancer » ou « Pas encore », sans « Toujours ».
    async fn send_launch_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let args = &a.payload["arguments"];
        let id = args["id"].as_str().unwrap_or("?");
        let (name, description) = match s.workflows.get(id) {
            Some(w) => (
                if w.metadata.name.is_empty() {
                    id.to_string()
                } else {
                    w.metadata.name.clone()
                },
                w.metadata
                    .description
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            ),
            None => (id.to_string(), "Workflow introuvable.".to_string()),
        };
        let mut text = format!("▶️ **Lancer « {name} » ?** (`{id}`)");
        if !description.is_empty() {
            text.push_str(&format!("\n{description}"));
        }
        let params = args["params"].as_object().cloned().unwrap_or_default();
        if !params.is_empty() {
            text.push_str("\n\n**Paramètres**");
            for (k, v) in &params {
                let v = v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string());
                let v: String = v.chars().take(200).collect();
                text.push_str(&format!("\n- `{k}` : {v}"));
            }
        }
        if let Some(brief) = args["brief"]
            .as_str()
            .map(str::trim)
            .filter(|b| !b.is_empty())
        {
            let short: String = brief.chars().take(1_200).collect();
            let more = if brief.chars().count() > 1_200 {
                "…"
            } else {
                ""
            };
            text.push_str(&format!("\n\n**Brief**\n{short}{more}"));
        }
        let ttl = 24 * 3_600_000;
        let launch = s
            .actions
            .create(k::APPROVE, a.id.as_str(), json!({}), ttl, true)
            .await?;
        let later = s
            .actions
            .create(
                k::DENY,
                a.id.as_str(),
                json!({"reason": "pas encore : le propriétaire veut continuer la discussion \
                                  avant de lancer"}),
                ttl,
                true,
            )
            .await?;
        let rows = vec![vec![
            ButtonSpec::callback("▶️ Lancer", &launch.token, ""),
            ButtonSpec::callback("⏸ Pas encore", &later.token, ""),
        ]];
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rows),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Carte `mcp_oauth_required` : bouton d'autorisation, collage, relance (§8.5).
    pub(super) async fn send_oauth_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        server: &str,
    ) -> anyhow::Result<()> {
        let d = &self.daemon;
        let s = &d.services;
        let Some(cfg) = (match d.hooks.mcp_supervisor() {
            Some(sup) => sup.config_of(server).await,
            None => None,
        }) else {
            return self
                .reply(
                    chat_id,
                    topic_id,
                    None,
                    &format!("Serveur MCP `{server}` inconnu (`/mcp`)."),
                )
                .await;
        };
        let start = match crate::mcp_auth::start(s, &cfg, None).await {
            Ok(st) => st,
            Err(e) => {
                return self
                    .reply(
                        chat_id,
                        topic_id,
                        None,
                        &format!("🔐 Autorisation impossible : {e}"),
                    )
                    .await;
            }
        };
        let ttl = crate::mcp_auth::REQUEST_TTL_MS;
        let mut tokens = BTreeMap::new();
        for action in [k::OAUTH_PASTED, k::OAUTH_RETRY] {
            let t = s
                .actions
                .create(action, server, json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let mut vars = BTreeMap::new();
        vars.insert("serveur".into(), server.to_string());
        vars.insert(
            "scopes".into(),
            if start.scopes.is_empty() {
                "par défaut".into()
            } else {
                start.scopes.join(" ")
            },
        );
        vars.insert(
            "mode".into(),
            if start.mode == "paste_back" {
                "si la page finit sur une erreur 127.0.0.1, colle son adresse ici (10 min)".into()
            } else {
                "retour automatique".into()
            },
        );
        vars.insert("url".into(), start.url.clone());
        let tpl = s
            .templates
            .get("mcp_oauth_required")
            .ok_or_else(|| anyhow::anyhow!("gabarit mcp_oauth_required absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&rendered.buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    /// Carte `memory_proposal` : les faits proposés, « Tout » ou « Rien ».
    async fn send_memory_card(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let items: Vec<String> = a.payload["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|i| i.as_str().map(|t| format!("- {t}")))
            .collect();
        let mut vars = BTreeMap::new();
        vars.insert(
            "items".into(),
            format!(
                "{}\n\nSource : `{}`",
                items.join("\n"),
                a.payload["source"].as_str().unwrap_or("?")
            ),
        );
        let ttl = 7 * 24 * 3_600_000;
        let mut tokens = BTreeMap::new();
        for action in [k::MEMORY_ACCEPT, k::MEMORY_AS_EXCEPTION, k::MEMORY_REJECT] {
            let t = s
                .actions
                .create(action, a.id.as_str(), json!({}), ttl, true)
                .await?;
            tokens.insert(action.to_string(), t.token);
        }
        let tpl = s
            .templates
            .get("memory_proposal")
            .ok_or_else(|| anyhow::anyhow!("gabarit memory_proposal absent"))?;
        let rendered = tpl
            .render(&vars, &tokens, &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        // Un fait tiré d'un document n'a pas de contexte d'exception : « Tout » ou
        // « Rien ». Une contradiction, si : elle garde les trois boutons (issue #145).
        let clash = a.payload["contradiction"].as_bool() == Some(true);
        let buttons: Vec<Vec<ButtonSpec>> = rendered
            .buttons
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|b| clash || !b.label.contains("exception"))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|row| !row.is_empty())
            .collect();
        let html = markdown_to_html(&substitute(&tpl.body, &vars));
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": html,
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
    pub(super) async fn send_destructive_confirm(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        a: &ApprovalRequest,
    ) -> anyhow::Result<()> {
        let s = &self.daemon.services;
        let confirm = s
            .actions
            .create(
                k::CONFIRM_DESTRUCTIVE,
                a.id.as_str(),
                json!({}),
                600_000,
                true,
            )
            .await?;
        let deny = s
            .actions
            .create(k::DENY, a.id.as_str(), json!({}), 600_000, true)
            .await?;
        let buttons = vec![vec![
            ButtonSpec::callback("⚠️ Confirmer", &confirm.token, "danger"),
            ButtonSpec::callback("Annuler", &deny.token, ""),
        ]];
        let text = format!(
            "⚠️ **Seconde confirmation**\n\n`{}` est une action destructive. Confirmer ?",
            a.subject
        );
        self.outbox_push(
            chat_id,
            topic_id,
            "sendMessage",
            json!({
                "chat_id": chat_id,
                "text": markdown_to_html(&text),
                "parse_mode": "HTML",
                "reply_markup": inline_keyboard(&buttons),
                "message_thread_id": topic_id,
            }),
        )
        .await
    }
}
