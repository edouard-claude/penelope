//! Écrans d'administration : serveur MCP, modèles, skills, secrets, mise à jour,
//! heures silencieuses, retour en arrière.

use super::*;
use penelope_mcp_host::testing::{FakeConnector, declare, server, tool};

/// Désactiver un serveur passe par une confirmation ; désactivé, son écran propose de
/// l'activer ; le journal est envoyé dans la conversation.
#[tokio::test]
async fn an_mcp_server_is_disabled_then_enabled_from_its_screen() {
    let (_d, g, t, _p) = gateway().await;
    let fake = Arc::new(FakeConnector::default());
    fake.serve(
        "redmine",
        server(Arc::new(std::sync::Mutex::new(vec![tool(
            "list_issues",
            json!({"readOnlyHint": true}),
        )]))),
    );
    let sup = penelope_mcp_host::testing::supervisor(g.daemon.services.clone(), fake.clone());
    declare(&sup, "redmine", "");
    sup.reload().await;
    g.daemon.hooks.set_mcp(sup.clone());

    let (_, buttons) = screen_of(&g, "mcp", json!({})).await;
    token_of(&buttons, "🔄");
    let (_, buttons) = screen_of(&g, "mcp.server", json!({"name": "redmine"})).await;
    press(&g, &token_of(&buttons, "📜 Journal")).await;
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("Aucune ligne de journal pour")),
        "journal envoyé"
    );
    confirm(&g, &token_of(&buttons, "⏻ Désactiver")).await;
    assert!(!sup.config_of("redmine").await.unwrap().enabled);

    let (_, buttons) = screen_of(&g, "mcp.server", json!({"name": "redmine"})).await;
    press(&g, &token_of(&buttons, "⏻ Activer")).await;
    assert!(sup.config_of("redmine").await.unwrap().enabled);
}

/// Le catalogue se parcourt, filtré ou non ; un modèle s'affecte à un alias depuis son
/// écran.
#[tokio::test]
async fn a_catalog_model_is_assigned_to_an_alias() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    s.catalog.upsert(
        (0..12)
            .map(|i| {
                penelope_llm::catalog::ModelInfo::minimal(
                    &format!("essai/modele-{i:02}"),
                    "openrouter",
                    128_000,
                )
            })
            .collect(),
    );
    let (text, _) = screen_of(&g, "models", json!({})).await;
    assert!(text.ends_with("12 modèle(s) au catalogue."), "{text}");
    let (text, buttons) = screen_of(&g, "models", json!({"filter": "modele"})).await;
    assert!(text.contains("**Catalogue « modele »** (12)"), "{text}");
    token_of(&buttons, "Suivant »");
    token_of(&buttons, "Tout le catalogue");
    let (text, _) = screen_of(&g, "models", json!({"filter": "modele", "page": 1})).await;
    assert!(text.ends_with("_Page 2/2_"), "{text}");

    let model = "openrouter:essai/modele-03";
    let (text, buttons) = screen_of(
        &g,
        "model.assign",
        json!({"model": model, "back": {"screen": "models", "args": {}}}),
    )
    .await;
    assert!(text.starts_with(&format!("Affecter `{model}`")), "{text}");
    token_of(&buttons, "« Retour");
    let alias = s.config.config().models.routing.low.clone();
    press(&g, &token_of(&buttons, &alias)).await;
    assert_eq!(
        s.config
            .config()
            .models
            .aliases
            .get(&alias)
            .map(String::as_str),
        Some(model)
    );
}

/// Un secret se supprime depuis son écran, après confirmation.
#[tokio::test]
async fn a_secret_is_removed_from_its_screen() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    s.platform.secrets.set("jeton_essai", "s3cr3t-xyz").unwrap();
    let (text, buttons) = screen_of(&g, "secrets", json!({})).await;
    assert!(text.contains("- `jeton_essai`"), "{text}");
    assert!(!text.contains("s3cr3t-xyz"));
    confirm(&g, &token_of(&buttons, "🗑 jeton_essai")).await;
    assert!(s.platform.secrets.get("jeton_essai").unwrap().is_none());
}

/// L'écran de mise à jour dit le dernier résultat de « Vérifier ».
#[tokio::test]
async fn the_upgrade_screen_shows_the_last_check() {
    let (_d, g, _t, _p) = gateway().await;
    let (text, _) = screen_of(
        &g,
        "upgrade",
        json!({"latest": "9.9.9", "up_to_date": false}),
    )
    .await;
    assert!(text.contains("🆕 **9.9.9** est disponible."), "{text}");
    let (text, _) = screen_of(
        &g,
        "upgrade",
        json!({"latest": "0.1.0", "up_to_date": true}),
    )
    .await;
    assert!(
        text.contains("✅ À jour (dernière publiée : 0.1.0)."),
        "{text}"
    );
    g.daemon
        .services
        .kv_set(
            "tg.upgrade.last_check",
            &json!({"error": "réseau coupé"}).to_string(),
        )
        .await
        .unwrap();
    let (text, buttons) = screen_of(&g, "upgrade", json!({})).await;
    assert!(text.contains("❌ réseau coupé"), "{text}");
    token_of(&buttons, "🔎 Vérifier");
    token_of(&buttons, "⬆️ Installer");
}

/// Les heures silencieuses se choisissent parmi les plages proposées, ou se coupent.
#[tokio::test]
async fn quiet_hours_are_set_and_cleared_from_their_screen() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let (_, buttons) = screen_of(&g, "quiet", json!({})).await;
    press(&g, &token_of(&buttons, "23:00-08:00")).await;
    assert_eq!(s.config.config().telegram.quiet_hours, "23:00-08:00");
    let (text, buttons) = screen_of(&g, "quiet", json!({})).await;
    assert!(text.contains("**23:00-08:00**"), "{text}");
    press(&g, &token_of(&buttons, "Désactiver")).await;
    assert_eq!(s.config.config().telegram.quiet_hours, "");
    let (text, _) = screen_of(&g, "quiet", json!({})).await;
    assert!(text.contains("**désactivées**"), "{text}");
}

/// « Revenir en arrière » défait le dernier échange de la conversation, après
/// confirmation, et le dit.
#[tokio::test]
async fn the_last_exchange_is_undone_from_the_rewind_screen() {
    let (_d, g, t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let origin = Origin::Telegram {
        chat_id: OWNER,
        topic_id: None,
        message_id: None,
    };
    let sid = g.daemon.chat_session_for(&origin).await.unwrap();
    let h = &s.context.history;
    for (u, a) in [("un", "1"), ("deux", "2")] {
        h.append(
            &sid,
            &penelope_llm::types::ChatMessage::user(u),
            5,
            0,
            false,
            None,
        )
        .await
        .unwrap();
        h.append(
            &sid,
            &penelope_llm::types::ChatMessage::assistant(a),
            5,
            0,
            false,
            None,
        )
        .await
        .unwrap();
    }
    let (_, buttons) = screen_of(&g, "rewind", json!({})).await;
    confirm(&g, &token_of(&buttons, "1")).await;
    assert_eq!(h.load(&sid, 0).await.unwrap().len(), 2);
    assert!(
        texts(&t.calls_to(tg::SEND_MESSAGE).await)
            .iter()
            .any(|x| x.contains("1 échange(s) défait(s)")),
        "le retour en arrière est dit"
    );
}
