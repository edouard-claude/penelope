//! Détecteur de boucles (§11).
//!
//! Un même `(outil, args)` répété 3 fois, ou un va-et-vient A/B 3 fois, donne un
//! avertissement au modèle ; au-delà, le tour est arrêté avec un rapport.

use penelope_kernel::canonical::{canonical_json, sha256_hex};
use serde_json::Value;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopVerdict {
    /// Rien à signaler.
    Ok,
    /// Avertissement injecté dans le résultat d'outil.
    Warn(String),
    /// Le tour doit s'arrêter, avec un rapport.
    Abort(String),
}

impl LoopVerdict {
    pub fn is_abort(&self) -> bool {
        matches!(self, LoopVerdict::Abort(_))
    }
    pub fn message(&self) -> Option<&str> {
        match self {
            LoopVerdict::Warn(m) | LoopVerdict::Abort(m) => Some(m),
            LoopVerdict::Ok => None,
        }
    }
}

/// Fenêtre glissante des appels d'un tour.
#[derive(Debug, Clone)]
pub struct LoopDetector {
    window: VecDeque<(String, String)>,
    repeats_threshold: usize,
    capacity: usize,
    warned: bool,
    /// Appels refusés pour arguments invalides, par outil (issue #117).
    invalid: std::collections::BTreeMap<String, usize>,
}

impl Default for LoopDetector {
    fn default() -> Self {
        LoopDetector::new(3)
    }
}

impl LoopDetector {
    pub fn new(repeats_threshold: usize) -> Self {
        LoopDetector {
            window: VecDeque::new(),
            repeats_threshold: repeats_threshold.max(2),
            capacity: 40,
            warned: false,
            invalid: Default::default(),
        }
    }

    /// Un appel refusé pour arguments invalides, avant toute exécution (issue #117) : le
    /// deuxième sur le même outil avertit, le suivant arrête le tour, même si les
    /// arguments changent à chaque fois.
    pub fn observe_invalid(&mut self, tool: &str) -> LoopVerdict {
        let n = self.invalid.entry(tool.to_string()).or_default();
        *n += 1;
        if *n < 2 {
            return LoopVerdict::Ok;
        }
        let n = *n;
        self.escalate(format!(
            "`{tool}` a été appelé {n} fois avec des arguments invalides. Relis les paramètres \
             attendus avant de réessayer, ou demande au propriétaire."
        ))
    }

    /// Empreinte d'un appel : nom plus arguments canoniques.
    pub fn fingerprint(tool: &str, args: &Value) -> String {
        sha256_hex(format!("{tool}|{}", canonical_json(args)).as_bytes())
    }

    /// Enregistre un appel et rend le verdict.
    pub fn observe(&mut self, tool: &str, args: &Value) -> LoopVerdict {
        let fp = Self::fingerprint(tool, args);
        self.window.push_back((tool.to_string(), fp.clone()));
        if self.window.len() > self.capacity {
            self.window.pop_front();
        }

        // 1. Répétition stricte du même appel.
        let identical = self.window.iter().filter(|(_, f)| f == &fp).count();
        if identical >= self.repeats_threshold {
            return self.escalate(format!(
                "l'outil `{tool}` a été appelé {identical} fois avec exactement les mêmes \
                 arguments. Change d'approche : le résultat ne changera pas."
            ));
        }

        // 2. Va-et-vient A/B.
        if let Some(n) = self.alternation_length()
            && n >= self.repeats_threshold
        {
            return self.escalate(format!(
                "alternance détectée entre deux appels, répétée {n} fois. Arrête de \
                     faire l'aller-retour et tranche."
            ));
        }
        LoopVerdict::Ok
    }

    fn escalate(&mut self, message: String) -> LoopVerdict {
        if self.warned {
            LoopVerdict::Abort(format!(
                "{message} Tour arrêté par le détecteur de boucles."
            ))
        } else {
            self.warned = true;
            LoopVerdict::Warn(message)
        }
    }

