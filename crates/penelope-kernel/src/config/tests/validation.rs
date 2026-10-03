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
        ("seul `polling` l'est", |c| {
            c.telegram.mode = "webhook".into();
            c.telegram.webhook_url = "https://example.invalid/tg".into();
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
        ("digest.agenda", |c| c.digest.agenda = "events_today".into()),
        ("digest.agenda", |c| {
            c.digest.agenda = "mcp__agenda__".into()
        }),
        ("webhooks.listen invalide", |c| {
            c.webhooks.listen = "pas une adresse".into()
        }),
        ("webhooks.listen doit porter un port", |c| {
            c.webhooks.listen = "127.0.0.1:0".into()
        }),
        ("webhooks.max_body_bytes", |c| {
            c.webhooks.max_body_bytes = 10
        }),
        ("webhooks.rate_per_minute", |c| {
            c.webhooks.rate_per_minute = 0
        }),
        ("webhooks.rate_per_minute", |c| {
            c.webhooks.prompt_turns_per_hour = 0
        }),
    ];
    for (want, mutate) in cases {
        let got = refused(*mutate);
        assert!(got.contains(want), "attendu « {want} », reçu : {got}");
    }
}

/// #295 : l'agenda du digest est un outil MCP qualifié, ou rien ; un espace autour ne
/// compte pas.
#[test]
fn the_digest_agenda_is_a_qualified_mcp_tool_or_nothing() {
    let mut c = cfg();
    assert!(c.digest.agenda.is_empty(), "éteint par défaut");
    c.validate().unwrap();
    c.digest.agenda = " mcp__agenda__events_today ".into();
    c.validate().unwrap();
    assert!(agenda_tool_is_qualified("mcp__agenda__events_today"));
    assert!(agenda_tool_is_qualified("mcp__mon-serveur__events_today"));
    for bad in [
        "events_today",
        "mcp__agenda",
        "mcp____events_today",
        "mcp__agenda__",
    ] {
        assert!(!agenda_tool_is_qualified(bad), "{bad}");
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

/// #289 : la destination S3 se valide au chargement : adresse et bucket ensemble, HTTPS
/// hors boucle locale, pas d'identifiants dans l'adresse, pas de chemin dans le bucket.
#[test]
fn the_s3_destination_is_checked_at_load() {
    let mut c = cfg();
    c.backup.s3.endpoint = "http://127.0.0.1:9000".into();
    c.backup.s3.bucket = "sauvegardes".into();
    c.validate().unwrap();
    assert!(c.backup.s3.enabled());
    c.backup.s3.endpoint = "https://s3.exemple.net".into();
    c.validate().unwrap();
    assert!(!cfg().backup.s3.enabled());

    let e = refused(|c| c.backup.s3.endpoint = "https://s3.exemple.net".into());
    assert!(e.contains("vont ensemble"), "{e}");
    let e = refused(|c| c.backup.s3.bucket = "seul".into());
    assert!(e.contains("vont ensemble"), "{e}");
    let e = refused(|c| {
        c.backup.s3.endpoint = "http://s3.exemple.net".into();
        c.backup.s3.bucket = "b".into();
    });
    assert!(e.contains("HTTPS obligatoire"), "{e}");
    let e = refused(|c| {
        c.backup.s3.endpoint = "https://AK:secret@s3.exemple.net".into();
        c.backup.s3.bucket = "b".into();
    });
    assert!(e.contains("identifiants"), "{e}");
    let e = refused(|c| {
        c.backup.s3.endpoint = "https://s3.exemple.net".into();
        c.backup.s3.bucket = "b/prefixe".into();
    });
    assert!(e.contains("backup.s3.prefix"), "{e}");
    let e = refused(|c| {
        c.backup.s3.endpoint = "s3.exemple.net".into();
        c.backup.s3.bucket = "b".into();
    });
    assert!(e.contains("https://"), "{e}");
}
