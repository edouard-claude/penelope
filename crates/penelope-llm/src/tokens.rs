//! Comptabilité des tokens (§5.3).
//!
//! Principe : **la vérité vient du provider**. L'estimateur local ne sert qu'à mesurer le
//! *delta* ajouté depuis la dernière réponse connue (l'« ancre »), et se calibre tout seul
//! sur l'écart constaté, par `model@provider`, en moyenne glissante.

use crate::types::{ChatMessage, Content, Usage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

/// Ancre d'usage : dernier comptage réel, plus l'empreinte du transcript à ce moment.
///
/// Persistée sur la session : elle survit au redémarrage du daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UsageAnchor {
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    /// Numéro de séquence du dernier message couvert par l'ancre.
    pub up_to_seq: i64,
    /// Empreinte du transcript couvert : détecte un `rewind` ou une compaction.
    pub fingerprint: String,
    pub model: String,
}

impl UsageAnchor {
    pub fn from_usage(u: &Usage, up_to_seq: i64, fingerprint: String, model: String) -> Self {
        UsageAnchor {
            prompt: u.prompt,
            completion: u.completion,
            cached: u.cached,
            up_to_seq,
            fingerprint,
            model,
        }
    }

    /// Tokens d'entrée du prochain appel si rien n'était ajouté.
    pub fn base_prompt(&self) -> u64 {
        // L'entrée du prochain appel contient l'entrée précédente **plus** la sortie
        // précédente, qui est devenue un message d'historique.
        self.prompt + self.completion
    }
}

/// Estimation locale, calibrée par modèle.
#[derive(Clone)]
pub struct TokenEstimator {
    /// Facteur multiplicatif par `model@provider`, moyenne glissante.
    calibration: Arc<RwLock<BTreeMap<String, Calibration>>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Calibration {
    pub factor: f64,
    pub samples: u32,
    /// Coût moyen observé d'une image, en tokens.
    pub image_tokens: f64,
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration {
            factor: 1.0,
            samples: 0,
            image_tokens: 800.0,
        }
    }
}

