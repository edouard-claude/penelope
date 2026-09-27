//! Refus d'une pièce jointe par le fournisseur (issue #231).
//!
//! Un 400 causé par une image (trop lourde, format refusé, illisible) ne se distingue
//! d'une requête malformée que par son texte. Les motifs sont ceux des corps d'erreur
//! réels des fournisseurs joints : OpenRouter (types canoniques, et corps amont dans
//! `metadata.raw`), Anthropic, OpenAI (dont le backend Codex). Chacun est précis :
//! « image » seul ne suffit jamais, un 400 sans rapport reste `BadRequest`.

use crate::types::{ChatMessage, Content};
use std::collections::BTreeMap;

/// Motifs cherchés dans le corps d'erreur mis en minuscules, du plus précis au plus
/// général, avec ce qu'on en dit au propriétaire.
const MARKERS: &[(&str, &str)] = &[
    // Types canoniques d'OpenRouter (`error.metadata.error_type`).
    ("image_too_large", "trop lourde"),
    ("image_too_small", "trop petite"),
    ("unsupported_image_format", "format refusé"),
    ("image_not_found", "introuvable"),
    ("image_download_failed", "introuvable"),
    // Anthropic : `messages.N.content.M.image.source.base64: image exceeds 5 MB
    // maximum: … bytes > 5242880 bytes`.
    ("image exceeds", "trop lourde"),
    ("image dimensions exceed", "trop grande"),
    // Anthropic : `… image.source.base64.data: Image does not match the provided media
    // type image/jpeg`.
    (
        "image does not match the provided media type",
        "format refusé",
    ),
    // Anthropic : `Could not process image`.
    ("could not process image", "illisible"),
    // OpenAI : codes `invalid_image_format`, `image_parse_error`, `invalid_image_url`,
    // `invalid_image`, message « You uploaded an unsupported image ».
    ("invalid_image_format", "format refusé"),
    ("you uploaded an unsupported image", "format refusé"),
    ("image_parse_error", "illisible"),
    ("invalid_image_url", "introuvable"),
    ("invalid_image", "illisible"),
    // Anthropic, validation d'un bloc image : `….image.source.media_type: Input should
    // be 'image/jpeg', …`.
    (".image.source", "refusée"),
];

/// Ce qu'on dit d'une image refusée quand aucun motif ne la qualifie.
pub const DEFAULT_MOTIF: &str = "refusée";

/// Le motif d'un refus d'image, s'il en est un : `text` est un corps d'erreur, un
/// message ou un type canonique, dans n'importe quelle casse.
pub fn motif(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    MARKERS
        .iter()
        .find(|(marker, _)| lower.contains(marker))
        .map(|(_, motif)| *motif)
}

/// L'empreinte d'une image : le SHA-256 de son URL (`data:` ou `https:`).
pub fn image_key(url: &str) -> String {
    penelope_kernel::canonical::sha256_hex(url.as_bytes())
}

/// Ce que le modèle lit à la place d'une image refusée.
pub fn mention(motif: &str) -> String {
    format!("[image retirée : refusée par le fournisseur ({motif})]")
}

/// Les images refusées d'une session, par empreinte, avec leur motif. La boucle les
/// retire de la copie qu'elle envoie ; le pliage du journal, de la requête qu'il dérive :
/// les deux passent par [`RejectedImages::prepare`], au même octet près.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RejectedImages(BTreeMap<String, String>);

impl RejectedImages {
    /// Ajoute les images `keys`, refusées pour `motif`.
    pub fn insert(&mut self, keys: &[String], motif: &str) {
        for key in keys {
            self.0.insert(key.clone(), motif.to_string());
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Remplace chaque image déjà refusée par sa mention ; rend les empreintes de celles
    /// qui restent, dans l'ordre, sans doublon.
    pub fn prepare(&self, messages: &mut [ChatMessage]) -> Vec<String> {
        let mut kept: Vec<String> = Vec::new();
        for c in messages.iter_mut().flat_map(|m| m.content.iter_mut()) {
            let Content::ImageUrl { url, .. } = c else {
                continue;
            };
            let key = image_key(url);
            match self.0.get(&key) {
                Some(motif) => *c = Content::text(mention(motif)),
                None if !kept.contains(&key) => kept.push(key),
                None => {}
            }
        }
        kept
    }
}

/// Ce qu'un fournisseur accepte d'une image : le poids de l'image **encodée en base64**,
/// comme il la mesure dans la requête, et le grand côté en pixels (issue #242). Une image
/// au-delà est réduite avant l'envoi plutôt que refusée puis retirée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_encoded_bytes: usize,
    pub max_side: u32,
}

impl ImageLimits {
    /// Anthropic : `image exceeds 5 MB maximum: … bytes > 5242880 bytes` sur les octets
    /// base64, et 8 000 px de côté.
    pub const ANTHROPIC: ImageLimits = ImageLimits {
        max_encoded_bytes: 5 * 1024 * 1024,
        max_side: 8_000,
    };
    /// Les autres (OpenAI, Gemini, Mistral par OpenRouter) : 20 Mo, au-delà des 10 Mo
    /// qu'une photo reçue peut peser ; rien n'est réduit pour eux.
    pub const DEFAULT: ImageLimits = ImageLimits {
        max_encoded_bytes: 20 * 1024 * 1024,
        max_side: 16_000,
    };

