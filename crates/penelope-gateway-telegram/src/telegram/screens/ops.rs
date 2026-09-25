//! Écrans d'exploitation : `mcp`, `mcp.server`, `models`, `model.assign`, `skills`, `skill`,
//! `doctor`, `secrets`, `upgrade`, `upgrade.switch`.

use super::*;

impl TelegramGateway {
    /// Écran `mcp`.
    pub(super) async fn screen_mcp(
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
            let v = rpc.call(m::MCP_LIST, json!({})).await?;
            let mut sc = Screen::new(mcp_list_text(&v));
            for srv in v["servers"].as_array().cloned().unwrap_or_default() {
                let server = srv["name"].as_str().unwrap_or("?");
                let state = srv["state"].as_str().unwrap_or("?");
                sc.rows.push(vec![
                    self.nav(
                        &format!("{} {}", super::mcp_state_icon(state), trunc(server, 24)),
                        "mcp.server",
                        json!({"name": server}),
                    )
                    .await?,
                    self.op("🔄", "mcp.restart", json!({"name": server}), here.clone())
                        .await?,
                    self.op("🧪", "mcp.test", json!({"name": server}), here.clone())
                        .await?,
                ]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `mcp.server`.
    pub(super) async fn screen_mcp_server(
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
            let server = args["name"].as_str().unwrap_or_default();
            let v = rpc.call(m::MCP_SHOW, json!({"name": server})).await?;
            let mut sc = Screen::new(mcp_show_text(&v));
            let p = json!({"name": server});
            sc.rows.push(vec![
                self.op("🔄 Redémarrer", "mcp.restart", p.clone(), here.clone())
                    .await?,
                self.op("🧪 Tester", "mcp.test", p.clone(), here.clone())
                    .await?,
            ]);
            let enabled = v["config"]["enabled"].as_bool().unwrap_or(true);
            sc.rows.push(vec![
                self.op("📜 Journal", "mcp.logs", p.clone(), here.clone())
                    .await?,
                if enabled {
                    self.guarded(
                        "⏻ Désactiver",
                        "mcp.disable",
                        p.clone(),
                        &format!("Désactiver le serveur `{server}` ?"),
                        here.clone(),
                    )
                    .await?
                } else {
                    self.op("⏻ Activer", "mcp.enable", p.clone(), here.clone())
                        .await?
                },
            ]);
            if v["status"]["state"].as_str() == Some("auth_required") {
                sc.rows.push(vec![
                    self.op("🔐 Autoriser", "mcp.auth", p.clone(), here.clone())
                        .await?,
                ]);
            }
            sc.rows
                .push(vec![self.nav("« Serveurs", "mcp", json!({})).await?]);
            sc
        };
        Ok(screen)
    }

    /// Écran `models`.
    pub(super) async fn screen_models(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let screen: Screen = {
            let filter = args["filter"].as_str().unwrap_or_default();
            let v = rpc.call(m::MODEL_LIST, json!({"filter": filter})).await?;
            let models = v["models"].as_array().cloned().unwrap_or_default();
            let mut t = routing_text(&v);
            t.push_str(&if models.is_empty() && filter.is_empty() {
                format!(
                    "\n{} modèle(s) au catalogue.",
                    super::shown(&v["catalog_size"])
                )
            } else if filter.is_empty() {
                format!(
                    "\n**Catalogue** ({}) : un modèle pour l'affecter à un alias.",
                    models.len()
                )
            } else {
                format!(
                    "\n**Catalogue « {filter} »** ({}) : un modèle pour l'affecter à un alias.",
                    models.len()
                )
            });
            let mut sc = Screen::new(t);
            let page = page_of(args);
            for x in models.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let id = x["id"].as_str().unwrap_or("?");
                let mut back_args = args.clone();
                if !back_args.is_object() {
                    back_args = json!({});
                }
                sc.rows.push(vec![
                    self.nav(
                        &format!(
                            "{} · {} $/M",
                            trunc(&short_model(id), 36),
                            super::shown(&x["usd_per_m_in"])
                        ),
                        "model.assign",
                        json!({"model": id, "back": back_of("models", &back_args)}),
                    )
                    .await?,
                ]);
            }
            self.pager(&mut sc, name, args, models.len()).await?;
            let mut last = vec![ButtonSpec::copy_text("🔎 Chercher", "/models ")];
            if !filter.is_empty() {
                last.push(self.nav("Tout le catalogue", "models", json!({})).await?);
            }
            sc.rows.push(last);
            sc
        };
        Ok(screen)
    }

    /// Écran `model.assign`.
    pub(super) async fn screen_model_assign(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let screen: Screen = {
            let model = args["model"].as_str().unwrap_or_default();
            let back = if args["back"].is_object() {
                args["back"].clone()
            } else {
                back_of("models", &json!({}))
            };
            let v = rpc.call(m::MODEL_LIST, json!({"filter": "\u{0}"})).await?;
            let mut sc = Screen::new(format!(
                "Affecter `{model}` à quel alias ?\nL'alias change pour toutes les sessions \
                 qui l'utilisent."
            ));
            let mut row = Vec::new();
            for a in v["aliases"].as_array().cloned().unwrap_or_default() {
                let alias = a["alias"].as_str().unwrap_or("?");
                row.push(
                    self.op(
                        alias,
                        "model.assign",
                        json!({"alias": alias, "model": model}),
                        back.clone(),
                    )
                    .await?,
                );
                if row.len() == 3 {
                    sc.rows.push(std::mem::take(&mut row));
                }
            }
            sc.rows.push(row);
            sc.rows.push(vec![
                self.nav(
                    "« Retour",
                    back["screen"].as_str().unwrap_or("models"),
                    back["args"].clone(),
                )
                .await?,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `skills`.
    pub(super) async fn screen_skills(
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
            let all = s.skills.all();
            if all.is_empty() {
                return Ok(Screen::new("Aucune skill chargée."));
            }
            let mut sc = Screen::new(format!(
                "**Skills** ({})\n📖 affiche, ⏪ revient à la version précédente.\n",
                all.len()
            ));
            let page = page_of(args);
            for sk in all.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                sc.text.push_str(&format!(
                    "\n- **{}** · {} · v{} : {}",
                    sk.name,
                    sk.scope.as_str(),
                    sk.version,
                    trunc(&sk.description, 90)
                ));
                let mut row = vec![
                    self.nav(
                        &format!("📖 {}", trunc(&sk.name, 28)),
                        "skill",
                        json!({"name": sk.name}),
                    )
                    .await?,
                ];
                if sk.scope == penelope_skills::Scope::User {
                    row.push(
                        self.guarded(
                            "⏪",
                            "skill.rollback",
                            json!({"name": sk.name}),
                            &format!("Revenir à la version précédente de `{}` ?", sk.name),
                            here.clone(),
                        )
                        .await?,
                    );
                }
                sc.rows.push(row);
            }
            self.pager(&mut sc, name, args, all.len()).await?;
            sc
        };
        Ok(screen)
    }

    /// Écran `skill`.
    pub(super) async fn screen_skill(
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
            let skill_name = args["name"].as_str().unwrap_or_default();
            let sk = s
                .skills
                .get(skill_name)
                .ok_or_else(|| anyhow::anyhow!("skill `{skill_name}` introuvable"))?;
            let mut sc = Screen::new(format!(
                "📖 **{}** · {} · v{}\n{}\n{}\n\n```\n{}\n```",
                sk.name,
                sk.scope.as_str(),
                sk.version,
                sk.description,
                if sk.activation.is_empty() {
                    String::new()
                } else {
                    format!("Déclencheurs : {}", sk.activation.join(", "))
                },
                trunc(&sk.body, 900).replace("```", "ʼʼʼ")
            ));
            let mut row = Vec::new();
            if sk.scope == penelope_skills::Scope::User {
                row.push(
                    self.guarded(
                        "⏪ Version précédente",
                        "skill.rollback",
                        json!({"name": sk.name}),
                        &format!("Revenir à la version précédente de `{}` ?", sk.name),
                        here.clone(),
                    )
                    .await?,
                );
            }
            row.push(self.nav("« Skills", "skills", json!({})).await?);
            sc.rows.push(row);
            sc
        };
        Ok(screen)
    }

    /// Écran `doctor`.
    pub(super) async fn screen_doctor(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let screen: Screen = {
            let checks = rpc.call(m::DOCTOR, json!({})).await?;
            let checks = checks.as_array().cloned().unwrap_or_default();
            let failed: Vec<&Value> = checks
                .iter()
                .filter(|c| c["ok"].as_bool() == Some(false))
                .collect();
            let mut t = if failed.is_empty() {
                format!(
                    "🩺 **Diagnostic** : {} contrôle(s), tout va bien.",
                    checks.len()
                )
            } else {
                format!(
                    "🩺 **Diagnostic** : {} alerte(s) sur {} contrôle(s)\n",
                    failed.len(),
                    checks.len()
                )
            };
            let mut targets: Vec<(&str, &str)> = Vec::new();
            for c in &failed {
                let id = c["id"].as_str().unwrap_or_default();
                t.push_str(&format!(
                    "\n⚠️ **{}** : {}",
                    c["label"].as_str().unwrap_or(id),
                    trunc(c["detail"].as_str().unwrap_or_default(), 300)
                ));
                if let Some(fix) = c["fix"].as_str() {
                    t.push_str(&format!("\n  ↳ `{}`", trunc(fix, 160)));
                }
                let target = if id.starts_with("mcp") {
                    Some(("🔌 MCP", "mcp"))
                } else if id.contains("budget") {
                    Some(("💶 Dépenses", "usage"))
                } else if id.contains("embedding") || id.contains("model") {
                    Some(("🧠 Modèles", "models"))
                } else {
                    None
                };
                if let Some(tg) = target
                    && !targets.contains(&tg)
                {
                    targets.push(tg);
                }
            }
            let ok: Vec<&str> = checks
                .iter()
                .filter(|c| c["ok"].as_bool() == Some(true))
                .filter_map(|c| c["label"].as_str())
                .collect();
            if !failed.is_empty() && !ok.is_empty() {
                t.push_str(&format!("\n\n✅ {}", trunc(&ok.join(", "), 600)));
            }
            let mut sc = Screen::new(t);
            let mut row = Vec::new();
            for (label, target) in targets {
                row.push(if target == "usage" {
                    self.command_button(label, target).await?
                } else {
                    self.nav(label, target, json!({})).await?
                });
            }
            sc.rows.push(row);
            sc.rows
                .push(vec![self.nav("🔁 Relancer", "doctor", json!({})).await?]);
            sc
        };
        Ok(screen)
    }

    /// Écran `secrets`.
    pub(super) async fn screen_secrets(
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
            let names = rpc.call(m::SECRET_LIST, json!({})).await?;
            let names: Vec<String> = names
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|x| {
                    x.as_str()
                        .map(String::from)
                        .or_else(|| x["name"].as_str().map(String::from))
                })
                .collect();
            let mut sc = Screen::new(format!(
                "🔑 **Secrets** ({})\nUn secret ne se saisit **jamais** dans une conversation : \
                 en SSH, `penelope secret set <nom>` puis coller la valeur à l'invite.",
                names.len()
            ));
            for n in &names {
                sc.text.push_str(&format!("\n- `{n}`"));
                sc.rows.push(vec![
                    self.guarded(
                        &format!("🗑 {}", trunc(n, 40)),
                        "secret.rm",
                        json!({"name": n}),
                        &format!("Supprimer le secret `{n}` ?"),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `upgrade`.
    pub(super) async fn screen_upgrade(
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
            // Le dernier résultat de « Vérifier » vaut tant que l'écran n'en porte pas.
            let cached: Value = s
                .kv_get("tg.upgrade.last_check")
                .await?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or(json!({}));
            let args = if args["latest"].is_string() || args["error"].is_string() {
                args
            } else {
                &cached
            };
            let mut t = format!(
                "⬆️ **Mise à jour** · version installée {}",
                penelope_daemon::VERSION
            );
            match (args["latest"].as_str(), args["up_to_date"].as_bool()) {
                (Some(latest), Some(true)) => {
                    t.push_str(&format!("\n✅ À jour (dernière publiée : {latest})."))
                }
                (Some(latest), _) => t.push_str(&format!("\n🆕 **{latest}** est disponible.")),
                _ => t.push_str("\n🔎 « Vérifier » interroge les releases publiées."),
            }
            if let Some(e) = args["error"].as_str() {
                t.push_str(&format!("\n❌ {e}"));
            }
            // Installation depuis les sources : l'installation passe par la bascule vers
            // les releases (issue #33).
            let from_sources = penelope_app::helpers::running_binary()
                .is_ok_and(|b| penelope_app::helpers::is_source_build(&b));
            if from_sources {
                t.push_str(
                    "\n📦 Installation depuis les sources : « Installer » propose de basculer \
                     vers les releases.",
                );
            }
            let mut sc = Screen::new(t);
            sc.rows.push(vec![
                self.op("🔎 Vérifier", "upgrade.check", json!({}), here.clone())
                    .await?,
            ]);
            if from_sources {
                sc.rows.push(vec![
                    self.nav("⬆️ Installer", "upgrade.switch", json!({}))
                        .await?,
                ]);
            } else {
                sc.rows.push(vec![
                    self.guarded(
                        "⬆️ Installer",
                        "upgrade.install",
                        json!({}),
                        "Installer la dernière version publiée puis redémarrer ?",
                        here.clone(),
                    )
                    .await?,
                    self.guarded(
                        "⏪ Revenir",
                        "upgrade.rollback",
                        json!({}),
                        "Revenir au binaire précédent puis redémarrer ?",
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `upgrade.switch`.
    pub(super) async fn screen_upgrade_switch(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let screen: Screen = {
            let cfg = s.config.config();
            let current = penelope_app::helpers::running_binary().map_err(anyhow::Error::msg)?;
            let install_dir = s.platform.dirs.expand(&cfg.upgrade.install_dir);
            let source = penelope_ops::upgrade::Source::from_config(&cfg);
            let latest: Option<String> = s
                .kv_get("tg.upgrade.last_check")
                .await?
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .and_then(|v| v["latest"].as_str().map(String::from));
            let preflight =
                penelope_ops::upgrade::switch_preflight(&penelope_ops::upgrade::Switch {
                    source: &source,
                    tag: None,
                    current: &current,
                    install_dir: &install_dir,
                    state_dir: &s.platform.dirs.state(),
                    now: s.clock.now_rfc3339(),
                    codesign: penelope_ops::upgrade::codesign_of(&cfg),
                    host: &penelope_ops::upgrade::SystemHost,
                });
            let mut t = format!(
                "📦 **Installation depuis les sources** (`{}`).\nBasculer vers les releases ? \
                 Le service lancera `{}/penelope`, un chemin stable que les mises à jour \
                 remplacent ; `make deploy` sur la machine y installe une compilation.\n",
                current.display(),
                install_dir.display()
            );
            match &preflight {
                Ok(p) => t.push_str(&format!(
                    "\n✅ Signature utilisable, `{}` inscriptible, service `{}` modifiable.",
                    install_dir.display(),
                    p.service_file.display()
                )),
                Err(e) => t.push_str(&format!("\n❌ {e}")),
            }
            let mut sc = Screen::new(t);
            if preflight.is_ok() {
                let label = match &latest {
                    Some(v) => format!("📦 Basculer et installer {v}"),
                    None => "📦 Basculer et installer la dernière version".to_string(),
                };
                sc.rows.push(vec![
                    self.op(
                        &label,
                        "upgrade.switch",
                        json!({}),
                        back_of("upgrade", &json!({})),
                    )
                    .await?,
                ]);
            }
            sc.rows.push(vec![
                self.op(
                    "Garder les sources",
                    "upgrade.keep_sources",
                    json!({}),
                    Value::Null,
                )
                .await?,
                self.nav("Annuler", "upgrade", json!({})).await?,
            ]);
            sc
        };
        Ok(screen)
    }
}
