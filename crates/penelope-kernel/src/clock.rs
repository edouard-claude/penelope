//! Horloge injectable : indispensable aux suites déterministes (§20.1).

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Source de temps. Le code métier ne lit JAMAIS `SystemTime::now()` directement : les
/// suites `ctx-safety`, `mem-learning` et `workflow` avancent le temps à la main.
pub trait Clock: Send + Sync + 'static {
    /// Millisecondes depuis l'epoch Unix (UTC).
    fn now_ms(&self) -> i64;

    fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(self.now_ms()).unwrap_or_default()
    }

    fn now_rfc3339(&self) -> String {
        self.now_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}

/// Horloge système.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// Horloge pilotée par les tests.
#[derive(Debug, Clone)]
pub struct TestClock(Arc<AtomicI64>);

impl TestClock {
    pub fn new(start_ms: i64) -> Self {
        TestClock(Arc::new(AtomicI64::new(start_ms)))
    }
    pub fn advance_ms(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
    pub fn advance_secs(&self, s: i64) {
        self.advance_ms(s * 1000);
    }
    pub fn advance_hours(&self, h: i64) {
        self.advance_ms(h * 3_600_000);
    }
    pub fn advance_days(&self, d: i64) {
        self.advance_ms(d * 86_400_000);
    }
    pub fn set_ms(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Default for TestClock {
    fn default() -> Self {
        // 2026-01-01T00:00:00Z
        TestClock::new(1_767_225_600_000)
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub type SharedClock = Arc<dyn Clock>;

pub fn system_clock() -> SharedClock {
    Arc::new(SystemClock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_advances() {
        let c = TestClock::new(0);
        assert_eq!(c.now_ms(), 0);
        c.advance_ms(1500);
        assert_eq!(c.now_ms(), 1500);
    }

    #[test]
    fn rfc3339_formats() {
        let c = TestClock::new(1_767_225_600_000);
        assert_eq!(c.now_rfc3339(), "2026-01-01T00:00:00.000Z");
    }
}
