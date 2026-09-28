//! Le juge d'approbation (issue #203) : son mode, et ce qu'il a fait en sept jours.

use penelope_app::services::Services;
use penelope_kernel::api::DoctorCheck;
use penelope_kernel::config::JudgeMode;

/// Ce que le juge a vu en sept jours : cartes `shell_exec`, celles qu'il a enrichies, les
/// lignes passées sans carte, ses échecs.
#[derive(Debug, Default, PartialEq)]
pub struct JudgeWeek {
    pub cards: i64,
    pub judged_cards: i64,
    pub automatic: i64,
    pub failures: i64,
}

async fn judge_week(s: &Services) -> JudgeWeek {
    let since = (s.clock.now_utc() - chrono::Duration::days(7))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    s.store
        .read(move |c| {
            let (cards, judged_cards) = c.query_row(
                "SELECT COUNT(*), COUNT(json_extract(payload, '$.judged'))
                 FROM approval_requests
                 WHERE kind = 'tool_call' AND subject = 'shell_exec' AND created_at >= ?1",
                [&since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let (automatic, failures) = c.query_row(
                "SELECT
                    COALESCE(SUM(json_extract(payload, '$.outcome') IN ('auto_read', 'regle_pouvoirs')), 0),
                    COALESCE(SUM(json_extract(payload, '$.outcome') = 'echec'), 0)
                 FROM events WHERE kind = 'approval.judged' AND ts >= ?1",
                [&since],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(JudgeWeek {
                cards,
                judged_cards,
                automatic,
                failures,
            })
        })
        .await
        .unwrap_or_default()
}

/// Le mode du juge et la part des cartes `shell_exec` qu'il a jugées sur sept jours. En
/// échec quand plus de la moitié de ses appels échouent : il ne sert alors qu'à
/// retarder les cartes.
pub async fn approval_judge_check(s: &Services) -> DoctorCheck {
    const ID: &str = "approval_judge";
    const LABEL: &str = "Juge d'approbation";
    let cfg = s.config.config();
    let mode = cfg.approval.judge;
    if mode == JudgeMode::Off {
        return DoctorCheck::ok(ID, LABEL, "mode off : aucune carte n'est jugée");
    }
    let w = judge_week(s).await;
    let share = if w.cards == 0 {
        "aucune carte `shell_exec`".to_string()
    } else {
        format!(
            "{} carte(s) `shell_exec` jugée(s) sur {} ({} %)",
            w.judged_cards,
            w.cards,
            (w.judged_cards as f64 * 100.0 / w.cards as f64).round() as i64
        )
    };
    let detail = format!(
        "mode {} (modèle de l'alias `{}`) ; 7 jours : {share}, {} ligne(s) passée(s) sans \
         carte, {} échec(s)",
        mode.as_str(),
        cfg.judge_alias(),
        w.automatic,
        w.failures
    );
    let calls = w.judged_cards + w.automatic + w.failures;
    if w.failures >= 3 && w.failures * 2 > calls {
        return DoctorCheck::fail(
            ID,
            LABEL,
            detail,
            Some(
                "le juge échoue plus d'une fois sur deux : vérifier le modèle du rôle \
                 `approval_judge` (`penelope model list`), ou `penelope config set \
                 approval.judge off`"
                    .into(),
            ),
        );
    }
    DoctorCheck::ok(ID, LABEL, detail)
}

/// Le jeu de décisions du juge (#233) : collecte active ou non, volume, plus ancien
/// échantillon, répartition des issues. Informatif : jamais en échec.
pub async fn approval_dataset_check(s: &Services) -> DoctorCheck {
    const ID: &str = "approval_dataset";
    const LABEL: &str = "Jeu de décisions du juge";
    let dataset = s.config.config().observability.dataset.clone();
    let Ok(st) = s.approvals.sample_stats().await else {
        return DoctorCheck::fail(ID, LABEL, "table `approval_samples` illisible", None);
    };
    let state = if dataset.approvals {
        "collecte active"
    } else {
        "collecte désactivée (`observability.dataset.approvals`)"
    };
    let retention = match dataset.retention_days {
        0 => "gardés sans limite".to_string(),
        d => format!("gardés {d} jours"),
    };
    if st.total == 0 {
        return DoctorCheck::ok(ID, LABEL, format!("{state} ; aucun échantillon"));
    }
    let oldest = st
        .oldest
        .as_deref()
        .map(|d| d.chars().take(10).collect::<String>())
        .unwrap_or_default();
    let outcomes = st
        .outcomes
        .iter()
        .map(|(o, n)| format!("{o} {n}"))
        .collect::<Vec<_>>()
        .join(", ");
    DoctorCheck::ok(
        ID,
        LABEL,
        format!(
            "{state} ; {} échantillon(s) depuis le {oldest}, {retention} ; issues : {outcomes} \
             (`penelope dataset export --kind approvals`)",
            st.total
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::event::EventDraft;
    use serde_json::json;
    use std::sync::Arc;

    async fn services() -> (tempfile::TempDir, Arc<Services>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, Arc::new(s))
    }

    async fn card(s: &Services, judged: serde_json::Value) {
        s.approvals
            .create(
                penelope_hitl::ApprovalKind::ToolCall,
                "shell_exec",
                penelope_kernel::risk::RiskClass::Write,
                json!({"arguments": {"command": "a; b"}, "judged": judged}),
                vec![],
                None,
                None,
                false,
            )
            .await
            .unwrap();
    }

    async fn judged(s: &Services, outcome: &str) {
        s.events
            .append(EventDraft::new(
                "approval.judged",
                json!({"outcome": outcome}),
            ))
            .await
            .unwrap();
    }

    /// `doctor` dit le mode du juge et la part des cartes jugées sur sept jours (#203).
    #[tokio::test]
    async fn doctor_says_the_mode_and_the_share_of_judged_cards() {
        let (_d, s) = services().await;
        card(&s, json!({"verdict": "sûr"})).await;
        card(&s, json!(null)).await;
        judged(&s, "carte").await;
        judged(&s, "auto_read").await;
        judged(&s, "echec").await;
        let c = approval_judge_check(&s).await;
        assert!(c.ok, "{c:?}");
        assert!(c.detail.contains("mode explain"), "{}", c.detail);
        assert!(
            c.detail
                .contains("1 carte(s) `shell_exec` jugée(s) sur 2 (50 %)"),
            "{}",
            c.detail
        );
        assert!(
            c.detail
                .contains("1 ligne(s) passée(s) sans carte, 1 échec(s)"),
            "{}",
            c.detail
        );

        // Un juge qui échoue plus d'une fois sur deux est signalé, avec la sortie.
        for _ in 0..3 {
            judged(&s, "echec").await;
        }
        let c = approval_judge_check(&s).await;
        assert!(
            !c.ok
                && c.fix
                    .as_deref()
                    .unwrap_or("")
                    .contains("approval.judge off")
        );

        s.config
            .mutate("test", |c| {
                c.approval.judge = JudgeMode::Off;
                Ok(vec!["approval.judge".into()])
            })
            .unwrap();
        let c = approval_judge_check(&s).await;
        assert!(c.ok && c.detail.contains("mode off"), "{c:?}");
    }

    /// `doctor` dit l'état de la collecte, le volume, le plus ancien et les issues (#233).
    #[tokio::test]
    async fn doctor_says_what_the_decision_dataset_holds() {
        let (_d, s) = services().await;
        let c = approval_dataset_check(&s).await;
        assert!(c.ok && c.detail.contains("collecte désactivée"), "{c:?}");
        assert!(c.detail.contains("aucun échantillon"), "{c:?}");
        s.store
            .write(|tx| {
                for (call, outcome) in [("c1", Some("auto")), ("c2", Some("auto")), ("c3", None)] {
                    tx.execute(
                        "INSERT INTO approval_samples(created_at, created_ms, session_id,
                            call_id, command_sha, input, floors, outcome)
                         VALUES('2026-09-01T10:00:00.000Z', 0, 's1', ?1, 'x', '{}', '{}', ?2)",
                        penelope_store::rusqlite::params![call, outcome],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        s.config
            .mutate("test", |c| {
                c.observability.dataset.approvals = true;
                Ok(vec!["observability.dataset.approvals".into()])
            })
            .unwrap();
        let c = approval_dataset_check(&s).await;
        assert!(c.ok, "{c:?}");
        assert!(
            c.detail.contains(
                "collecte active ; 3 échantillon(s) depuis le 2026-09-01, gardés 365 jours ; \
                 issues : auto 2, en_attente 1"
            ),
            "{}",
            c.detail
        );
    }
}
