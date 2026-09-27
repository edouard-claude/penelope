//! Cache de prompt (§5, issue #17) : empreinte de chaque requête de conversation et cause
//! probable d'un raté, comparée à l'appel précédent de la session. `penelope usage --by
//! miss` en fait le bilan ; le fournisseur amont qui a servi reste épinglé tant que la
//! session est active.
//!
//! Les parties pures (empreinte, fournisseur collant, cause d'un raté) vivent dans
//! `penelope_llm::cache`, la lecture du dernier appel dans le `BudgetLedger` du kernel
//! (épopée #208, T27), le contexte volatil et le préfixe stable dans
//! `penelope_conversation::prefix` (#236).

use penelope_llm::cache::PreviousCall;

/// Le dernier appel de conversation de la session (voir `BudgetLedger::previous_call`).
pub async fn previous_call(
    s: &penelope_app::services::Services,
    session_id: &str,
) -> anyhow::Result<Option<PreviousCall>> {
    Ok(s.budget.previous_call(session_id).await?)
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
}
