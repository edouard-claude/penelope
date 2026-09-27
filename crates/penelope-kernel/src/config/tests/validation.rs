//! Chaque refus de `Config::validate` (§4.4 étape 2), une valeur fautive à la fois : le
//! message nomme la clé à corriger.

use super::*;

type Mutation = fn(&mut Config);

fn refused(mutate: Mutation) -> String {
    let mut c = cfg();
    mutate(&mut c);
    c.validate()
        .expect_err("la configuration devait être refusée")
        .to_string()
}

#[test]
fn each_invalid_value_is_refused_with_its_key() {
    let cases: &[(&str, Mutation)] = &[
        ("owner.telegram_user_id", |c| c.owner.telegram_user_id = 0),
        ("fuseau inconnu", |c| {
            c.owner.timezone = "Mars/Olympus".into()
        }),
        ("runtime_stream_bind invalide", |c| {
            c.observability.runtime_consumers = vec![consumer("a", "s")];
            c.observability.runtime_stream_bind = "pas une adresse".into();
        }),
        ("127.0.0.1 avec un port non nul", |c| {
            c.observability.runtime_consumers = vec![consumer("a", "s")];
            c.observability.runtime_stream_bind = "0.0.0.0:7070".into();
        }),
        ("127.0.0.1 avec un port non nul", |c| {
            c.observability.runtime_consumers = vec![consumer("a", "s")];
            c.observability.runtime_stream_bind = "127.0.0.1:0".into();
        }),
        ("nom unique et un token_secret", |c| {
            c.observability.runtime_stream_bind = "127.0.0.1:7070".into();
            c.observability.runtime_consumers = vec![consumer("a", "s"), consumer("a", "t")];
        }),
        ("nom unique et un token_secret", |c| {
            c.observability.runtime_stream_bind = "127.0.0.1:7070".into();
            c.observability.runtime_consumers = vec![consumer("a", " ")];
        }),
        ("telegram.mode", |c| c.telegram.mode = "push".into()),
        ("telegram.webhook_url", |c| {
            c.telegram.mode = "webhook".into();
            c.telegram.webhook_url.clear();
        }),
        ("heure invalide", |c| {
            c.telegram.quiet_hours = "22h-7h".into()
        }),
        ("models.roles.chat_default", |c| {
            c.models
                .roles
                .insert("chat_default".into(), "fantome".into());
        }),
        ("models.routing référence", |c| {
            c.models.routing.high = "fantome".into()
        }),
        ("alias source inconnu", |c| {
            c.models
                .routing
                .fallback
                .insert("fantome".into(), vec!["main".into()]);
        }),
        ("alias cible inconnu", |c| {
            c.models
                .routing
                .fallback
                .insert("main".into(), vec!["fantome".into()]);
        }),
        ("alias `main`", |c| {
            c.models
                .aliases
                .insert("main".into(), "sans-fournisseur".into());
        }),
        ("models.locate_frame", |c| {
            c.models.locate_frame = "cm".into()
        }),
        ("tools.approval_mode", |c| {
            c.tools.approval_mode = "jamais".into()
        }),
        ("compaction_threshold", |c| {
            c.context.compaction_threshold = 0.99
        }),
        ("tail_min_tokens", |c| {
            c.context.tail_min_tokens = c.context.tail_max_tokens + 1
        }),
        ("max_prompt_tokens", |c| {
            c.context.max_prompt_tokens = 10_000
        }),
        ("max_tool_result_share", |c| {
            c.context.max_tool_result_share = 1.5
        }),
        ("consolidation_reasoning", |c| {
            c.memory.consolidation_reasoning = "parfois".into()
        }),
        ("providers.codex.quota_alert_ratio doit être", |c| {
            c.providers.codex.quota_alert_ratio = 1.5
        }),
        ("au plus quota_stop_ratio", |c| {
            c.providers.codex.quota_alert_ratio = 0.9;
            c.providers.codex.quota_stop_ratio = 0.8;
        }),
        ("runners.heartbeat", |c| {
            c.runners.lease_ttl = "60s".into();
            c.runners.heartbeat = "45s".into();
        }),
        ("mcp.registry_mode", |c| {
            c.mcp.registry_mode = "paresseux".into()
        }),
        ("mcp.oauth_redirect_mode", |c| {
            c.mcp.oauth_redirect_mode = "courrier".into()
        }),
        ("mcp.public_callback_url", |c| {
            c.mcp.oauth_redirect_mode = "public_callback".into();
            c.mcp.public_callback_url.clear();
        }),
        ("mcp.callback_host", |c| {
            c.mcp.callback_host = "0.0.0.0".into()
        }),
        ("mcp.policy.external", |c| {
            c.mcp.policy.external = "peut-être".into()
        }),
        ("runners.count", |c| c.runners.count = 0),
        ("runners.count", |c| c.runners.count = 65),
        ("sandbox.default_profile", |c| {
            c.sandbox.default_profile = "prison".into()
        }),
        ("budget.alert_ratio", |c| c.budget.alert_ratio = 0.0),
    ];
    for (want, mutate) in cases {
        let got = refused(*mutate);
        assert!(got.contains(want), "attendu « {want} », reçu : {got}");
    }
}

fn consumer(name: &str, secret: &str) -> RuntimeConsumer {
    RuntimeConsumer {
        name: name.into(),
        token_secret: secret.into(),
        ..Default::default()
    }
}

/// Une configuration de flux runtime correcte passe.
#[test]
fn a_local_runtime_stream_with_named_consumers_is_accepted() {
    let mut c = cfg();
    c.observability.runtime_stream_bind = "127.0.0.1:7070".into();
    c.observability.runtime_consumers = vec![consumer("a", "s"), consumer("b", "t")];
    c.validate().unwrap();
}

/// Seuils propres à un modèle, avec ou sans fournisseur, et plage de silence.
#[test]
fn model_thresholds_and_quiet_hours_are_read() {
    let mut c = cfg();
    c.context
        .model_thresholds
        .insert("z-ai/glm-5.3".into(), 0.6);
    assert_eq!(c.compaction_threshold_for("openrouter:z-ai/glm-5.3"), 0.6);
    assert_eq!(
        c.compaction_threshold_for("openrouter:autre"),
        c.context.compaction_threshold
    );
    assert_eq!(c.model_threshold("autre"), None);
    c.telegram.quiet_hours = "22:00-07:00".into();
    assert!(c.quiet_range().is_some());
    c.telegram.quiet_hours.clear();
    assert!(c.quiet_range().is_none());
}

/// Une clé retirée se reconnaît, ainsi que tout ce qui est dessous.
#[test]
fn retired_keys_are_recognised_with_their_children() {
    assert!(retired("history").is_some());
    assert!(retired("history.source").is_some());
    assert!(retired("historyx").is_none());
    assert!(retired("memory").is_none());
}
