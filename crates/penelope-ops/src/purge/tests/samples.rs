//! Le jeu de décisions du juge (#233) : sa rétention est la sienne, la purge d'une
//! session l'emporte.

use super::*;

async fn sample(s: &Services, session: &str, call: &str) {
    let (sid, cid, now) = (session.to_string(), call.to_string(), s.clock.now_rfc3339());
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO approval_samples(created_at, created_ms, session_id, call_id,
                    command_sha, input, floors)
                 VALUES(?1, 0, ?2, ?3, 'x', '{\"command\":\"ls; pwd\"}', '{}')",
                params![now, sid, cid],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn calls(s: &Services) -> Vec<String> {
    s.store
        .read(|c| {
            let mut st = c.prepare("SELECT call_id FROM approval_samples ORDER BY call_id")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap()
}

/// `session.purge` supprime les échantillons de la session, pas ceux des autres.
#[tokio::test]
async fn purging_a_session_takes_its_samples() {
    let (_dir, s, _clock) = services().await;
    let (a, b) = (chat(&s).await, chat(&s).await);
    sample(&s, &a, "c1").await;
    sample(&s, &b, "c2").await;
    let report = session(&s, &a, "test").await.unwrap();
    assert_eq!(report["tables"]["approval_samples"], 1, "{report}");
    assert_eq!(calls(&s).await, vec!["c2".to_string()]);
}

/// La rétention de `retention.days` ne touche pas les échantillons ; celle de
/// `observability.dataset.retention_days` si, et `0` les garde tous.
#[tokio::test]
async fn samples_follow_their_own_retention() {
    let (_dir, s, clock) = services().await;
    let sid = chat(&s).await;
    sample(&s, &sid, "c_ancien").await;
    clock.advance_days(91);
    sample(&s, &sid, "c_recent").await;

    let report = retention(&s).await.unwrap();
    assert_eq!(report["approval_samples"], 0, "{report}");
    assert_eq!(calls(&s).await.len(), 2, "90 jours ne suffisent pas");

    s.config
        .mutate("test", |c| {
            c.observability.dataset.retention_days = 0;
            Ok(vec!["observability.dataset.retention_days".into()])
        })
        .unwrap();
    clock.advance_days(400);
    assert_eq!(retention(&s).await.unwrap()["approval_samples"], 0);
    assert_eq!(calls(&s).await.len(), 2, "0 : rien n'est effacé");

    s.config
        .mutate("test", |c| {
            c.observability.dataset.retention_days = 365;
            Ok(vec!["observability.dataset.retention_days".into()])
        })
        .unwrap();
    // Jour 492 : l'ancien a 492 jours, le récent 400.
    sample(&s, &sid, "c_neuf").await;
    let report = retention(&s).await.unwrap();
    assert_eq!(report["approval_samples"], 2, "{report}");
    assert_eq!(calls(&s).await, vec!["c_neuf".to_string()]);
}
