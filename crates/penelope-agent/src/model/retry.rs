//! Politique de nouvelles tentatives d'un appel au modèle, sans réseau ni horloge.
//!
//! `call_model` appelle le modèle ; `RetryPlan` décide seul de la suite d'une erreur :
//! rappeler le même modèle après une attente, passer au modèle de repli suivant, ou
//! abandonner. L'état (candidats, tentatives, attente cumulée, flux déjà relancé) tient
//! dans le plan, ce qui rend chaque décision testable par une table (issue #50, #5).

use penelope_llm::types::LlmError;

/// Attente maximale honorée pour un `Retry-After`.
pub(super) const RETRY_AFTER_MAX_SECS: u64 = 20;
/// Attente avant de relancer un flux coupé sans `Retry-After`.
pub(super) const STREAM_RETRY_SECS: u64 = 2;

/// Attente avant la n-ième nouvelle tentative d'avant flux : 1 s, 2 s, 4 s… (issue #50).
pub(super) fn retry_backoff_secs(done: u32) -> u64 {
    1u64 << done.min(4)
}

/// Où l'appel a échoué.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Avant le flux : la requête n'a pas abouti (5xx, délai de connexion, 429…).
    BeforeStream,
    /// Pendant le flux, avant que rien ne soit montré à l'utilisateur.
    InStream,
    /// Pendant le flux, après du texte ou un appel d'outil déjà montrés.
    AfterText,
}

/// Suite décidée après une erreur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryAction {
    /// Rappeler le même modèle après `wait_s` secondes.
    RetrySame { wait_s: u64 },
    /// Passer au modèle suivant de la liste.
    Fallback { model_id: String },
    /// Plus rien à essayer : l'erreur est rendue.
    GiveUp,
}

/// État des tentatives d'un appel au modèle.
///
/// Chez OpenRouter, les replis partent dans la requête et OpenRouter bascule lui-même
/// avant le premier jeton : les replis côté client ne s'ajoutent que si le fournisseur
/// lui-même est injoignable, ou si le flux est coupé.
#[derive(Debug, Clone)]
pub(crate) struct RetryPlan {
    candidates: Vec<String>,
    fallback_models: Vec<String>,
    server_side_fallback: bool,
    max_retries: u32,
    attempt: usize,
    retries: u32,
    waited_secs: u64,
    stream_retried: bool,
    client_fallbacks: bool,
}

impl RetryPlan {
    pub(crate) fn new(
        model_id: &str,
        fallback_models: &[String],
        server_side_fallback: bool,
        max_retries: u32,
    ) -> Self {
        let mut candidates = vec![model_id.to_string()];
        if !server_side_fallback {
            candidates.extend(fallback_models.iter().filter(|m| *m != model_id).cloned());
        }
        RetryPlan {
            candidates,
            fallback_models: fallback_models.to_vec(),
            server_side_fallback,
            max_retries,
            attempt: 0,
            retries: 0,
            waited_secs: 0,
            stream_retried: false,
            client_fallbacks: !server_side_fallback,
        }
    }

    /// Modèle de la tentative en cours.
    pub(crate) fn model(&self) -> &str {
        &self.candidates[self.attempt]
    }

    /// Vrai pour le modèle principal : lui seul garde le fournisseur amont collant.
    pub(crate) fn is_primary(&self) -> bool {
        self.attempt == 0
    }

    /// Replis confiés au serveur dans la requête (OpenRouter), sinon aucun.
    pub(crate) fn server_fallbacks(&self) -> Vec<String> {
        if self.server_side_fallback {
            self.fallback_models.clone()
        } else {
            Vec::new()
        }
    }

    /// Nouvelles tentatives d'avant flux faites sur le modèle en cours.
    pub(crate) fn retries(&self) -> u32 {
        self.retries
    }

    /// Attente cumulée entre les tentatives d'avant flux, en secondes.
    pub(crate) fn waited_secs(&self) -> u64 {
        self.waited_secs
    }

    /// Décide de la suite d'une erreur. `cancelled` : un arrêt a été demandé ; il interdit
    /// d'attendre, pas de changer de modèle avant le flux.
    pub(crate) fn on_error(&mut self, e: &LlmError, phase: Phase, cancelled: bool) -> RetryAction {
        let retryable = penelope_llm::Router::should_fallback(e);
        match phase {
            Phase::AfterText => RetryAction::GiveUp,
            Phase::BeforeStream => {
                // Incident passager (5xx, délai de connexion, limite de débit) : on rappelle
                // le même modèle après 1 s, 2 s, 4 s, ou après le `Retry-After` s'il est
                // court (issue #50). Tant qu'un autre modèle reste à essayer, une seule
                // attente : le repli coûte moins cher qu'une attente de plus. Sur le dernier
                // candidat, tout le budget de tentatives sert.
                let others_left = self.attempt + 1 < self.candidates.len()
                    || (!self.client_fallbacks
                        && self
                            .fallback_models
                            .iter()
                            .any(|m| !self.candidates.contains(m)));
                let budget = if others_left {
                    self.max_retries.min(1)
                } else {
                    self.max_retries
                };
                if retryable && self.retries < budget && !cancelled {
                    let secs = e
                        .retry_after
                        .filter(|s| *s <= RETRY_AFTER_MAX_SECS)
                        .unwrap_or_else(|| retry_backoff_secs(self.retries));
                    self.retries += 1;
                    self.waited_secs += secs;
                    return RetryAction::RetrySame { wait_s: secs };
                }
                // Le fournisseur lui-même est injoignable : son repli côté serveur ne joue
                // pas, on prend la main avec les replis d'alias.
                if retryable {
                    self.take_over_fallbacks();
                }
                if retryable && self.attempt + 1 < self.candidates.len() {
                    self.retries = 0;
                    self.attempt += 1;
                    return RetryAction::Fallback {
                        model_id: self.model().to_string(),
                    };
                }
                RetryAction::GiveUp
            }
            Phase::InStream => {
                // Coupure avant tout texte : un nouvel essai, puis les replis, côté client
                // même avec OpenRouter dont le repli ne joue qu'avant le flux (issue #5).
                if !retryable || cancelled {
                    return RetryAction::GiveUp;
                }
                if !self.stream_retried {
                    self.stream_retried = true;
                    let secs = e
                        .retry_after
                        .filter(|s| *s <= RETRY_AFTER_MAX_SECS)
                        .unwrap_or(STREAM_RETRY_SECS);
                    return RetryAction::RetrySame { wait_s: secs };
                }
                self.take_over_fallbacks();
                if self.attempt + 1 < self.candidates.len() {
                    self.attempt += 1;
                    return RetryAction::Fallback {
                        model_id: self.model().to_string(),
                    };
                }
                RetryAction::GiveUp
            }
        }
    }

    /// Ajoute les replis d'alias à la liste, une fois.
    fn take_over_fallbacks(&mut self) {
        if self.client_fallbacks {
            return;
        }
        self.client_fallbacks = true;
        for m in &self.fallback_models {
            if !self.candidates.contains(m) {
                self.candidates.push(m.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests;
