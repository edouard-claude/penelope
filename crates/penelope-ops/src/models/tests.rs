use super::*;
use penelope_kernel::clock::TestClock;
use std::sync::Arc;

const SOL: &str = "codex:gpt-5.6-sol";
const GEMINI: &str = "openrouter:google/gemini-3.1-flash-image";

async fn services() -> (tempfile::TempDir, Services) {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_800_000_000_000);
    let s = Services::for_tests(dir.path().to_path_buf(), Arc::new(clock))
        .await
        .unwrap();
    (dir, s)
}

async fn call(s: &Services, m: &str, p: Value) -> anyhow::Result<Value> {
    rpc(s, m, &p).await
}

/// Le modèle effectif et la raison d'un rôle, tels que `model.list` les rend.
async fn row(s: &Services, role: &str) -> (String, String) {
    let v = list(s, None).await;
    let r = v["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["role"] == role)
        .cloned()
        .unwrap_or_else(|| panic!("{role} absent : {v}"));
    (
        r["model"].as_str().unwrap_or_default().to_string(),
        r["reason"].as_str().unwrap_or_default().to_string(),
    )
}

/// #334 : les commandes que le propriétaire lancera pour son profil « Codex (crédits) » :
/// création (la garde est un choix explicite), voix locale intacte, images sur
/// OpenRouter, bascule sans redémarrage, retour au profil d'avant.
#[tokio::test]
async fn the_owner_creates_and_switches_to_a_codex_profile() {
    let (_dir, s) = services().await;
    let name = "Codex (crédits)";
    let refused = call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "new", "name": name, "primary": SOL}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        refused.contains("codex_background") && refused.contains("suspension"),
        "{refused}"
    );
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "new", "name": name, "primary": SOL,
               "codex_background": "allow", "use": true}),
    )
    .await
    .unwrap();
    call(
        &s,
        method::MODEL_SET,
        json!({"target": "image_generate", "model": GEMINI}),
    )
    .await
    .unwrap();
    call(
        &s,
        method::MODEL_SET,
        json!({"target": "vision", "model": GEMINI}),
    )
    .await
    .unwrap();

    let v = list(&s, None).await;
    assert_eq!(v["profile"], name);
    assert_eq!(v["codex_background"], "allow");
    for role in [
        "chat_default",
        "classifier",
        "dream",
        "workflow",
        "compaction",
    ] {
        assert_eq!(
            row(&s, role).await,
            (SOL.to_string(), "primary".to_string()),
            "{role}"
        );
    }
    assert_eq!(
        row(&s, "image_describe").await,
        (GEMINI.to_string(), "capability".to_string())
    );
    let (stt, why) = row(&s, "stt").await;
    assert_eq!(why, "local");
    assert!(!stt.starts_with("codex:"), "{stt}");

    // Le profil d'avant reste là, tel quel.
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "use", "name": "defaut"}),
    )
    .await
    .unwrap();
    let (classifier, why) = row(&s, "classifier").await;
    assert_eq!(why, "override");
    assert_eq!(
        Some(classifier.as_str()),
        s.config.config().alias_model("fast")
    );
}

/// #333 : sous la garde, un rôle de fond ne prend pas l'abonnement, la voix jamais ;
/// garde levée, il le prend, et la table le montre.
#[tokio::test]
async fn the_guard_refuses_codex_for_background_roles_unless_lifted() {
    let (_dir, s) = services().await;
    let err = call(
        &s,
        method::MODEL_SET,
        json!({"target": "dream", "model": SOL}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("abonnement ChatGPT"), "{err}");
    let err = call(
        &s,
        method::MODEL_SET,
        json!({"target": "stt", "model": SOL}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("voix"), "{err}");

    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "guard", "name": "defaut", "codex_background": "allow"}),
    )
    .await
    .unwrap();
    call(
        &s,
        method::MODEL_SET,
        json!({"target": "dream", "model": SOL}),
    )
    .await
    .unwrap();
    assert_eq!(row(&s, "dream").await.0, SOL);

    // La garde rétablie : la table dit où le travail de fond part.
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "guard", "name": "defaut", "codex_background": "deny"}),
    )
    .await
    .unwrap();
    assert_eq!(row(&s, "dream").await.1, "codex_guard");
}

/// #334 : une surcharge retirée rend le rôle au principal ; un profil se duplique, se
/// renomme, et ne se supprime pas tant qu'il est actif.
#[tokio::test]
async fn overrides_and_profiles_are_edited() {
    let (_dir, s) = services().await;
    let cfg = s.config.config();
    let primary = cfg.alias_model(&cfg.primary_label()).unwrap().to_string();
    let v = call(&s, method::MODEL_UNSET, json!({"target": "classifier"}))
        .await
        .unwrap();
    assert_eq!(v["model"], primary);
    assert_eq!(row(&s, "classifier").await, (primary, "primary".into()));
    assert!(!s.config.config().models.is_derived("defaut"), "écrit");

    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "copy", "name": "eco"}),
    )
    .await
    .unwrap();
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "use", "name": "eco"}),
    )
    .await
    .unwrap();
    let err = call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "rm", "name": "eco"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("actif"), "{err}");
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "rename", "name": "eco", "to": "économie"}),
    )
    .await
    .unwrap();
    assert_eq!(s.config.config().models.active_name(), "économie");
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "use", "name": "defaut"}),
    )
    .await
    .unwrap();
    call(
        &s,
        method::MODEL_PROFILE,
        json!({"action": "rm", "name": "économie"}),
    )
    .await
    .unwrap();
    assert_eq!(s.config.config().models.profile_names(), ["defaut"]);
}
