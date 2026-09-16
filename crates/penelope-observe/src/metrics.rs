//! Métriques au format d'exposition Prometheus (§16).
//!
//! Registre minimal : compteurs, jauges et histogrammes à buckets fixes. Servi sur
//! `127.0.0.1:9464` par le daemon, désactivable par configuration.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

type Labels = Vec<(String, String)>;

#[derive(Default)]
struct Registry {
    counters: BTreeMap<(String, String), f64>,
    gauges: BTreeMap<(String, String), f64>,
    histograms: BTreeMap<(String, String), Histogram>,
    help: BTreeMap<String, (&'static str, &'static str)>,
}

#[derive(Clone)]
struct Histogram {
    buckets: Vec<f64>,
    counts: Vec<u64>,
    sum: f64,
    total: u64,
}

impl Histogram {
    fn new(buckets: &[f64]) -> Self {
        Histogram {
            buckets: buckets.to_vec(),
            counts: vec![0; buckets.len()],
            sum: 0.0,
            total: 0,
        }
    }
    fn observe(&mut self, v: f64) {
        for (i, b) in self.buckets.iter().enumerate() {
            if v <= *b {
                self.counts[i] += 1;
            }
        }
        self.sum += v;
        self.total += 1;
    }
    /// Quantile approché à partir des buckets cumulés (suffisant pour p50/p95 §8.6).
    fn quantile(&self, q: f64) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let target = (self.total as f64 * q).ceil() as u64;
        for (i, c) in self.counts.iter().enumerate() {
            if *c >= target {
                return self.buckets[i];
            }
        }
        *self.buckets.last().unwrap_or(&0.0)
    }
}

/// Buckets de latence en millisecondes.
pub const LATENCY_BUCKETS_MS: &[f64] = &[
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10_000.0, 30_000.0,
    60_000.0, 300_000.0,
];

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

