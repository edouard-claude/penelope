//! Contradictions posées au propriétaire, journal expiré, entrées inutilisées.

use super::*;

/// Pose la carte d'une contradiction : les deux entrées tronquées, leur fichier, et les
/// trois boutons (remplacer, exception, ignorer). Le digest n'en donne que le compte
/// (issue #145).
pub(super) async fn ask_about_clash(
    s: &Services,
    clash: &Clash,
    ids: &[String],
) -> anyhow::Result<()> {
    let file = s
        .memory
        .get(&clash.existing_uid)
        .await
        .ok()
        .flatten()
        .map(|e| e.file)
        .unwrap_or_default();
    s.approvals
        .create(
            penelope_hitl::ApprovalKind::MemoryProposal,
            "mémoire",
            penelope_kernel::risk::RiskClass::Write,
            serde_json::json!({
                "contradiction": true,
                "existing_uid": clash.existing_uid,
                "existing": short(&clash.existing),
                "proposed": clash.proposed,
                "quand": clash.quand,
                "file": file,
                "candidates": ids,
                "source": if file.is_empty() { "mémoire".into() } else { file.clone() },
                "items": [format!(
                    "Nouveau : « {} »",
                    short(&clash.proposed)
                ), format!("En mémoire : « {} »", short(&clash.existing))],
            }),
            vec!["Remplacer".into(), "Exception".into(), "Ignorer".into()],
            None,
            None,
            false,
        )
        .await?;
    Ok(())
}

/// Une carte de contradiction expirée sans réponse. Elle **ne se repose pas** le
/// lendemain : le candidat garde l'état `question` (hors de `pending`, donc hors de la
/// passe suivante) et reste listé par `penelope mem candidates` ; la question, elle, est
/// rangée dans `DREAMS.md` (issue #145).
pub async fn file_unanswered_clash(s: &Services, a: &penelope_hitl::ApprovalRequest) {
    if a.kind != penelope_hitl::ApprovalKind::MemoryProposal
        || a.payload.get("contradiction").and_then(|v| v.as_bool()) != Some(true)
    {
        return;
    }
    let text = |k: &str| {
        a.payload
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let (uid, existing, proposed) = (text("existing_uid"), text("existing"), text("proposed"));
    let day = today(s);
    let vault = crate::helpers::vault_dir(s);
    let line = format!(
        "- {day} · sans réponse · nouveau « {} » contre [[memoire#^{uid}]] « {} »\n",
        short(&proposed),
        short(&existing)
    );
    let r = crate::vault_ops::update_note(&vault, "DREAMS.md", None, &day, |raw| {
        const SECTION: &str = "## Questions sans réponse";
        let mut body = if raw.trim().is_empty() {
            "# Revue\n".to_string()
        } else {
            raw.to_string()
        };
        if !body.ends_with('\n') {
            body.push('\n');
        }
        match body.find(SECTION) {
            // Sous la section, avant ce qui suit : les questions restent groupées.
            Some(at) => {
                let start = at + SECTION.len();
                let end = body[start..]
                    .find("\n## ")
                    .map(|i| start + i + 1)
                    .unwrap_or(body.len());
                body.insert_str(end, &line);
            }
            None => body.push_str(&format!("\n{SECTION}\n\n{line}")),
        }
        Ok(body)
    });
    if let Err(e) = r {
        tracing::warn!(error = %e, "question sans réponse non rangée dans DREAMS.md");
        return;
    }
    let _ = s
        .events
        .append(EventDraft::new(
            "memory.clash_unanswered",
            json!({"approval": a.id.as_str(), "uid": uid}),
        ))
        .await;
}

/// Retraits des états passagers expirés du journal (`projets.md`, « États en cours »).
pub(super) async fn expired_journal(s: &Services, vault: &Path, day: &str) -> Vec<Operation> {
    let Ok(raw) = std::fs::read_to_string(vault.join("projets.md")) else {
        return Vec::new();
    };
    let _ = s;
    let (entries, _) = penelope_memory::vault::parse_entries(&raw);
    entries
        .into_iter()
        .filter(|e| e.section == penelope_memory::grid::JOURNAL_SECTION)
        .filter(|e| e.annotations.expire.as_deref().is_some_and(|x| x < day))
        .map(|e| Operation::RetireEntry {
            uid: e.uid,
            reason: "état passager expiré".into(),
        })
        .collect()
}

/// Entrées durables jamais rappelées depuis 60 jours (retour d'usage, issue #37). Le suivi
/// commence au premier rêve qui le connaît : rien n'est proposé avant 60 jours de mesure.
pub(super) async fn unused_entries(d: &Arc<Daemon>, day: &str) -> Vec<String> {
    const KEY: &str = "memory.usage_since";
    let since = match d.services.kv_get(KEY).await.ok().flatten() {
        Some(v) => v,
        None => {
            let _ = d.services.kv_set(KEY, day).await;
            day.to_string()
        }
    };
    let cutoff = penelope_memory::grid::unused_cutoff(day);
    if since.as_str() > cutoff.as_str() {
        return Vec::new();
    }
    let s = &d.services;
    let entries = s
        .memory
        .unrecalled_since(
            &[Level::Coeur, Level::Projet, Level::Cure],
            &cutoff,
            penelope_memory::grid::SEEN_BEFORE_RETIRE,
        )
        .await
        .unwrap_or_default();
    // Ce qui est servi d'office dans l'instantané n'a pas d'usage mesurable par entrée :
    // le proposer au retrait retirerait ce qui sert le plus (issue #62).
    let injected = crate::conversation::snapshot_uids(s, &crate::session_project::Scope::All).await;
    let vault = crate::helpers::vault_dir(s);
    let resolver = penelope_memory::wiki::Resolver::scan(&vault);
    entries
        .iter()
        .filter(|e| !injected.contains(&e.uid))
        .take(10)
        .map(|e| {
            format!(
                "« {} » ([[{}#^{}]])",
                short(&e.text),
                resolver.link_target(&e.file),
                e.uid
            )
        })
        .collect()
}
