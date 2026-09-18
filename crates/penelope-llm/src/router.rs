//! Routage par complexité, modèle collant, repli et escalade (§10.3).
//!
//! Ordre imposé par le PRD :
//! 1. règles déterministes (image, génération d'image, modèle d'étape, rôle d'étape) ;
//! 2. classifieur pour les messages de chat ambigus ;
//! 3. **sticky** : le modèle est figé pour la session, hors frontières explicites ;
//! 4. escalade vers un sub-agent, sans changer le modèle de la session ;
//! 5. repli d'alias sur panne.

use crate::catalog::Catalog;
use crate::types::{LlmError, LlmErrorKind, Result};
use penelope_kernel::config::Config;
use serde::{Deserialize, Serialize};

/// Ce que le harnais sait du tour au moment de router.
#[derive(Debug, Clone, Default)]
pub struct RouteInput {
    pub message: String,
    pub has_image_attachment: bool,
    pub wants_image_generation: bool,
    /// Modèle explicitement demandé par une étape de workflow.
    pub step_model: Option<String>,
    /// Rôle de l'étape ou du sous-système (`code`, `compaction`, `classifier`…).
    pub role: Option<String>,
    /// Modèle épinglé sur la session par le propriétaire (`/model`) : il prime sur le
    /// collant et sur le classifieur, pas sur les règles d'image.
    pub pinned: Option<StickyModel>,
    /// Modèle collant de la session, s'il y en a un.
    pub sticky: Option<StickyModel>,
    /// Vrai aux frontières où le modèle peut changer (nouvelle session, compaction
    /// niveau 3, délégation, `/model`).
    pub at_boundary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickyModel {
    pub alias: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub alias: String,
    pub model_id: String,
    pub reason: RouteReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteReason {
    /// Pièce jointe image.
    Vision,
    /// Demande de génération d'image.
    ImageGeneration,
    /// Modèle explicite d'une étape de workflow.
    StepModel,
    /// Rôle d'étape ou de sous-système.
    Role,
    /// Modèle épinglé sur la session par le propriétaire.
    Pinned,
    /// Modèle collant de la session.
    Sticky,
    /// Sortie du classifieur.
    Classifier,
    /// Message manifestement trivial : aucun classifieur appelé (issue #74).
    Trivial,
    /// Défaut de configuration.
    Default,
    /// Repli après panne.
    Fallback,
    /// Escalade après échec qualifié.
    Escalation,
}

/// Sortie attendue du classifieur (JSON validé, ≤ 200 tokens).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    pub complexity: Complexity,
    #[serde(default)]
    pub needs_tools: bool,
    #[serde(default)]
    pub domain: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    Low,
    Medium,
    High,
}

/// Schéma JSON du classifieur, validé à la réception.
pub fn classifier_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "complexity": {"type": "string", "enum": ["low", "medium", "high"]},
            "needs_tools": {"type": "boolean"},
            "domain": {"type": "string", "maxLength": 40}
        },
        "required": ["complexity"],
        "additionalProperties": false
    })
}

pub const CLASSIFIER_PROMPT: &str = "Tu classes une demande utilisateur pour choisir un \
modèle. Réponds UNIQUEMENT par un objet JSON : \
{\"complexity\":\"low|medium|high\",\"needs_tools\":bool,\"domain\":\"...\"}. \
`low` : question factuelle courte, reformulation, petite commande. \
`medium` : tâche ordinaire, plusieurs étapes, lecture de code. \
`high` : raisonnement long, architecture, débogage difficile, arbitrage.";

