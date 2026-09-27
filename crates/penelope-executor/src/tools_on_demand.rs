//! Outils natifs à la demande, exposés à la session qui s'en sert (issue #104).
//!
//! ```text
//!  tour N    : noyau (17) + méta-outils (3)        ← « Merci » ne paie que ça
//!              tool_search "planifier" → schedule_create
//!              tool_describe / tool_call schedule_create   ── marqué pour la session
//!  frontière : noyau + méta-outils + schedule_create        ── appel direct possible
//!  …
//!  tour N+11 : sans usage depuis 10 tours, schedule_create sort de la liste
//! ```
//!
//! La liste ne change qu'à une frontière de cache (issue #236, [`turn_tools`]) : un outil
//! découvert y entre, un outil oublié en sort, à la première pause plus longue que le
//! cache ou à la compaction suivante ; d'ici là, `tool_call` l'atteint.

use penelope_app::services::Services;
use std::collections::BTreeMap;

/// Tours sans usage après lesquels un outil découvert quitte la liste de la session.
pub const FORGET_AFTER: u32 = 10;

fn key(session_id: &str) -> String {
    format!("session.tools.{session_id}")
}

/// Lit, modifie et réécrit la table d'une session dans une seule transaction du
/// rédacteur : deux outils touchés en parallèle (#85) ne s'écrasent pas.
async fn update<R: Send + 'static>(
    s: &Services,
    session_id: &str,
    f: impl FnOnce(&mut BTreeMap<String, u32>) -> Option<R> + Send + 'static,
) -> Option<R> {
    let k = key(session_id);
    s.store
        .write(move |tx| {
            let mut map: BTreeMap<String, u32> = penelope_store::kv_get(tx, &k)?
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or_default();
            let out = f(&mut map);
            if out.is_some() {
                penelope_store::kv_set(tx, &k, &serde_json::to_string(&map).unwrap_or_default())?;
            }
            Ok(out)
        })
        .await
        .ok()
        .flatten()
}

/// Un outil à la demande vient d'être décrit ou appelé : il est exposé directement à la
/// session à partir de sa prochaine frontière ([`turn_tools`]), et son compteur
/// d'inactivité repart de zéro.
pub async fn touch(s: &Services, session_id: &str, tool: &str) {
    if session_id.is_empty() || !penelope_tools::is_on_demand(tool) {
        return;
    }
    let tool = tool.to_string();
    update(s, session_id, move |map| {
        (map.get(&tool) != Some(&0)).then(|| {
            map.insert(tool, 0);
        })
    })
    .await;
}

/// Outils à la demande à exposer au tour qui commence, triés : chaque compteur avance
/// d'un tour, ceux restés sans usage au-delà de [`FORGET_AFTER`] sortent.
pub async fn exposed_for_turn(s: &Services, session_id: &str) -> Vec<String> {
    update(s, session_id, |map| {
        if map.is_empty() {
            return None;
        }
        for n in map.values_mut() {
            *n += 1;
        }
        map.retain(|_, n| *n <= FORGET_AFTER);
        Some(map.keys().cloned().collect())
    })
    .await
    .unwrap_or_default()
}

/// Outils offerts à un tour de conversation : le noyau, les outils à la demande
/// découverts par la session (#104), les outils MCP `eager` et promus. Hors frontière
/// (`boundary` faux : cache chaud, préfixe retenu), la liste gelée au dernier passage est
/// resservie telle quelle (#236) : chez Anthropic les outils précèdent le prompt système,
/// une entrée ou une sortie casserait tout le cache. Les compteurs d'inactivité avancent
/// à chaque tour.
pub async fn turn_tools(
    s: &Services,
    session_id: &str,
    mcp: Option<&dyn penelope_app::ports::McpGateway>,
    boundary: bool,
) -> Vec<penelope_llm::ToolDef> {
    let discovered = exposed_for_turn(s, session_id).await;
    if !boundary && let Some(tools) = penelope_app::frozen_tools::frozen(s, session_id).await {
        return tools;
    }
    let mut tools = crate::executor::chat_tool_defs(&discovered);
    if let Some(m) = mcp {
        tools.extend(m.eager_tools().await);
        for t in m.promoted_tools().await {
            if !tools.iter().any(|d| d.name == t.name) {
                tools.push(t);
            }
        }
    }
    penelope_app::frozen_tools::freeze(s, session_id, &tools).await;
    tools
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

    /// #236 : un outil décrit entre deux frontières n'entre pas dans la liste ; il y
    /// entre à la frontière suivante, et un outil oublié n'en sort qu'à une frontière.
    #[tokio::test]
    async fn the_tool_list_only_moves_at_a_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap();
        let names = |tools: &[penelope_llm::ToolDef]| -> Vec<String> {
            tools.iter().map(|t| t.name.clone()).collect()
        };
        let first = turn_tools(&s, "s1", None, true).await;
        touch(&s, "s1", "schedule_create").await;
        for _ in 0..3 {
            assert_eq!(turn_tools(&s, "s1", None, false).await, first, "gelée");
        }
        let after = turn_tools(&s, "s1", None, true).await;
        assert!(names(&after).contains(&"schedule_create".to_string()));
        // Oublié après dix tours sans usage, mais resservi tant que le cache est chaud.
        for _ in 0..FORGET_AFTER {
            assert_eq!(turn_tools(&s, "s1", None, false).await, after);
        }
        assert_eq!(turn_tools(&s, "s1", None, true).await, first);
    }

    /// Des lectures d'outils à la demande dans un même lot partent en parallèle (#85) :
    /// aucune promotion ne se perd (relevé du lot s-outils).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_touches_keep_every_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        for _ in 0..5 {
            let tasks: Vec<_> = penelope_tools::ON_DEMAND
                .iter()
                .map(|tool| {
                    let s = s.clone();
                    tokio::spawn(async move { touch(&s, "s1", tool).await })
                })
                .collect();
            for t in tasks {
                t.await.unwrap();
            }
            let mut want: Vec<String> = penelope_tools::ON_DEMAND
                .iter()
                .map(|t| t.to_string())
                .collect();
            want.sort();
            assert_eq!(exposed_for_turn(&s, "s1").await, want);
            // Le tour suivant repart d'une session vierge.
            let k = key("s1");
            s.store
                .write(move |tx| {
                    tx.execute("DELETE FROM kv WHERE k = ?1", [&k])?;
                    Ok(())
                })
                .await
                .unwrap();
        }
    }
}