    /// Vrai si une image de `bytes` octets bruts et de grand côté `side` passe.
    pub fn fits(&self, bytes: usize, side: Option<u32>) -> bool {
        encoded_len(bytes) <= self.max_encoded_bytes && side.is_none_or(|s| s <= self.max_side)
    }
}

/// Longueur en base64 de `bytes` octets.
pub fn encoded_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

/// Les limites d'image du fournisseur qui sert `model_id` (`anthropic/claude-…` par
/// OpenRouter, ou un nom `claude-…` servi ailleurs).
pub fn image_limits(model_id: &str) -> ImageLimits {
    let model = crate::catalog::strip_provider(model_id).to_lowercase();
    if model.starts_with("anthropic/") || model.contains("claude") {
        ImageLimits::ANTHROPIC
    } else {
        ImageLimits::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use crate::types::{LlmError, LlmErrorKind};
    use serde_json::json;

    fn kind(status: u16, body: &str) -> LlmErrorKind {
        LlmError::from_status(status, body).kind
    }

    /// Anthropic en direct, corps relevé tel quel : image au-delà de 5 Mo.
    #[test]
    fn anthropic_image_too_large() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.30.content.0.tool_result.content.0.image.source.base64: image exceeds 5 MB maximum: 5577016 bytes > 5242880 bytes"},"request_id":"req_011CVPs4RgUBL3LkQnMmiiXe"}"#;
        let e = LlmError::from_status(400, body);
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "trop lourde");
    }

