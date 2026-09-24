//! Digest du matin : familles de rejets, résumé de la nuit.

use super::*;

// ------------------------------------------------------------------ digest

/// Famille d'un motif de rejet : ce qui précède sa précision (« imprécis : sujet ou
/// phrase incomplets » donne « imprécis »). Les lignes du rapport ont la forme
/// `« texte » : motif` ou `opération : motif`.
pub(crate) fn rejection_family(line: &str) -> String {
    let reason = match (line.starts_with('«'), line.find("» : ")) {
        (true, Some(i)) => &line[i + "» : ".len()..],
        _ => line.split_once(" : ").map(|(_, r)| r).unwrap_or(line),
    };
    let family = reason
        .find(['(', ':', ',', ';'])
        .map(|i| &reason[..i])
        .unwrap_or(reason)
        .trim();
    if family.is_empty() {
        return "sans motif".into();
    }
    let n = family.chars().count();
    if n > 60 {
        format!("{}…", family.chars().take(59).collect::<String>())
    } else {
        family.to_string()
    }
}

/// Motifs de rejet regroupés par famille, du plus fréquent au plus rare.
pub(crate) fn rejection_families<'a>(
    lines: impl IntoIterator<Item = &'a String>,
) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for l in lines {
        *counts.entry(rejection_family(l)).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Familles montrées au digest ; les autres sont additionnées.
const DIGEST_FAMILIES: usize = 5;

/// La nuit en trois chiffres et ses motifs d'écart (issue #109). `quiet` : les rapports
/// des nuits consécutives sans rien promouvoir, la dernière comprise.
pub(crate) fn night_summary(id: &str, report: &DreamReport, quiet: &[DreamReport]) -> String {
    let mut t = format!(
        "📊 {} candidat(s) examiné(s) : {} promu(s), {} écarté(s){}.\n",
        report.candidates_seen,
        report.promoted,
        report.rejected.len(),
        if report.deferred > 0 {
            format!(", {} reporté(s)", report.deferred)
        } else {
            String::new()
        }
    );
    let families = rejection_families(&report.rejected);
    if !families.is_empty() {
        t.push_str("Motifs d'écart :\n");
        for (family, n) in families.iter().take(DIGEST_FAMILIES) {
            t.push_str(&format!("- {family} : {n}\n"));
        }
        let rest: usize = families.iter().skip(DIGEST_FAMILIES).map(|(_, n)| n).sum();
        if rest > 0 {
            t.push_str(&format!("- autres motifs : {rest}\n"));
        }
    }
    if quiet.len() >= 2 {
        let seen: u32 = quiet.iter().map(|r| r.candidates_seen).sum();
        let dominant = rejection_families(quiet.iter().flat_map(|r| r.rejected.iter()));
        t.push_str(&format!(
            "⚠️ {} nuits de suite sans rien retenir ({seen} candidat(s) examiné(s))",
            quiet.len()
        ));
        match dominant.first() {
            Some((family, n)) => t.push_str(&format!(
                ", motif dominant « {family} » ({n} fois) : un réglage à revoir plutôt \
                 qu'une fatalité.\n"
            )),
            None if seen == 0 => {
                t.push_str(" : aucun candidat noté, la relecture des échanges ne propose rien.\n")
            }
            None => t.push_str(".\n"),
        }
    }
    if report.candidates_seen > 0 || !report.rejected.is_empty() {
        t.push_str(&format!(
            "Détail : `DREAMS.md` du vault, rêve `{id}` (tri et motifs).\n"
        ));
    }
    t
}

/// Rapports des dernières passes terminées, la plus récente d'abord.
async fn recent_reports(s: &Services, n: usize) -> anyhow::Result<Vec<DreamReport>> {
    let n = n as i64;
    Ok(s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT stats FROM dream_runs WHERE phase = 'done'
                 ORDER BY started_at DESC LIMIT ?1",
            )?;
            let rows = st.query_map([n], |r| r.get::<_, String>(0))?;
            let mut v = Vec::new();
            for r in rows {
                v.push(serde_json::from_str::<DreamReport>(&r?).unwrap_or_default());
            }
            Ok(v)
        })
        .await?)
}

