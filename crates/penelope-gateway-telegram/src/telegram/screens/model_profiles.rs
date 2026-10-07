//! Écrans de `/model` (#334) : profils, familles de rôles, choix d'un modèle, « Tout
//! voir » et écarts en cours.
//!
//! ```text
//! /model ─► model.home : profils, familles, principal, tout voir, écarts, cette session
//!             ├─ ● profil actif ──► model.profile : dupliquer, renommer, garde, supprimer
//!             ├─ ○ autre profil ──► confirmer ─► basculer
//!             ├─ famille ─────────► model.family ─► model.role ─► suivre le principal
//!             │                                                └► model.pick (fournisseur,
//!             │                                                     puis modèle, prix, fenêtre)
//!             ├─ 🔁 principal ────► model.pick { target: primary }
//!             ├─ 📋 tout voir ────► model.all
//!             └─ ⚠️ écarts ───────► model.deviations
//! ```

use super::*;
use penelope_kernel::config::{CODEX_RISK, Family};

/// Icône d'une famille : la raison la plus parlante parmi ses rôles.
fn family_icon(rows: &[Value]) -> &'static str {
    let has = |r: &str| rows.iter().any(|x| x["reason"] == r);
    if has("codex_guard") {
        "⛔"
    } else if has("override") {
        "✎"
    } else if has("capability") {
        "⚡"
    } else if has("local") {
        ""
    } else {
        "✓"
    }
}

