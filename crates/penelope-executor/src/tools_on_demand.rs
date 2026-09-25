//! Outils natifs à la demande, exposés à la session qui s'en sert (issue #104).
//!
//! ```text
//!  tour N    : noyau (17) + méta-outils (3)        ← « Merci » ne paie que ça
//!              tool_search "planifier" → schedule_create
//!              tool_describe / tool_call schedule_create   ── marqué pour la session
//!  tour N+1  : noyau + méta-outils + schedule_create        ── appel direct possible
//!  …
//!  tour N+11 : sans usage depuis 10 tours, schedule_create sort de la liste
//! ```

use penelope_app::services::Services;
use std::collections::BTreeMap;

/// Tours sans usage après lesquels un outil découvert quitte la liste de la session.
pub const FORGET_AFTER: u32 = 10;

fn key(session_id: &str) -> String {
    format!("session.tools.{session_id}")
}

async fn load(s: &Services, session_id: &str) -> BTreeMap<String, u32> {
    let k = key(session_id);
    s.store
        .read(move |c| penelope_store::kv_get(c, &k))
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

async fn save(s: &Services, session_id: &str, map: &BTreeMap<String, u32>) {
    let (k, v) = (
        key(session_id),
        serde_json::to_string(map).unwrap_or_default(),
    );
    let _ = s
        .store
        .write(move |tx| penelope_store::kv_set(tx, &k, &v))
        .await;
}

/// Un outil à la demande vient d'être décrit ou appelé : il est exposé directement aux
/// tours suivants de la session, et son compteur d'inactivité repart de zéro.
pub async fn touch(s: &Services, session_id: &str, tool: &str) {
    if session_id.is_empty() || !penelope_tools::is_on_demand(tool) {
        return;
    }
    let mut map = load(s, session_id).await;
    if map.get(tool) == Some(&0) {
        return;
    }
    map.insert(tool.to_string(), 0);
    save(s, session_id, &map).await;
}

/// Outils à la demande à exposer au tour qui commence, triés : chaque compteur avance
/// d'un tour, ceux restés sans usage au-delà de [`FORGET_AFTER`] sortent.
pub async fn exposed_for_turn(s: &Services, session_id: &str) -> Vec<String> {
    let mut map = load(s, session_id).await;
    if map.is_empty() {
        return Vec::new();
    }
    for n in map.values_mut() {
        *n += 1;
    }
    map.retain(|_, n| *n <= FORGET_AFTER);
    save(s, session_id, &map).await;
    map.into_keys().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use penelope_kernel::clock::TestClock;
    use std::sync::Arc;

    /// Un outil découvert reste exposé [`FORGET_AFTER`] tours sans usage, puis sort ; un
    /// usage remet son compteur à zéro. Les outils du noyau et les sessions anonymes ne
    /// sont pas suivis.
    #[tokio::test]
    async fn a_discovered_tool_is_forgotten_after_idle_turns() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        touch(&s, "s1", "shell_exec").await;
        touch(&s, "", "schedule_create").await;
        assert!(exposed_for_turn(&s, "s1").await.is_empty());
        assert!(exposed_for_turn(&s, "").await.is_empty());

        touch(&s, "s1", "schedule_create").await;
        for turn in 1..=FORGET_AFTER {
            let list = exposed_for_turn(&s, "s1").await;
            assert!(
                list.contains(&"schedule_create".into()),
                "tour {turn} : {list:?}"
            );
            if turn == 5 {
                touch(&s, "s1", "git_status").await;
            }
        }
        // Tour 11 : `schedule_create` sort, `git_status` (touché au tour 5) reste.
        assert_eq!(exposed_for_turn(&s, "s1").await, ["git_status"]);
        assert!(exposed_for_turn(&s, "s2").await.is_empty(), "par session");
    }
}
