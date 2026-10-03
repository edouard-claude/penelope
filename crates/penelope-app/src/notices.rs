//! Notes du harnais pour le prochain tour d'une session (issue #302).
//!
//! Ce qui arrive hors d'un tour (le propriétaire clique « vas-y » sur la carte d'un plan,
//! le run part) n'a pas de place dans le transcript : un message inséré hors tour
//! tomberait entre un appel d'outil en attente et son résultat. La note attend donc en
//! `kv`, part dans le contexte volatil du prochain message du propriétaire, figée avec
//! lui (`penelope_conversation::prefix::settle`), et n'est retirée qu'une fois partie.
//!
//! ```text
//!  clic « vas-y » ──► run lancé ──► notices::push(session, « Run r_plan_… lancé… »)
//!                                             │
//!  message suivant du propriétaire ──► peek ──┴──► <evenements>…</evenements> (T4, figé)
//!                                     consume(n) une fois figé
//! ```

use crate::services::Services;
use penelope_store::rusqlite::{OptionalExtension, Transaction, params};
use penelope_store::{Result, StoreError};

/// Clé `kv` des notes d'une session.
pub fn key(session: &str) -> String {
    format!("notices.{session}")
}

fn read(tx: &Transaction<'_>, key: &str) -> Result<Vec<String>> {
    let raw: Option<String> = tx
        .query_row("SELECT v FROM kv WHERE k = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(raw
        .map(|r| serde_json::from_str(&r))
        .transpose()
        .map_err(|e| StoreError::other(e.to_string()))?
        .unwrap_or_default())
}

fn write(tx: &Transaction<'_>, key: &str, notes: &[String]) -> Result<()> {
    if notes.is_empty() {
        tx.execute("DELETE FROM kv WHERE k = ?1", [key])?;
        return Ok(());
    }
    let value = serde_json::to_string(notes).map_err(|e| StoreError::other(e.to_string()))?;
    tx.execute(
        "INSERT INTO kv(k, v, ts) VALUES(?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(k) DO UPDATE SET v = excluded.v, ts = excluded.ts",
        params![key, value],
    )?;
    Ok(())
}

/// Ajoute une note en fin de liste.
pub async fn push(s: &Services, session: &str, text: &str) -> anyhow::Result<()> {
    let (k, text) = (key(session), text.to_string());
    s.store
        .write(move |tx| {
            let mut notes = read(tx, &k)?;
            notes.push(text);
            write(tx, &k, &notes)
        })
        .await?;
    Ok(())
}

/// Les notes en attente, dans l'ordre, sans les retirer.
pub async fn peek(s: &Services, session: &str) -> anyhow::Result<Vec<String>> {
    let k = key(session);
    let raw = s.kv_get(&k).await?;
    Ok(raw
        .map(|r| serde_json::from_str::<Vec<String>>(&r))
        .transpose()?
        .unwrap_or_default())
}

/// Retire les `n` premières notes : celles qui viennent de partir. Une note ajoutée
/// entre-temps reste.
pub async fn consume(s: &Services, session: &str, n: usize) -> anyhow::Result<()> {
    let k = key(session);
    s.store
        .write(move |tx| {
            let notes = read(tx, &k)?;
            let rest: Vec<String> = notes.into_iter().skip(n).collect();
            write(tx, &k, &rest)
        })
        .await?;
    Ok(())
}

/// Le bloc posé dans le contexte volatil du message qui suit.
pub fn block(notes: &[String]) -> String {
    let mut out =
        String::from("<evenements>\nDepuis ton dernier tour, hors de cette conversation :\n");
    for n in notes {
        out.push_str("- ");
        out.push_str(n.trim());
        out.push('\n');
    }
    out.push_str("</evenements>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    async fn services() -> (tempfile::TempDir, Services) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        (dir, s)
    }

    /// Les notes partent dans l'ordre et ne sont retirées que pour ce qui est parti :
    /// une note arrivée entre la lecture et le retrait attend le tour suivant.
    #[tokio::test]
    async fn notes_are_kept_in_order_and_consumed_only_once_sent() {
        let (_dir, s) = services().await;
        assert!(peek(&s, "s1").await.unwrap().is_empty());
        push(&s, "s1", "Run `r_1` lancé.").await.unwrap();
        push(&s, "s1", "Run `r_2` lancé.").await.unwrap();
        push(&s, "s2", "autre session").await.unwrap();
        let notes = peek(&s, "s1").await.unwrap();
        assert_eq!(notes, ["Run `r_1` lancé.", "Run `r_2` lancé."]);
        push(&s, "s1", "Run `r_3` lancé.").await.unwrap();
        consume(&s, "s1", notes.len()).await.unwrap();
        assert_eq!(peek(&s, "s1").await.unwrap(), ["Run `r_3` lancé."]);
        consume(&s, "s1", 1).await.unwrap();
        assert!(peek(&s, "s1").await.unwrap().is_empty());
        assert!(s.kv_get(&key("s1")).await.unwrap().is_none(), "clé retirée");
        assert_eq!(peek(&s, "s2").await.unwrap(), ["autre session"]);
    }

    #[test]
    fn the_block_lists_the_notes() {
        let b = block(&["Run `r_1` lancé.".into(), " deux ".into()]);
        assert!(b.starts_with("<evenements>\n"), "{b}");
        assert!(b.contains("\n- Run `r_1` lancé.\n- deux\n"), "{b}");
        assert!(b.ends_with("</evenements>"), "{b}");
    }
}