fn rows_of(v: &Value, family: &str) -> Vec<Value> {
    v["roles"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|r| r["family"] == family)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Une ligne de rôle : icône, nom, modèle effectif, raison, et la garde s'il y a lieu.
fn role_line(r: &Value) -> String {
    let guarded = r["guarded"]
        .as_str()
        .map(|g| format!(" → `{}` sous la garde", short_model(g)))
        .unwrap_or_default();
    format!(
        "{} {} : `{}`{guarded} _({})_",
        r["icon"].as_str().unwrap_or(" "),
        r["name"].as_str().unwrap_or("?"),
        r["model"]
            .as_str()
            .map(short_model)
            .unwrap_or("aucun".into()),
        r["why"].as_str().unwrap_or(""),
    )
}

/// Préfixe d'un identifiant pour une entrée du catalogue.
fn prefixed(info: &penelope_llm::catalog::ModelInfo) -> String {
    let p = match info.provider.as_str() {
        "openrouter" | "codex" => info.provider.as_str(),
        _ => "local",
    };
    format!("{p}:{}", penelope_llm::catalog::strip_provider(&info.id))
}

impl TelegramGateway {
    async fn model_view(&self) -> anyhow::Result<Value> {
        penelope_daemon::rpc::Rpc::new(self.daemon.clone())
            .call(m::MODEL_LIST, json!({}))
            .await
    }

    /// Écran `model.home` : la maquette de #334.
    pub(super) async fn screen_model_home(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let back = back_of("model.home", &json!({}));
        let primary = v["primary"]["model"].as_str().unwrap_or("?");
        let mut text = format!(
            "🧠 Profil actif : « {} »  ·  principal {} · {}",
            v["profile"].as_str().unwrap_or("?"),
            penelope_llm::catalog::provider_of(primary),
            short_model(primary)
        );
        if v["codex_background"] == "allow" {
            text.push_str("\n🔓 Garde Codex levée : tout passe par l'abonnement.");
        }
        // L'état de cette session, en une ligne : épinglée ou automatique.
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        if let Ok(session) = self.daemon.chat_session_for(&origin).await {
            let pin = penelope_app::helpers::pinned_model(&self.daemon.services, &session).await;
            text.push_str(&format!(
                "\nCette session : {}.",
                match pin {
                    Some(p) => format!("épinglée sur `{}`, sans repli", p.alias),
                    None => "automatique".into(),
                }
            ));
        }
        let mut sc = Screen::new(text);
        let mut row = Vec::new();
        for p in v["profiles"].as_array().cloned().unwrap_or_default() {
            let name = p["name"].as_str().unwrap_or("?");
            row.push(if p["active"].as_bool().unwrap_or(false) {
                self.nav(&format!("● {name}"), "model.profile", json!({"name": name}))
                    .await?
            } else {
                self.guarded(
                    &format!("○ {name}"),
                    "model.profile",
                    json!({"action": "use", "name": name}),
                    &format!("Basculer sur le profil « {name} » ? Toutes les sessions suivent."),
                    back.clone(),
                )
                .await?
            });
        }
        row.push(self.nav("+ Nouveau profil", "model.new", json!({})).await?);
        sc.rows.push(row);
        let mut families = Vec::new();
        for f in Family::ALL {
            let icon = family_icon(&rows_of(&v, f.key()));
            let label = if icon.is_empty() {
                f.label().to_string()
            } else {
                format!("{} {icon}", f.label())
            };
            families.push(
                self.nav(&label, "model.family", json!({"family": f.key()}))
                    .await?,
            );
        }
        sc.rows.push(families[..2].to_vec());
        sc.rows.push(families[2..].to_vec());
        sc.rows.push(vec![
            self.nav(
                "🔁 Modèle principal",
                "model.pick",
                json!({"target": "primary"}),
            )
            .await?,
            self.nav("📋 Tout voir", "model.all", json!({})).await?,
            self.nav("⚠️ Écarts en cours", "model.deviations", json!({}))
                .await?,
        ]);
        sc.rows.push(vec![
            self.command_button_with("📌 Cette session", "model", "session")
                .await?,
        ]);
        Ok(sc)
    }

    /// Écran `model.family` : les rôles d'une famille, un bouton par rôle.
    pub(super) async fn screen_model_family(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let key = args["family"].as_str().unwrap_or("conversation");
        let family = Family::parse(key).unwrap_or(Family::Conversation);
        let rows = rows_of(&v, family.key());
        let mut text = format!("**{}**\n\n", family.label());
        for r in &rows {
            text.push_str(&role_line(r));
            text.push('\n');
        }
        if family == Family::Background {
            text.push_str("\nLes veilles planifiées suivent la conversation, sous la garde Codex.");
        }
        let mut sc = Screen::new(text);
        for r in &rows {
            let role = r["role"].as_str().unwrap_or("?");
            sc.rows.push(vec![
                self.nav(
                    &format!(
                        "{} {}",
                        r["icon"].as_str().unwrap_or(""),
                        r["name"].as_str().unwrap_or(role)
                    ),
                    "model.role",
                    json!({"role": role, "family": key}),
                )
                .await?,
            ]);
        }
        sc.rows
            .push(vec![self.nav("« Retour", "model.home", json!({})).await?]);
        Ok(sc)
    }

    /// Écran `model.role` : le modèle d'un rôle, et ce qu'on peut en faire.
    pub(super) async fn screen_model_role(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let role = args["role"].as_str().unwrap_or_default();
        let r = v["roles"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["role"] == role).cloned())
            .unwrap_or(Value::Null);
        let mut sc = Screen::new(format!(
            "{}\n\nModèle effectif : `{}`.",
            role_line(&r),
            r["model"].as_str().unwrap_or("aucun")
        ));
        let back = back_of(name, args);
        let voice = penelope_kernel::config::VOICE_ROLES.contains(&role);
        let mut row = Vec::new();
        if matches!(r["reason"].as_str(), Some("override")) {
            row.push(
                self.op(
                    "✓ Suivre le principal",
                    "model.unset",
                    json!({"target": role}),
                    back.clone(),
                )
                .await?,
            );
        }
        let target = match role {
            "image_describe" if r["reason"] == "capability" => "vision",
            other => other,
        };
        row.push(
            self.nav(
                if voice {
                    "🏠 Choisir un modèle local"
                } else {
                    "🔀 Choisir un modèle"
                },
                "model.pick",
                json!({"target": target, "back": back.clone()}),
            )
            .await?,
        );
        sc.rows.push(row);
        sc.rows.push(vec![
            self.nav(
                "« Retour",
                "model.family",
                json!({"family": args["family"].as_str().unwrap_or("conversation")}),
            )
            .await?,
        ]);
        Ok(sc)
    }

    /// Écran `model.pick` : le fournisseur, puis le modèle, avec prix et fenêtre.
    pub(super) async fn screen_model_pick(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let target = args["target"].as_str().unwrap_or("primary").to_string();
        let back = if args["back"].is_object() {
            args["back"].clone()
        } else {
            back_of("model.home", &json!({}))
        };
        let s = &self.daemon.services;
        let Some(provider) = args["provider"].as_str() else {
            let cfg = s.config.config();
            let mut sc = Screen::new(format!(
                "Modèle pour `{target}` : quel fournisseur ? Un alias garde son nom."
            ));
            let mut row = Vec::new();
            for (p, on) in [
                ("openrouter", cfg.providers.openrouter.enabled),
                ("codex", cfg.providers.codex.enabled),
                (
                    "local",
                    cfg.providers.local.enabled || cfg.providers.extra.values().any(|e| e.enabled),
                ),
            ] {
                if on {
                    let mut a = args.clone();
                    a["provider"] = json!(p);
                    row.push(self.nav(p, name, a).await?);
                }
            }
            sc.rows.push(row);
            let mut aliases = Vec::new();
            for alias in cfg.models.aliases.keys() {
                aliases.push(
                    self.op(
                        alias,
                        "model.set",
                        json!({"target": target, "model": alias}),
                        back.clone(),
                    )
                    .await?,
                );
                if aliases.len() == 3 {
                    sc.rows.push(std::mem::take(&mut aliases));
                }
            }
            sc.rows.push(aliases);
            sc.rows.push(vec![
                self.nav(
                    "« Retour",
                    back["screen"].as_str().unwrap_or("model.home"),
                    back["args"].clone(),
                )
                .await?,
            ]);
            return Ok(sc);
        };
        let filter = args["filter"].as_str().unwrap_or_default().to_lowercase();
        let models: Vec<penelope_llm::catalog::ModelInfo> = s
            .catalog
            .list(None)
            .into_iter()
            .filter(|i| match provider {
                "openrouter" | "codex" => i.provider == provider,
                _ => i.provider != "openrouter" && i.provider != "codex",
            })
            .filter(|i| filter.is_empty() || i.id.to_lowercase().contains(&filter))
            .collect();
        let mut sc = Screen::new(format!(
            "Modèle `{provider}` pour `{target}` ({} au catalogue{}) :",
            models.len(),
            if filter.is_empty() {
                String::new()
            } else {
                format!(", « {filter} »")
            }
        ));
        let page = page_of(args);
        for i in models.iter().skip(page * PER_PAGE).take(PER_PAGE) {
            let price = (i.price_prompt * 1_000_000.0 * 100.0).round() / 100.0;
            let label = format!(
                "{} · {price} $/M · {}k",
                trunc(&short_model(&i.id), 30),
                i.context_window / 1000
            );
            sc.rows.push(vec![
                self.op(
                    &label,
                    "model.set",
                    json!({"target": target, "model": prefixed(i)}),
                    back.clone(),
                )
                .await?,
            ]);
        }
        self.pager(&mut sc, name, args, models.len()).await?;
        let mut a = args.clone();
        a["provider"] = Value::Null;
        sc.rows.push(vec![
            ButtonSpec::copy_text("🔎 Chercher", &format!("/model {target} {provider}:")),
            self.nav("« Fournisseurs", name, a).await?,
        ]);
        Ok(sc)
    }

    /// Écran `model.all` : la table complète rôle → modèle effectif → raison.
    pub(super) async fn screen_model_all(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let mut text = format!(
            "📋 **Profil « {} »**, garde Codex `{}`\n",
            v["profile"].as_str().unwrap_or("?"),
            v["codex_background"].as_str().unwrap_or("deny")
        );
        for f in Family::ALL {
            text.push_str(&format!("\n**{}**\n", f.label()));
            for r in rows_of(&v, f.key()) {
                text.push_str(&role_line(&r));
                text.push('\n');
            }
        }
        text.push_str(
            "\n✓ suit le principal · ✎ surcharge · ⚡ capacité · 🏠 local · ⛔ garde Codex",
        );
        let mut sc = Screen::new(text);
        sc.rows
            .push(vec![self.nav("« Retour", "model.home", json!({})).await?]);
        Ok(sc)
    }

    /// Écran `model.deviations` : replis et garde des dernières 24 h.
    pub(super) async fn screen_model_deviations(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let mut text = String::from("⚠️ **Écarts en cours** (24 h)\n\n");
        let deviations = v["deviations"].as_array().cloned().unwrap_or_default();
        for d in &deviations {
            text.push_str(&format!("{}\n", d["text"].as_str().unwrap_or("?")));
        }
        let guarded: Vec<Value> = v["roles"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| r["guarded"].is_string())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !guarded.is_empty() {
            text.push_str("\nSous la garde Codex, à chaque appel :\n");
            for r in &guarded {
                text.push_str(&role_line(r));
                text.push('\n');
            }
        }
        if deviations.is_empty() && guarded.is_empty() {
            text.push_str("Aucun : ce que le profil choisit est ce qui tourne.");
        }
        let mut sc = Screen::new(text);
        sc.rows
            .push(vec![self.nav("« Retour", "model.home", json!({})).await?]);
        Ok(sc)
    }

    /// Écran `model.profile` : un profil et ses gestes.
    pub(super) async fn screen_model_profile(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let v = self.model_view().await?;
        let profile = args["name"].as_str().unwrap_or_default();
        let p = v["profiles"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["name"] == profile).cloned())
            .unwrap_or(Value::Null);
        let allow = p["codex_background"] == "allow";
        let mut text = format!(
            "**Profil « {profile} »**{}\nPrincipal `{}` ({}), {} surcharge(s).\nGarde Codex : {}.",
            if p["active"].as_bool().unwrap_or(false) {
                " (actif)"
            } else {
                ""
            },
            p["primary"].as_str().unwrap_or("?"),
            p["model"].as_str().unwrap_or("?"),
            p["overrides"].as_u64().unwrap_or(0),
            if allow {
                "levée, tout passe par Codex"
            } else {
                "active, le travail de fond passe ailleurs, en le disant"
            }
        );
        if p["derived"].as_bool().unwrap_or(false) {
            text.push_str("\nDéduit des clés d'avant 1.0.47 : écrit à la première modification.");
        }
        let back = back_of(name, args);
        let mut sc = Screen::new(text);
        let mut row = vec![
            self.op(
                "📄 Dupliquer",
                "model.profile",
                json!({"action": "copy", "name": format!("{profile} (copie)"), "from": profile}),
                back.clone(),
            )
            .await?,
        ];
        if profile != penelope_kernel::config::DEFAULT_PROFILE {
            row.push(ButtonSpec::copy_text(
                "✏️ Renommer",
                &format!("/model profil renommer {profile} → "),
            ));
        }
        sc.rows.push(row);
        let (label, value) = if allow {
            ("🛡 Rétablir la garde Codex", "deny")
        } else {
            ("🔓 Lever la garde Codex", "allow")
        };
        let question = if allow {
            "Rétablir la garde ? Le travail de fond passera hors de Codex, en le disant."
                .to_string()
        } else {
            format!(
                "Lever la garde ? Tout passera par Codex, travail de fond compris. {CODEX_RISK}"
            )
        };
        let mut row = vec![
            self.guarded(
                label,
                "model.profile",
                json!({"action": "guard", "name": profile, "codex_background": value}),
                &question,
                back.clone(),
            )
            .await?,
        ];
        if !p["active"].as_bool().unwrap_or(false) && !p["derived"].as_bool().unwrap_or(false) {
            row.push(
                self.guarded(
                    "🗑 Supprimer",
                    "model.profile",
                    json!({"action": "rm", "name": profile}),
                    &format!("Supprimer le profil « {profile} » ?"),
                    back_of("model.home", &json!({})),
                )
                .await?,
            );
        }
        sc.rows.push(row);
        sc.rows
            .push(vec![self.nav("« Retour", "model.home", json!({})).await?]);
        Ok(sc)
    }

    /// Écran `model.new` : nommer un profil, copie de l'actif.
    pub(super) async fn screen_model_new(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        _name: &str,
        _args: &Value,
    ) -> anyhow::Result<Screen> {
        let mut sc = Screen::new(
            "**Nouveau profil** : sous le nom écrit après la commande, tout y suit le \
             principal de l'actif, images et embeddings repris, garde Codex active ; on y \
             change ensuite le principal et les rôles. « 📄 Dupliquer » sur un profil le \
             copie tel quel.",
        );
        sc.rows.push(vec![
            ButtonSpec::copy_text("✏️ Nommer le profil", "/model profil nouveau "),
            self.nav("« Retour", "model.home", json!({})).await?,
        ]);
        Ok(sc)
    }

    /// Opérations des écrans de `/model`.
    pub(super) async fn perform_models(&self, op: &str, p: &Value) -> anyhow::Result<Done> {
        let rpc = penelope_daemon::rpc::Rpc::new(self.daemon.clone());
        Ok(match op {
            "model.set" => {
                let v = rpc.call(m::MODEL_SET, p.clone()).await?;
                let (target, model) = (shown(&v["target"]), shown(&v["model"]));
                match v["avertissement"].as_str() {
                    Some(w) => Done::note(format!("✅ {target} → {model}"), format!("⚠️ {w}")),
                    None => Done::toast(format!("✅ {target} → {}", short_model(&model))),
                }
            }
            "model.unset" => {
                let v = rpc.call(m::MODEL_UNSET, p.clone()).await?;
                Done::toast(format!("✓ {} suit le principal", shown(&v["target"])))
            }
            _ => {
                let v = rpc.call(m::MODEL_PROFILE, p.clone()).await?;
                Done::toast(format!("🧠 Profil actif : « {} »", shown(&v["profile"])))
            }
        })
    }
}
