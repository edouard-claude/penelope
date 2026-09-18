//! Boucles de fond surveillées (issue #84).
//!
//! Une tâche tokio qui panique s'arrête, et son `JoinHandle` n'était lu qu'à l'arrêt du
//! daemon : un ordonnanceur mort ne planifiait plus rien, un runner mort réduisait le pool
//! en silence, et `status` répondait « en marche ». Chaque boucle passe désormais par
//! [`spawn_supervised`] :
//!
//! ```text
//!  boucle ──panique──► journal (nom + message) ─► compteur ─► daemon.task_panicked
//!     ▲                                                              │
//!     └──────────── relance après 1 s, 2 s, 4 s … 5 min ◄────────────┘
//!                   (sauf arrêt du daemon)
//! ```
//!
//! `doctor` signale une boucle relancée dans la dernière heure ou une boucle morte,
//! `status` donne le nombre de runners vivants.

use crate::runtime::Daemon;
use futures::FutureExt;
use penelope_kernel::event::EventDraft;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Attente maximale entre deux relances.
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// Une boucle qui a tenu plus longtemps repart avec l'attente minimale.
const STABLE_AFTER: Duration = Duration::from_secs(300);

/// État d'une boucle de fond.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TaskState {
    pub alive: bool,
    /// Fin sans panique alors que le daemon ne s'arrête pas : la boucle ne tourne plus.
    pub ended: bool,
    pub panics: u64,
    pub last_panic: Option<String>,
    pub last_panic_ms: Option<i64>,
}

/// Registre des boucles de fond du processus.
#[derive(Default)]
pub struct Tasks {
    inner: Mutex<BTreeMap<String, TaskState>>,
}

impl Tasks {
    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<String, TaskState>) -> T) -> T {
        match self.inner.lock() {
            Ok(mut g) => f(&mut g),
            Err(p) => f(&mut p.into_inner()),
        }
    }

    fn set_alive(&self, name: &str, alive: bool) {
        self.with(|m| {
            let t = m.entry(name.to_string()).or_default();
            t.alive = alive;
            if alive {
                t.ended = false;
            }
        });
    }

    fn set_ended(&self, name: &str) {
        self.with(|m| m.entry(name.to_string()).or_default().ended = true);
    }

    /// Compte une panique, d'une boucle ou d'un tour (`penelope_task_panics_total`).
    pub fn record_panic(&self, name: &str, message: &str, now_ms: i64) {
        self.with(|m| {
            let t = m.entry(name.to_string()).or_default();
            t.panics += 1;
            t.last_panic = Some(message.chars().take(300).collect());
            t.last_panic_ms = Some(now_ms);
        });
    }

    pub fn snapshot(&self) -> BTreeMap<String, TaskState> {
        self.with(|m| m.clone())
    }

    /// Boucles vivantes dont le nom commence par `prefix` (`runner-` : le pool).
    pub fn alive(&self, prefix: &str) -> usize {
        self.with(|m| {
            m.iter()
                .filter(|(n, t)| n.starts_with(prefix) && t.alive)
                .count()
        })
    }
}

/// Message d'une panique, tel que le thread l'aurait affiché.
pub fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&'static str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panique sans message".into())
}

/// Journalise, compte et date une panique, et la verse au journal d'audit.
pub async fn report_panic(d: &Daemon, name: &str, message: &str) {
    tracing::error!(tache = name, panique = message, "panique rattrapée");
    d.tasks
        .record_panic(name, message, d.services.clock.now_ms());
    let _ = d
        .services
        .events
        .append(EventDraft::new(
            "daemon.task_panicked",
            json!({"task": name, "panic": message}),
        ))
        .await;
}

/// Lance `make()` et le relance après une panique, avec une attente croissante, tant que
/// le daemon ne s'arrête pas. Une fin normale n'est pas relancée : pendant l'arrêt c'est
/// attendu, sinon la boucle est marquée finie et `doctor` le dit.
pub fn spawn_supervised<F, Fut>(
    d: Arc<Daemon>,
    name: impl Into<String>,
    make: F,
) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let name = name.into();
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            d.tasks.set_alive(&name, true);
            let started = tokio::time::Instant::now();
            let outcome = std::panic::AssertUnwindSafe(make()).catch_unwind().await;
            d.tasks.set_alive(&name, false);
            let Err(payload) = outcome else {
                if !d.handle.is_shutting_down() {
                    tracing::warn!(tache = %name, "boucle de fond terminée hors arrêt");
                    d.tasks.set_ended(&name);
                }
                return;
            };
            report_panic(&d, &name, &panic_text(payload.as_ref())).await;
            if started.elapsed() >= STABLE_AFTER {
                backoff = Duration::from_secs(1);
            }
            if !wait_unless_shutdown(&d, backoff).await {
                return;
            }
            tracing::warn!(tache = %name, attente = ?backoff, "boucle de fond relancée");
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    })
}