    /// Longueur de l'alternance A/B/A/B… en fin de fenêtre, en nombre de cycles.
    fn alternation_length(&self) -> Option<usize> {
        let v: Vec<&String> = self.window.iter().map(|(_, f)| f).collect();
        if v.len() < 4 {
            return None;
        }
        let a = v[v.len() - 2];
        let b = v[v.len() - 1];
        if a == b {
            return None;
        }
        let mut cycles = 0;
        let mut i = v.len();
        while i >= 2 {
            let (x, y) = (v[i - 2], v[i - 1]);
            if x == a && y == b {
                cycles += 1;
                i -= 2;
            } else {
                break;
            }
        }
        (cycles >= 2).then_some(cycles)
    }

    /// Un nouveau message utilisateur remet le compteur à zéro.
    pub fn reset(&mut self) {
        self.window.clear();
        self.warned = false;
        self.invalid.clear();
    }

    pub fn calls_in_window(&self) -> usize {
        self.window.len()
    }

    /// Rapport final, joint au message d'arrêt.
    pub fn report(&self) -> String {
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for (tool, _) in &self.window {
            *counts.entry(tool.as_str()).or_insert(0) += 1;
        }
        let mut lines: Vec<String> = counts
            .into_iter()
            .map(|(t, n)| format!("- {t} : {n} appels"))
            .collect();
        lines.sort();
        format!("Appels du tour :\n{}", lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identical_calls_warn_then_abort() {
        let mut d = LoopDetector::new(3);
        let args = json!({"path":"a.rs"});
        assert_eq!(d.observe("fs_read", &args), LoopVerdict::Ok);
        assert_eq!(d.observe("fs_read", &args), LoopVerdict::Ok);
        let third = d.observe("fs_read", &args);
        assert!(matches!(third, LoopVerdict::Warn(_)), "{third:?}");
        assert!(third.message().unwrap().contains("mêmes arguments"));

        let fourth = d.observe("fs_read", &args);
        assert!(fourth.is_abort());
    }

    #[test]
    fn different_arguments_are_not_a_loop() {
        let mut d = LoopDetector::new(3);
        for i in 0..10 {
            assert_eq!(
                d.observe("fs_read", &json!({ "path": format!("f{i}.rs") })),
                LoopVerdict::Ok
            );
        }
    }

    #[test]
    fn argument_order_does_not_hide_a_loop() {
        let mut d = LoopDetector::new(3);
        d.observe("t", &json!({"a":1,"b":2}));
        d.observe("t", &json!({"b":2,"a":1}));
        let v = d.observe("t", &json!({"a":1,"b":2}));
        assert!(matches!(v, LoopVerdict::Warn(_)), "{v:?}");
    }

    #[test]
    fn alternation_is_detected() {
        let mut d = LoopDetector::new(3);
        let a = json!({"x":1});
        let b = json!({"x":2});
        // A B A B A B : trois cycles complets.
        d.observe("t", &a);
        d.observe("t", &b);
        d.observe("t", &a);
        let v = d.observe("t", &b);
        assert!(matches!(v, LoopVerdict::Warn(_) | LoopVerdict::Ok), "{v:?}");
        d.observe("t", &a);
        let v = d.observe("t", &b);
        assert!(
            matches!(v, LoopVerdict::Warn(_) | LoopVerdict::Abort(_)),
            "l'alternance doit finir par être signalée : {v:?}"
        );
    }

    #[test]
    fn reset_clears_the_window() {
        let mut d = LoopDetector::new(3);
        let args = json!({});
        d.observe("t", &args);
        d.observe("t", &args);
        d.reset();
        assert_eq!(d.calls_in_window(), 0);
        assert_eq!(d.observe("t", &args), LoopVerdict::Ok);
    }

    #[test]
    fn report_counts_calls_per_tool() {
        let mut d = LoopDetector::new(5);
        d.observe("fs_read", &json!({"p":1}));
        d.observe("fs_read", &json!({"p":2}));
        d.observe("shell_exec", &json!({"c":"ls"}));
        let r = d.report();
        assert!(r.contains("fs_read : 2 appels"));
        assert!(r.contains("shell_exec : 1 appels"));
    }

    #[test]
    fn window_is_bounded() {
        let mut d = LoopDetector::new(100);
        for i in 0..200 {
            d.observe("t", &json!({ "i": i }));
        }
        assert!(d.calls_in_window() <= 40);
    }
}
