//! Mémoire, intentions et historique.

use super::*;

impl NativeToolExecutor {
    /// Mémoire.
    pub(super) async fn memory_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        let v = match name {
            "mem_search" => {
                let filter = penelope_memory::SearchFilter {
                    level: args
                        .get("level")
                        .and_then(|v| v.as_str())
                        .and_then(penelope_memory::Level::parse),
                    projet: args
                        .get("projet")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    include_episodic: b_arg(args, "include_episodic").unwrap_or(false),
                    // Recherche explicite : les documents ingérés en font partie, encadrés.
                    include_untrusted: true,
                    slug: args.get("slug").and_then(|v| v.as_str()).map(String::from),
                    limit: u_arg(args, "limit").unwrap_or(10),
                    ..Default::default()
                };
                let query = str_arg(args, "query")?;
                let vector = match &self.orchestrator {
                    Some(o) => o.embed_query(&query).await,
                    None => None,
                };
                let hits = s.memory.search(&query, vector, &filter, &[]).await?;
                // Rien trouvé : le périmètre et ce qui en sort, jamais un silence (issue #15).
                if hits.is_empty() {
                    return Ok(ToolOutcome::ok(
                        penelope_vault::vault_inventory::empty_search_note(s).await,
                    ));
                }
                // Retour d'usage (issues #37 et #105) : trouvé par une recherche, donc
                // servi ; utile si la réponse d'une conversation le reprend.
                let in_chat = matches!(
                    s.sessions.get(&self.env.session_id).await,
                    Ok(Some(ref x)) if x.kind == penelope_kernel::session::SessionKind::Chat
                );
                let uids: Vec<String> = hits.iter().map(|h| h.entry.uid.clone()).collect();
                penelope_vault::usage_feedback::served(
                    s,
                    &self.env.session_id,
                    in_chat,
                    &uids,
                    &query,
                )
                .await;
                let mut out = Vec::new();
                for h in &hits {
                    let untrusted = h.entry.etype == penelope_memory::ingest::SOURCE_ETYPE
                        && s.memory.origin_of(&h.entry.uid).await?
                            != Some(penelope_memory::Origin::Owner);
                    let texte = if untrusted {
                        penelope_memory::provenance::frame_untrusted(&h.entry.text, &h.entry.file)
                    } else {
                        h.entry.text.clone()
                    };
                    out.push(json!({
                        "uid": h.entry.uid, "texte": texte,
                        "niveau": h.entry.level.as_str(), "fichier": h.entry.file,
                        "type": h.entry.etype,
                        "score": (h.score * 1000.0).round() / 1000.0,
                    }));
                }
                json!(out)
            }
            "mem_neighbors" => penelope_vault::concepts::neighbors(s, &str_arg(args, "slug")?)
                .await
                .map_err(|e| ToolError::Other(e.to_string()))?,
            "mem_get" => {
                let mut entries = match args.get("uid").and_then(|v| v.as_str()) {
                    Some(uid) => s.memory.get(uid).await?.into_iter().collect(),
                    None => s.memory.by_slug(&str_arg(args, "slug")?).await?,
                };
                // Un document ingéré se lit par passages, encadrés s'ils ne sont pas fiables.
                const MAX_PASSAGES: usize = 40;
                let total = entries.len();
                entries.truncate(MAX_PASSAGES);
                for e in entries.iter_mut() {
                    if e.etype == penelope_memory::ingest::SOURCE_ETYPE
                        && s.memory.origin_of(&e.uid).await? != Some(penelope_memory::Origin::Owner)
                    {
                        e.text = penelope_memory::provenance::frame_untrusted(&e.text, &e.file);
                    }
                }
                if args.get("uid").is_some() {
                    serde_json::to_value(entries.into_iter().next()).unwrap_or_default()
                } else {
                    json!({
                        "entrees": entries,
                        "total": total,
                        "tronque": total > MAX_PASSAGES,
                    })
                }
            }
            "mem_note" => {
                let ctype = penelope_memory::CandidateType::parse(&str_arg(args, "type")?)
                    .ok_or_else(|| ToolError::Invalid("type inconnu".into()))?;
                // Un secret part dans le magasin, la note n'en garde que la référence
                // (issue #37).
                let (texte, _) = penelope_vault::secret_shelf::shelve(s, &str_arg(args, "texte")?)
                    .map_err(ToolError::Denied)?;
                penelope_vault::vault_ops::write_filter(&texte).map_err(ToolError::Denied)?;
                // Une règle dictée par le propriétaire compte comme la sienne, à condition
                // d'en citer l'extrait mot pour mot (issue #24).
                let citation = args.get("citation").and_then(|v| v.as_str());
                let from_owner = match citation {
                    Some(c) => self.cited_by_owner(c).await?,
                    None => false,
                };
                let origin = if from_owner {
                    penelope_memory::Origin::Owner
                } else {
                    penelope_memory::Origin::Agent
                };
                let mut c = penelope_memory::Candidate::new(
                    ctype,
                    &texte,
                    origin,
                    "interactive",
                    &s.clock.now_rfc3339(),
                )
                .in_session(&self.env.session_id);
                if let Some(i) = u_arg(args, "importance") {
                    c = c.with_importance(i.clamp(1, 10) as u8);
                }
                if let Some(q) = args.get("quand").and_then(|v| v.as_str()) {
                    let w = penelope_memory::When::parse(q).map_err(ToolError::Invalid)?;
                    c = c.with_when(w);
                }
                let n = s
                    .candidates
                    .record(vec![c], cfg.memory.review_max_candidates.max(1))
                    .await?;
                json!({
                    "noted": n == 1,
                    "origine": origin.as_str(),
                    "remarque": match (citation.is_some(), from_owner) {
                        (_, true) => "citation vérifiée : consolidé comme une règle du propriétaire au prochain rêve",
                        (true, false) => "citation introuvable mot pour mot dans les messages du propriétaire de ce tour : la règle lui sera demandée",
                        (false, false) => "consolidé lors du prochain rêve",
                    },
                })
            }
            "mem_remember" => {
                let level = match str_arg(args, "niveau")?.as_str() {
                    "profil" => penelope_memory::Level::Profil,
                    "coeur" => penelope_memory::Level::Coeur,
                    "projet" => penelope_memory::Level::Projet,
                    _ => penelope_memory::Level::Cure,
                };
                let vault = penelope_app::helpers::vault_dir(s);
                let uid = penelope_vault::vault_ops::remember(
                    s,
                    &vault,
                    level,
                    &str_arg(args, "texte")?,
                    &self.env.session_id,
                )
                .await
                .map_err(ToolError::Denied)?;
                json!({"uid": uid, "niveau": level.as_str()})
            }
            "mem_forget" => {
                let vault = penelope_app::helpers::vault_dir(s);
                let done = penelope_vault::vault_ops::forget(s, &vault, &str_arg(args, "uid")?)
                    .await
                    .map_err(ToolError::Io)?;
                json!({"forgotten": done})
            }
            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Intentions.
    pub(super) async fn intent_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;
        let cfg = s.config.config();

        let v = match name {
            "intent_create" => {
                let texte = str_arg(args, "texte")?;
                if let penelope_memory::intents::IntentKind::Temporal(when) =
                    penelope_memory::intents::classify_intent(&texte)
                {
                    return Err(ToolError::Invalid(format!(
                        "intention datée (« {when} ») : c'est un rappel, pas une intention. \
                         Utiliser `schedule_create` avec kind `cron`, spec `{{\"expr\": \"<minute> \
                         <heure> <jour> <mois> *\", \"once\": true}}` (sans `once` s'il se répète) \
                         et target `{{\"type\": \"notify\", \"template\": \"⏰ …\"}}` ; `time_now` \
                         donne la date du jour"
                    )));
                }
                let mut triggers: Vec<String> = args
                    .get("declencheurs")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if triggers.is_empty() {
                    triggers = penelope_memory::intents::extract_triggers(&texte);
                }
                let ms = |d: &str| {
                    penelope_kernel::config::parse_duration(d)
                        .map(|x| x.as_millis() as i64)
                        .ok()
                };
                let i = s
                    .intents
                    .create(
                        &texte,
                        triggers,
                        None,
                        ms(&cfg.memory.intents.cooldown).unwrap_or(86_400_000),
                        cfg.memory.intents.fire_budget,
                        ms(&cfg.memory.intents.expiry),
                    )
                    .await?;
                serde_json::to_value(i).unwrap_or_default()
            }
            "intent_list" => serde_json::to_value(s.intents.all().await?).unwrap_or_default(),
            "intent_cancel" => json!({"cancelled": s.intents.cancel(&str_arg(args, "id")?).await?}),

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }

    /// Historique.
    pub(super) async fn history_tools(
        &self,
        name: &str,
        args: &Value,
        _cancel: &penelope_llm::CancelToken,
    ) -> ToolResult<ToolOutcome> {
        let s = &self.services;

        let v = match name {
            "history_grep" => {
                let scope = args
                    .get("scope")
                    .and_then(|v| v.as_str())
                    .unwrap_or("session");
                let session = (scope != "all").then_some(self.env.session_id.as_str());
                let hits = s
                    .context
                    .history
                    .grep(&str_arg(args, "query")?, session, 20)
                    .await?;
                let mut hits = serde_json::to_value(hits).unwrap_or_default();
                with_session_labels(s, &mut hits).await;
                if scope != "all" && hits.as_array().is_some_and(|h| h.is_empty()) {
                    json!({
                        "extraits": hits,
                        "note": "rien dans cette session : `scope: \"all\"` cherche dans les \
                                 sessions précédentes",
                    })
                } else {
                    hits
                }
            }
            "history_describe" => {
                let m = s.context.lcm.describe(&str_arg(args, "node_id")?).await?;
                serde_json::to_value(m).unwrap_or_default()
            }
            "history_expand" => {
                let node = s
                    .context
                    .lcm
                    .get(&str_arg(args, "node_id")?)
                    .await?
                    .ok_or_else(|| ToolError::Invalid("nœud introuvable".into()))?;
                let (from, to) = (node.from_seq.unwrap_or(0), node.to_seq.unwrap_or(i64::MAX));
                let page = u_arg(args, "page").unwrap_or(0);
                let entries = s.context.history.load(&node.session_id, from).await?;
                let msgs: Vec<Value> = entries
                    .iter()
                    .filter(|e| e.seq <= to)
                    .skip(page * 20)
                    .take(20)
                    .map(|e| json!({"seq": e.seq, "role": e.message.role.as_str(), "texte": e.message.text()}))
                    .collect();
                json!({"node": node.id, "page": page, "messages": msgs})
            }
            "history_expand_query" => {
                let q = str_arg(args, "question")?;
                let terms = penelope_context::store::significant_terms(&q);
                let ranked = s.context.history.grep_terms(&terms, None, 20, 30).await?;
                let mut hits = serde_json::to_value(ranked).unwrap_or_default();
                with_session_labels(s, &mut hits).await;
                json!({"question": q, "mots": terms, "extraits": hits})
            }
            "artifact_read" => {
                let cursor = u_arg(args, "cursor").unwrap_or(0) as u64;
                let v = s
                    .context
                    .history
                    .read_artifact(&str_arg(args, "id")?, cursor, 16_000)
                    .await?;
                serde_json::to_value(v).unwrap_or_default()
            }

            other => return Err(ToolError::Unknown(other.to_string())),
        };
        Ok(ToolOutcome::ok(v))
    }
}
