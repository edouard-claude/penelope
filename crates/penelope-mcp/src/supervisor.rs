//! Cycle de vie et supervision des serveurs MCP (§8.6).
//!
//! États : `configured → connecting → ready → degraded → failed → disabled`, plus
//! `auth_required`. Redémarrage à backoff exponentiel (1 s à 5 min, jitter), passage en
//! `failed` après 8 échecs, **démarrage paresseux** et éviction LRU sous plafond de
//! processus : indispensable avec 80 serveurs.

use crate::config::ServerConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    Configured,
    Connecting,
    Ready,
    Degraded,
    Failed,
    Disabled,
    AuthRequired,
}

impl ServerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerState::Configured => "configured",
            ServerState::Connecting => "connecting",
            ServerState::Ready => "ready",
            ServerState::Degraded => "degraded",
            ServerState::Failed => "failed",
            ServerState::Disabled => "disabled",
            ServerState::AuthRequired => "auth_required",
        }
    }
    pub fn parse(s: &str) -> Option<ServerState> {
        Some(match s {
            "configured" => ServerState::Configured,
            "connecting" => ServerState::Connecting,
            "ready" => ServerState::Ready,
            "degraded" => ServerState::Degraded,
            "failed" => ServerState::Failed,
            "disabled" => ServerState::Disabled,
            "auth_required" => ServerState::AuthRequired,
            _ => return None,
        })
    }
    /// Un serveur utilisable pour un appel.
    pub fn is_usable(&self) -> bool {
        matches!(self, ServerState::Ready | ServerState::Degraded)
    }
}

/// Backoff exponentiel avec jitter : 1 s → 5 min (§8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub failures: u32,
    pub max_failures: u32,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            failures: 0,
            max_failures: 8,
        }
    }
}

impl Backoff {
    pub const BASE_MS: u64 = 1_000;
    pub const MAX_MS: u64 = 300_000;

    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }
    pub fn reset(&mut self) {
        self.failures = 0;
    }
    pub fn exhausted(&self) -> bool {
        self.failures >= self.max_failures
    }

    /// Délai avant la prochaine tentative, jitter compris (±20 %).
    pub fn delay_ms(&self) -> u64 {
        if self.failures == 0 {
            return 0;
        }
        let exp = Self::BASE_MS.saturating_mul(1u64 << (self.failures - 1).min(20));
        let base = exp.min(Self::MAX_MS);
        let jitter = jitter_ratio();
        ((base as f64) * jitter) as u64
    }

    /// Délai sans jitter, pour les tests déterministes.
    pub fn delay_ms_deterministic(&self) -> u64 {
        if self.failures == 0 {
            return 0;
        }
        Self::BASE_MS
            .saturating_mul(1u64 << (self.failures - 1).min(20))
            .min(Self::MAX_MS)
    }
}

fn jitter_ratio() -> f64 {
    let mut b = [0u8; 2];
    let _ = getrandom::getrandom(&mut b);
    let r = u16::from_le_bytes(b) as f64 / u16::MAX as f64; // [0,1]
    0.8 + r * 0.4 // [0.8, 1.2]
}

/// État observable d'un serveur.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub name: String,
    pub state: ServerState,
    pub transport: String,
    pub protocol: Option<String>,
    pub tool_count: usize,
    pub failures: u32,
    pub last_error: Option<String>,
    pub last_ok: Option<String>,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub calls: u64,
    pub errors: u64,
    pub running: bool,
    pub lazy: bool,
    /// Le processus du serveur joint le trousseau macOS (`sandbox.allow_keychain_for`, ou
    /// profil `full` autorisé).
    #[serde(default)]
    pub keychain: bool,
}