/// Demande **explicite** de génération d'image (§10.3 règle 1) : un verbe de création
/// suivi, à trois mots au plus, d'« une image », « un dessin », « une illustration »
/// (ou leur forme anglaise), sur des mots entiers.
///
/// La règle court-circuite le classifieur et envoie le tour entier au modèle d'image :
/// elle ne doit jamais prendre « génère un script », « régénère les tests », « illustre
/// par un exemple » ou « dessine l'architecture en ASCII » (issue #81). Toute autre
/// demande d'image passe par le modèle de conversation et son outil `image_generate`.
pub fn looks_like_image_request(msg: &str) -> bool {
    const VERBS: &[&str] = &[
        "génère",
        "genere",
        "générer",
        "generer",
        "génères",
        "crée",
        "cree",
        "créer",
        "creer",
        "fais",
        "faire",
        "fait",
        "dessine",
        "dessiner",
        "produis",
        "generate",
        "create",
        "make",
        "draw",
    ];
    const OBJECTS: &[[&str; 2]] = &[
        ["une", "image"],
        ["un", "dessin"],
        ["une", "illustration"],
        ["une", "photo"],
        ["an", "image"],
        ["a", "picture"],
        ["a", "drawing"],
        ["an", "illustration"],
    ];
    /// Un mot du logiciel dans la phrase : ce n'est pas une image qu'on attend.
    const SOFTWARE: &[&str] = &[
        "script",
        "test",
        "tests",
        "rapport",
        "fichier",
        "code",
        "ascii",
        "diagramme",
        "schéma",
        "schema",
        "tableau",
        "markdown",
        "mermaid",
        "svg",
        "json",
        "csv",
        "docker",
        "dockerfile",
    ];
    let m = msg.to_lowercase();
    if m.contains('`') || m.contains("://") || (m.contains('/') && m.contains('.')) {
        return false;
    }
    let words: Vec<&str> = m
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if words.iter().any(|w| SOFTWARE.contains(w)) {
        return false;
    }
    words.iter().enumerate().any(|(i, w)| {
        VERBS.contains(w)
            && (i + 1..(i + 5).min(words.len().saturating_sub(1))).any(|j| {
                OBJECTS
                    .iter()
                    .any(|o| words[j] == o[0] && words[j + 1] == o[1])
            })
    })
}

/// Message trivial : salutation, accusé de réception ou interjection, sans demande.
///
/// Rien de lexical au-delà : une phrase qui contient une question, un chemin, une URL, du
/// code ou plus de six mots n'est pas triviale (issue #74).
pub fn is_trivial(message: &str) -> bool {
    let m = message.trim().to_lowercase();
    if m.is_empty() || m.chars().count() > 40 {
        return false;
    }
    if m.contains('?')
        || m.contains('/')
        || m.contains('`')
        || m.contains("http")
        || m.contains('\n')
    {
        return false;
    }
    let words: Vec<&str> = m.split_whitespace().collect();
    if words.len() > 6 {
        return false;
    }
    const TRIVIAL: &[&str] = &[
        "ok",
        "okay",
        "d'accord",
        "daccord",
        "merci",
        "merci !",
        "parfait",
        "super",
        "génial",
        "genial",
        "bien",
        "très bien",
        "tres bien",
        "salut",
        "bonjour",
        "bonsoir",
        "coucou",
        "hello",
        "hey",
        "bonne nuit",
        "bonne journée",
        "bonne journee",
        "à demain",
        "a demain",
        "au revoir",
        "bye",
        "oui",
        "non",
        "yes",
        "no",
        "top",
        "nickel",
        "ça marche",
        "ca marche",
        "c'est noté",
        "noté",
        "note",
        "vu",
        "compris",
        "entendu",
    ];
    let cleaned = m
        .trim_end_matches(['.', '!', '…', ' ', ':', ';', ','])
        .trim();
    // Renforçateurs admis dans un message par ailleurs trivial (« merci beaucoup »).
    const MODIFIERS: &[&str] = &["beaucoup", "bien", "très", "tres", "infiniment", "trop"];
    TRIVIAL.contains(&cleaned)
        || cleaned.split_whitespace().all(|w| {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
            TRIVIAL.contains(&w) || MODIFIERS.contains(&w)
        })
}

pub struct Router {
    catalog: Catalog,
}

impl Router {
    pub fn new(catalog: Catalog) -> Self {
        Router { catalog }
    }

    /// Règles déterministes. Renvoie `None` si un classifieur est nécessaire.
    pub fn route_deterministic(&self, cfg: &Config, input: &RouteInput) -> Option<Decision> {
        if let Some(m) = &input.step_model {
            // Un modèle d'étape peut être un alias ou un identifiant complet.
            let (alias, id) = match cfg.alias_model(m) {
                Some(id) => (m.clone(), id.to_string()),
                None => (m.clone(), m.clone()),
            };
            return Some(Decision {
                alias,
                model_id: id,
                reason: RouteReason::StepModel,
            });
        }
        if input.has_image_attachment
            && let Some(d) = self.by_role(cfg, "image_describe", RouteReason::Vision)
        {
            return Some(d);
        }
        if (input.wants_image_generation || looks_like_image_request(&input.message))
            && let Some(d) = self.by_role(cfg, "image_generate", RouteReason::ImageGeneration)
        {
            return Some(d);
        }
        if let Some(role) = &input.role
            && role != "chat_default"
            && let Some(d) = self.by_role(cfg, role, RouteReason::Role)
        {
            return Some(d);
        }
        // Choix explicite du propriétaire pour cette session.
        if let Some(p) = &input.pinned {
            return Some(Decision {
                alias: p.alias.clone(),
                model_id: p.model_id.clone(),
                reason: RouteReason::Pinned,
            });
        }
        // Sticky : hors frontière, on ne change pas de modèle (préservation du cache).
        if let Some(s) = &input.sticky
            && cfg.models.routing.sticky
            && !input.at_boundary
        {
            return Some(Decision {
                alias: s.alias.clone(),
                model_id: s.model_id.clone(),
                reason: RouteReason::Sticky,
            });
        }
        if !cfg.models.routing.classifier {
            return Some(self.default_decision(cfg));
        }
        // « ok », « merci », « salut » : pas la peine de payer un aller-retour de
        // classifieur avant de répondre (issue #74).
        if is_trivial(&input.message) {
            let mut d = self.default_decision(cfg);
            d.reason = RouteReason::Trivial;
            return Some(d);
        }
        None
    }