/// Digest du matin (§6.8 sortie, §14.5 `digest`).
/// `mcp` : le superviseur, pour l'audit du lundi.
pub async fn digest_text(
    d: &Arc<Daemon>,
    mcp: Option<Arc<dyn McpAdmin>>,
) -> anyhow::Result<String> {
    // Ce que le digest lit au-dessus du rêve (planifications, compactage), calculé ici
    // et transmis en données (T26).
    let s = &d.services;
    // Planifications dont la dernière exécution a échoué, ou n'a rien livré (#39, #120).
    let failing: Vec<String> = {
        let mut v = Vec::new();
        for sched in s.schedules.list().await.unwrap_or_default() {
            if sched.state == "active"
                && let Some(err) = &sched.last_error
            {
                v.push(format!(
                    "- {} : {err}",
                    crate::scheduler::label(d, &sched).await
                ));
            }
        }
        v
    };
    let inputs = penelope_dream::DigestInputs {
        failing_schedules: failing,
        struggling_sessions: crate::compaction::struggling_sessions(s).await,
        due_today: crate::scheduler::due_today(d).await,
    };
    digest_with(&d.services, inputs, mcp).await
}

/// Corps du digest, sans le daemon : les entrées d'au-dessus arrivent en données.
async fn digest_with(
    s: &Arc<Services>,
    inputs: penelope_dream::DigestInputs,
    mcp: Option<Arc<dyn McpAdmin>>,
) -> anyhow::Result<String> {
    let mut t = format!("☀️ **Digest du {}**\n", today(s));
    // Une nuit ratée se dit : le rapport précédent ne passe pas pour celui de la nuit.
    let failed = last_failure(s).await;
    if let Some(reason) = &failed {
        let nights = s
            .kv_get(FAILED_NIGHTS_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(1)
            .max(1);
        let pending = s
            .candidates
            .pending(None)
            .await
            .map(|c| c.len())
            .unwrap_or(0);
        t.push_str(&format!(
            "\n🧠 Pas de consolidation cette nuit{} : {}. {pending} candidat(s) en attente \
             pour la prochaine.\n",
            if nights > 1 {
                format!(" ({nights} nuits de suite)")
            } else {
                String::new()
            },
            reason.chars().take(200).collect::<String>()
        ));
    }
    match last_report(s).await?.filter(|_| failed.is_none()) {
        Some((id, finished, report)) => {
            t.push_str(&format!(
                "\n🧠 {} _(passe `{id}`, {})_\n",
                report.render_digest(),
                &finished[..16.min(finished.len())]
            ));
            let quiet: Vec<DreamReport> = recent_reports(s, 14)
                .await?
                .into_iter()
                .take_while(|r| r.promoted == 0)
                .collect();
            t.push_str(&night_summary(&id, &report, &quiet));
            if !report.reflections.is_empty() {
                t.push_str("\nRéflexions :\n");
                for r in report.reflections.iter().take(5) {
                    t.push_str(&format!("- {r}\n"));
                }
            }
        }
        None if failed.is_some() => {}
        None => t.push_str("\n🧠 Pas encore de consolidation.\n"),
    }
    let failing = inputs.failing_schedules;
    if !failing.is_empty() {
        t.push_str(&format!(
            "\n⏰ {} planification(s) en échec (`/schedules`) :\n{}\n",
            failing.len(),
            failing.into_iter().take(5).collect::<Vec<_>>().join("\n")
        ));
    }
    // Sessions qui ne se résument plus : elles coûtent plus cher à chaque tour (#131).
    let struggling = inputs.struggling_sessions;
    if !struggling.is_empty() {
        t.push_str("\n🗜 Résumé de session en échec :\n");
        for (title, n, per_turn) in struggling {
            t.push_str(&format!(
                "- « {title} » : {n} échec(s) en 24 h{}\n",
                per_turn
                    .map(|c| format!(", {c:.3} $ par tour"))
                    .unwrap_or_default()
            ));
        }
    }
    // Ce qui part aujourd'hui, et où (#124).
    let due = inputs.due_today;
    if !due.is_empty() {
        t.push_str(&format!(
            "\n🗓 Aujourd'hui :\n{}\n",
            due.into_iter().take(10).collect::<Vec<_>>().join("\n")
        ));
    }
    let pending = s.approvals.pending(100).await?;
    if !pending.is_empty() {
        t.push_str(&format!(
            "\n📋 {} demande(s) en attente : {}\n",
            pending.len(),
            match crate::helpers::deep_link(s, "approvals").await {
                Some(link) => format!("[ouvrir]({link})"),
                None => "`/approvals`".into(),
            }
        ));
    }
    let runs = s.runs.list(None, 50).await?;
    let (mut done, mut blocked, mut running) = (0, 0, 0);
    for r in &runs {
        match r.state {
            penelope_workflow::RunState::Done => done += 1,
            penelope_workflow::RunState::Blocked => blocked += 1,
            penelope_workflow::RunState::Running | penelope_workflow::RunState::Paused => {
                running += 1
            }
            _ => {}
        }
    }
    if done + blocked + running > 0 {
        t.push_str(&format!(
            "\n🔧 Runs récents : {done} terminé(s), {blocked} bloqué(s), {running} en cours{}\n",
            match (
                blocked > 0,
                crate::helpers::deep_link(s, "runs_stuck").await
            ) {
                (true, Some(link)) => format!(" · [reprendre]({link})"),
                _ => String::new(),
            }
        ));
    }
    if let Some(w) = crate::vault_git::warning(s) {
        t.push_str(&format!("\n⚠️ {w}\n"));
    }
    // Journal de la veille, à ouvrir dans le wiki (issue #29).
    let yesterday_note = (s.clock.now_utc() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    if crate::helpers::vault_dir(s)
        .join(format!("journal/{yesterday_note}.md"))
        .exists()
    {
        t.push_str(&format!("\n📓 Journal d'hier : [[{yesterday_note}]]\n"));
    }
    // Termes employés dans les sources sans définition (issue #22).
    let undefined = crate::concepts::to_define(&crate::helpers::vault_dir(s));
    if !undefined.is_empty() {
        let shown: Vec<&str> = undefined.iter().take(5).map(String::as_str).collect();
        t.push_str(&format!(
            "\n🧩 {} concept(s) à définir : {}{} (`concepts/_a-definir.md`)\n",
            undefined.len(),
            shown.join(", "),
            if undefined.len() > 5 { "…" } else { "" }
        ));
    }
    // Le lundi, l'audit de la mémoire et son écart sur la semaine (issue #23).
    if chrono::Datelike::weekday(&s.clock.now_utc()) == chrono::Weekday::Mon {
        match crate::mem_audit::run(s, mcp).await {
            Ok(audit) => {
                let delta = audit
                    .delta
                    .map(|x| {
                        format!(
                            " ({x:+} depuis le {})",
                            audit.previous_date.clone().unwrap_or_default()
                        )
                    })
                    .unwrap_or_default();
                t.push_str(&format!("\n📈 Mémoire : {}/100{delta}", audit.total));
                if let Some(link) = crate::helpers::deep_link(s, "audit").await {
                    t.push_str(&format!(" · [détail]({link})"));
                }
                if let Some(best) = crate::mem_audit::best_next(&audit) {
                    t.push_str(&format!(" · prochaine action : {}", best.next));
                }
                t.push('\n');
            }
            Err(e) => tracing::warn!(error = %e, "audit hebdomadaire de la mémoire"),
        }
    }
    let yesterday = (s.clock.now_utc() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let by_day = s.budget.report("day", None, Some(&yesterday), 2).await?;
    if let Some(row) = by_day.iter().find(|r| r.key == yesterday) {
        t.push_str(&format!("\n💶 Dépense d'hier : {:.2} $\n", row.cost_usd).replace('.', ","));
    }
    Ok(t)
}
