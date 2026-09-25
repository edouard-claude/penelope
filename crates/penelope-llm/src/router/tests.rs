/// #74 : un message trivial ne passe pas par le classifieur, une vraie demande oui.
#[test]
fn trivial_messages_skip_the_classifier() {
    for m in [
        "ok",
        "OK !",
        "merci",
        "Merci beaucoup",
        "salut",
        "bonjour",
        "d'accord",
        "ça marche",
        "oui",
        "non",
        "parfait, merci",
        "bonne nuit",
    ] {
        assert!(is_trivial(m), "« {m} » doit être trivial");
    }
    for m in [
        "et ensuite ?",
        "corrige le bug de facturation",
        "bonjour, où en est la facturation",
        "lis ~/notes.md",
        "regarde https://example.com",
        "ok mais avant ça, relis le ticket 4821 et dis-moi",
        "",
    ] {
        assert!(!is_trivial(m), "« {m} » ne doit pas être trivial");
    }

    // Routage : un message trivial part sur le modèle par défaut, sans classifieur.
    let r = router();
    let cfg = cfg();
    let d = r
        .route_deterministic(
            &cfg,
            &RouteInput {
                message: "merci !".into(),
                ..Default::default()
            },
        )
        .expect("décision sans classifieur");
    assert_eq!(d.reason, RouteReason::Trivial);
    assert_eq!(d.alias, r.default_decision(&cfg).alias);
    assert!(
        r.route_deterministic(
            &cfg,
            &RouteInput {
                message: "corrige le bug de facturation".into(),
                ..Default::default()
            }
        )
        .is_none(),
        "une vraie demande passe par le classifieur"
    );
}
use super::*;
use crate::catalog::ModelInfo;

fn cfg() -> Config {
    Config::sample(1)
}

fn router() -> Router {
    let c = Catalog::new();
    c.upsert(vec![
        ModelInfo::minimal("deepseek/deepseek-v4-pro", "openrouter", 256_000),
        ModelInfo::minimal("deepseek/deepseek-v4-flash", "openrouter", 128_000),
        ModelInfo::minimal("z-ai/glm-5.2", "openrouter", 200_000),
        ModelInfo::minimal("google/gemini-3.1-flash-image", "openrouter", 1_000_000),
    ]);
    Router::new(c)
}

