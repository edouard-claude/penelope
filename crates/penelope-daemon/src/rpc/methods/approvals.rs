//! Méthodes d'approbation (HITL) : trancher, reprendre le tour, règles.

use super::*;

impl Rpc {
    /// Approbations, heures calmes et règles.
    pub(super) async fn approvals(&self, method: &str, p: &Value) -> anyhow::Result<Value> {
        let s = self.services();
        match method {
            method::APPROVALS => Ok(serde_json::to_value(s.approvals.pending(50).await?)?),
            method::APPROVE => {
                let id = required_str(p, "id")?;
                let always = p.get("always").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut d = if always {
                    penelope_hitl::Decision::approve_always("cli")
                } else {
                    penelope_hitl::Decision::approve_once("cli")
                };
                // Effet incertain (#83) : « c'est fait » ou « relancer », à dire.
                match p.get("effect").and_then(|v| v.as_str()) {
                    Some("done") => d.choice = penelope_agent::EFFECT_DONE.into(),
                    Some("retry") => d.choice = penelope_agent::EFFECT_RETRY.into(),
                    Some(other) => anyhow::bail!("--effect {other} : attendu done ou retry"),
                    None => {}
                }
                self.decide_and_resume(&id, &d).await
            }
            method::DENY => {
                let id = required_str(p, "id")?;
                let reason = p.get("reason").and_then(|r| r.as_str()).map(String::from);
                self.decide_and_resume(&id, &penelope_hitl::Decision::deny("cli", reason))
                    .await
            }
            method::QUIET => {
                if let Some(range) = p
                    .get("range")
                    .or_else(|| p.get("arg"))
                    .and_then(|v| v.as_str())
                {
                    let r = range.to_string();
                    let g = self.daemon.publish_config("cli", move |c| {
                        c.telegram.quiet_hours = r.clone();
                        Ok(vec!["telegram.quiet_hours".into()])
                    })?;
                    Ok(json!({"quiet_hours": range, "generation": g}))
                } else {
                    Ok(json!({"quiet_hours": s.config.config().telegram.quiet_hours}))
                }
            }
            method::POLICIES => {
                // Chaque règle avec ce qui la rend inutile, s'il y a lieu (issue #111).
                let now = s.clock.now_ms();
                let rules: Vec<Value> = s
                    .policies
                    .active_rules()
                    .await?
                    .iter()
                    .map(|r| {
                        let mut v = serde_json::to_value(r).unwrap_or_default();
                        if let Some(note) = crate::approval_mode::rule_note(r, now) {
                            v["remarque"] = json!(note);
                        }
                        v
                    })
                    .collect();
                Ok(json!(rules))
            }
            method::POLICY_REVOKE => {
                let id = required_str(p, "id")?;
                Ok(json!({"revoked": s.policies.revoke(&id).await?}))
            }
            other => Err(anyhow::anyhow!("méthode inconnue : {other}")),
        }
    }

    /// Tranche une approbation et remet la suite du tour en file, sur le canal de la
    /// session : un tour né sur Telegram y répond, même approuvé depuis la CLI.
    async fn decide_and_resume(
        &self,
        id: &str,
        decision: &penelope_hitl::Decision,
    ) -> anyhow::Result<Value> {
        let s = self.services();
        if s.approvals.get(id).await?.is_none() {
            anyhow::bail!("demande {id} introuvable");
        }
        let before = s.approvals.get(id).await?.map(|a| a.state);
        crate::agent::decide_approval(s, id, decision).await?;
        let a = s
            .approvals
            .get(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("demande {id} introuvable"))?;
        if before != Some(penelope_hitl::ApprovalState::Pending) {
            anyhow::bail!(
                "demande {id} déjà tranchée : {} via {}",
                a.state.as_str(),
                a.decided_via.clone().unwrap_or_default()
            );
        }
        // Propositions de mémoire : la décision s'applique ici, aucun tour à reprendre.
        if a.kind == penelope_hitl::ApprovalKind::MemoryProposal {
            if a.state == penelope_hitl::ApprovalState::Approved {
                let written =
                    penelope_dream::ingest::apply_memory_proposal(&self.daemon.dream(), id).await?;
                let mut v = serde_json::to_value(&a)?;
                v["written"] = json!(written);
                return Ok(v);
            }
            return Ok(serde_json::to_value(a)?);
        }
        if let Some(sid) = &a.session_id {
            let origin = match s.sessions.get(sid).await? {
                Some(sess) if sess.tg_chat_id.is_some() => Origin::Telegram {
                    chat_id: sess.tg_chat_id.unwrap_or_default(),
                    topic_id: sess.tg_topic_id,
                    message_id: None,
                },
                _ => Origin::Cli,
            };
            self.daemon.enqueue_resume(sid, id, &origin).await?;
        }
        Ok(serde_json::to_value(a)?)
    }
}
