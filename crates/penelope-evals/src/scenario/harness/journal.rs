//! Le journal contre les caches, à la fin de chaque scénario (épopée #208, T12 et T13 ;
//! `design/v1/source-de-verite.md` §4.2).
//!
//! Trois contrôles sur la base que le scénario laisse, après le relevé du monde (ils ne
//! changent donc ni `expected.jsonl` ni `surface.jsonl`) :
//! 1. `history verify` : zéro divergence ;
//! 2. toutes les lignes non scellées effacées (messages, plein texte, contextes figés,
//!    nœuds LCM), puis `history reindex` : zéro divergence ;
//! 3. la refonte redonne les mêmes messages, les mêmes contextes, les mêmes résumés et le
//!    même plein texte qu'avant l'effacement, numéros de ligne compris.

use penelope_app::services::Services;
use penelope_store::rusqlite;
use serde_json::{Value, json};

/// Ce que la refonte doit redonner à l'identique.
async fn caches(s: &Services) -> anyhow::Result<Vec<Value>> {
    Ok(s.store
        .read(|c| {
            let mut out = Vec::new();
            let mut take = |sql: &str, cols: usize| -> penelope_store::Result<()> {
                let mut st = c.prepare(sql)?;
                let rows = st.query_map([], |r| {
                    (0..cols)
                        .map(|i| r.get::<_, rusqlite::types::Value>(i).map(sql_json))
                        .collect::<Result<Vec<_>, _>>()
                })?;
                for r in rows {
                    let mut r = r?;
                    // Le contenu se compare comme du JSON : le journal range les clés des
                    // arguments d'appel, la ligne V0 garde l'ordre du fournisseur.
                    if r[0] == "message"
                        && let Some(text) = r[4].as_str()
                    {
                        r[4] = serde_json::from_str(text).unwrap_or_else(|_| r[4].clone());
                    }
                    out.push(Value::Array(r));
                }
                Ok(())
            };
            take(
                "SELECT 'message', session_id, seq, role, content, tool_call_id, tool_name,
                        tokens_est, episode, eager, artifact_id, compacted, event_id, sealed,
                        source_turn_id
                 FROM messages ORDER BY session_id, seq",
                15,
            )?;
            take(
                "SELECT 'context', session_id, seq, context FROM message_context
                 ORDER BY session_id, seq",
                4,
            )?;
            // L'identifiant d'une copie de fork est tiré au hasard (V0 comme refonte) :
            // seul compte ce qu'il couvre, et s'il est prolongé.
            take(
                "SELECT 'summary', session_id, from_seq, to_seq, summary, anchors, tokens_src,
                        tokens_self, superseded_by IS NULL, event_id,
                        CASE WHEN event_id IS NULL THEN NULL ELSE id END
                 FROM lcm_nodes ORDER BY session_id, from_seq, to_seq, summary",
                11,
            )?;
            take(
                "SELECT 'fts', f.session_id, m.seq, f.content FROM messages_fts f
                 JOIN messages m ON m.id = f.msg_id ORDER BY f.session_id, m.seq",
                4,
            )?;
            Ok(out)
        })
        .await?)
}

fn sql_json(v: rusqlite::types::Value) -> Value {
    use rusqlite::types::Value as V;
    match v {
        V::Null => Value::Null,
        V::Integer(i) => json!(i),
        V::Real(f) => json!(f),
        V::Text(t) => json!(t),
        V::Blob(b) => json!(b.len()),
    }
}

/// Rend le nombre de lignes de cache que la refonte a redonnées à l'identique.
pub(super) async fn check(s: &Services, name: &str) -> anyhow::Result<usize> {
    let history = &s.context.history;
    let report = history.verify(None, None).await?;
    anyhow::ensure!(
        report.ok,
        "scénario {name} : le journal diverge des caches : {}",
        serde_json::to_string_pretty(&report.divergences)?
    );
    let before = caches(s).await?;
    s.store
        .write(|tx| {
            tx.execute(
                "DELETE FROM messages_fts WHERE msg_id IN
                    (SELECT id FROM messages WHERE sealed = 0)",
                [],
            )?;
            tx.execute("DELETE FROM messages WHERE sealed = 0", [])?;
            tx.execute("DELETE FROM message_context", [])?;
            tx.execute("DELETE FROM lcm_edges", [])?;
            tx.execute("DELETE FROM lcm_nodes", [])?;
            Ok(())
        })
        .await?;
    let rebuilt = history.reindex(None).await?;
    anyhow::ensure!(
        rebuilt.ok,
        "scénario {name} : refonte refusée : {:?}",
        rebuilt.refused
    );
    let again = history.verify(None, None).await?;
    anyhow::ensure!(
        again.ok,
        "scénario {name} : divergences après refonte : {}",
        serde_json::to_string_pretty(&again.divergences)?
    );
    let after = caches(s).await?;
    for (b, a) in before.iter().zip(&after) {
        anyhow::ensure!(
            b == a,
            "scénario {name} : la refonte ne redonne pas les caches :\n  avant {b}\n  après {a}"
        );
    }
    anyhow::ensure!(
        before.len() == after.len(),
        "scénario {name} : {} lignes de cache avant la refonte, {} après",
        before.len(),
        after.len()
    );
    Ok(before.len())
}