#[test]
fn image_attachment_routes_to_vision() {
    let d = router()
        .route_deterministic(
            &cfg(),
            &RouteInput {
                has_image_attachment: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(d.reason, RouteReason::Vision);
    assert_eq!(d.alias, "vision");
}

#[test]
fn image_request_routes_to_image_model() {
    let d = router()
        .route_deterministic(
            &cfg(),
            &RouteInput {
                message: "génère une image de coucher de soleil".into(),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(d.reason, RouteReason::ImageGeneration);
}

/// #81 : une demande de code ou de rédaction n'est jamais envoyée au modèle d'image.
#[test]
fn only_an_explicit_image_request_goes_to_the_image_model() {
    let r = router();
    let routed = |m: &str| {
        r.route_deterministic(
            &cfg(),
            &RouteInput {
                message: m.into(),
                ..Default::default()
            },
        )
        .map(|d| d.reason)
    };
    for m in [
        "génère un script de déploiement",
        "régénère les tests",
        "génère le rapport de la semaine",
        "illustre ta réponse par un exemple",
        "illustre par un exemple",
        "dessine l'architecture en ASCII",
        "dessine-moi l'architecture en ASCII",
        "fais une image docker de l'appli",
        "génère une image du fichier `schema.png` depuis le code",
    ] {
        assert_eq!(routed(m), None, "{m} : le classifieur décide");
    }
    for m in [
        "génère une image d'un chat",
        "Crée une image de coucher de soleil",
        "fais-moi un dessin de phare",
        "generate an image of a cat on a roof",
        "draw me a picture of the sea",
    ] {
        assert_eq!(routed(m), Some(RouteReason::ImageGeneration), "{m}");
    }
}

#[test]
fn step_model_wins_over_everything() {
    let d = router()
        .route_deterministic(
            &cfg(),
            &RouteInput {
                has_image_attachment: true,
                step_model: Some("reasoning".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(d.reason, RouteReason::StepModel);
    assert_eq!(d.model_id, "openrouter:z-ai/glm-5.2");
}

#[test]
fn step_role_selects_its_alias() {
    let d = router()
        .route_deterministic(
            &cfg(),
            &RouteInput {
                role: Some("code".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(d.alias, "reasoning");
    assert_eq!(d.reason, RouteReason::Role);
}

#[test]
fn a_pinned_model_beats_sticky_and_classifier_but_not_images() {
    let pinned = StickyModel {
        alias: "reasoning".into(),
        model_id: "openrouter:z-ai/glm-5.2".into(),
    };
    let input = RouteInput {
        message: "ok vas-y".into(),
        pinned: Some(pinned.clone()),
        sticky: Some(StickyModel {
            alias: "main".into(),
            model_id: "openrouter:deepseek/deepseek-v4-pro".into(),
        }),
        ..Default::default()
    };
    let d = router().route_deterministic(&cfg(), &input).unwrap();
    assert_eq!(d.reason, RouteReason::Pinned);
    assert_eq!(d.alias, "reasoning");

    let mut no_classifier = cfg();
    no_classifier.models.routing.classifier = false;
    let d = router()
        .route_deterministic(&no_classifier, &input)
        .unwrap();
    assert_eq!(d.reason, RouteReason::Pinned);

    let image = RouteInput {
        message: "dessine-moi une image d'un phare au crépuscule".into(),
        ..input
    };
    let d = router().route_deterministic(&cfg(), &image).unwrap();
    assert_eq!(d.reason, RouteReason::ImageGeneration);
}

/// CA 10 : le modèle est collant, un changement n'a lieu qu'aux frontières.
#[test]
fn ca_10_1_sticky_model_survives_until_a_boundary() {
    let r = router();
    let sticky = StickyModel {
        alias: "main".into(),
        model_id: "openrouter:deepseek/deepseek-v4-pro".into(),
    };
    let input = RouteInput {
        message: "une question quelconque".into(),
        sticky: Some(sticky.clone()),
        ..Default::default()
    };
    let d = r.route_deterministic(&cfg(), &input).unwrap();
    assert_eq!(d.reason, RouteReason::Sticky);
    assert_eq!(d.model_id, sticky.model_id);

    // À la frontière, le classifieur reprend la main.
    let at_boundary = RouteInput {
        at_boundary: true,
        ..input
    };
    assert!(r.route_deterministic(&cfg(), &at_boundary).is_none());
}

/// CA 10 : un classifieur mocké `high` route vers `reasoning`.
#[test]
fn ca_10_2_high_complexity_routes_to_reasoning() {
    let d = router().route_with_classification(
        &cfg(),
        &Classification {
            complexity: Complexity::High,
            needs_tools: true,
            domain: "architecture".into(),
        },
    );
    assert_eq!(d.alias, "reasoning");
    assert_eq!(d.model_id, "openrouter:z-ai/glm-5.2");
    let d = router().route_with_classification(
        &cfg(),
        &Classification {
            complexity: Complexity::Low,
            needs_tools: false,
            domain: String::new(),
        },
    );
    assert_eq!(d.alias, "fast");
}

#[test]
fn classifier_output_is_schema_validated() {
    let s = classifier_schema();
    let good = serde_json::json!({"complexity":"high","needs_tools":true,"domain":"x"});
    assert!(penelope_kernel::schema::validate(&s, &good).is_empty());
    let bad = serde_json::json!({"complexity":"enorme"});
    assert!(!penelope_kernel::schema::validate(&s, &bad).is_empty());
}

/// CA 10 : une panne simulée du provider déclenche le repli.
#[test]
fn ca_10_3_fallback_chain_is_used_on_transient_failure() {
    let r = router();
    let chain = r.fallback_chain(&cfg(), "main");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].alias, "fast");
    assert_eq!(chain[0].reason, RouteReason::Fallback);

    assert!(Router::should_fallback(&LlmError::transient("503")));
    assert!(Router::should_fallback(&LlmError::new(
        LlmErrorKind::RateLimited,
        "429"
    )));
    assert!(
        !Router::should_fallback(&LlmError::new(LlmErrorKind::BadRequest, "schéma")),
        "une requête malformée ne doit pas déclencher de repli"
    );
    assert!(
        !Router::should_fallback(&LlmError::context_length("trop long")),
        "un dépassement de contexte se règle par compaction, pas par repli"
    );
}

#[test]
fn escalation_goes_one_rung_up_only() {
    let r = router();
    let c = cfg();
    assert_eq!(r.escalation(&c, "fast").unwrap().alias, "main");
    assert_eq!(r.escalation(&c, "main").unwrap().alias, "reasoning");
    assert!(
        r.escalation(&c, "reasoning").is_none(),
        "pas d'escalade au-delà du rang le plus haut"
    );
}

#[test]
fn alias_validation_rejects_unknown_models() {
    let r = router();
    let mut c = cfg();
    r.validate_alias(&c, "main").unwrap();
    assert!(r.validate_alias(&c, "inexistant").is_err());
    c.models
        .aliases
        .insert("main".into(), "openrouter:pas/au/catalogue".into());
    let e = r.validate_alias(&c, "main").unwrap_err();
    assert_eq!(e.kind, LlmErrorKind::UnknownModel);
}

#[test]
fn tool_support_follows_the_catalog() {
    let c = Catalog::new();
    let mut sans = ModelInfo::minimal("vieux/modele", "openrouter", 8192);
    sans.supported_parameters.clear();
    c.upsert(vec![
        ModelInfo::minimal("moderne/modele", "openrouter", 128_000),
        sans,
    ]);
    let r = Router::new(c);
    assert_eq!(r.supports_tools("vieux/modele"), Some(false));
    assert_eq!(r.supports_tools("moderne/modele"), Some(true));
    assert_eq!(r.supports_tools("inconnu/modele"), None);
}

#[test]
fn classifier_disabled_falls_back_to_default() {
    let mut c = cfg();
    c.models.routing.classifier = false;
    let d = router()
        .route_deterministic(&c, &RouteInput::default())
        .unwrap();
    assert_eq!(d.reason, RouteReason::Default);
    assert_eq!(d.alias, "main");
}
