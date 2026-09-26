//! Méthodes de mémoire, de coffre, d'intentions et d'accueil.

use super::*;

impl Rpc {
    /// Mémoire, coffre, intentions et accueil.
    pub(super) async fn memory(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::MEM_SEARCH => {
                let q = required_str(p, "query")?;
                let hits = s
                    .memory
                    .search(&q, None, &penelope_memory::SearchFilter::explicit(), &[])
                    .await?;
                Ok(json!(
                    hits.iter()
                        .map(|h| json!({
                            "uid": h.entry.uid,
                            "text": h.entry.text,
                            "level": h.entry.level.as_str(),
                            "file": h.entry.file,
                            "score": h.score,
                        }))
                        .collect::<Vec<_>>()
                ))
            }
            method::MEM_SHOW => {
                let uid = required_str(p, "uid")?;
                Ok(serde_json::to_value(s.memory.get(&uid).await?)?)
            }
            method::MEM_SIGNALS => {
                let uid = required_str(p, "uid")?;
                let entry = s
                    .memory
                    .get(&uid)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("aucune entrée `{uid}`"))?;
                let sig = s.memory.signals_of(&uid).await?;
                Ok(json!({
                    "uid": uid,
                    "fichier": entry.file,
                    "texte": entry.text,
                    "rappels": sig.recalls,
                    "rappels_utiles": sig.useful_recalls,
                    "vu_sans_etre_retenu": sig.seen,
                    "succes": sig.successes,
                    "contradictions": sig.contradictions,
                    "dernier_rappel": sig.last_recall,
                    "requetes_distinctes": sig.distinct_queries.len(),
                    "facteur_usage": (penelope_memory::index::usage_factor(&sig) * 1000.0).round()
                        / 1000.0,
                }))
            }
            method::MEM_HISTORY => {
                penelope_dream::dream::history(
                    s,
                    p.get("uid").and_then(|v| v.as_str()),
                    p.get("file").and_then(|v| v.as_str()),
                )
                .await
            }
            method::MEM_RESTORE => {
                let id = p
                    .get("id")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
                    })
                    .ok_or_else(|| anyhow::anyhow!("paramètre `id` (entier) obligatoire"))?;
                penelope_dream::dream::restore(s, id).await
            }
            method::MEM_REINDEX => {
                let vault = penelope_app::helpers::vault_dir(s);
                penelope_vault::vault_ops::migrate_wiki(s, &vault)
                    .await
                    .map_err(anyhow::Error::msg)?;
                let n = penelope_vault::vault_ops::reindex(s, &vault)
                    .await
                    .map_err(anyhow::Error::msg)?;
                // `embeddings` : tous les vecteurs recalculés avec le modèle courant.
                if p.get("embeddings").and_then(|v| v.as_bool()) == Some(true) {
                    let report =
                        penelope_vault::embeddings::backfill(&self.daemon.embedder(), true).await?;
                    return Ok(json!({"entries": n, "embeddings": report}));
                }
                penelope_vault::embeddings::spawn_backfill(self.daemon.embedder());
                Ok(json!({"entries": n}))
            }
            method::MEM_RETRY_REJECTED => Ok(json!({
                "retried": s.candidates.retry_origin_rejections().await?,
            })),
            method::MEM_AUDIT => {
                let audit = penelope_vault::mem_audit::run(
                    &self.daemon.services,
                    self.daemon.hooks.mcp_supervisor(),
                )
                .await?;
                Ok(penelope_vault::mem_audit::to_json(&audit))
            }
            method::ONBOARD_NEXT | method::ONBOARD_ANSWER | method::ONBOARD_WRITE => {
                let d = &self.daemon;
                let cli = d.chat_session_for(&penelope_app::bus::Origin::Cli);
                penelope_dream::onboarding::rpc(&d.services, method, p, cli).await
            }
            method::MEM_FORGET => {
                let uid = required_str(p, "uid")?;
                let vault = penelope_app::helpers::vault_dir(s);
                let done = penelope_vault::vault_ops::forget(s, &vault, &uid)
                    .await
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"uid": uid, "forgotten": done}))
            }
            method::MEM_SPLIT => mem_split(&self.daemon, p).await,
            method::MEM_CANDIDATES => mem_candidates(s).await,
            method::MEM_DREAM => {
                let dry_run = p.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
                let outcome = penelope_dream::dream::run(
                    &self.daemon.dream(),
                    &self.daemon.hooks.messenger,
                    dry_run,
                )
                .await?;
                let mut v = serde_json::to_value(&outcome)?;
                v["text"] = json!(outcome.report.render());
                Ok(v)
            }
            method::MEM_LEARNED => {
                let days = p
                    .get("days")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|x| x.parse().ok()))
                    })
                    .unwrap_or(7);
                Ok(json!(penelope_dream::dream::learned(s, days).await?))
            }
            method::VAULT_SYNC => {
                let day = s.clock.now_rfc3339()[..10].to_string();
                penelope_dream::dream::vault_sync(&self.daemon.services, &format!("sync: {day}"))
                    .await
                    .map_err(anyhow::Error::msg)
            }
            method::VAULT_CHECK => Ok(penelope_dream::dream::vault_check(s).await),
            method::VAULT_LINT => {
                let vault = penelope_app::helpers::vault_dir(s);
                let (report, proposals) = penelope_dream::dream::wiki_review(s, &vault).await;
                let mut text = if report.is_clean() {
                    format!("✅ Wiki valide : {} note(s), aucun problème.", report.notes)
                } else {
                    format!(
                        "{} problème(s) sur {} note(s) :\n- {}",
                        report.problems(),
                        report.notes,
                        report.summary().join("\n- ")
                    )
                };
                if !proposals.is_empty() {
                    text.push_str(&format!("\nÀ trancher :\n- {}", proposals.join("\n- ")));
                }
                Ok(json!({"report": report, "proposals": proposals, "text": text}))
            }
            method::MEM_DIFF => {
                let since = p.get("since").and_then(|v| v.as_str()).unwrap_or_default();
                if !since.is_empty() && since != "dream" {
                    anyhow::bail!("`--since` n'accepte que `dream`");
                }
                penelope_vault::vault_git::diff(s, since == "dream")
                    .await
                    .map_err(anyhow::Error::msg)
            }
            method::INTENT_LIST => Ok(serde_json::to_value(s.intents.all().await?)?),
            method::INTENT_CANCEL => {
                let id = required_str(p, "id")?;
                Ok(json!({"cancelled": s.intents.cancel(&id).await?}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }
}

/// Candidats en attente, **et** questions sans réponse : elles ne repassent pas en
/// consolidation, elles doivent rester visibles (issue #145).
async fn mem_candidates(s: &Services) -> anyhow::Result<Value> {
    let mut v = s.candidates.pending(None).await?;
    v.extend(s.candidates.in_question().await?);
    Ok(serde_json::to_value(v)?)
}

/// Propose le découpage d'une entrée fourre-tout : une carte, jamais une écriture (#145).
async fn mem_split(d: &Core, p: &Value) -> anyhow::Result<Value> {
    let uid = required_str(p, "uid")?;
    let id = penelope_vault::mem_split::propose(&d.services, d.providers.as_ref(), &uid).await?;
    Ok(json!({"approval": id}))
}
