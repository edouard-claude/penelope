//! Heures calmes (#296) : ce qui est proactif attend la fin de la plage.
//!
//! ```text
//! livraison proactive (alerte MCP, lien d'autorisation, relance…)
//!   ├─ propriétaire actif (réponse à son message) ───────────────────► part
//!   ├─ hors des heures calmes ──────────────────────────────────────► part
//!   └─ pendant les heures calmes ─► quiet_queue (store) ─► fin de plage ─► groupées,
//!                                                                      « 🌙 Pendant les
//!                                                                      heures calmes : »
//! ```
//!
//! La plage est celle du propriétaire (`Config::quiet_range`, clé `telegram.quiet_hours`
//! pour la configuration installée) ; la notion n'appartient pas au canal, ce module ne le
//! nomme pas. Les planifications ne passent pas par cette file : l'ordonnanceur retient
//! leur tir lui-même, avec la logique du rattrapage après veille (#228), pour qu'un
//! créneau manqué de nuit ne soit annoncé qu'une fois. La file est dans la base, pas en
//! mémoire : un redémarrage du daemon ne perd rien.

use crate::bus::Origin;
use crate::ports::Messenger;
use crate::services::Services;
use penelope_store::rusqlite::params;

/// Vrai si l'instant présent tombe dans les heures calmes du propriétaire.
pub fn is_quiet(s: &Services) -> bool {
    s.config.config().quiet_at(s.clock.now_ms())
}

/// `22:00-07:00`, si des heures calmes sont réglées.
pub fn range_text(s: &Services) -> Option<String> {
    s.config.config().quiet_range().map(|r| r.text())
}

/// Une livraison retenue.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub id: i64,
    pub created_at: String,
    pub kind: String,
    pub origin: Origin,
    pub text: String,
}

/// Retient `text` pour `origin` jusqu'à la fin des heures calmes. `kind` dit d'où il
/// vient (`mcp_notice`, `mcp_auth`…), pour les journaux.
pub async fn hold(s: &Services, kind: &str, origin: &Origin, text: &str) -> anyhow::Result<i64> {
    let row = (
        kind.to_string(),
        origin.to_value().to_string(),
        text.to_string(),
    );
    let now = s.clock.now_rfc3339();
    let id = s
        .store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO quiet_queue(created_at, kind, origin, text) VALUES(?1,?2,?3,?4)",
                params![now, row.0, row.1, row.2],
            )?;
            Ok(tx.last_insert_rowid())
        })
        .await?;
    tracing::info!(kind, id, "livraison retenue par les heures calmes");
    Ok(id)
}

/// Envoie `text` tout de suite, ou le retient si les heures calmes sont en cours.
/// Renvoie vrai s'il a été retenu.
pub async fn deliver_or_hold(
    s: &Services,
    messenger: &dyn Messenger,
    kind: &str,
    origin: &Origin,
    text: &str,
) -> Result<bool, String> {
    if is_quiet(s) {
        hold(s, kind, origin, text)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(true);
    }
    messenger.send_text(origin, text).await?;
    Ok(false)
}

