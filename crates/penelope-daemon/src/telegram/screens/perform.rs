//! Opérations des boutons d'écran : effectuer, toast, redessiner (issue #30).

use super::*;

impl TelegramGateway {
    // ------------------------------------------------------------ opérations

    #[allow(clippy::too_many_lines)] // gel 0.17 : lot G (telegram/screens/perform.rs)
    pub(super) async fn perform(
        &self,
        chat_id: i64,
        topic_id: Option<i64>,
        op: &str,
        p: &Value,
    ) -> anyhow::Result<Done> {
        let d = &self.daemon;
        let s = &d.services;
        let rpc = crate::rpc::Rpc::new(d.clone());
        let str_of = |key: &str| p[key].as_str().unwrap_or_default().to_string();
        let origin = Origin::Telegram {
            chat_id,
            topic_id,
            message_id: None,
        };
        Ok(match op {
            "wf.run" => {
                let id = str_of("id");
                let entry = s
                    .workflows
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("workflow `{id}` introuvable"))?;
                let title = if entry.metadata.name.is_empty() {
                    id.clone()
                } else {
                    entry.metadata.name.clone()
                };
                self.run_by_conversation(chat_id, topic_id, 0, &entry, "")
                    .await?;
                Done::quiet(format!("Plan de « {title} » en préparation"))
            }
            "wf.plan.go" => {
                let session = str_of("session");
                let version = p["version"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("version manquante"))?;
                let plans = penelope_workflow::plan::PlanStore::new(s.store.clone());
                let mut draft = plans
                    .get(&session)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("plan introuvable"))?;
                let previous = draft.clone();
                draft.approve(version).map_err(|e| anyhow::anyhow!("{e}"))?;
                plans.replace(&session, &previous, &draft).await?;
                Done::note(
                    "Plan approuvé",
                    format!(
                        "✅ Plan v{} de « {} » approuvé et conservé. Prêt pour l'exécution par la prochaine tranche.",
                        draft.plan.version(),
                        draft.plan.goal()
                    ),
                )
            }
            // Rafale de messages : le propriétaire choisit ce qu'on en fait (issue #49).
            "burst.one" | "burst.ingest" | "burst.each" | "burst.drop" => {
                let id = str_of("id");
                let key = format!("tg.burst.{id}");
                let raw = s.kv_get(&key).await?.unwrap_or_default();
                if raw.is_empty() {
                    return Ok(Done::quiet("Rafale déjà traitée."));
                }
                s.kv_set(&key, "").await?;
                let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
                let session = v["session"].as_str().unwrap_or_default().to_string();
                let joined = v["joined"].as_str().unwrap_or_default().to_string();
                let parts: Vec<String> = v["parts"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let origin = Origin::Telegram {
                    chat_id,
                    topic_id,
                    message_id: v["message_id"].as_i64(),
                };
                match op {
                    "burst.one" => {
                        d.enqueue_message(&session, &joined, &origin, None).await?;
                        Done::quiet("Traité comme un seul document")
                    }
                    "burst.each" => {
                        for part in &parts {
                            d.enqueue_message(&session, part, &origin, None).await?;
                        }
                        Done::quiet(format!("{} messages remis en file", parts.len()))
                    }
                    "burst.ingest" => {
                        let (daemon, sess) = (d.clone(), session.clone());
                        let name = format!(
                            "collage-{}.md",
                            s.clock.now_rfc3339().chars().take(19).collect::<String>()
                        );
                        let chan = origin.clone();
                        tokio::spawn(async move {
                            // Déclarée pour la session : `/stop tout` l'interrompt (#155).
                            let (ingest_id, cancel) = daemon.bus.start_ingest(&sess);
                            let outcome = crate::ingest::ingest(
                                &daemon,
                                &name,
                                joined.into_bytes(),
                                "telegram",
                                // Texte écrit par le propriétaire lui-même.
                                penelope_memory::Origin::Owner,
                                Some(&sess),
                                &cancel,
                            )
                            .await;
                            daemon.bus.end_ingest(&sess, ingest_id);
                            let note = match outcome {
                                Ok(i) => i.report(),
                                Err(e) => format!("📄 Ingestion impossible : {e}"),
                            };
                            if let Some(m) = daemon.hooks.messenger() {
                                let _ = m.send_text(&chan, &note).await;
                            }
                        });
                        Done::quiet("Ingestion lancée")
                    }
                    _ => Done::quiet("Rien n'a été traité"),
                }
            }
            "run.pause" | "run.resume" | "run.cancel" => {
                let run = str_of("run");
                let control = match op {
                    "run.pause" => penelope_workflow::Control::Pause,
                    "run.resume" => penelope_workflow::Control::Resume,
                    _ => penelope_workflow::Control::Cancel,
                };
                let state = crate::workflow::control(d, &run, &control).await?;
                Done::toast(format!("Run {} : {}", run, state.as_str()))
            }
            // Livrer dans la conversation (et le sujet) où l'écran est affiché (#124).
            "schedule.here" => {
                let id = str_of("id");
                let to = crate::scheduler::retarget(s, &id, chat_id, topic_id)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Done::toast(format!("📍 Livrera ici : {to}"))
            }
            "schedule.run" | "schedule.pause" | "schedule.resume" | "schedule.rm" => {
                let id = str_of("id");
                let method = match op {
                    "schedule.run" => m::SCHEDULE_RUN_NOW,
                    "schedule.pause" => m::SCHEDULE_PAUSE,
                    "schedule.resume" => m::SCHEDULE_RESUME,
                    _ => m::SCHEDULE_RM,
                };
                rpc.call(method, json!({"id": id})).await?;
                Done::toast(match op {
                    "schedule.run" => "⚡ Déclenché",
                    "schedule.pause" => "⏸ En pause",
                    "schedule.resume" => "▶️ Repris",
                    _ => "🗑 Supprimé",
                })
            }
            "mcp.restart" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_RESTART, json!({"name": name})).await?;
                Done::toast(format!(
                    "🔄 {name} : {} outil(s), {}",
                    super::shown(&v["tool_count"]),
                    v["state"].as_str().unwrap_or("?")
                ))
            }
            "mcp.test" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_TEST, json!({"name": name})).await?;
                if v["ok"].as_bool() == Some(true) {
                    Done::toast(format!("✅ {name} répond ({} ms)", super::shown(&v["ms"])))
                } else {
                    Done::toast(format!(
                        "❌ {name} : {}",
                        v["error"].as_str().unwrap_or("échec")
                    ))
                }
            }
            "mcp.enable" | "mcp.disable" => {
                let name = str_of("name");
                let method = if op == "mcp.enable" {
                    m::MCP_ENABLE
                } else {
                    m::MCP_DISABLE
                };
                rpc.call(method, json!({"name": name})).await?;
                Done::toast(if op == "mcp.enable" {
                    format!("▶️ {name} activé")
                } else {
                    format!("⏸ {name} désactivé")
                })
            }
            "mcp.logs" => {
                let name = str_of("name");
                let v = rpc.call(m::MCP_LOGS, json!({"name": name})).await?;
                let lines: Vec<String> = v["lines"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .take(30)
                    .rev()
                    .filter_map(|l| l.as_str().map(String::from))
                    .collect();
                let note = if lines.is_empty() {
                    format!("Aucune ligne de journal pour `{name}`.")
                } else {
                    format!("```\n{}\n```", lines.join("\n").replace("```", "ʼʼʼ"))
                };
                Done {
                    toast: "📜 Journal".into(),
                    note: Some(note),
                    redraw: true,
                }
            }
            "mcp.auth" => {
                self.send_oauth_card(chat_id, topic_id, &str_of("name"))
                    .await?;
                Done {
                    toast: "🔐 Lien d'autorisation envoyé".into(),
                    note: None,
                    redraw: true,
                }
            }
            "model.assign" => {
                let (alias, model) = (str_of("alias"), str_of("model"));
                let v = rpc
                    .call(m::MODEL_SET, json!({"alias": alias, "model": model}))
                    .await?;
                Done::note(
                    format!("✅ {alias} → {}", short_model(&model)),
                    format!(
                        "✅ `{alias}` → `{model}` (génération {}).",
                        super::shown(&v["generation"])
                    ),
                )
            }
            "skill.rollback" => {
                let name = str_of("name");
                rpc.call(m::SKILL_ROLLBACK, json!({"name": name})).await?;
                Done::toast(format!("⏪ {name} : version précédente"))
            }
            "mem.forget" => {
                let vault = crate::helpers::vault_dir(s);
                match crate::vault_ops::forget(s, &vault, &str_of("uid"))
                    .await
                    .map_err(anyhow::Error::msg)?
                {
                    true => Done::toast("🗑 Oublié"),
                    false => Done::toast("Rien à oublier"),
                }
            }
            "mem.validate" => {
                let uid = str_of("uid");
                let e = s
                    .memory
                    .get(&uid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("entrée `{uid}` introuvable"))?;
                let vault = crate::helpers::vault_dir(s);
                let day = crate::vault_ops::day(s);
                crate::vault_ops::update_note(&vault, &e.file, Some(&uid), &day, |raw| {
                    penelope_memory::edit::update_annotations(raw, &uid, |a| {
                        a.revue = Some(day.clone())
                    })
                    .ok_or_else(|| format!("entrée `{uid}` absente de {}", e.file))
                })
                .map_err(anyhow::Error::msg)?;
                Done::toast("✅ Entrée validée")
            }
            "practice.status" => {
                let (slug, statut) = (str_of("slug"), str_of("statut"));
                let vault = crate::helpers::vault_dir(s);
                let rel = format!("pratiques/{}", penelope_platform::slugify(&slug)) + ".md";
                crate::vault_ops::update_note(
                    &vault,
                    &rel,
                    None,
                    &crate::vault_ops::day(s),
                    |raw| {
                        let mut practice = penelope_memory::vault::Practice::parse(raw, &slug)?;
                        practice.statut = penelope_memory::vault::PracticeStatus::parse(&statut);
                        Ok(practice.render())
                    },
                )
                .map_err(anyhow::Error::msg)?;
                Done::toast(if statut == "active" {
                    "✅ Pratique validée"
                } else {
                    "🚫 Pratique contestée"
                })
            }
            "intent.cancel" => {
                rpc.call(m::INTENT_CANCEL, json!({"id": str_of("id")}))
                    .await?;
                Done::toast("❌ Intention annulée")
            }
            "policy.revoke" => {
                rpc.call(m::POLICY_REVOKE, json!({"id": str_of("id")}))
                    .await?;
                Done::toast("🗑 Règle retirée")
            }
            "secret.rm" => {
                let name = str_of("name");
                rpc.call(m::SECRET_RM, json!({"name": name})).await?;
                Done::toast(format!("🗑 Secret {name} supprimé"))
            }
            "quiet.set" => {
                let range = str_of("range");
                rpc.call(m::QUIET, json!({"range": range})).await?;
                Done::toast(if range.is_empty() {
                    "🔔 Heures silencieuses désactivées".to_string()
                } else {
                    format!("🌙 Silence de {range}")
                })
            }
            "restart" => {
                rpc.call(m::RESTART, json!({})).await?;
                Done::quiet("🔁 Redémarrage demandé")
            }
            "session.close" => {
                let id = str_of("session");
                let v = crate::session_ops::close(d, &id)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Done {
                    toast: "🔒 Session fermée".into(),
                    note: Some(format!(
                        "🔒 Session {} fermée.{}",
                        v["title"]
                            .as_str()
                            .map(|t| format!("« {t} »"))
                            .unwrap_or_else(|| format!("`{id}`")),
                        super::cancelled_note(v["cancelled"].as_u64().unwrap_or(0) as usize)
                    )),
                    redraw: false,
                }
            }
            "session.rewind" => {
                let turns = p["turns"].as_u64().unwrap_or(1) as usize;
                let session = d.chat_session_for(&origin).await?;
                let v = crate::session_ops::rewind(d, &session, turns)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Done {
                    toast: format!("⏪ {turns} échange(s) défait(s)"),
                    note: Some(format!(
                        "⏪ {turns} échange(s) défait(s) ({} messages mis de côté dans `{}`).",
                        super::shown(&v["removed"]),
                        v["archive"].as_str().unwrap_or("?")
                    )),
                    redraw: false,
                }
            }
            "upgrade.check" => {
                let v = rpc.call(m::UPGRADE, json!({"check": true})).await;
                let args = match v {
                    Ok(v) => json!({"latest": v["latest"], "up_to_date": v["up_to_date"]}),
                    Err(e) => json!({"error": e.to_string()}),
                };
                // L'écran porte le résultat : il est redessiné ici plutôt que par le retour.
                let _ = s.kv_set("tg.upgrade.last_check", &args.to_string()).await;
                Done::toast("🔎 Vérifié")
            }
            "upgrade.keep_sources" => {
                Done::quiet("Les sources restent : `make deploy` sur la machine")
            }
            "upgrade.install" | "upgrade.rollback" | "upgrade.switch" => {
                let params = match (op, p["tag"].as_str()) {
                    ("upgrade.rollback", _) => json!({"rollback": true}),
                    ("upgrade.switch", _) => json!({"switch": true}),
                    (_, Some(tag)) => json!({"tag": tag}),
                    _ => json!({}),
                };
                let (daemon, messenger) = (d.clone(), d.hooks.messenger());
                tokio::spawn(async move {
                    let rpc = crate::rpc::Rpc::new(daemon);
                    let text = match rpc.call(m::UPGRADE, params).await {
                        Ok(v) => crate::upgrade::render(&v),
                        Err(e) => format!("❌ {e}"),
                    };
                    if let Some(m) = messenger {
                        let _ = m.send_text(&origin, &text).await;
                    }
                });
                Done::quiet(match op {
                    "upgrade.install" => "⬆️ Installation lancée",
                    "upgrade.switch" => "📦 Bascule vers les releases lancée",
                    _ => "⏪ Retour arrière lancé",
                })
            }
            "prompt.run" => {
                let (server, prompt) = (str_of("server"), str_of("prompt"));
                let sup = d
                    .hooks
                    .mcp_supervisor()
                    .ok_or_else(|| anyhow::anyhow!("aucun serveur MCP chargé"))?;
                let listed = sup
                    .prompts(&server)
                    .await
                    .map_err(anyhow::Error::msg)?
                    .into_iter()
                    .find(|x| x["name"].as_str() == Some(prompt.as_str()))
                    .ok_or_else(|| {
                        anyhow::anyhow!("prompt `{prompt}` introuvable sur `{server}`")
                    })?;
                let has_args = listed["arguments"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty());
                if has_args {
                    let state =
                        penelope_telegram::forms::FormState::new(&prompt, prompt_schema(&listed))
                            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                    let pending = json!({
                        "prompt": {"server": server, "name": prompt},
                        "choice": format!("{server} · {prompt}"),
                        "state": state,
                        "topic": topic_id,
                        "since": d.services.clock.now_rfc3339(),
                    });
                    s.kv_set(&form_key(chat_id, topic_id), &pending.to_string())
                        .await?;
                    self.send_form_step(chat_id, &pending).await?;
                    Done::quiet(format!("Arguments de {prompt}"))
                } else {
                    let n = self
                        .run_mcp_prompt(chat_id, topic_id, &server, &prompt, json!({}))
                        .await?;
                    Done::quiet(format!("💬 {n} message(s) envoyés au modèle"))
                }
            }
            "session.forget" => {
                let id = str_of("session");
                let vault = crate::helpers::vault_dir(s);
                let uids = s.memory.forget_session(&id).await?;
                for uid in &uids {
                    crate::vault_ops::forget(s, &vault, uid)
                        .await
                        .map_err(anyhow::Error::msg)?;
                }
                Done::toast(format!("🧹 {} entrée(s) oubliée(s)", uids.len()))
            }
            "notes.adopt" => {
                let copied = crate::session_notes::copy(s, &str_of("from"), &str_of("to")).await?;
                Done::quiet(if copied {
                    "📓 Notes reprises"
                } else {
                    "Aucune note à reprendre"
                })
            }
            "noop" => Done::quiet("Annulé"),
            other => Done::quiet(format!("Opération inconnue : {other}")),
        })
    }
}
