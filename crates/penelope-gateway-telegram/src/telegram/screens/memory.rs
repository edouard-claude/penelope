//! Écrans de la mémoire : `forget`, `forget.sessions`, `learned`, `entry`, `practices`,
//! `practice`, `intentions`, `policies`.

use super::*;

impl TelegramGateway {
    /// Écran `forget`.
    pub(super) async fn screen_forget(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let rpc = penelope_daemon::rpc::Rpc::new(d.clone());
        let here = back_of(name, args);
        let screen: Screen = {
            let query = args["query"].as_str().unwrap_or_default();
            let items: Vec<(String, String)> = if query.is_empty() {
                let mut seen = std::collections::BTreeSet::new();
                penelope_dream::dream::learned(s, 30)
                    .await?
                    .into_iter()
                    .filter_map(|i| {
                        let uid = i["uid"].as_str()?.to_string();
                        let text = i["text"].as_str()?.to_string();
                        seen.insert(uid.clone()).then_some((uid, text))
                    })
                    .take(PER_PAGE)
                    .collect()
            } else {
                rpc.call(m::MEM_SEARCH, json!({"query": query}))
                    .await?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|h| {
                        Some((
                            h["uid"].as_str()?.to_string(),
                            h["text"].as_str().unwrap_or_default().to_string(),
                        ))
                    })
                    .take(PER_PAGE)
                    .collect()
            };
            let mut sc = Screen::new(match (query.is_empty(), items.is_empty()) {
                (true, true) => "Rien de récent à oublier : cherche une entrée.".to_string(),
                (true, false) => {
                    "**Oublier** : entrées récentes, ou cherche une entrée.\n".to_string()
                }
                (false, true) => format!("Aucune entrée pour « {query} »."),
                (false, false) => format!("**Oublier** : entrées pour « {query} ».\n"),
            });
            for (i, (uid, text)) in items.iter().enumerate() {
                sc.text
                    .push_str(&format!("\n{}. {}", i + 1, trunc(text, 160)));
                sc.rows.push(vec![
                    self.guarded(
                        &format!("🗑 {}. {}", i + 1, trunc(text, 36)),
                        "mem.forget",
                        json!({"uid": uid}),
                        &format!("Oublier « {} » ?", trunc(text, 200)),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            sc.rows
                .push(vec![ButtonSpec::copy_text("🔎 Chercher", "/oublie ")]);
            sc
        };
        Ok(screen)
    }

    /// Écran `learned`.
    pub(super) async fn screen_learned(
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
            let days = args["days"].as_i64().unwrap_or(7);
            let items = penelope_dream::dream::learned(s, days).await?;
            let mut sc = Screen::new(if items.is_empty() {
                format!("Rien appris sur les {days} derniers jours.")
            } else {
                format!("📚 **Appris sur {days} jours** ({})\n", items.len())
            });
            let page = page_of(args);
            for i in items.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let text = i["text"].as_str().unwrap_or("(entrée retirée depuis)");
                sc.text.push_str(&format!(
                    "\n- {} _({}, {})_",
                    trunc(text, 160),
                    i["file"].as_str().unwrap_or("?"),
                    i["ts"].as_str().and_then(|t| t.get(..10)).unwrap_or("")
                ));
                if let Some(uid) = i["uid"].as_str().filter(|_| i["text"].is_string()) {
                    sc.rows.push(vec![
                        self.nav(
                            &format!("🔎 {}", trunc(text, 40)),
                            "entry",
                            json!({"uid": uid, "back": here.clone()}),
                        )
                        .await?,
                    ]);
                }
            }
            self.pager(&mut sc, name, args, items.len()).await?;
            let mut row = Vec::new();
            for n in [7, 30, 90] {
                if n != days {
                    row.push(
                        self.nav(&format!("{n} jours"), "learned", json!({"days": n}))
                            .await?,
                    );
                }
            }
            sc.rows.push(row);
            sc
        };
        Ok(screen)
    }

    /// Écran `entry`.
    pub(super) async fn screen_entry(
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
            let uid = args["uid"].as_str().unwrap_or_default();
            let back = if args["back"].is_object() {
                args["back"].clone()
            } else {
                back_of("learned", &json!({}))
            };
            let e = s
                .memory
                .get(uid)
                .await?
                .ok_or_else(|| anyhow::anyhow!("entrée `{uid}` introuvable"))?;
            let u = uid.to_string();
            let prov: Option<(String, Option<String>, String)> = s
                .store
                .read(move |c| {
                    let mut st = c.prepare(
                        "SELECT origin, source_ref, observed_at FROM mem_provenance WHERE uid = ?1",
                    )?;
                    let mut rows = st.query([&u])?;
                    Ok(match rows.next()? {
                        Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?)),
                        None => None,
                    })
                })
                .await?;
            let mut t = format!(
                "**Entrée** `{uid}`\n{}\n\nFichier `{}` · niveau {} · statut {}",
                e.text,
                e.file,
                e.level.as_str(),
                e.statut
            );
            if let Some((origin, source, at)) = prov {
                t.push_str(&format!(
                    "\nOrigine {origin}{} · {}",
                    source.map(|x| format!(" · `{x}`")).unwrap_or_default(),
                    at.get(..10).unwrap_or(&at)
                ));
            }
            let mut sc = Screen::new(t);
            sc.rows.push(vec![
                self.op(
                    "✅ Valider",
                    "mem.validate",
                    json!({"uid": uid}),
                    here.clone(),
                )
                .await?,
                self.guarded(
                    "🚫 Rejeter",
                    "mem.forget",
                    json!({"uid": uid}),
                    &format!("Retirer « {} » de la mémoire ?", trunc(&e.text, 200)),
                    back.clone(),
                )
                .await?,
            ]);
            sc.rows.push(vec![
                self.nav(
                    "« Retour",
                    back["screen"].as_str().unwrap_or("learned"),
                    back["args"].clone(),
                )
                .await?,
            ]);
            sc
        };
        Ok(screen)
    }

    /// Écran `practices`.
    pub(super) async fn screen_practices(
        &self,
        _chat_id: i64,
        _topic_id: Option<i64>,
        name: &str,
        args: &Value,
    ) -> anyhow::Result<Screen> {
        let d = &self.daemon;
        let s = &d.services;
        let screen: Screen = {
            let dir = penelope_app::helpers::vault_dir(s).join("pratiques");
            let mut found: Vec<(String, penelope_memory::vault::Practice)> =
                std::fs::read_dir(&dir)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let slug = name.strip_suffix(".md")?.to_string();
                        let raw = std::fs::read_to_string(e.path()).ok()?;
                        let p = penelope_memory::vault::Practice::parse(&raw, &slug).ok()?;
                        Some((slug, p))
                    })
                    .collect();
            found.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sc = Screen::new(if found.is_empty() {
                "Aucune pratique dans le vault (`pratiques/`).".to_string()
            } else {
                format!("📐 **Pratiques** ({})\n", found.len())
            });
            let page = page_of(args);
            for (slug, p) in found.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                sc.rows.push(vec![
                    self.nav(
                        &format!("📐 {} · {}", trunc(&p.title, 32), p.statut.as_str()),
                        "practice",
                        json!({"slug": slug}),
                    )
                    .await?,
                ]);
            }
            self.pager(&mut sc, name, args, found.len()).await?;
            sc
        };
        Ok(screen)
    }

    /// Écran `practice`.
    pub(super) async fn screen_practice(
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
            let slug = args["slug"].as_str().unwrap_or_default();
            let path = penelope_app::helpers::vault_dir(s).join(format!("pratiques/{slug}.md"));
            let p = std::fs::read_to_string(&path)
                .map_err(|_| anyhow::anyhow!("pratique `{slug}` introuvable"))
                .and_then(|raw| {
                    penelope_memory::vault::Practice::parse(&raw, slug).map_err(anyhow::Error::msg)
                })?;
            let mut t = format!(
                "📐 **{}** · confiance {:.1} · {}\n\nDéfaut : {}",
                p.title,
                p.confiance,
                p.statut.as_str(),
                p.default_entry
                    .as_ref()
                    .map(|e| e.text.as_str())
                    .unwrap_or("(aucun)")
            );
            for e in &p.exceptions {
                t.push_str(&format!(
                    "\n- Exception : {} ({})",
                    e.text,
                    e.annotations
                        .quand
                        .as_ref()
                        .map(|q| q.render())
                        .unwrap_or_default()
                ));
            }
            if !p.ecarts.is_empty() {
                t.push_str(&format!("\n{} écart(s) observé(s).", p.ecarts.len()));
            }
            let mut sc = Screen::new(t);
            sc.rows.push(vec![
                self.op(
                    "✅ Valider",
                    "practice.status",
                    json!({"slug": slug, "statut": "active"}),
                    here.clone(),
                )
                .await?,
                self.op(
                    "🚫 Rejeter",
                    "practice.status",
                    json!({"slug": slug, "statut": "contestee"}),
                    here.clone(),
                )
                .await?,
            ]);
            sc.rows
                .push(vec![self.nav("« Pratiques", "practices", json!({})).await?]);
            sc
        };
        Ok(screen)
    }

    /// Écran `intentions`.
    pub(super) async fn screen_intentions(
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
            let armed: Vec<_> = s
                .intents
                .all()
                .await?
                .into_iter()
                .filter(|i| i.etat == penelope_memory::intents::IntentState::Armee)
                .collect();
            let mut sc = Screen::new(if armed.is_empty() {
                "Aucune intention armée.".to_string()
            } else {
                format!("🎯 **Intentions armées** ({})\n", armed.len())
            });
            for i in &armed {
                sc.text.push_str(&format!(
                    "\n- {} · déclencheurs : {} · {}/{} tir(s)",
                    trunc(&i.texte, 140),
                    i.declencheurs.join(", "),
                    i.tirs,
                    i.budget_tirs
                ));
                sc.rows.push(vec![
                    self.op(
                        &format!("❌ {}", trunc(&i.texte, 40)),
                        "intent.cancel",
                        json!({"id": i.id}),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `policies`.
    pub(super) async fn screen_policies(
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
            let rules = s.policies.active_rules().await?;
            let mut sc = Screen::new(if rules.is_empty() {
                "Aucune règle d'autorisation active.".to_string()
            } else {
                format!("🛡 **Règles d'autorisation** ({})\n", rules.len())
            });
            for r in &rules {
                let label = r
                    .tool
                    .clone()
                    .or_else(|| r.server.clone().map(|x| format!("serveur {x}")))
                    .unwrap_or_else(|| "toutes les actions".into());
                // Portée réelle de la règle : « command commence par cargo test »
                // (issue #67).
                let scope = r
                    .arg_match
                    .as_ref()
                    .map(|p| format!(" · {}", penelope_hitl::policy::describe_pattern(p)))
                    .unwrap_or_default();
                sc.text.push_str(&format!(
                    "\n- `{label}`{scope} · {:?} · {:?} · {} usage(s)",
                    r.decision, r.window, r.hits
                ));
                // Une règle qui ne sert à rien se voit (issue #111).
                if let Some(note) = penelope_daemon::approval_mode::rule_note(r, s.clock.now_ms()) {
                    sc.text.push_str(&format!(" · ⚠️ {note}"));
                }
                sc.rows.push(vec![
                    self.guarded(
                        &format!("🗑 {}", trunc(&label, 40)),
                        "policy.revoke",
                        json!({"id": r.id}),
                        &format!("Retirer la règle « {label} » ?"),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            sc
        };
        Ok(screen)
    }

    /// Écran `forget.sessions`.
    pub(super) async fn screen_forget_sessions(
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
            let sessions = s
                .sessions
                .list(Some(penelope_kernel::session::SessionKind::Chat), 50)
                .await?;
            let mut sc = Screen::new(
                "🧹 **Oublier une session** : tout ce que la mémoire a retenu d'elle est retiré \
                 (entrées et candidats). La conversation elle-même reste.",
            );
            let page = page_of(args);
            for sess in sessions.iter().skip(page * PER_PAGE).take(PER_PAGE) {
                let label = penelope_conversation::titles::label(sess);
                sc.rows.push(vec![
                    self.guarded(
                        &format!("🗑 {}", trunc(&label, 40)),
                        "session.forget",
                        json!({"session": sess.id.to_string()}),
                        &format!("Oublier tout ce qui vient de « {label} » ?"),
                        here.clone(),
                    )
                    .await?,
                ]);
            }
            self.pager(&mut sc, name, args, sessions.len()).await?;
            sc
        };
        Ok(screen)
    }
}
