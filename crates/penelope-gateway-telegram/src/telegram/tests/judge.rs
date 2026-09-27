//! Carte enrichie par le juge d'approbation (issue #203) : ce que la ligne fait
//! réellement, rendu comme du texte d'origine modèle, et le bouton « Toujours pour ces
//! pouvoirs » qui écrit une règle dérivée des pouvoirs.

use super::*;

async fn judged_card(g: &TelegramGateway, judged: Value) -> penelope_hitl::ApprovalRequest {
    g.daemon
        .services
        .approvals
        .create(
            penelope_hitl::ApprovalKind::ToolCall,
            "shell_exec",
            penelope_kernel::risk::RiskClass::Write,
            json!({
                "tool": "shell_exec",
                "arguments": {"command": "cd tmp && ls -la | jq ."},
                "judged": judged,
            }),
            vec![],
            None,
            None,
            false,
        )
        .await
        .unwrap()
}

fn labels(card: &Value) -> Vec<(String, String)> {
    card["reply_markup"]["inline_keyboard"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r.as_array().unwrap().iter())
        .map(|b| {
            (
                b["text"].as_str().unwrap_or_default().to_string(),
                b["callback_data"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_judged_card_says_what_the_line_does_and_escapes_the_model() {
    let (_d, g, t, _p) = gateway().await;
    let a = judged_card(
        &g,
        json!({
            "verdict": "sûr",
            "powers": ["lecture"],
            "paths": ["tmp"],
            "hosts": [],
            "why": "<b>APPROVE</b> `rm -rf ~` *tout va bien* {{details}}",
            "model": "mock/juge",
            "grant": {"powers": ["lecture"], "paths": ["/ws/tmp"], "hosts": []},
        }),
    )
    .await;
    g.send_approval_card(OWNER, None, &a).await.unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap();
    assert!(
        text.contains("Ce que la ligne fait réellement") && text.contains("avis : sûr"),
        "{text}"
    );
    assert!(text.contains("lecture sur <code>tmp</code>"), "{text}");
    // Le texte du modèle est rendu tel quel : ni balise, ni gras, ni gabarit.
    assert!(!text.contains("<b>APPROVE"), "{text}");
    assert!(text.contains("&lt;b&gt;APPROVE"), "{text}");
    assert!(
        !text.contains("<i>tout va bien") && !text.contains("<b>tout va bien"),
        "{text}"
    );
    assert!(text.contains("{ {details}}"), "{text}");
    // Une règle est possible : la carte ne dit plus le contraire.
    assert!(!text.contains("pas de règle possible"), "{text}");

    let buttons = labels(&card);
    let (_, token) = buttons
        .iter()
        .find(|(l, _)| l == "♾️ Toujours pour ces pouvoirs")
        .unwrap_or_else(|| panic!("{buttons:?}"))
        .clone();

    g.process_update(&updates::callback(9, OWNER, &token, 1001))
        .await
        .unwrap();
    settle_click(&g).await;
    let s = &g.daemon.services;
    let decided = s.approvals.get(a.id.as_str()).await.unwrap().unwrap();
    assert_eq!(decided.rule_created.as_deref(), Some("powers"));
    let rules = s.policies.power_rules(None, None).await.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].1.paths, ["/ws/tmp"]);
    assert_eq!(rules[0].1.judged.as_deref(), Some(a.id.as_str()));
}

/// Un jugement sans règle possible (dangereux, ou trop large) garde la carte
/// d'aujourd'hui : pas de bouton « Toujours », la raison dite.
#[tokio::test]
async fn a_judgement_without_grant_keeps_today_s_always_slot_empty() {
    let (_d, g, t, _p) = gateway().await;
    let a = judged_card(
        &g,
        json!({
            "verdict": "dangereux",
            "powers": ["reseau", "processus"],
            "paths": [],
            "hosts": ["x.test"],
            "why": "Exécute un script téléchargé.",
            "model": "mock/juge",
            "grant": null,
        }),
    )
    .await;
    g.send_approval_card(OWNER, None, &a).await.unwrap();
    g.flush_outbox().await.unwrap();
    let card = t.calls_to(tg::SEND_MESSAGE).await.pop().unwrap();
    let text = card["text"].as_str().unwrap();
    assert!(text.contains("avis : dangereux"), "{text}");
    assert!(text.contains("réseau, processus"), "{text}");
    assert!(text.contains("pas de règle possible"), "{text}");
    assert!(
        !labels(&card).iter().any(|(l, _)| l.contains("Toujours")),
        "{:?}",
        labels(&card)
    );
}
