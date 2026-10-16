//! `penelope usage --by hook` (#385) : le temps passé dans les hooks, par hook, lu dans
//! le journal (`hook.ran`). Sans lui, un hook lent se lisait comme un modèle lent.

use penelope_app::services::Services;
use serde_json::{Value, json};

/// Une ligne par hook : exécutions, temps cumulé, dernière exécution ; filtrable par
/// session et par date (`AAAA-MM-JJ`).
pub async fn report(
    s: &Services,
    session: Option<&str>,
    since: Option<&str>,
    limit: i64,
) -> anyhow::Result<Value> {
    let (session, since) = (session.map(String::from), since.map(String::from));
    let rows = s
        .store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT json_extract(payload, '$.name') AS name,
                        json_extract(payload, '$.event') AS event,
                        COUNT(*), COALESCE(SUM(json_extract(payload, '$.ms')), 0), MAX(ts)
                 FROM events
                 WHERE kind = 'hook.ran'
                   AND (?1 IS NULL OR session_id = ?1)
                   AND (?2 IS NULL OR ts >= ?2)
                 GROUP BY name, event
                 ORDER BY 4 DESC
                 LIMIT ?3",
            )?;
            let rows = st.query_map(
                penelope_store::rusqlite::params![session, since, limit],
                |r| {
                    Ok(json!({
                        "key": r.get::<_, Option<String>>(0)?,
                        "label": r.get::<_, Option<String>>(1)?,
                        "calls": r.get::<_, i64>(2)?,
                        "durationMs": r.get::<_, i64>(3)?,
                        "last": r.get::<_, Option<String>>(4)?,
                    }))
                },
            )?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .await?;
    Ok(Value::Array(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn the_time_spent_in_hooks_is_summed_per_hook() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        for (name, ms) in [("fmt", 1200), ("fmt", 800), ("garde", 30)] {
            s.events
                .append(
                    penelope_kernel::event::EventDraft::new(
                        "hook.ran",
                        json!({"name": name, "event": "post_tool", "verdict": "continue", "ms": ms}),
                    )
                    .session("s1"),
                )
                .await
                .unwrap();
        }
        let rows = report(&s, None, None, 20).await.unwrap();
        assert_eq!(rows[0]["key"], "fmt");
        assert_eq!(rows[0]["calls"], 2);
        assert_eq!(rows[0]["durationMs"], 2000);
        assert_eq!(rows[1]["key"], "garde");
        assert!(
            report(&s, Some("autre"), None, 20)
                .await
                .unwrap()
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
