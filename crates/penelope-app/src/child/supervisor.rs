//! Côté daemon : lancer l'enfant, lire ses lignes, l'arrêter sur `/stop`, sur sa durée
//! ou sur sa mémoire, et rendre la cause de sa fin.

use super::{FromChild, PROTOCOL, ToChild, line};
use penelope_llm::CancelToken;
use penelope_platform::process::{ExitInfo, rss_kb};
use penelope_platform::{ProcessHost, ProcessSpec};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Les bornes d'un run.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Mémoire résidente de l'enfant au-delà de laquelle il est tué (kio).
    pub max_rss_kb: Option<u64>,
    pub max_duration: Duration,
    /// Entre SIGTERM et SIGKILL.
    pub grace: Duration,
    /// Pas du sondage (jeton, durée, mémoire).
    pub poll: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_rss_kb: Some(2 * 1024 * 1024),
            max_duration: Duration::from_secs(3600),
            grace: Duration::from_secs(5),
            poll: Duration::from_millis(200),
        }
    }
}

/// Pourquoi le run a fini.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub enum Cause {
    Done,
    Failed {
        error: String,
    },
    /// `/stop`.
    Interrupted,
    Timeout,
    Oom {
        rss_kb: u64,
    },
    /// Mort sans rendre de résultat : code ou signal.
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Protocol {
        error: String,
    },
}

/// Le compte rendu d'un run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub cause: Cause,
    pub result: Option<Value>,
    pub progress: Vec<String>,
    pub pid: u32,
    pub rss_max_kb: u64,
}

impl Report {
    /// Un enfant mort (signal, sortie sans résultat, mémoire) est relancé une seule fois ;
    /// un échec rendu, un `/stop` ou un délai dépassé ne l'est pas.
    pub fn retry_allowed(&self, attempts: u32) -> bool {
        attempts == 0 && matches!(self.cause, Cause::Exit { .. } | Cause::Oom { .. })
    }

    /// La cause en une phrase, pour le journal et la session.
    pub fn describe(&self) -> String {
        match &self.cause {
            Cause::Done => "terminé".into(),
            Cause::Failed { error } => format!("échoué : {error}"),
            Cause::Interrupted => "arrêté sur demande".into(),
            Cause::Timeout => "arrêté : durée maximale dépassée".into(),
            Cause::Oom { rss_kb } => {
                format!("arrêté : mémoire dépassée ({} Mio)", rss_kb / 1024)
            }
            Cause::Exit { code, signal } => format!(
                "mort sans résultat, {}",
                ExitInfo {
                    code: *code,
                    signal: *signal
                }
                .describe()
            ),
            Cause::Protocol { error } => format!("protocole : {error}"),
        }
    }
}

/// Lance `spec`, lui envoie `init` et le suit jusqu'à sa fin.
pub async fn supervise(
    host: &dyn ProcessHost,
    spec: ProcessSpec,
    init: ToChild,
    limits: &Limits,
    cancel: &CancelToken,
) -> Report {
    let mut report = Report {
        cause: Cause::Done,
        result: None,
        progress: Vec::new(),
        pid: 0,
        rss_max_kb: 0,
    };
    let mut child = match host.spawn(spec, None).await {
        Ok(c) => c,
        Err(e) => {
            report.cause = Cause::Protocol {
                error: e.to_string(),
            };
            return report;
        }
    };
    report.pid = child.pid;
    let mut stdin = child.stdin();
    let Some(stdout) = child.stdout() else {
        report.cause = Cause::Protocol {
            error: "pas de sortie standard".into(),
        };
        let _ = host.terminate(&mut child, limits.grace).await;
        return report;
    };
    if let Some(w) = stdin.as_mut() {
        let _ = w.write_all(line(&init).as_bytes()).await;
        let _ = w.flush().await;
    }
    let started = Instant::now();
    let mut lines = BufReader::new(stdout).lines();
    let mut tick = tokio::time::interval(limits.poll);
    // La cause qui décide l'arrêt ; `None` tant que l'enfant vit.
    let mut stop: Option<(Cause, Duration)> = None;
    let mut finished: Option<Cause> = None;
    while stop.is_none() && finished.is_none() {
        tokio::select! {
            next = lines.next_line() => match next {
                Ok(Some(l)) => match serde_json::from_str::<FromChild>(&l) {
                    Ok(FromChild::Ready { protocol, .. }) if protocol != PROTOCOL => {
                        stop = Some((Cause::Protocol { error: format!("protocole {protocol}") }, limits.grace));
                    }
                    Ok(FromChild::Progress { text }) => report.progress.push(text),
                    Ok(FromChild::Done { result }) => {
                        report.result = Some(result);
                        finished = Some(Cause::Done);
                    }
                    Ok(FromChild::Failed { error }) => finished = Some(Cause::Failed { error }),
                    _ => {}
                },
                // Sortie fermée sans résultat : l'enfant est mort.
                _ => {
                    let exit = child.wait_exit(limits.grace).await;
                    finished = Some(Cause::Exit {
                        code: exit.and_then(|e| e.code),
                        signal: exit.and_then(|e| e.signal),
                    });
                }
            },
            _ = tick.tick() => {
                let rss = rss_kb(report.pid).unwrap_or(0);
                report.rss_max_kb = report.rss_max_kb.max(rss);
                if cancel.is_cancelled() {
                    if let Some(w) = stdin.as_mut() {
                        let _ = w.write_all(line(&ToChild::Interrupt).as_bytes()).await;
                        let _ = w.flush().await;
                    }
                    stop = Some((Cause::Interrupted, limits.grace));
                } else if started.elapsed() > limits.max_duration {
                    stop = Some((Cause::Timeout, limits.grace));
                } else if limits.max_rss_kb.is_some_and(|max| rss > max) {
                    // Pas de grâce : un enfant qui enfle ne doit pas enfler cinq secondes de plus.
                    stop = Some((Cause::Oom { rss_kb: rss }, Duration::ZERO));
                }
            }
        }
    }
    if let Some((cause, grace)) = stop {
        report.cause = cause;
        // Sur `/stop`, l'enfant a sa grâce pour rendre la main de lui-même.
        if report.cause == Cause::Interrupted && child.wait_exit(grace).await.is_some() {
            return report;
        }
        let _ = host.terminate(&mut child, grace).await;
        return report;
    }
    report.cause = finished.unwrap_or(Cause::Done);
    // Un résultat rendu : l'enfant n'a plus rien à faire, il sort ou il est arrêté.
    drop(stdin);
    if child.wait_exit(limits.grace).await.is_none() {
        let _ = host.terminate(&mut child, limits.grace).await;
    }
    report
}
