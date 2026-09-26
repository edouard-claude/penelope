//! Cache de prompt (§5, issue #17) : empreinte de chaque requête de conversation et cause
//! probable d'un raté, comparée à l'appel précédent de la session. `penelope usage --by
//! miss` en fait le bilan ; le fournisseur amont qui a servi reste épinglé tant que la
//! session est active.
//!
//! Les parties pures (empreinte, fournisseur collant, cause d'un raté) vivent dans
//! `penelope_llm::cache`, la lecture du dernier appel dans le `BudgetLedger` du kernel
//! (épopée #208, T27) ; ici, le contexte volatil et le préfixe stable.

use penelope_llm::cache::{CACHE_TTL_MS, PreviousCall};

/// Le dernier appel de conversation de la session (voir `BudgetLedger::previous_call`).
pub async fn previous_call(
    s: &penelope_app::services::Services,
    session_id: &str,
) -> anyhow::Result<Option<PreviousCall>> {
    Ok(s.budget.previous_call(session_id).await?)
}

/// Bloc de contexte volatil, tel qu'il précède le texte d'un message utilisateur.
pub fn context_block(volatile: &str) -> String {
    format!("<contexte>\n{}\n</contexte>\n\n", volatile.trim())
}

/// Fige le contexte volatil (T4) avec le dernier message utilisateur de la session, avant
/// son premier envoi : il l'accompagnera dans toutes les requêtes suivantes, reprises
/// après approbation et tours suivants compris, au lieu de se déplacer à chaque tour et
/// de réécrire l'historique.
pub async fn freeze_volatile(
    s: &penelope_app::services::Services,
    session_id: &str,
    tiers: &mut penelope_context::Tiers,
) -> anyhow::Result<()> {
    let sid = session_id.to_string();
    let last_user: Option<i64> = s
        .store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT MAX(seq) FROM messages WHERE session_id = ?1 AND role = 'user' AND sealed IS NOT 2",
                [sid],
                |r| r.get(0),
            )?)
        })
        .await?;
    let Some(seq) = last_user else {
        return Ok(());
    };
    if !tiers.volatile.trim().is_empty() {
        s.context
            .history
            .freeze_context(session_id, seq, &context_block(&tiers.volatile))
            .await?;
    }
    tiers.volatile.clear();
    Ok(())
}

/// Préfixe stable (T0 à T2) : tant que le cache de la session est chaud, un préfixe
/// modifié (nouvel instantané mémoire, skill ou serveur MCP) attend la prochaine pause ou
/// la prochaine compaction, qui le cassent de toute façon. Le préfixe retenu est celui du
/// dernier `conv.system` de la session (`HistoryStore::retained_prefix`, T16).
pub async fn stable_prefix(
    s: &penelope_app::services::Services,
    session_id: &str,
    tiers: &mut penelope_context::Tiers,
) -> anyhow::Result<()> {
    let warm = previous_call(s, session_id)
        .await?
        .is_some_and(|p| s.clock.now_ms() - p.ts_ms < CACHE_TTL_MS);
    if warm
        && let Some(stored) = s.context.history.retained_prefix(session_id).await?
        && (&stored.identity, &stored.index, &stored.context)
            != (&tiers.identity, &tiers.index, &tiers.context)
    {
        tracing::debug!(
            session = session_id,
            "préfixe modifié : attend un cache froid"
        );
        tiers.identity = stored.identity;
        tiers.index = stored.index;
        tiers.context = stored.context;
    }
    journal_prefix(s, session_id, tiers).await;
    Ok(())
}

