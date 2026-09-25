//! Crons système, échecs de la nuit, synchronisation et contrôle du vault.

use super::*;

// ------------------------------------------------------------------ crons système

/// Déclenche la consolidation et le digest à leurs horaires (§19 `dreaming_cron`,
/// `digest_cron`). Le premier passage mémorise l'instant sans rien lancer. `digest` :
/// qui fournit au digest ses entrées d'au-dessus du rêve, lues au moment de l'écrire.
pub async fn system_crons(
    d: &Context,
    digest: Arc<dyn DigestSource>,
    messenger: &Slot<dyn Messenger>,
    mcp: &Slot<dyn McpAdmin>,
) -> anyhow::Result<()> {
    let s = &d.services;
    let cfg = s.config.config();
    let now = s.clock.now_ms();
    for (name, expr) in [
        ("dream", cfg.memory.dreaming_cron.clone()),
        ("digest", cfg.memory.digest_cron.clone()),
    ] {
        if expr.trim().is_empty() {
            continue;
        }
        let key = format!("system.cron.{name}.last");
        let Some(last) = s.kv_get(&key).await?.and_then(|v| v.parse::<i64>().ok()) else {
            s.kv_set(&key, &now.to_string()).await?;
            continue;
        };
        let Ok(cron) = penelope_kernel::cron::Cron::parse(&expr) else {
            continue;
        };
        let Some(next) = cron.next_after_ms(last, &cfg.owner.timezone) else {
            continue;
        };
        if now < next {
            continue;
        }
        s.kv_set(&key, &now.to_string()).await?;
        let (d2, digest, messenger, mcp) =
            (d.clone(), digest.clone(), messenger.clone(), mcp.clone());
        match name {
            "dream" => {
                tokio::spawn(async move { nightly(&d2, &messenger).await });
            }
            _ => {
                tokio::spawn(async move {
                    let inputs = digest.digest_inputs().await;
                    match digest_with(&d2, inputs, mcp.get()).await {
                        Ok(text) => {
                            if let Some(m) = messenger.get() {
                                // Avis sans session : il part au foyer (`telegram.home`,
                                // issue #143), pas au chat privé (issue #145).
                                let origin = crate::bus::Origin::Internal {
                                    source: "digest".into(),
                                };
                                let _ = m.send_text(&origin, &text).await;
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "digest du matin"),
                    }
                });
            }
        }
    }
    Ok(())
}

/// Passe nocturne : une nuit ratée ne passe jamais en silence (issue #127).
pub async fn nightly(d: &Context, messenger: &Slot<dyn Messenger>) {
    match run(d, messenger, false).await {
        Ok(o) => tracing::info!(run = %o.run_id, "consolidation nocturne terminée"),
        Err(e) => {
            tracing::warn!(error = %e, "consolidation nocturne");
        }
    }
}

/// Nuits ratées d'affilée, et la raison de la dernière alerte.
pub(super) const FAILED_NIGHTS_KEY: &str = "dream.failed_nights";
pub(super) const FAILED_REASON_KEY: &str = "dream.failed_reason";

/// Une nuit sans consolidation : événement, ligne datée dans `DREAMS.md` (sinon le vault
/// laisse croire qu'il n'y avait rien à consolider), et message au propriétaire comme
/// pour la sauvegarde, à la première nuit ratée ou quand la raison change : une panne
/// qui dure ne répète pas le même message chaque nuit, le digest la rappelle.
pub async fn night_failed(d: &Context, messenger: &Slot<dyn Messenger>, reason: &str) {
    failure_reported(d, messenger, reason, false).await;
}

/// `always` : le message part même si la raison n'a pas changé — une passe lancée à la
/// main doit rendre compte à qui vient de la lancer (issue #152).
pub async fn failure_reported(
    d: &Context,
    messenger: &Slot<dyn Messenger>,
    reason: &str,
    always: bool,
) {
    let s = &d.services;
    let reason: String = reason.chars().take(300).collect();
    let nights = s
        .kv_get(FAILED_NIGHTS_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    let _ = s.kv_set(FAILED_NIGHTS_KEY, &nights.to_string()).await;
    let previous = s.kv_get(FAILED_REASON_KEY).await.ok().flatten();
    let _ = s.kv_set(FAILED_REASON_KEY, &reason).await;
    let pending = s
        .candidates
        .pending(None)
        .await
        .map(|c| c.len())
        .unwrap_or(0);
    let (run_id, stats) = last_run(s).await.unwrap_or_default();
    let wrote = stats.promoted > 0 || stats.journal_expired > 0;
    let what = if wrote {
        format!(
            "{} entrée(s) écrite(s) avant l'arrêt, gardées et marquées ; les {pending} \
             candidat(s) encore en attente repassent la nuit prochaine",
            stats.promoted
        )
    } else {
        format!(
            "rien n'a été écrit ; les {pending} candidat(s) en attente repassent la nuit prochaine"
        )
    };
    let _ = s
        .events
        .append(EventDraft::new(
            "memory.dream_failed",
            json!({"run": run_id, "error": reason, "nights": nights, "pending": pending}),
        ))
        .await;
    let vault = crate::helpers::vault_dir(s);
    let day = today(s);
    let written = crate::vault_ops::update_note(&vault, "DREAMS.md", None, &day, |raw| {
        let mut body = if raw.trim().is_empty() {
            "# Revue\n".to_string()
        } else {
            raw.to_string()
        };
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push_str(&format!(
            "\n## Rêve du {day} : échec (`{run_id}`)\n\nLa passe s'est arrêtée : {reason}. \
             {}.\n",
            capitalized(&what)
        ));
        Ok(body)
    });
    if written.is_ok() {
        let message = format!("{}{day} ({run_id}) : échec", crate::vault_git::DREAM_PREFIX);
        if let Err(e) = vault_sync(&d.services, &message).await {
            tracing::warn!(error = %e, "commit du vault après une nuit ratée");
        }
    }
    if always || nights == 1 || previous.as_deref() != Some(reason.as_str()) {
        let streak = if nights > 1 {
            format!(" ({nights} nuits de suite)")
        } else {
            String::new()
        };
        if let Some(m) = messenger.get() {
            let _ = m
                .send_text(
                    &crate::bus::Origin::Internal {
                        source: "dream".into(),
                    },
                    &format!(
                        "⚠️ La consolidation de cette nuit a échoué{streak} : {reason}. \
                         {}.",
                        capitalized(&what)
                    ),
                )
                .await;
        }
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Dernière passe lancée, quelle que soit son issue : identifiant et rapport.
pub(super) async fn last_run(s: &Services) -> anyhow::Result<(String, DreamReport)> {
    Ok(s.store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT id, COALESCE(stats, '{}') FROM dream_runs
                 ORDER BY started_at DESC, rowid DESC LIMIT 1",
            )?;
            let mut rows = st.query([])?;
            Ok(match rows.next()? {
                Some(r) => {
                    let stats: String = r.get(1)?;
                    (r.get(0)?, serde_json::from_str(&stats).unwrap_or_default())
                }
                None => (String::new(), DreamReport::default()),
            })
        })
        .await?)
}

/// La dernière passe a échoué : sa raison, pour le digest.
pub(super) async fn last_failure(s: &Services) -> Option<String> {
    s.store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT phase, COALESCE(error, '') FROM dream_runs
                 ORDER BY started_at DESC, rowid DESC LIMIT 1",
            )?;
            let mut rows = st.query([])?;
            Ok(match rows.next()? {
                Some(r) if r.get::<_, String>(0)? == "failed" => Some(r.get::<_, String>(1)?),
                _ => None,
            })
        })
        .await
        .ok()
        .flatten()
}