    /// Anthropic : le type déclaré ne correspond pas aux octets.
    #[test]
    fn anthropic_media_type_mismatch() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.1.content.27.image.source.base64.data: Image does not match the provided media type image/jpeg"},"request_id":"req_011CVLAiaeFTuua5SkEUowhX"}"#;
        let e = LlmError::from_status(400, body);
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "format refusé");
    }

    /// Anthropic : image illisible.
    #[test]
    fn anthropic_could_not_process_image() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Could not process image"}}"#;
        let e = LlmError::from_status(400, body);
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "illisible");
    }

    /// OpenAI (et le backend Codex, qui passe par `from_status`) : format refusé.
    #[test]
    fn openai_invalid_image_format() {
        let body = json!({"error": {
            "message": "You uploaded an unsupported image. Please make sure your image has of one the following formats: ['png', 'jpeg', 'gif', 'webp'].",
            "type": "invalid_request_error",
            "param": null,
            "code": "invalid_image_format"
        }})
        .to_string();
        let e = LlmError::from_status(400, &body);
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "format refusé");
        let e = crate::codex::codex_error(400, &body, &reqwest::header::HeaderMap::new());
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "format refusé");
    }

    /// OpenAI : image illisible (`image_parse_error`).
    #[test]
    fn openai_image_parse_error() {
        let body = json!({"error": {
            "message": "You uploaded an unsupported image. Please make sure your image is below 20 MB in size and is of one the following formats: ['png', 'jpeg', 'gif', 'webp'].",
            "type": "invalid_request_error",
            "param": null,
            "code": "image_parse_error"
        }})
        .to_string();
        assert_eq!(kind(400, &body), LlmErrorKind::AttachmentRejected);
    }

    /// OpenRouter : le type canonique suffit, quel que soit le message.
    #[test]
    fn openrouter_canonical_image_types() {
        for (t, motif) in [
            ("image_too_large", "trop lourde"),
            ("unsupported_image_format", "format refusé"),
            ("invalid_image", "illisible"),
            ("image_download_failed", "introuvable"),
        ] {
            let body = json!({"error": {"code": 400, "message": "Provider returned error",
                "metadata": {"error_type": t, "provider_name": "Mistral"}}})
            .to_string();
            let e = LlmError::from_status(400, &body);
            assert_eq!(e.kind, LlmErrorKind::AttachmentRejected, "{t}");
            assert_eq!(e.attachment_motif(), motif, "{t}");
        }
        // En plein flux aussi.
        let e = LlmError::mid_stream("bad image".into(), false, Some("image_too_large".into()));
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
    }

    /// OpenRouter sans type canonique : le corps amont, dans `metadata.raw`, est lu.
    #[test]
    fn openrouter_wrapping_an_anthropic_body() {
        let raw = r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.2.content.1.image.source.base64: image exceeds 5 MB maximum: 7340032 bytes > 5242880 bytes"}}"#;
        let body = json!({"error": {"code": 400, "message": "Provider returned error",
            "metadata": {"raw": raw, "provider_name": "Anthropic"}}})
        .to_string();
        let e = LlmError::from_status(400, &body);
        assert_eq!(e.kind, LlmErrorKind::AttachmentRejected);
        assert_eq!(e.attachment_motif(), "trop lourde");
        assert!(e.message.contains("Anthropic"), "{}", e.message);
    }

    /// Ce qui marchait reste : un 400 sans rapport est `BadRequest`, un dépassement de
    /// fenêtre reste `ContextLength`, un 413 aussi, même s'il parle d'image.
    #[test]
    fn unrelated_errors_keep_their_kind() {
        for body in [
            "invalid tool schema",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"messages.3: `tool_use` ids were found without `tool_result` blocks immediately after"}}"#,
            r#"{"error":{"message":"Invalid value for 'content': expected a string, got null.","type":"invalid_request_error","param":"messages.[1].content","code":null}}"#,
            r#"{"error":{"message":"the image generation model is unavailable","code":400}}"#,
        ] {
            assert_eq!(kind(400, body), LlmErrorKind::BadRequest, "{body}");
        }
        assert_eq!(
            kind(400, "This model's maximum context length is 128000 tokens"),
            LlmErrorKind::ContextLength
        );
        assert_eq!(
            kind(
                413,
                r#"{"error":{"message":"image exceeds the request size"}}"#
            ),
            LlmErrorKind::ContextLength
        );
        assert!(!LlmErrorKind::AttachmentRejected.is_retryable());
    }

    /// Une image refusée devient sa mention, partout où elle revient ; les autres
    /// partent et sont rendues une fois chacune, dans l'ordre.
    #[test]
    fn rejected_images_are_replaced_by_their_mention() {
        use super::{RejectedImages, image_key};
        use crate::types::{ChatMessage, Content};
        let img = |url: &str| Content::ImageUrl {
            url: url.into(),
            detail: None,
        };
        let photo = |url: &str| ChatMessage {
            content: vec![Content::text("vois"), img(url)],
            ..ChatMessage::user("")
        };
        let mut messages = vec![photo("data:a"), photo("data:b"), photo("data:a")];
        let mut rejected = RejectedImages::default();
        assert!(rejected.is_empty());
        assert_eq!(
            rejected.clone().prepare(&mut messages.clone()),
            vec![image_key("data:a"), image_key("data:b")]
        );
        rejected.insert(&[image_key("data:a")], "trop lourde");
        assert_eq!(rejected.prepare(&mut messages), vec![image_key("data:b")]);
        let text = "[image retirée : refusée par le fournisseur (trop lourde)]";
        assert_eq!(messages[0].content[1], Content::text(text));
        assert_eq!(messages[2].content[1], Content::text(text));
        assert_eq!(messages[1].content[1], img("data:b"));
    }

    /// #242 : la limite est celle du fournisseur, mesurée sur l'image encodée.
    #[test]
    fn image_limits_follow_the_provider() {
        use super::{ImageLimits, encoded_len, image_limits};
        assert_eq!(
            image_limits("openrouter:anthropic/claude-sonnet-4.5"),
            ImageLimits::ANTHROPIC
        );
        assert_eq!(
            image_limits("anthropic/claude-opus-4"),
            ImageLimits::ANTHROPIC
        );
        assert_eq!(
            image_limits("deepseek/deepseek-v4-pro"),
            ImageLimits::DEFAULT
        );
        assert_eq!(image_limits("codex:gpt-5"), ImageLimits::DEFAULT);
        assert_eq!(encoded_len(3), 4);
        assert_eq!(encoded_len(4), 8);
        let a = ImageLimits::ANTHROPIC;
        // 4 Mo bruts pèsent 5,3 Mo en base64 : au-delà des 5 Mo d'Anthropic.
        assert!(!a.fits(4_000_000, Some(2_000)));
        assert!(a.fits(3_900_000, Some(2_000)));
        assert!(!a.fits(1_000, Some(8_001)));
        assert!(a.fits(1_000, None));
        // Une photo reçue (10 Mo au plus) passe telle quelle ailleurs.
        assert!(ImageLimits::DEFAULT.fits(10 * 1024 * 1024, Some(4_000)));
    }
}