/// Tout ce qui attend, dans l'ordre d'arrivée.
pub async fn held(s: &Services) -> anyhow::Result<Vec<Held>> {
    Ok(s.store
        .read(|c| {
            let mut st = c.prepare(
                "SELECT id, created_at, kind, origin, text FROM quiet_queue ORDER BY id",
            )?;
            let rows = st.query_map([], |r| {
                let origin: String = r.get(3)?;
                Ok(Held {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    kind: r.get(2)?,
                    origin: serde_json::from_str::<serde_json::Value>(&origin)
                        .map(|v| Origin::from_payload(&serde_json::json!({"origin": v})))
                        .unwrap_or(Origin::Internal {
                            source: "heures calmes".into(),
                        }),
                    text: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?)
}

/// Nombre de livraisons en attente.
pub async fn held_count(s: &Services) -> anyhow::Result<i64> {
    Ok(s.store
        .read(|c| Ok(c.query_row("SELECT count(*) FROM quiet_queue", [], |r| r.get(0))?))
        .await?)
}

/// Oublie des livraisons parties.
pub async fn forget(s: &Services, ids: &[i64]) -> anyhow::Result<()> {
    let ids = ids.to_vec();
    s.store
        .write(move |tx| {
            for id in ids {
                tx.execute("DELETE FROM quiet_queue WHERE id = ?1", [id])?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

/// En-tête des livraisons groupées à la fin de la plage.
pub const HEADER: &str = "🌙 Pendant les heures calmes :";

/// Relances d'approbation échues (§9.2, #97) : `(demande, palier 1 ou 2, conversation)`.
/// Hors heures calmes, chacune a son message et sa carte. Une relance dont l'échéance
/// tombait dans la plage a été retenue (#296) : une seule relance par conversation nomme
/// toutes les demandes, sous l'en-tête, puis leurs cartes ; jamais une rafale. Appelée
/// hors de la plage seulement.
pub async fn remind_approvals(
    s: &Services,
    m: &dyn Messenger,
    due: Vec<(penelope_hitl::ApprovalRequest, u8, Origin)>,
) {
    let cfg = s.config.config();
    let held = |a: &penelope_hitl::ApprovalRequest, stage: u8| {
        let created = chrono::DateTime::parse_from_rfc3339(&a.created_at)
            .map(|t| t.timestamp_millis())
            .unwrap_or_default();
        let due_at = created + if stage == 1 { 3_600_000 } else { 6 * 3_600_000 };
        cfg.quiet_range()
            .is_some_and(|r| r.contains_at(due_at, &cfg.owner.timezone))
    };
    let mut groups: Vec<(Origin, Vec<(penelope_hitl::ApprovalRequest, u8)>)> = Vec::new();
    for (a, stage, origin) in due {
        match groups.iter_mut().find(|(o, _)| *o == origin) {
            Some((_, g)) => g.push((a, stage)),
            None => groups.push((origin, vec![(a, stage)])),
        }
    }
    for (origin, group) in groups {
        let any_held = group.iter().any(|(a, stage)| held(a, *stage));
        let text = match group.as_slice() {
            [(a, stage)] if !any_held => {
                let since = if *stage == 1 { "1 h" } else { "6 h" };
                format!(
                    "⏰ Rappel {stage}/2 : une demande attend ta réponse depuis {since} \
                     (`{}`). Sans réponse, elle expire au bout de 24 h.",
                    a.subject
                )
            }
            _ => {
                let subjects: Vec<String> = group
                    .iter()
                    .map(|(a, _)| format!("`{}`", a.subject))
                    .collect();
                let header = if any_held {
                    format!("{HEADER} ")
                } else {
                    String::new()
                };
                let (count, verb) = match group.len() {
                    1 => ("une demande attend".to_string(), "elle expire"),
                    n => (format!("{n} demandes attendent"), "chacune expire"),
                };
                format!(
                    "{header}⏰ Rappel : {count} ta réponse ({}). Sans réponse, {verb} au \
                     bout de 24 h.",
                    subjects.join(", ")
                )
            }
        };
        if let Err(e) = m.send_text(&origin, &text).await {
            tracing::warn!(error = %e, "relance d'approbation non envoyée");
        }
        for (a, _) in &group {
            if let Err(e) = m.send_approval(&origin, a.id.as_str()).await {
                tracing::warn!(demande = %a.id.as_str(), error = %e, "rappel d'approbation");
            }
        }
    }
}

/// Heure locale lisible d'un instant RFC 3339, « 3h12 ».
pub fn local_hour(s: &Services, rfc3339: &str) -> String {
    let zone = s
        .config
        .config()
        .owner
        .timezone
        .parse::<chrono_tz::Tz>()
        .unwrap_or(chrono_tz::Tz::UTC);
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| t.with_timezone(&zone).format("%-Hh%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RecordingMessenger;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    async fn services(quiet: &str) -> (tempfile::TempDir, Arc<Services>, Arc<TestClock>) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let quiet = quiet.to_string();
        s.publish_config("test", move |c| {
            c.owner.telegram_user_id = 42;
            c.telegram.quiet_hours = quiet;
            Ok(vec!["telegram.quiet_hours".into()])
        })
        .unwrap();
        (dir, s, clock)
    }

    /// Un message né pendant la plage attend dans la base ; hors plage, il part.
    #[tokio::test]
    async fn a_message_waits_in_the_store_during_quiet_hours_and_leaves_otherwise() {
        // TestClock : 2026-01-01T00:00Z = 04:00 à La Réunion, dans 22:00-07:00.
        let (_dir, s, clock) = services("22:00-07:00").await;
        let rec = RecordingMessenger::new();
        let origin = crate::helpers::owner_origin_of(&s);
        assert!(is_quiet(&s));
        assert_eq!(range_text(&s).as_deref(), Some("22:00-07:00"));
        assert!(
            deliver_or_hold(&s, rec.as_ref(), "mcp_notice", &origin, "🔧 outil changé")
                .await
                .unwrap()
        );
        assert!(rec.texts().is_empty());
        let waiting = held(&s).await.unwrap();
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].kind, "mcp_notice");
        assert_eq!(waiting[0].origin, origin);
        assert_eq!(waiting[0].text, "🔧 outil changé");
        assert_eq!(held_count(&s).await.unwrap(), 1);
        assert_eq!(local_hour(&s, &waiting[0].created_at), "4h00");

        // 07:00 : la plage est finie, un nouveau message part tout de suite.
        clock.advance_ms(3 * 3_600_000);
        assert!(!is_quiet(&s));
        assert!(
            !deliver_or_hold(&s, rec.as_ref(), "mcp_notice", &origin, "🔧 autre")
                .await
                .unwrap()
        );
        assert_eq!(rec.texts(), vec!["🔧 autre".to_string()]);

        forget(&s, &[waiting[0].id]).await.unwrap();
        assert!(held(&s).await.unwrap().is_empty());
    }

    /// Sans plage réglée, rien n'est jamais retenu.
    #[tokio::test]
    async fn without_quiet_hours_nothing_is_held() {
        let (_dir, s, _clock) = services("").await;
        assert!(!is_quiet(&s));
        assert_eq!(range_text(&s), None);
    }

    /// Relances échues pendant la plage : une seule par conversation, sous l'en-tête, qui
    /// nomme les demandes, puis leurs cartes ; une relance échue de jour garde « Rappel
    /// 1/2 » ; deux demandes de jour dans la même conversation sont groupées sans en-tête.
    #[tokio::test]
    async fn held_reminders_leave_once_per_conversation_under_the_header() {
        // Demandes créées à 4h00 : leur T+1 h, 5h00, tombe dans 22:00-07:00.
        let (_dir, s, clock) = services("22:00-07:00").await;
        let rec = RecordingMessenger::new();
        let ask = |subject: &'static str| {
            let s = s.clone();
            async move {
                s.approvals
                    .create(
                        penelope_hitl::ApprovalKind::ToolCall,
                        subject,
                        penelope_kernel::risk::RiskClass::Write,
                        serde_json::json!({}),
                        vec![],
                        None,
                        None,
                        false,
                    )
                    .await
                    .unwrap()
            }
        };
        let (a, b, c) = (
            ask("shell_exec").await,
            ask("fs_write").await,
            ask("http").await,
        );
        let home = crate::helpers::owner_origin_of(&s);
        let topic = Origin::Telegram {
            chat_id: -100,
            topic_id: Some(7),
            message_id: None,
        };
        remind_approvals(
            &s,
            rec.as_ref(),
            vec![
                (a.clone(), 1, home.clone()),
                (b.clone(), 1, home.clone()),
                (c.clone(), 1, topic.clone()),
            ],
        )
        .await;
        assert_eq!(
            rec.sent(),
            vec![
                (
                    home.clone(),
                    "🌙 Pendant les heures calmes : ⏰ Rappel : 2 demandes attendent ta réponse \
                     (`shell_exec`, `fs_write`). Sans réponse, chacune expire au bout de 24 h."
                        .to_string()
                ),
                (
                    topic,
                    "🌙 Pendant les heures calmes : ⏰ Rappel : une demande attend ta réponse \
                     (`http`). Sans réponse, elle expire au bout de 24 h."
                        .to_string()
                ),
            ]
        );
        assert_eq!(
            rec.approvals(),
            vec![a.id.0.clone(), b.id.0.clone(), c.id.0.clone()]
        );

        // De jour (demandes créées à 9h00) : le rappel ordinaire, et un groupe sans en-tête.
        clock.advance_ms(5 * 3_600_000);
        let d = ask("mem_forget").await;
        let e = ask("fs_delete").await;
        let rec = RecordingMessenger::new();
        remind_approvals(&s, rec.as_ref(), vec![(d.clone(), 1, home.clone())]).await;
        remind_approvals(
            &s,
            rec.as_ref(),
            vec![(d, 1, home.clone()), (e, 2, home.clone())],
        )
        .await;
        let texts = rec.texts();
        assert_eq!(
            texts[0],
            "⏰ Rappel 1/2 : une demande attend ta réponse depuis 1 h (`mem_forget`). Sans \
             réponse, elle expire au bout de 24 h."
        );
        assert_eq!(
            texts[1],
            "⏰ Rappel : 2 demandes attendent ta réponse (`mem_forget`, `fs_delete`). Sans \
             réponse, chacune expire au bout de 24 h."
        );
    }
}
