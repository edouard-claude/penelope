use super::*;
use crate::model_watch::{NOTICE_EVENT, Place, fallback_key};
use penelope_kernel::clock::TestClock;

async fn services() -> (tempfile::TempDir, Services, TestClock) {
    let dir = tempfile::tempdir().unwrap();
    let clock = TestClock::new(1_800_000_000_000);
    let s = Services::for_tests(dir.path().to_path_buf(), Arc::new(clock.clone()))
        .await
        .unwrap();
    (dir, s, clock)
}

async fn notices(s: &Services) -> Vec<String> {
    s.events
        .session_events_of_kind("s1", NOTICE_EVENT)
        .await
        .unwrap()
        .iter()
        .map(|e| e.payload["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// #333 : un écart est dit une fois, pas à chaque appel ; un autre modèle de repli est
/// un nouvel état ; le retour au modèle choisi est dit, puis plus rien.
#[tokio::test]
async fn a_deviation_is_announced_once_per_change() {
    let (_dir, s, clock) = services().await;
    let place = Place {
        session: Some("s1"),
        origin: None,
    };
    let key = fallback_key("s1");
    let codex = "codex:gpt-5.6-sol";
    let flash = "openrouter:deepseek/deepseek-v4.1-flash";
    for _ in 0..3 {
        watch(&s)
            .deviate(&place, &key, flash, "⚠️ repli sur `deepseek-v4.1-flash`")
            .await;
        clock.advance_secs(30);
    }
    assert_eq!(notices(&s).await, ["⚠️ repli sur `deepseek-v4.1-flash`"]);
    let recent = watch(&s).recent().await;
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].served, flash);

    // Le modèle de repli ne revient pas « à la normale » : il est l'écart.
    watch(&s).settle(&place, &key, flash, "✅ retour").await;
    assert_eq!(notices(&s).await.len(), 1);

    watch(&s)
        .settle(&place, &key, codex, "✅ retour sur `gpt-5.6-sol`")
        .await;
    watch(&s)
        .settle(&place, &key, codex, "✅ retour sur `gpt-5.6-sol`")
        .await;
    assert_eq!(
        notices(&s).await,
        [
            "⚠️ repli sur `deepseek-v4.1-flash`",
            "✅ retour sur `gpt-5.6-sol`"
        ]
    );
    assert!(watch(&s).recent().await.is_empty());
}

/// Un écart qui ne s'est plus produit depuis 24 h n'est plus « en cours ».
#[tokio::test]
async fn an_old_deviation_is_no_longer_recent() {
    let (_dir, s, clock) = services().await;
    watch(&s)
        .deviate(&Place::default(), "guard.rêve", "openrouter:x/y", "⚠️")
        .await;
    assert_eq!(watch(&s).recent().await.len(), 1);
    clock.advance_hours(25);
    assert!(watch(&s).recent().await.is_empty());
}

/// Fournisseurs d'un test : tout modèle sauf ceux de Codex, compte déconnecté.
struct NoCodex;

#[async_trait::async_trait]
impl ProviderSource for NoCodex {
    async fn provider_for(&self, model_id: &str) -> Result<Arc<dyn Provider>, String> {
        if model_id.starts_with("codex:") {
            return Err("aucun compte ChatGPT connecté".into());
        }
        Ok(Arc::new(penelope_llm::mock::MockProvider::new()))
    }
    fn provider_override_active(&self) -> Option<Arc<dyn Provider>> {
        None
    }
}

/// #335 : un principal sans fournisseur ne fait plus échouer le tour avant la chaîne de
/// repli ; le repli joignable sert, annoncé (#333). Épinglé, le modèle n'a pas de repli
/// et échoue en le disant.
#[tokio::test]
async fn an_unreachable_primary_falls_back_unless_pinned() {
    let (_dir, s, _clock) = services().await;
    s.publish_config("test", |c| {
        c.models
            .aliases
            .insert("main".into(), "codex:gpt-5.6-sol".into());
        c.models
            .routing
            .fallback
            .insert("main".into(), vec!["fast".into()]);
        Ok(vec!["models".into()])
    })
    .unwrap();
    let r = route(&s, &NoCodex, "s1", "main", "codex:gpt-5.6-sol")
        .await
        .unwrap();
    let fast = s.config.config().alias_model("fast").unwrap().to_string();
    assert_eq!(r.model_id, fast);
    assert!(r.fallbacks.is_empty());
    let said = notices(&s).await;
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("injoignable"), "{said:?}");

    s.kv_set(&crate::helpers::pin_key("s1"), "main")
        .await
        .unwrap();
    let err = route(&s, &NoCodex, "s1", "main", "codex:gpt-5.6-sol")
        .await
        .err()
        .unwrap();
    assert!(err.contains("compte ChatGPT"), "{err}");
}