/// Attend `total`, par tranches courtes. Faux si le daemon s'arrête entre-temps.
async fn wait_unless_shutdown(d: &Daemon, total: Duration) -> bool {
    let step = Duration::from_millis(100);
    let deadline = tokio::time::Instant::now() + total;
    while tokio::time::Instant::now() < deadline {
        if d.handle.is_shutting_down() {
            return false;
        }
        tokio::time::sleep(step.min(deadline - tokio::time::Instant::now())).await;
    }
    !d.handle.is_shutting_down()
}

/// Contrôle `doctor` : boucle relancée dans la dernière heure, ou boucle finie.
pub fn doctor_check(d: &Daemon) -> penelope_kernel::api::DoctorCheck {
    use penelope_kernel::api::DoctorCheck;
    const ID: &str = "tasks";
    const LABEL: &str = "Boucles de fond";
    let now = d.services.clock.now_ms();
    let tasks = d.tasks.snapshot();
    let mut problems = Vec::new();
    for (name, t) in &tasks {
        if t.ended {
            problems.push(format!("`{name}` ne tourne plus"));
        }
        if let Some(at) = t.last_panic_ms
            && now - at < 3_600_000
        {
            let what = if name == "tour" {
                format!("{} tour(s) arrêté(s) par une panique", t.panics)
            } else {
                format!("`{name}` relancée {} fois", t.panics)
            };
            problems.push(format!(
                "{what}, dernière panique il y a {} min : {}",
                (now - at) / 60_000,
                t.last_panic.as_deref().unwrap_or("?")
            ));
        }
    }
    if problems.is_empty() {
        let alive = tasks.values().filter(|t| t.alive).count();
        DoctorCheck::ok(
            ID,
            LABEL,
            format!(
                "{alive} vivante(s), dont {} runner(s) ; aucune panique dans l'heure",
                d.tasks.alive("runner-")
            ),
        )
    } else {
        DoctorCheck::fail(
            ID,
            LABEL,
            problems.join(" ; "),
            Some("daemon.err.log (niveau error), puis penelope restart".into()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::SystemClock);
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        (dir, Arc::new(Daemon::from_services(s)))
    }

    /// #84 : une boucle qui panique est relancée, la panique est comptée, nommée et
    /// versée au journal ; `doctor` la signale.
    /// Attend qu'une condition soit vraie, 5 s au plus (horloge réelle : le journal
    /// d'audit écrit par un thread, le temps suspendu de tokio avancerait sans lui).
    async fn until(mut f: impl FnMut() -> bool) {
        for _ in 0..500 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition jamais remplie");
    }

    #[tokio::test]
    async fn a_panicking_loop_is_restarted_and_reported() {
        let (_dir, d) = daemon().await;
        let runs = Arc::new(AtomicUsize::new(0));
        let h = {
            let runs = runs.clone();
            let d2 = d.clone();
            spawn_supervised(d.clone(), "essai", move || {
                let runs = runs.clone();
                let d = d2.clone();
                async move {
                    if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("index hors bornes dans la boucle d'essai");
                    }
                    while !d.handle.is_shutting_down() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            })
        };
        until(|| runs.load(Ordering::SeqCst) == 2).await;
        until(|| d.tasks.snapshot().get("essai").is_some_and(|t| t.alive)).await;
        let t = d.tasks.snapshot()["essai"].clone();
        assert!(t.alive);
        assert_eq!(t.panics, 1);
        assert!(t.last_panic.unwrap().contains("hors bornes"));
        let events = d.services.events.range(0, 100).await.unwrap();
        assert!(events.iter().any(|e| e.kind == "daemon.task_panicked"));
        let c = doctor_check(&d);
        assert!(
            !c.ok && c.detail.contains("`essai` relancée 1 fois"),
            "{c:?}"
        );

        d.handle.shutdown();
        tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("l'arrêt n'est pas retardé")
            .unwrap();
    }

    /// #84 : pendant l'attente avant relance, l'arrêt du daemon n'attend pas la fin du
    /// délai.
    #[tokio::test]
    async fn shutdown_interrupts_the_restart_backoff() {
        let (_dir, d) = daemon().await;
        let h = spawn_supervised(d.clone(), "toujours", || async {
            panic!("panique à chaque démarrage");
        });
        // Deux paniques : la boucle attend maintenant 2 s avant la troisième.
        until(|| {
            d.tasks
                .snapshot()
                .get("toujours")
                .is_some_and(|t| t.panics >= 2)
        })
        .await;
        d.handle.shutdown();
        tokio::time::timeout(Duration::from_millis(500), h)
            .await
            .expect("arrêt immédiat")
            .unwrap();
    }
}