fn key(name: &str, labels: &Labels) -> (String, String) {
    let mut l: Labels = labels.clone();
    l.sort();
    let rendered = l
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
        .collect::<Vec<_>>()
        .join(",");
    (name.to_string(), rendered)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn with<T>(f: impl FnOnce(&mut Registry) -> T) -> T {
    let mut g = match registry().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    f(&mut g)
}

pub fn describe(name: &str, help: &'static str, unit: &'static str) {
    with(|r| {
        r.help.insert(name.to_string(), (help, unit));
    });
}

pub fn counter_inc(name: &str, labels: &[(&str, &str)], by: f64) {
    let l = own(labels);
    with(|r| {
        *r.counters.entry(key(name, &l)).or_insert(0.0) += by;
    });
}

pub fn gauge_set(name: &str, labels: &[(&str, &str)], v: f64) {
    let l = own(labels);
    with(|r| {
        r.gauges.insert(key(name, &l), v);
    });
}

pub fn histogram_observe(name: &str, labels: &[(&str, &str)], v: f64) {
    let l = own(labels);
    with(|r| {
        r.histograms
            .entry(key(name, &l))
            .or_insert_with(|| Histogram::new(LATENCY_BUCKETS_MS))
            .observe(v);
    });
}

/// Quantile approché d'un histogramme (utilisé pour les p50/p95 par serveur MCP).
pub fn histogram_quantile(name: &str, labels: &[(&str, &str)], q: f64) -> f64 {
    let l = own(labels);
    with(|r| {
        r.histograms
            .get(&key(name, &l))
            .map(|h| h.quantile(q))
            .unwrap_or(0.0)
    })
}

pub fn counter_value(name: &str, labels: &[(&str, &str)]) -> f64 {
    let l = own(labels);
    with(|r| r.counters.get(&key(name, &l)).copied().unwrap_or(0.0))
}

fn own(labels: &[(&str, &str)]) -> Labels {
    labels
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Rend le format texte Prometheus.
pub fn render() -> String {
    with(|r| {
        let mut out = String::new();
        let mut seen_help: std::collections::BTreeSet<String> = Default::default();

        for ((name, labels), v) in &r.counters {
            emit_help(&mut out, r, name, "counter", &mut seen_help);
            emit_sample(&mut out, name, labels, *v);
        }
        for ((name, labels), v) in &r.gauges {
            emit_help(&mut out, r, name, "gauge", &mut seen_help);
            emit_sample(&mut out, name, labels, *v);
        }
        for ((name, labels), h) in &r.histograms {
            emit_help(&mut out, r, name, "histogram", &mut seen_help);
            for (i, b) in h.buckets.iter().enumerate() {
                let l = join_labels(labels, &format!("le=\"{b}\""));
                out.push_str(&format!("{name}_bucket{{{l}}} {}\n", h.counts[i]));
            }
            let l = join_labels(labels, "le=\"+Inf\"");
            out.push_str(&format!("{name}_bucket{{{l}}} {}\n", h.total));
            emit_sample(&mut out, &format!("{name}_sum"), labels, h.sum);
            emit_sample(&mut out, &format!("{name}_count"), labels, h.total as f64);
        }
        out
    })
}

fn emit_help(
    out: &mut String,
    r: &Registry,
    name: &str,
    kind: &str,
    seen: &mut std::collections::BTreeSet<String>,
) {
    if seen.contains(name) {
        return;
    }
    if let Some((help, _unit)) = r.help.get(name) {
        out.push_str(&format!("# HELP {name} {help}\n"));
    }
    out.push_str(&format!("# TYPE {name} {kind}\n"));
    seen.insert(name.to_string());
}

fn emit_sample(out: &mut String, name: &str, labels: &str, v: f64) {
    if labels.is_empty() {
        out.push_str(&format!("{name} {v}\n"));
    } else {
        out.push_str(&format!("{name}{{{labels}}} {v}\n"));
    }
}

fn join_labels(base: &str, extra: &str) -> String {
    if base.is_empty() {
        extra.to_string()
    } else {
        format!("{base},{extra}")
    }
}

/// Métriques déclarées par Pénélope.
pub fn register_default_metrics() {
    describe("penelope_turns_total", "Tours traités", "");
    describe("penelope_turn_duration_ms", "Durée d'un tour", "ms");
    describe("penelope_llm_requests_total", "Requêtes LLM", "");
    describe("penelope_llm_tokens_total", "Tokens consommés", "");
    describe("penelope_llm_cost_usd_total", "Coût cumulé", "usd");
    describe("penelope_tool_calls_total", "Appels d'outils", "");
    describe("penelope_mcp_request_duration_ms", "Latence MCP", "ms");
    describe("penelope_mcp_servers_ready", "Serveurs MCP prêts", "");
    describe("penelope_approvals_pending", "Approbations en attente", "");
    describe("penelope_compactions_total", "Compactions", "");
    describe("penelope_effects_unknown", "Effets en état inconnu", "");
    describe("penelope_rss_bytes", "Mémoire résidente du daemon", "bytes");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_gauges_render() {
        counter_inc("test_counter_x", &[("kind", "a")], 2.0);
        counter_inc("test_counter_x", &[("kind", "a")], 1.0);
        gauge_set("test_gauge_x", &[], 42.0);
        let out = render();
        assert!(out.contains("test_counter_x{kind=\"a\"} 3"), "{out}");
        assert!(out.contains("test_gauge_x 42"), "{out}");
        assert_eq!(counter_value("test_counter_x", &[("kind", "a")]), 3.0);
    }

    #[test]
    fn histogram_quantiles_are_monotonic() {
        for v in [5.0, 20.0, 40.0, 900.0, 3000.0] {
            histogram_observe("test_hist_y", &[("server", "s")], v);
        }
        let p50 = histogram_quantile("test_hist_y", &[("server", "s")], 0.5);
        let p95 = histogram_quantile("test_hist_y", &[("server", "s")], 0.95);
        assert!(p50 <= p95, "p50={p50} p95={p95}");
        assert!(render().contains("test_hist_y_bucket"));
    }

    #[test]
    fn labels_are_escaped() {
        gauge_set("test_gauge_esc", &[("path", "a\"b")], 1.0);
        assert!(render().contains(r#"path="a\"b""#));
    }
}
