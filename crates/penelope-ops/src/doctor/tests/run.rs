//! Passe complète : secret de la connexion Codex, chaîne d'audit altérée.

use super::*;
use penelope_kernel::event::EventDraft;
use serde_json::json;

fn find<'a>(checks: &'a [DoctorCheck], id: &str) -> &'a DoctorCheck {
    checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("contrôle `{id}` absent"))
}

/// #142 : le fournisseur `codex` activé sans compte connecté est signalé, avec la
/// commande qui connecte ; connecté, le secret est dit présent.
#[tokio::test]
async fn an_enabled_codex_provider_needs_its_connection() {
    let (_d, s) = services().await;
    s.publish_config("test", |c| {
        c.providers.codex.enabled = true;
        Ok(vec!["providers.codex.enabled".into()])
    })
    .unwrap();
    let checks = run(&s).await;
    let codex = find(&checks, "secret.codex.oauth");
    assert!(!codex.ok, "{codex:?}");
    assert_eq!(codex.fix.as_deref(), Some("penelope model auth codex"));

    s.platform.secrets.set("codex.oauth", "{}").unwrap();
    let checks = run(&s).await;
    let codex = find(&checks, "secret.codex.oauth");
    assert!(codex.ok && codex.detail == "présent", "{codex:?}");
}

/// Un événement réécrit après coup rompt la chaîne : le contrôle est critique.
#[tokio::test]
async fn a_tampered_audit_chain_is_critical() {
    let (_d, s) = services().await;
    for n in 0..3 {
        s.events
            .append(EventDraft::new("essai", json!({"n": n})))
            .await
            .unwrap();
    }
    s.store
        .write(|tx| {
            tx.execute(
                "UPDATE events SET payload = '{\"n\": 99}' WHERE id = (SELECT min(id) FROM events)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let checks = run(&s).await;
    let audit = find(&checks, "audit");
    assert!(!audit.ok, "{audit:?}");
    assert_eq!(audit.severity, "error");
}
