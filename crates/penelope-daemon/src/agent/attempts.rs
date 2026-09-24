//! Tentatives hors surface : ce qu'un appel au modèle a coûté sans donner de réponse
//! (issue #206, épopée #208, tâche T9).
//!
//! Un flux coupé après du texte, une erreur d'avant flux, un repli, une réponse vide
//! relancée : chacun devient un `conv.attempt` du journal, qui n'entre jamais dans la
//! surface (`design/v1/source-de-verite.md` §2.2). Seule la consigne de relance d'une
//! réponse vide est lue par la requête suivante du même tour ; en phase 1 elle reste
//! ajoutée à la main par la boucle, avec le même texte que le pliage.
//!
//! Aucune ligne d'usage n'est écrite ici : une réponse vide est déjà comptée par
//! `budget.record`, et une tentative échouée n'a pas d'usage connu.

use super::*;
use penelope_app::journal::{AttemptCause, AttemptPayload, ConvEvent};
use std::sync::atomic::{AtomicU32, Ordering};

/// Tentatives gardées par tour : au-delà, elles ne sont plus que dans les journaux
/// du daemon. Une boucle de replis ne remplit pas la base.
pub const MAX_ATTEMPTS_PER_TURN: u32 = 10;

/// Consigne ajoutée à la requête qui suit une réponse vide. Vue du modèle seulement :
/// elle est dans le `conv.attempt`, jamais dans l'historique.
pub const EMPTY_RETRY_PROMPT: &str = "(Relance automatique : ta réponse précédente était vide. \
     Réponds maintenant, en texte, au dernier message.)";

/// Caractères du partiel cités au propriétaire et dans les journaux.
const EXCERPT_CHARS: usize = 200;

/// Les tentatives d'une exécution de la boucle : l'étape en cours, la consigne de
/// relance en vigueur et le compte, pour le plafond.
#[derive(Default)]
pub(super) struct Attempts {
    step: AtomicU32,
    recorded: AtomicU32,
    retry_prompt: Mutex<Option<&'static str>>,
}

impl Attempts {
    /// Étape (itération + 1) de l'appel qui part.
    pub(super) fn at_step(&self, step: u32) {
        self.step.store(step, Ordering::SeqCst);
    }

    /// La consigne que les requêtes suivantes ajoutent, jusqu'à une réponse écrite.
    pub(super) fn set_retry_prompt(&self, prompt: Option<&'static str>) {
        *self.retry_prompt.lock().unwrap_or_else(|p| p.into_inner()) = prompt;
    }

    pub(super) fn retry_prompt(&self) -> Option<&'static str> {
        *self.retry_prompt.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Journalise une tentative. Le partiel et l'erreur sont rédigés (#134). Au-delà du
    /// plafond, seule la ligne de journal reste. Un échec d'écriture ne change pas
    /// l'issue de l'appel : il ne coûte que la trace.
    pub(super) async fn record(&self, s: &AgentServices, spec: &TurnSpec, mut p: AttemptPayload) {
        p.turn = spec.turn_id.clone();
        p.step = self.step.load(Ordering::SeqCst);
        // La requête qui suit ajoute encore la consigne : le pliage doit la retrouver
        // sur la dernière tentative, quelle qu'en soit la cause.
        if p.retry_prompt.is_none() {
            p.retry_prompt = self.retry_prompt().map(String::from);
        }
        let redact = |t: &mut Option<String>| {
            if let Some(v) = t.as_mut() {
                *v = penelope_observe::redact(v);
            }
        };
        redact(&mut p.partial_text);
        redact(&mut p.partial_reasoning);
        redact(&mut p.error);
        tracing::warn!(
            turn = spec.turn_id.as_deref(),
            session = %spec.session_id,
            step = p.step,
            cause = p.cause.as_str(),
            model = p.model.as_deref(),
            provider = p.provider.as_deref(),
            upstream = p.upstream.as_deref(),
            error = p.error.as_deref(),
            text = p.partial_text.as_deref().map(excerpt).as_deref(),
            "tentative sans réponse"
        );
        let n = self.recorded.fetch_add(1, Ordering::SeqCst);
        if n >= MAX_ATTEMPTS_PER_TURN {
            if n == MAX_ATTEMPTS_PER_TURN {
                tracing::warn!(
                    session = %spec.session_id,
                    max = MAX_ATTEMPTS_PER_TURN,
                    "plafond de tentatives du tour atteint : les suivantes ne sont plus journalisées"
                );
            }
            return;
        }
        let event = ConvEvent::Attempt(p);
        if let Err(e) = s
            .events
            .append(EventDraft::new(event.kind(), event.payload()).session(&spec.session_id))
            .await
        {
            tracing::warn!(session = %spec.session_id, error = %e, "tentative non journalisée");
        }
    }
}

/// Ce qu'un flux a déjà livré, texte et raisonnement : le partiel d'une tentative, s'il
/// casse.
#[derive(Default)]
pub(super) struct Partial(Mutex<(String, String)>);

impl Partial {
    pub(super) fn text(&self, t: &str) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .push_str(t);
    }

    pub(super) fn reasoning(&self, t: &str) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .1
            .push_str(t);
    }

    /// Verse le partiel dans la tentative ; rend le texte, que le propriétaire a vu.
    pub(super) fn into_attempt(self, attempt: &mut AttemptPayload) -> String {
        let (text, reasoning) = self.0.into_inner().unwrap_or_else(|p| p.into_inner());
        attempt.partial_text = (!text.is_empty()).then(|| text.clone());
        attempt.partial_reasoning = (!reasoning.is_empty()).then_some(reasoning);
        text
    }
}

/// Cause d'une tentative échouée : un repli quand la suite change de modèle, sinon le
/// moment de l'échec.
pub(super) fn failure_cause(streamed: bool, fell_back: bool) -> AttemptCause {
    match (fell_back, streamed) {
        (true, _) => AttemptCause::Fallback,
        (false, true) => AttemptCause::StreamCut,
        (false, false) => AttemptCause::BeforeStream,
    }
}

/// Le début d'un partiel, sur une ligne, borné.
pub(super) fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(EXCERPT_CHARS).collect();
    cut.push('…');
    cut
}

/// Ce qui est dit au propriétaire d'une réponse coupée en cours d'écriture : le début
/// reçu, cité, et où il est gardé.
pub(super) fn stream_cut_message(error: &str, partial: &str) -> String {
    let kept = excerpt(&penelope_observe::redact(partial));
    format!(
        "{error}\n\nLa réponse a été coupée en cours d'écriture. Son début est gardé dans le \
         journal du tour, hors de la conversation : « {kept} »"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_partial_is_cited_by_its_beginning() {
        let long = "mot ".repeat(100);
        let cited = excerpt(&long);
        assert_eq!(cited.chars().count(), EXCERPT_CHARS + 1);
        assert!(cited.ends_with('…'));
        assert_eq!(excerpt("  deux\nlignes "), "deux lignes");
        let msg = stream_cut_message("provider indisponible", "Voici le début");
        assert!(msg.contains("coupée en cours d'écriture"), "{msg}");
        assert!(msg.contains("« Voici le début »"), "{msg}");
    }

    #[test]
    fn a_fallback_names_the_attempt_whatever_the_phase() {
        assert_eq!(failure_cause(false, false), AttemptCause::BeforeStream);
        assert_eq!(failure_cause(true, false), AttemptCause::StreamCut);
        assert_eq!(failure_cause(false, true), AttemptCause::Fallback);
        assert_eq!(failure_cause(true, true), AttemptCause::Fallback);
    }
}