/// Le préfixe retenu entre au journal quand il change, en entier (`conv.system`,
/// épopée #208, T6). Un échec ne coûte que l'événement : le tour continue.
async fn journal_prefix(
    s: &penelope_app::services::Services,
    session_id: &str,
    tiers: &penelope_context::Tiers,
) {
    if let Err(e) = s.context.history.journal_system(session_id, tiers).await {
        tracing::warn!(session = session_id, error = %e, "préfixe non journalisé");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Daemon;
    use penelope_app::bus::Origin;
    use penelope_app::engine::TurnIntake;
    use penelope_app::services::Services;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::{MockProvider, Scripted};
    use penelope_llm::types::ToolCall;
    use serde_json::json;
    use std::sync::Arc;

    /// CA 5 étendu au transcript (issue #17) : d'un appel au suivant, dans un tour comme
    /// d'un tour à l'autre, la requête envoyée reprend la précédente octet pour octet ; le
    /// contexte volatil reste avec son message et l'empreinte est enregistrée.
    #[tokio::test]
    async fn ca_5_4_each_request_extends_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();

        let turn = |text: &'static str| {
            let d = d.clone();
            let sid = sid.clone();
            async move {
                d.enqueue_message(&sid, text, &Origin::Cli, None)
                    .await
                    .unwrap();
                let t = d.services.turns.claim("test").await.unwrap().unwrap();
                let out = d.run_turn(&t).await;
                d.services.turns.complete(&t).await.unwrap();
                out
            }
        };
        p.reply(r#"{"complexity":"medium"}"#);
        p.push(Scripted::ToolCalls(
            String::new(),
            vec![ToolCall {
                id: "c1".into(),
                name: "self_status".into(),
                arguments: json!({"section": "model"}),
            }],
        ));
        p.reply("Voici l'état.");
        turn("où en es-tu ?").await;
        // Deux minutes plus tard : l'heure du contexte volatil a changé, le cache est chaud.
        clock.advance_secs(120);
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("D'accord.");
        turn("merci").await;

        let chats: Vec<serde_json::Value> = p
            .requests()
            .iter()
            .filter(|r| {
                r.messages
                    .first()
                    .is_some_and(|m| m.text().contains("agent personnel autonome"))
            })
            .map(|r| penelope_llm::provider::to_openai_body(r)["messages"].clone())
            .collect();
        assert_eq!(chats.len(), 3, "deux appels au premier tour, un au second");
        for pair in chats.windows(2) {
            let (before, after) = (pair[0].as_array().unwrap(), pair[1].as_array().unwrap());
            assert!(after.len() > before.len());
            assert_eq!(
                &after[..before.len()],
                &before[..],
                "la requête suivante réécrit la précédente"
            );
        }
        let first_user = chats[2].as_array().unwrap()[1]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(first_user.starts_with("<contexte>"), "{first_user}");
        assert!(first_user.ends_with("où en es-tu ?"), "{first_user}");

        let rows = s.budget.report("miss", Some(&sid), None, 10).await.unwrap();
        assert!(!rows.is_empty());
        let fp = previous_call(&s, &sid).await.unwrap().unwrap();
        assert!(
            fp.request_hash.is_some() && fp.msg_count.unwrap() >= 4,
            "{fp:?}"
        );
    }

    /// T6 (épopée #208) : un préfixe modifié à cache chaud attend, sans entrer au
    /// journal ; à cache froid, il y entre avec la raison `cold`.
    #[tokio::test]
    async fn a_changed_prefix_is_journaled_only_when_the_cache_is_cold() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock.clone())
                .await
                .unwrap(),
        );
        let d = Daemon::from_services(s.clone());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let tiers = |skills: &str| penelope_context::Tiers {
            identity: "Tu es Pénélope.".into(),
            index: skills.into(),
            ..Default::default()
        };
        let systems = || async {
            let events = s.events.session_events(&sid, 0).await.unwrap();
            events
                .into_iter()
                .filter(|e| e.kind == "conv.system")
                .map(|e| e.payload["reason"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        stable_prefix(&d.services, &sid, &mut tiers("revue"))
            .await
            .unwrap();
        s.budget
            .record(penelope_kernel::budget::UsageRecord {
                session_id: Some(sid.clone()),
                role: Some("chat".into()),
                model: "m".into(),
                provider: "mock".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // Skill rechargée à cache chaud : le préfixe d'avant est gardé, rien de neuf.
        let mut warm = tiers("revue, deploiement");
        stable_prefix(&d.services, &sid, &mut warm).await.unwrap();
        assert_eq!(warm.index, "revue");
        assert_eq!(systems().await, ["first"]);
        // À cache froid, le nouveau préfixe sort et entre au journal.
        clock.advance_ms(CACHE_TTL_MS + 1);
        stable_prefix(&d.services, &sid, &mut tiers("revue, deploiement"))
            .await
            .unwrap();
        assert_eq!(systems().await, ["first", "cold"]);
    }
}
