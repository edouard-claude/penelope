use super::*;
use penelope_llm::types::LlmErrorKind;

use Phase::{AfterText, BeforeStream, InStream};
use RetryAction::{Fallback, GiveUp, RetrySame};

/// Une erreur de la table : genre, `Retry-After`, phase, arrêt demandé.
type Step = (LlmErrorKind, Option<u64>, Phase, bool);

const T: LlmErrorKind = LlmErrorKind::Transient;

fn err(kind: LlmErrorKind, retry_after: Option<u64>) -> LlmError {
    let mut e = LlmError::new(kind, "incident");
    e.retry_after = retry_after;
    e
}

fn fallback(m: &str) -> RetryAction {
    Fallback {
        model_id: m.to_string(),
    }
}

fn wait(s: u64) -> RetryAction {
    RetrySame { wait_s: s }
}

/// Rejoue une suite d'erreurs sur un plan neuf et rend les actions décidées.
fn replay(
    fallbacks: &[&str],
    openrouter: bool,
    max_retries: u32,
    steps: &[Step],
) -> (RetryPlan, Vec<RetryAction>) {
    let fallbacks: Vec<String> = fallbacks.iter().map(|s| s.to_string()).collect();
    let mut plan = RetryPlan::new("m/a", &fallbacks, openrouter, max_retries);
    let actions = steps
        .iter()
        .map(|(kind, after, phase, cancelled)| {
            plan.on_error(&err(kind.clone(), *after), *phase, *cancelled)
        })
        .collect();
    (plan, actions)
}

/// La table de vérité : (cas, replis, OpenRouter, tentatives permises, erreurs) → actions.
#[test]
fn the_retry_table_holds() {
    type Row = (
        &'static str,
        &'static [&'static str],
        bool,
        u32,
        Vec<Step>,
        Vec<RetryAction>,
    );
    let rows: Vec<Row> = vec![
        (
            "avant flux, un repli reste : une seule attente, puis le repli",
            &["m/b"],
            false,
            3,
            vec![(T, None, BeforeStream, false); 2],
            vec![wait(1), fallback("m/b")],
        ),
        (
            "avant flux, dernier candidat : tout le budget, attente doublée, puis abandon",
            &[],
            false,
            3,
            vec![(T, None, BeforeStream, false); 4],
            vec![wait(1), wait(2), wait(4), GiveUp],
        ),
        (
            "le repli, devenu dernier candidat, a droit à tout le budget",
            &["m/b"],
            false,
            3,
            vec![(T, None, BeforeStream, false); 6],
            vec![wait(1), fallback("m/b"), wait(1), wait(2), wait(4), GiveUp],
        ),
        (
            "erreur non passagère : ni attente ni repli",
            &["m/b"],
            false,
            3,
            vec![(LlmErrorKind::Auth, None, BeforeStream, false)],
            vec![GiveUp],
        ),
        (
            "modèle inconnu : repli immédiat s'il n'y a plus d'attente permise",
            &["m/b"],
            false,
            0,
            vec![(LlmErrorKind::UnknownModel, None, BeforeStream, false)],
            vec![fallback("m/b")],
        ),
        (
            "Retry-After court honoré, long remplacé par l'attente doublée",
            &[],
            false,
            3,
            vec![
                (LlmErrorKind::RateLimited, Some(5), BeforeStream, false),
                (LlmErrorKind::RateLimited, Some(60), BeforeStream, false),
            ],
            vec![wait(5), wait(2)],
        ),
        (
            "arrêt demandé avant flux : pas d'attente, le repli reste permis",
            &["m/b"],
            false,
            3,
            vec![(T, None, BeforeStream, true)],
            vec![fallback("m/b")],
        ),
        (
            "arrêt demandé avant flux sans repli : abandon",
            &[],
            false,
            3,
            vec![(T, None, BeforeStream, true)],
            vec![GiveUp],
        ),
        (
            "OpenRouter : une attente, puis les replis d'alias pris en main",
            &["m/b"],
            true,
            3,
            vec![(T, None, BeforeStream, false); 2],
            vec![wait(1), fallback("m/b")],
        ),
        (
            "OpenRouter sans repli : tout le budget sur le modèle principal",
            &[],
            true,
            3,
            vec![(T, None, BeforeStream, false); 4],
            vec![wait(1), wait(2), wait(4), GiveUp],
        ),
        (
            "flux coupé avant texte : un nouvel essai, puis repli, puis abandon",
            &["m/b"],
            false,
            3,
            vec![(T, None, InStream, false); 3],
            vec![wait(2), fallback("m/b"), GiveUp],
        ),
        (
            "flux coupé chez OpenRouter : repli côté client malgré tout",
            &["m/b"],
            true,
            3,
            vec![(T, None, InStream, false); 2],
            vec![wait(2), fallback("m/b")],
        ),
        (
            "flux coupé avec Retry-After court",
            &[],
            false,
            3,
            vec![(LlmErrorKind::RateLimited, Some(7), InStream, false)],
            vec![wait(7)],
        ),
        (
            "flux coupé : erreur non passagère ou arrêt demandé, abandon",
            &["m/b"],
            false,
            3,
            vec![
                (LlmErrorKind::ContentFilter, None, InStream, false),
                (T, None, InStream, true),
            ],
            vec![GiveUp, GiveUp],
        ),
        (
            "après du texte : jamais de repli silencieux",
            &["m/b"],
            false,
            3,
            vec![(T, None, AfterText, false)],
            vec![GiveUp],
        ),
    ];
    for (case, fallbacks, openrouter, max, steps, expected) in rows {
        let (_, actions) = replay(fallbacks, openrouter, max, &steps);
        assert_eq!(actions, expected, "{case}");
    }
}

#[test]
fn a_fallback_restarts_the_count_but_not_the_waiting() {
    let (plan, _) = replay(&["m/b"], false, 3, &vec![(T, None, BeforeStream, false); 3]);
    assert_eq!(plan.model(), "m/b");
    assert!(!plan.is_primary());
    assert_eq!(plan.retries(), 1);
    assert_eq!(plan.waited_secs(), 2);
}

#[test]
fn the_primary_model_is_not_its_own_fallback() {
    let fallbacks = vec!["m/a".to_string(), "m/b".to_string()];
    let mut plan = RetryPlan::new("m/a", &fallbacks, false, 0);
    assert!(plan.is_primary());
    assert!(plan.server_fallbacks().is_empty());
    assert_eq!(
        plan.on_error(&err(T, None), BeforeStream, false),
        fallback("m/b")
    );
    assert_eq!(plan.on_error(&err(T, None), BeforeStream, false), GiveUp);
}

#[test]
fn openrouter_carries_the_fallbacks_in_the_request() {
    let plan = RetryPlan::new("m/a", &["m/b".to_string()], true, 3);
    assert_eq!(plan.server_fallbacks(), vec!["m/b".to_string()]);
}
