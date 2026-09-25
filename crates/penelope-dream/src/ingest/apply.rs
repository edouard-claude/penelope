use super::*;

/// Tranche une contradiction (issue #145) : remplacer l'entrée en mémoire, garder les
/// deux en notant le contexte, ou ignorer le candidat. Rend la phrase à afficher.
pub async fn apply_contradiction(
    d: &Context,
    approval_id: &str,
    action: &str,
) -> anyhow::Result<String> {
    let s = &d.services;
    let Some(a) = s.approvals.get(approval_id).await? else {
        anyhow::bail!("demande {approval_id} introuvable");
    };
    let flag = format!("memory_clash.applied.{approval_id}");
    if s.kv_get(&flag).await?.is_some() {
        return Ok("ℹ️ Déjà tranché.".into());
    }
    let ids: Vec<String> = a.payload["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let proposed = a.payload["proposed"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let uid = a.payload["existing_uid"].as_str().unwrap_or_default();
    let vault = crate::helpers::vault_dir(s);
    let note = match action {
        // Ignorer : le candidat est écarté, la mémoire ne bouge pas.
        k if k.ends_with("reject") => {
            s.candidates
                .set_state(
                    &ids,
                    "rejected",
                    Some("contradiction écartée par le propriétaire"),
                )
                .await?;
            "🗑 Ignoré : la mémoire ne change pas.".to_string()
        }
        // Exception : les deux entrées cohabitent, la nouvelle porte son contexte.
        k if k.ends_with("as_exception") => {
            let quand = a.payload["quand"].as_str().unwrap_or_default().to_string();
            if quand.is_empty() {
                return Ok(
                    "✍️ Dans quel cas ? Réponds en une ligne (« chez le client X », \
                     « en astreinte »…) et je l'écris comme exception."
                        .into(),
                );
            }
            let text = format!("{proposed} (quand {quand})");
            match crate::vault_ops::remember(s, &vault, Level::Cure, &text, "dream").await {
                Ok(_) => format!("🧠 Exception notée : « {proposed} » quand {quand}."),
                Err(e) => format!("❌ {e}"),
            }
        }
        // Remplacer : l'ancienne entrée est retirée, la nouvelle prend sa place.
        _ => {
            let removed = crate::vault_ops::forget(s, &vault, uid)
                .await
                .unwrap_or(false);
            match crate::vault_ops::remember(s, &vault, Level::Cure, &proposed, "dream").await {
                Ok(_) if removed => "♻️ Remplacé : l'ancienne entrée est retirée.".to_string(),
                Ok(_) => "🧠 Écrit : l'ancienne entrée était déjà partie.".to_string(),
                Err(e) => format!("❌ {e}"),
            }
        }
    };
    if !note.starts_with("✍️") {
        s.kv_set(&flag, &note).await?;
        if !ids.is_empty() && !note.starts_with("🗑") {
            s.candidates.set_state(&ids, "promoted", None).await?;
        }
    }
    Ok(note)
}

/// Applique une proposition de mémoire approuvée : chaque fait rejoint `notes.md`, la
/// fiche source en provenance. Idempotent : une seconde application n'écrit rien.
pub async fn apply_memory_proposal(d: &Context, approval_id: &str) -> anyhow::Result<usize> {
    let s = &d.services;
    let Some(a) = s.approvals.get(approval_id).await? else {
        anyhow::bail!("demande {approval_id} introuvable");
    };
    if a.kind != ApprovalKind::MemoryProposal || a.state != ApprovalState::Approved {
        return Ok(0);
    }
    let flag = format!("memory_proposal.applied.{approval_id}");
    if s.kv_get(&flag).await?.is_some() {
        return Ok(0);
    }
    // Règles notées par l'agent et confirmées : elles deviennent celles du propriétaire et
    // entrent en mémoire à la prochaine consolidation (issue #24).
    if a.payload["confirm"].as_bool() == Some(true) {
        let ids: Vec<String> = a.payload["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let confirmed = s.candidates.confirm_by_owner(&ids).await?;
        s.kv_set(&flag, &confirmed.to_string()).await?;
        return Ok(confirmed);
    }
    let vault = crate::helpers::vault_dir(s);
    // Découpage d'une entrée fourre-tout (issue #145) : les faits prennent le niveau de
    // l'entrée d'origine, qui est retirée une fois tous écrits.
    if a.payload["split"].as_bool() == Some(true) {
        let written = apply_split(d, &a, &vault).await?;
        s.kv_set(&flag, &written.to_string()).await?;
        return Ok(written);
    }
    let source = a.payload["source"].as_str().unwrap_or_default().to_string();
    let mut written = 0;
    for item in a.payload["items"].as_array().cloned().unwrap_or_default() {
        let Some(text) = item.as_str() else { continue };
        let prov = Provenance {
            origin: Origin::Agent,
            session_kind: "ingestion".into(),
            observed_at: s.clock.now_rfc3339(),
            supersedes_uid: None,
            source_ref: Some(source.clone()),
            session_id: None,
        };
        match crate::vault_ops::remember_with(s, &vault, Level::Cure, text, prov).await {
            Ok(_) => written += 1,
            Err(e) => {
                tracing::warn!(approval = %approval_id, error = %e, "fait refusé à l'écriture")
            }
        }
    }
    s.kv_set(&flag, &written.to_string()).await?;
    s.events
        .append(EventDraft::new(
            "memory.proposal_applied",
            json!({"approval": approval_id, "source": source, "written": written}),
        ))
        .await?;
    Ok(written)
}

/// Applique un découpage accepté : chaque fait devient une entrée du niveau d'origine,
/// puis l'entrée fourre-tout est retirée. Si aucun fait ne s'écrit, l'originale reste
/// (issue #145).
async fn apply_split(
    d: &Context,
    a: &penelope_hitl::ApprovalRequest,
    vault: &Path,
) -> anyhow::Result<usize> {
    let s = &d.services;
    let uid = a.payload["uid"].as_str().unwrap_or_default().to_string();
    let level = a.payload["level"]
        .as_str()
        .and_then(Level::parse)
        .unwrap_or(Level::Cure);
    let source = a.payload["file"].as_str().unwrap_or_default().to_string();
    let mut written = 0;
    for item in a.payload["items"].as_array().cloned().unwrap_or_default() {
        let Some(text) = item.as_str() else { continue };
        let prov = Provenance {
            origin: Origin::Owner,
            session_kind: "decoupage".into(),
            observed_at: s.clock.now_rfc3339(),
            supersedes_uid: Some(uid.clone()),
            source_ref: Some(source.clone()),
            session_id: None,
        };
        match crate::vault_ops::remember_with(s, vault, level, text, prov).await {
            Ok(_) => written += 1,
            Err(e) => tracing::warn!(uid = %uid, error = %e, "fait refusé au découpage"),
        }
    }
    if written > 0
        && let Err(e) = crate::vault_ops::forget(s, vault, &uid).await
    {
        tracing::warn!(uid = %uid, error = %e, "entrée d'origine non retirée après découpage");
    }
    s.events
        .append(EventDraft::new(
            "memory.split_applied",
            json!({"approval": a.id.as_str(), "uid": uid, "written": written}),
        ))
        .await?;
    Ok(written)
}