// ------------------------------------------------------------------ vault

/// Vérifie le vault : frontmatter, pratiques, entrées sans uid, contenu interdit.
pub async fn vault_check(s: &Services) -> Value {
    let vault = crate::helpers::vault_dir(s);
    let mut issues = Vec::new();
    let mut files = 0;
    for rel in markdown_files(&vault) {
        let Ok(raw) = std::fs::read_to_string(vault.join(&rel)) else {
            continue;
        };
        files += 1;
        if let Err(e) = penelope_kernel::frontmatter::parse(&raw) {
            issues.push(json!({"file": rel, "severity": "error", "message": e.to_string()}));
            continue;
        }
        if let Some(stem) = rel
            .strip_prefix("pratiques/")
            .and_then(|f| f.strip_suffix(".md"))
        {
            match Practice::parse(&raw, stem) {
                Ok(p) => {
                    for (e, why) in p.invalid_entries() {
                        issues.push(json!({"file": rel, "line": e.line, "severity": "warning", "message": why}));
                    }
                }
                Err(e) => issues.push(json!({"file": rel, "severity": "error", "message": e})),
            }
        }
        if rel.starts_with("sources/") {
            continue;
        }
        let (_, rewritten) = penelope_memory::vault::parse_entries(&raw);
        if rewritten.is_some() {
            issues.push(json!({"file": rel, "severity": "info", "message": "entrées sans uid : `penelope mem reindex` les numérote"}));
        }
        for (i, line) in raw.lines().enumerate() {
            if penelope_observe::contains_secret(line) {
                issues.push(json!({"file": rel, "line": i + 1, "severity": "error", "message": "secret ou numéro sensible dans le vault"}));
            }
        }
    }
    // Contenu présent mais hors de l'index : nommé, jamais silencieux (issue #15).
    let inventory = crate::vault_inventory::inventory(s).await.ok();
    if let Some(inv) = &inventory {
        for g in &inv.not_indexed {
            issues.push(json!({"file": g.path, "severity": "warning", "message": format!("hors index : {}", g.reason)}));
        }
    }
    json!({
        "files": files,
        "ok": issues.iter().all(|i| i["severity"] != "error"),
        "issues": issues,
        "inventory": inventory,
    })
}

/// Répertoire d'un fichier du vault, pour les chemins affichés.
pub fn vault_path(s: &Services, rel: &str) -> PathBuf {
    crate::helpers::vault_dir(s).join(rel)
}