impl Default for TokenEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenEstimator {
    pub fn new() -> Self {
        TokenEstimator {
            calibration: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub fn calibration_of(&self, model: &str) -> Calibration {
        self.calibration
            .read()
            .ok()
            .and_then(|g| g.get(model).copied())
            .unwrap_or_default()
    }

    /// Enregistre un écart observé entre estimation et comptage réel.
    ///
    /// Moyenne glissante bornée : un seul appel aberrant ne peut pas décaler le facteur
    /// de plus de 20 %, et le facteur reste dans [0,5 ; 2,0].
    pub fn calibrate(&self, model: &str, estimated: u64, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let observed = actual as f64 / estimated as f64;
        let Ok(mut g) = self.calibration.write() else {
            return;
        };
        let c = g.entry(model.to_string()).or_default();
        let weight = (1.0 / (c.samples as f64 + 2.0)).clamp(0.05, 0.2);
        c.factor = (c.factor * (1.0 - weight) + observed * weight).clamp(0.5, 2.0);
        c.samples = c.samples.saturating_add(1);
    }

    pub fn set_image_tokens(&self, model: &str, tokens: f64) {
        if let Ok(mut g) = self.calibration.write() {
            g.entry(model.to_string()).or_default().image_tokens = tokens.clamp(1.0, 20_000.0);
        }
    }

    /// Estimation brute d'un texte, avant calibration.
    pub fn raw_text_tokens(text: &str) -> u64 {
        if text.is_empty() {
            return 0;
        }
        // Heuristique : ~3,6 caractères par token en français et en anglais, ~3,0 pour du
        // code dense (beaucoup de ponctuation, peu d'espaces). On mesure la densité de
        // caractères non alphanumériques pour arbitrer.
        let chars = text.chars().count() as f64;
        let punct = text
            .chars()
            .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
            .count() as f64;
        let ratio = if chars > 0.0 { punct / chars } else { 0.0 };
        let per_token = if ratio > 0.18 { 3.0 } else { 3.6 };
        // Les caractères non ASCII coûtent davantage (multi-octets).
        let non_ascii = text.chars().filter(|c| !c.is_ascii()).count() as f64;
        let adjusted = chars + non_ascii * 0.5;
        (adjusted / per_token).ceil() as u64
    }

    pub fn text_tokens(&self, model: &str, text: &str) -> u64 {
        let raw = Self::raw_text_tokens(text) as f64;
        (raw * self.calibration_of(model).factor).ceil() as u64
    }

    /// Estimation d'un message complet, surcharge de protocole comprise.
    pub fn message_tokens(&self, model: &str, m: &ChatMessage) -> u64 {
        let cal = self.calibration_of(model);
        let mut raw = 4.0; // enveloppe de rôle et séparateurs
        for c in &m.content {
            match c {
                Content::Text { text } => raw += Self::raw_text_tokens(text) as f64,
                Content::ImageUrl { .. } => raw += cal.image_tokens,
                Content::InputAudio { data, .. } => {
                    raw += (data.len() as f64 / 1000.0).ceil();
                }
            }
        }
        for tc in &m.tool_calls {
            raw += 8.0;
            raw += Self::raw_text_tokens(&tc.name) as f64;
            raw += Self::raw_text_tokens(&tc.arguments.to_string()) as f64;
        }
        if m.tool_call_id.is_some() {
            raw += 6.0;
        }
        (raw * cal.factor).ceil() as u64
    }

    pub fn messages_tokens(&self, model: &str, msgs: &[ChatMessage]) -> u64 {
        msgs.iter().map(|m| self.message_tokens(model, m)).sum()
    }

    /// Estimation de la taille d'une définition d'outil injectée dans le prompt.
    pub fn tool_tokens(&self, model: &str, t: &crate::types::ToolDef) -> u64 {
        let raw = Self::raw_text_tokens(&t.name) as f64
            + Self::raw_text_tokens(&t.description) as f64
            + Self::raw_text_tokens(&t.parameters.to_string()) as f64
            + 10.0;
        (raw * self.calibration_of(model).factor).ceil() as u64
    }
}

/// État d'usage d'une session : ancre + delta estimé depuis l'ancre.
#[derive(Debug, Clone, Default)]
pub struct UsageState {
    pub anchor: Option<UsageAnchor>,
    pub delta_estimate: u64,
}

impl UsageState {
    /// Estimation courante des tokens d'entrée du prochain appel.
    pub fn estimated_prompt(&self) -> u64 {
        match &self.anchor {
            Some(a) => a.base_prompt() + self.delta_estimate,
            None => self.delta_estimate,
        }
    }

    /// Taux de remplissage de la fenêtre.
    pub fn usage_ratio(&self, window: u64) -> f64 {
        if window == 0 {
            return 0.0;
        }
        self.estimated_prompt() as f64 / window as f64
    }

    /// Vrai si aucune preuve du provider n'a encore été reçue : dans ce cas, le PRD
    /// demande d'attendre une requête pour obtenir la preuve avant de compacter, sauf si
    /// l'estimation dépasse la fenêtre maximale annoncée (§5.3).
    pub fn is_unanchored(&self) -> bool {
        self.anchor.is_none()
    }

    pub fn should_wait_for_proof(&self, window: u64) -> bool {
        self.is_unanchored() && self.estimated_prompt() < window
    }

    /// Met à jour l'ancre après une réponse réelle.
    pub fn anchor_on(&mut self, anchor: UsageAnchor) {
        self.anchor = Some(anchor);
        self.delta_estimate = 0;
    }

    /// Ajoute le coût estimé d'un message ajouté depuis l'ancre.
    pub fn add_delta(&mut self, tokens: u64) {
        self.delta_estimate = self.delta_estimate.saturating_add(tokens);
    }

    /// Invalide l'ancre : le transcript a changé sous elle (rewind, compaction).
    pub fn invalidate_if_fingerprint_changed(&mut self, current: &str) {
        if let Some(a) = &self.anchor {
            if a.fingerprint != current {
                self.anchor = None;
            }
        }
    }
}

/// Empreinte d'un transcript : détecte tout changement structurel sous l'ancre.
pub fn transcript_fingerprint(msgs: &[ChatMessage]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for m in msgs {
        h.update(m.role.as_str().as_bytes());
        h.update([0u8]);
        h.update(m.text().as_bytes());
        h.update([0u8]);
        for tc in &m.tool_calls {
            h.update(tc.name.as_bytes());
            h.update(tc.arguments.to_string().as_bytes());
        }
        h.update([1u8]);
    }
    let d = h.finalize();
    d.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_zero() {
        assert_eq!(TokenEstimator::raw_text_tokens(""), 0);
    }

    #[test]
    fn prose_estimate_is_in_a_plausible_range() {
        let prose = "Le déploiement de la nouvelle version a été validé par l'équipe hier soir.";
        let t = TokenEstimator::raw_text_tokens(prose);
        // ~74 caractères : entre 15 et 30 tokens est raisonnable.
        assert!((15..=30).contains(&t), "estimation aberrante : {t}");
    }

    #[test]
    fn code_is_denser_than_prose() {
        let code = "fn main(){let x=vec![1,2,3];println!(\"{:?}\",x);}";
        let prose = "une phrase de longueur comparable en francais courant ici";
        let c = TokenEstimator::raw_text_tokens(code) as f64 / code.len() as f64;
        let p = TokenEstimator::raw_text_tokens(prose) as f64 / prose.len() as f64;
        assert!(c > p, "le code doit coûter plus par caractère : {c} vs {p}");
    }

    #[test]
    fn calibration_converges_towards_reality() {
        let e = TokenEstimator::new();
        let model = "openrouter:m";
        for _ in 0..40 {
            // Le provider compte systématiquement 20 % de plus que l'estimation.
            e.calibrate(model, 1000, 1200);
        }
        let f = e.calibration_of(model).factor;
        assert!((f - 1.2).abs() < 0.05, "facteur mal calibré : {f}");
    }

    #[test]
    fn calibration_is_bounded() {
        let e = TokenEstimator::new();
        for _ in 0..200 {
            e.calibrate("m", 10, 100_000);
        }
        assert!(e.calibration_of("m").factor <= 2.0);
        for _ in 0..200 {
            e.calibrate("m2", 100_000, 1);
        }
        assert!(e.calibration_of("m2").factor >= 0.5);
    }

    #[test]
    fn calibration_ignores_degenerate_samples() {
        let e = TokenEstimator::new();
        e.calibrate("m", 0, 100);
        e.calibrate("m", 100, 0);
        assert_eq!(e.calibration_of("m").samples, 0);
    }

    #[test]
    fn message_tokens_include_tool_calls() {
        let e = TokenEstimator::new();
        let plain = ChatMessage::assistant("ok");
        let with_call =
            ChatMessage::assistant("ok").with_tool_calls(vec![crate::types::ToolCall {
                id: "c".into(),
                name: "fs_read".into(),
                arguments: serde_json::json!({"path":"src/main.rs"}),
            }]);
        assert!(e.message_tokens("m", &with_call) > e.message_tokens("m", &plain));
    }

    #[test]
    fn images_use_the_learned_cost() {
        let e = TokenEstimator::new();
        let m = ChatMessage {
            role: crate::types::Role::User,
            content: vec![Content::ImageUrl {
                url: "data:image/png;base64,AA".into(),
                detail: None,
            }],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            cache_marker: false,
            reasoning: None,
            reasoning_details: None,
        };
        let before = e.message_tokens("m", &m);
        e.set_image_tokens("m", 2000.0);
        let after = e.message_tokens("m", &m);
        assert!(after > before);
    }

    #[test]
    fn anchor_accounts_for_previous_completion() {
        let a = UsageAnchor {
            prompt: 10_000,
            completion: 500,
            cached: 8_000,
            up_to_seq: 12,
            fingerprint: "f".into(),
            model: "m".into(),
        };
        assert_eq!(a.base_prompt(), 10_500);
    }

    #[test]
    fn usage_state_tracks_delta_and_ratio() {
        let mut s = UsageState::default();
        assert!(s.is_unanchored());
        s.anchor_on(UsageAnchor {
            prompt: 90_000,
            completion: 1_000,
            cached: 0,
            up_to_seq: 3,
            fingerprint: "f".into(),
            model: "m".into(),
        });
        assert_eq!(s.estimated_prompt(), 91_000);
        s.add_delta(4_000);
        assert_eq!(s.estimated_prompt(), 95_000);
        assert!((s.usage_ratio(100_000) - 0.95).abs() < 1e-9);
    }

    #[test]
    fn anchor_is_invalidated_when_transcript_changes() {
        let mut s = UsageState::default();
        s.anchor_on(UsageAnchor {
            fingerprint: "abc".into(),
            ..Default::default()
        });
        s.invalidate_if_fingerprint_changed("abc");
        assert!(s.anchor.is_some());
        s.invalidate_if_fingerprint_changed("xyz");
        assert!(s.anchor.is_none(), "un rewind doit invalider l'ancre");
    }

    #[test]
    fn unanchored_waits_for_proof_unless_over_window() {
        let mut s = UsageState::default();
        s.add_delta(50_000);
        assert!(s.should_wait_for_proof(200_000));
        s.add_delta(200_000);
        assert!(
            !s.should_wait_for_proof(200_000),
            "au-delà de la fenêtre annoncée, on n'attend plus"
        );
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let a = vec![ChatMessage::user("a"), ChatMessage::assistant("b")];
        let b = vec![ChatMessage::user("a"), ChatMessage::assistant("c")];
        assert_ne!(transcript_fingerprint(&a), transcript_fingerprint(&b));
        assert_eq!(
            transcript_fingerprint(&a),
            transcript_fingerprint(&a.clone())
        );
    }
}
