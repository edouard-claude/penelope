//! Assertion anti-veille (§2.8).
//!
//! Le daemon tient une assertion tant qu'un run ou un tour est actif, et la relâche au
//! repos. L'assertion est **comptée** : plusieurs tours simultanés ne créent qu'un seul
//! inhibiteur, relâché au dernier.
//!
//! macOS : `/usr/bin/caffeinate -i` maintenu comme processus enfant. Décision
//! d'architecture : l'API `IOPMAssertionCreateWithName` exigerait du FFI `unsafe` dans un
//! workspace qui interdit `unsafe` (§3.2) ; `caffeinate` est l'outil système documenté
//! pour exactement cet usage et se relâche tout seul si le daemon meurt.

use crate::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub trait PowerManager: Send + Sync {
    fn mechanism(&self) -> &'static str;
    /// Prend une assertion ; renvoie un garde qui la relâche à la destruction.
    fn prevent_sleep(&self, reason: &str) -> Result<SleepGuard>;
    /// Nombre d'assertions actives.
    fn active(&self) -> u32;
}

/// Garde RAII : relâche l'assertion quand il est détruit.
pub struct SleepGuard {
    inner: Arc<dyn GuardInner>,
}

impl Drop for SleepGuard {
    fn drop(&mut self) {
        self.inner.release();
    }
}

impl SleepGuard {
    pub fn new(inner: Arc<dyn GuardInner>) -> Self {
        SleepGuard { inner }
    }
}

pub trait GuardInner: Send + Sync {
    fn release(&self);
}

/// Compteur partagé + processus inhibiteur.
pub struct CountingPower {
    count: Arc<AtomicU32>,
    child: Arc<std::sync::Mutex<Option<std::process::Child>>>,
    program: &'static str,
    args: &'static [&'static str],
}

impl CountingPower {
    /// Backend macOS.
    pub fn caffeinate() -> Self {
        CountingPower {
            count: Arc::new(AtomicU32::new(0)),
            child: Arc::new(std::sync::Mutex::new(None)),
            program: "/usr/bin/caffeinate",
            // -i : empêche la mise en veille système par inactivité.
            args: &["-i"],
        }
    }

    /// Backend inerte (tests, OS sans implémentation).
    pub fn noop() -> Self {
        CountingPower {
            count: Arc::new(AtomicU32::new(0)),
            child: Arc::new(std::sync::Mutex::new(None)),
            program: "",
            args: &[],
        }
    }

    fn acquire(&self) {
        if self.count.fetch_add(1, Ordering::SeqCst) == 0 && !self.program.is_empty() {
            let mut guard = self.child.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_none() {
                match std::process::Command::new(self.program)
                    .args(self.args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(c) => *guard = Some(c),
                    Err(e) => tracing::warn!(error = %e, "assertion anti-veille indisponible"),
                }
            }
        }
    }
}

struct CountingGuard {
    count: Arc<AtomicU32>,
    child: Arc<std::sync::Mutex<Option<std::process::Child>>>,
}

impl GuardInner for CountingGuard {
    fn release(&self) {
        if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            let mut guard = self.child.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(mut c) = guard.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

impl PowerManager for CountingPower {
    fn mechanism(&self) -> &'static str {
        if self.program.is_empty() {
            "aucun (inerte)"
        } else {
            "caffeinate -i (PreventUserIdleSystemSleep)"
        }
    }

    fn prevent_sleep(&self, reason: &str) -> Result<SleepGuard> {
        tracing::debug!(reason, "assertion anti-veille prise");
        self.acquire();
        Ok(SleepGuard::new(Arc::new(CountingGuard {
            count: self.count.clone(),
            child: self.child.clone(),
        })))
    }

    fn active(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertions_are_counted_and_released() {
        let p = CountingPower::noop();
        assert_eq!(p.active(), 0);
        let a = p.prevent_sleep("run r1").unwrap();
        let b = p.prevent_sleep("tour t2").unwrap();
        assert_eq!(p.active(), 2);
        drop(a);
        assert_eq!(p.active(), 1, "un seul relâchement ne libère pas tout");
        drop(b);
        assert_eq!(p.active(), 0, "l'assertion est relâchée au repos");
    }

    #[test]
    fn mechanism_is_reported() {
        assert!(
            CountingPower::caffeinate()
                .mechanism()
                .contains("caffeinate")
        );
        assert!(CountingPower::noop().mechanism().contains("inerte"));
    }
}
