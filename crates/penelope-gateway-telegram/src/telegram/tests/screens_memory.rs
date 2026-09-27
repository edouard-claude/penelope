//! Écrans de la mémoire, de bout en bout : appris, entrée, oubli, pratiques,
//! intentions, règles, sessions à oublier.

use super::*;

const PRACTICE: &str = "---\ntype: pratique\nid: langage-backend\nconfiance: 0.8\n---\n# Langage backend\n\n## Défaut\n- Go (stdlib) <!-- uid: DEF1 -->\n\n## Exceptions\n- Rust <!-- uid: EXC1 --> <!-- quand: tache=code -->\n\n## Écarts observés\n- Python pour un script <!-- uid: ECA1 -->\n";

async fn vault_with_entry(g: &TelegramGateway) -> std::path::PathBuf {
    let s = &g.daemon.services;
    let vault = penelope_app::helpers::vault_dir(s);
    std::fs::create_dir_all(vault.join("pratiques")).unwrap();
    std::fs::write(
        vault.join("profil.md"),
        "# Profil\n\n## Préférences\n- Café le matin <!-- uid: CAF1 -->\n",
    )
    .unwrap();
    std::fs::write(vault.join("pratiques/langage-backend.md"), PRACTICE).unwrap();
    penelope_vault::vault_ops::reindex(s, &vault).await.unwrap();
    let now = s.clock.now_rfc3339();
    s.store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO mem_history(uid, file, op, ts) VALUES ('CAF1', 'profil.md', 'add_entry', ?1)",
                [now],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    vault
}

/// `learned` mène à l'entrée ; la valider pose sa date de revue ; `forget` la retire
/// après confirmation.
#[tokio::test]
async fn a_learned_entry_is_validated_then_forgotten() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let vault = vault_with_entry(&g).await;

    let (text, buttons) = screen_of(&g, "learned", json!({})).await;
    assert!(text.starts_with("📚 **Appris sur 7 jours** (1)"), "{text}");
    assert!(text.contains("Café le matin _(profil.md, "), "{text}");
    token_of(&buttons, "🔎 Café le matin");
    token_of(&buttons, "30 jours");
    let (text, _) = screen_of(&g, "learned", json!({"days": 90})).await;
    assert!(text.contains("Appris sur 90 jours"), "{text}");

    let (text, buttons) = screen_of(&g, "entry", json!({"uid": "CAF1"})).await;
    assert!(text.contains("Fichier `profil.md`"), "{text}");
    press(&g, &token_of(&buttons, "✅ Valider")).await;
    let raw = std::fs::read_to_string(vault.join("profil.md")).unwrap();
    assert!(raw.contains("revue"), "{raw}");
    assert!(
        g.build_screen(OWNER, None, "entry", &json!({"uid": "ZZZ"}))
            .await
            .is_err()
    );

    let (text, buttons) = screen_of(&g, "forget", json!({})).await;
    assert!(text.contains("1. Café le matin"), "{text}");
    confirm(&g, &token_of(&buttons, "🗑 1.")).await;
    assert_eq!(
        s.memory.get("CAF1").await.unwrap().unwrap().statut,
        "retiree"
    );
    assert!(
        !std::fs::read_to_string(vault.join("profil.md"))
            .unwrap()
            .contains("Café")
    );
    let (text, _) = screen_of(&g, "forget", json!({"query": "introuvable"})).await;
    assert_eq!(text, "Aucune entrée pour « introuvable ».");
}

/// Une pratique se lit avec son défaut, ses exceptions et ses écarts ; « Rejeter » la
/// conteste, « Valider » la rend active.
#[tokio::test]
async fn a_practice_is_read_contested_and_validated() {
    let (_d, g, _t, _p) = gateway().await;
    let vault = vault_with_entry(&g).await;
    let (text, buttons) = screen_of(&g, "practices", json!({})).await;
    assert!(text.starts_with("📐 **Pratiques** (1)"), "{text}");
    token_of(&buttons, "📐 Langage backend");

    let (text, buttons) = screen_of(&g, "practice", json!({"slug": "langage-backend"})).await;
    assert!(text.contains("Défaut : Go (stdlib)"), "{text}");
    assert!(text.contains("- Exception : Rust (tache=code)"), "{text}");
    assert!(text.contains("1 écart(s) observé(s)."), "{text}");
    press(&g, &token_of(&buttons, "🚫 Rejeter")).await;
    let raw = std::fs::read_to_string(vault.join("pratiques/langage-backend.md")).unwrap();
    assert!(raw.contains("contestee"), "{raw}");
    let (_, buttons) = screen_of(&g, "practice", json!({"slug": "langage-backend"})).await;
    press(&g, &token_of(&buttons, "✅ Valider")).await;
    let raw = std::fs::read_to_string(vault.join("pratiques/langage-backend.md")).unwrap();
    assert!(raw.contains("statut: active"), "{raw}");
    assert!(
        g.build_screen(OWNER, None, "practice", &json!({"slug": "absente"}))
            .await
            .is_err()
    );
}

/// Une intention armée s'annule depuis son écran ; une règle se retire après
/// confirmation.
#[tokio::test]
async fn intentions_and_rules_are_withdrawn_from_their_screens() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let (text, _) = screen_of(&g, "intentions", json!({})).await;
    assert_eq!(text, "Aucune intention armée.");
    s.intents
        .create("Relancer Martin", vec!["martin".into()], None, 0, 3, None)
        .await
        .unwrap();
    let (text, buttons) = screen_of(&g, "intentions", json!({})).await;
    assert!(
        text.contains("Relancer Martin · déclencheurs : martin · 0/3 tir(s)"),
        "{text}"
    );
    press(&g, &token_of(&buttons, "❌ Relancer Martin")).await;
    let (text, _) = screen_of(&g, "intentions", json!({})).await;
    assert_eq!(text, "Aucune intention armée.");

    let (text, _) = screen_of(&g, "policies", json!({})).await;
    assert_eq!(text, "Aucune règle d'autorisation active.");
    s.policies
        .create_rule(
            penelope_hitl::policy::RuleScope::Tool,
            Some("send_message"),
            None,
            None,
            penelope_kernel::risk::PolicyDecision::Auto,
            penelope_kernel::risk::PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    let (text, buttons) = screen_of(&g, "policies", json!({})).await;
    assert!(text.contains("`send_message`"), "{text}");
    confirm(&g, &token_of(&buttons, "🗑 send_message")).await;
    assert!(s.policies.active_rules().await.unwrap().is_empty());
}

/// Oublier une session retire ce que la mémoire a retenu d'elle.
#[tokio::test]
async fn a_session_is_forgotten_from_its_screen() {
    let (_d, g, _t, _p) = gateway().await;
    let s = g.daemon.services.clone();
    let sid = s
        .sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Devis ACME".into()),
        )
        .await
        .unwrap()
        .id
        .to_string();
    let (text, buttons) = screen_of(&g, "forget.sessions", json!({})).await;
    assert!(text.starts_with("🧹 **Oublier une session**"), "{text}");
    confirm(&g, &token_of(&buttons, "🗑 Devis ACME")).await;
    assert!(
        s.sessions.get(&sid).await.unwrap().is_some(),
        "la conversation reste"
    );
}