/// Décide quels serveurs `lazy_start` inactifs évincer pour respecter le plafond de
/// processus (§8.6).
///
/// Ordre d'éviction : les moins récemment utilisés d'abord ; un serveur non `lazy` n'est
/// jamais évincé.
pub fn evict_lru(
    running: &BTreeMap<String, (bool, i64)>,
    max_processes: usize,
    now_ms: i64,
    idle_after_ms: i64,
) -> Vec<String> {
    let mut candidates: Vec<(&String, i64)> = running
        .iter()
        .filter(|(_, (lazy, _))| *lazy)
        .map(|(n, (_, last_used))| (n, *last_used))
        .collect();
    candidates.sort_by_key(|(_, t)| *t);

    let mut out = Vec::new();

    // 1. Les inactifs depuis plus longtemps que `idle_timeout` s'arrêtent d'eux-mêmes.
    for (name, last) in &candidates {
        if now_ms - *last >= idle_after_ms {
            out.push((*name).clone());
        }
    }

    // 2. S'il reste trop de processus, on évince les plus anciens encore actifs.
    let remaining = running.len().saturating_sub(out.len());
    if remaining > max_processes {
        let excess = remaining - max_processes;
        for (name, _) in candidates.iter() {
            if out.len() >= excess + out.len().min(excess) && out.contains(name) {
                continue;
            }
            if out.contains(name) {
                continue;
            }
            out.push((*name).clone());
            if running.len() - out.len() <= max_processes {
                break;
            }
        }
    }
    out
}

/// Transitions autorisées.
pub fn next_state(current: ServerState, event: LifecycleEvent) -> ServerState {
    use LifecycleEvent::*;
    use ServerState::*;
    match (current, event) {
        (_, Disable) => Disabled,
        (Disabled, Enable) => Configured,
        (_, AuthNeeded) => AuthRequired,
        (AuthRequired, Authorised) => Connecting,
        (_, ConnectStarted) => Connecting,
        // `Connected` mène toujours à `Ready`, y compris depuis `Degraded`.
        (_, Connected) => Ready,
        (Ready, PartialFailure) => Degraded,
        (_, ConnectFailed { exhausted: true }) => Failed,
        (_, ConnectFailed { exhausted: false }) => Connecting,
        (_, Stopped) => Configured,
        (s, _) => s,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    ConnectStarted,
    Connected,
    PartialFailure,
    ConnectFailed { exhausted: bool },
    AuthNeeded,
    Authorised,
    Stopped,
    Disable,
    Enable,
}

/// Métriques par serveur (§8.6).
#[derive(Debug, Clone, Default)]
pub struct ServerMetrics {
    pub calls: u64,
    pub errors: u64,
    latencies: Vec<f64>,
}

impl ServerMetrics {
    pub fn record(&mut self, ms: f64, ok: bool) {
        self.calls += 1;
        if !ok {
            self.errors += 1;
        }
        if self.latencies.len() >= 1000 {
            self.latencies.remove(0);
        }
        self.latencies.push(ms);
    }

    pub fn quantile(&self, q: f64) -> f64 {
        if self.latencies.is_empty() {
            return 0.0;
        }
        let mut v = self.latencies.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((v.len() as f64 - 1.0) * q).round() as usize;
        v[idx.min(v.len() - 1)]
    }

    pub fn error_rate(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.errors as f64 / self.calls as f64
        }
    }
}