    /// Applique la sortie du classifieur.
    pub fn route_with_classification(&self, cfg: &Config, c: &Classification) -> Decision {
        let alias = match c.complexity {
            Complexity::Low => &cfg.models.routing.low,
            Complexity::Medium => &cfg.models.routing.medium,
            Complexity::High => &cfg.models.routing.high,
        };
        Decision {
            alias: alias.clone(),
            model_id: cfg
                .alias_model(alias)
                .map(String::from)
                .unwrap_or_else(|| alias.clone()),
            reason: RouteReason::Classifier,
        }
    }

    pub fn default_decision(&self, cfg: &Config) -> Decision {
        let alias = cfg.role_alias("chat_default");
        Decision {
            model_id: cfg
                .alias_model(&alias)
                .map(String::from)
                .unwrap_or_else(|| alias.clone()),
            alias,
            reason: RouteReason::Default,
        }
    }

    fn by_role(&self, cfg: &Config, role: &str, reason: RouteReason) -> Option<Decision> {
        let alias = cfg.models.roles.get(role)?;
        let id = cfg.alias_model(alias)?;
        Some(Decision {
            alias: alias.clone(),
            model_id: id.to_string(),
            reason,
        })
    }

    /// Chaîne de repli d'un alias (§10.3 point 5).
    pub fn fallback_chain(&self, cfg: &Config, alias: &str) -> Vec<Decision> {
        cfg.models
            .routing
            .fallback
            .get(alias)
            .map(|chain| {
                chain
                    .iter()
                    .filter_map(|a| {
                        cfg.alias_model(a).map(|id| Decision {
                            alias: a.clone(),
                            model_id: id.to_string(),
                            reason: RouteReason::Fallback,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Vrai si l'erreur justifie un repli d'alias.
    pub fn should_fallback(err: &LlmError) -> bool {
        matches!(
            err.kind,
            LlmErrorKind::Transient | LlmErrorKind::RateLimited | LlmErrorKind::UnknownModel
        )
    }

    /// Modèle d'escalade pour un sub-agent (§10.3 point 4) : le rang supérieur, jamais
    /// le modèle de la session.
    pub fn escalation(&self, cfg: &Config, current_alias: &str) -> Option<Decision> {
        let ladder = [
            cfg.models.routing.low.as_str(),
            cfg.models.routing.medium.as_str(),
            cfg.models.routing.high.as_str(),
        ];
        let pos = ladder.iter().position(|a| *a == current_alias)?;
        let next = ladder.get(pos + 1)?;
        if *next == current_alias {
            return None;
        }
        cfg.alias_model(next).map(|id| Decision {
            alias: next.to_string(),
            model_id: id.to_string(),
            reason: RouteReason::Escalation,
        })
    }

    /// Vérifie qu'un alias pointe vers un modèle présent au catalogue (§10.2 :
    /// « un alias invalide ou absent du catalogue est rejeté »).
    pub fn validate_alias(&self, cfg: &Config, alias: &str) -> Result<()> {
        let Some(id) = cfg.alias_model(alias) else {
            return Err(LlmError::new(
                LlmErrorKind::UnknownModel,
                format!("alias inconnu : `{alias}`"),
            ));
        };
        if self.catalog.is_empty() {
            // Catalogue pas encore synchronisé : on ne bloque pas le démarrage.
            return Ok(());
        }
        if self.catalog.get(id).is_none() {
            return Err(LlmError::new(
                LlmErrorKind::UnknownModel,
                format!(
                    "`{id}` est absent du catalogue ({} modèles connus)",
                    self.catalog.len()
                ),
            ));
        }
        Ok(())
    }

    /// Ce modèle appelle-t-il des outils ? `None` : le catalogue ne le connaît pas, on ne
    /// préjuge de rien (issue #54).
    pub fn supports_tools(&self, model_id: &str) -> Option<bool> {
        self.catalog.get(model_id).map(|i| i.supports_tools())
    }
}

#[cfg(test)]
mod tests {

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
}
