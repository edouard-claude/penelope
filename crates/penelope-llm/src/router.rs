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
    /// Vrai aux frontières où le modèle peut changer : cache du fournisseur froid,
    /// contexte compacté ou nouvel épisode depuis le dernier appel (le préfixe change de
    /// toute façon, issue #82). Le collant est alors ignoré, le classifieur décide.
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
mod tests;