/// Faut-il démarrer ce serveur au boot ?
pub fn should_start_eagerly(cfg: &ServerConfig) -> bool {
    cfg.enabled && (!cfg.lazy_start || cfg.eager_schemas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let mut b = Backoff::default();
        assert_eq!(b.delay_ms_deterministic(), 0);
        let expected = [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000];
        for e in expected {
            b.record_failure();
            assert_eq!(b.delay_ms_deterministic(), e);
        }
        for _ in 0..10 {
            b.record_failure();
        }
        assert_eq!(b.delay_ms_deterministic(), Backoff::MAX_MS);
    }

    #[test]
    fn backoff_exhausts_after_eight_failures() {
        let mut b = Backoff::default();
        for _ in 0..7 {
            b.record_failure();
            assert!(!b.exhausted());
        }
        b.record_failure();
        assert!(b.exhausted(), "8 échecs consécutifs ⇒ failed");
        b.reset();
        assert!(!b.exhausted());
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let mut b = Backoff::default();
        b.record_failure();
        b.record_failure();
        for _ in 0..50 {
            let d = b.delay_ms();
            assert!((1_600..=2_400).contains(&d), "délai hors bornes : {d}");
        }
    }

    #[test]
    fn lifecycle_transitions() {
        use LifecycleEvent::*;
        use ServerState::*;
        assert_eq!(next_state(Configured, ConnectStarted), Connecting);
        assert_eq!(next_state(Connecting, Connected), Ready);
        assert_eq!(next_state(Ready, PartialFailure), Degraded);
        assert_eq!(next_state(Degraded, Connected), Ready);
        assert_eq!(
            next_state(Connecting, ConnectFailed { exhausted: false }),
            Connecting
        );
        assert_eq!(
            next_state(Connecting, ConnectFailed { exhausted: true }),
            Failed
        );
        assert_eq!(next_state(Ready, AuthNeeded), AuthRequired);
        assert_eq!(next_state(AuthRequired, Authorised), Connecting);
        assert_eq!(next_state(Ready, Disable), Disabled);
        assert_eq!(next_state(Disabled, Enable), Configured);
        // Un serveur désactivé ne se reconnecte pas tout seul.
        assert_eq!(next_state(Disabled, Connected), Ready);
    }

    #[test]
    fn usable_states() {
        assert!(ServerState::Ready.is_usable());
        assert!(ServerState::Degraded.is_usable());
        for s in [
            ServerState::Failed,
            ServerState::Disabled,
            ServerState::AuthRequired,
            ServerState::Connecting,
            ServerState::Configured,
        ] {
            assert!(!s.is_usable(), "{s:?}");
        }
    }

    #[test]
    fn idle_servers_are_stopped_first() {
        let now = 1_000_000;
        let mut running = BTreeMap::new();
        running.insert("a".to_string(), (true, now - 900_000)); // inactif 15 min
        running.insert("b".to_string(), (true, now - 60_000)); // actif
        running.insert("c".to_string(), (false, now - 900_000)); // non lazy : protégé

        let evicted = evict_lru(&running, 24, now, 600_000);
        assert_eq!(evicted, vec!["a"], "seul l'inactif lazy s'arrête");
    }

    #[test]
    fn process_cap_evicts_least_recently_used() {
        let now = 1_000_000;
        let mut running = BTreeMap::new();
        for i in 0..5 {
            running.insert(format!("s{i}"), (true, now - (i as i64) * 1_000));
        }
        // Plafond de 3 : deux serveurs doivent partir, les moins récemment utilisés.
        let evicted = evict_lru(&running, 3, now, 600_000);
        assert_eq!(evicted.len(), 2, "{evicted:?}");
        assert!(evicted.contains(&"s4".to_string()));
        assert!(evicted.contains(&"s3".to_string()));
    }

    #[test]
    fn non_lazy_servers_are_never_evicted() {
        let now = 1_000_000;
        let mut running = BTreeMap::new();
        for i in 0..5 {
            running.insert(format!("s{i}"), (false, now - (i as i64) * 1_000));
        }
        assert!(evict_lru(&running, 1, now, 600_000).is_empty());
    }

    #[test]
    fn metrics_quantiles() {
        let mut m = ServerMetrics::default();
        for i in 1..=100 {
            m.record(i as f64, i % 10 != 0);
        }
        assert_eq!(m.calls, 100);
        assert_eq!(m.errors, 10);
        assert!((m.error_rate() - 0.1).abs() < 1e-9);
        assert!((m.quantile(0.5) - 50.0).abs() <= 1.0);
        assert!((m.quantile(0.95) - 95.0).abs() <= 1.0);
    }

    #[test]
    fn eager_start_rules() {
        let mut c = ServerConfig::stdio("s", "cmd", &[]);
        c.lazy_start = true;
        assert!(
            !should_start_eagerly(&c),
            "lazy par défaut : démarrage différé"
        );
        c.eager_schemas = true;
        assert!(
            should_start_eagerly(&c),
            "un serveur à schémas eager doit être prêt pour construire T1"
        );
        c.eager_schemas = false;
        c.lazy_start = false;
        assert!(should_start_eagerly(&c));
        c.enabled = false;
        assert!(!should_start_eagerly(&c));
    }
}
