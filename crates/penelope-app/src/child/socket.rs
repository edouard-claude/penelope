//! L'enfant sur un socket Unix (#395, reprise) : il survit au daemon.
//!
//! Lancé par `penelope run --socket <chemin>`, l'enfant écoute sur son socket. Le premier
//! superviseur lui envoie `init` ; si le daemon redémarre, le suivant envoie `resume`, et
//! l'enfant lui renvoie les appels MCP restés sans réponse. Ce qui part sans superviseur
//! attend le prochain. Le résultat final est aussi écrit à côté du socket
//! ([`result_path`]) avant d'être envoyé : un daemon qui revient après la fin le lit là.
//! Une durée maximale borne l'enfant même sans superviseur.

use super::serve::{ChildCtx, ChildRun};
use super::supervisor::serve_mcp;
use super::{Cause, FromChild, Limits, PROTOCOL, Relay, Report, ToChild, VERSION, line};
use crate::ports::McpGateway;
use penelope_llm::CancelToken;
use penelope_platform::process::{process_exists, rss_kb, terminate_group};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

/// Où l'enfant écrit son résultat final.
pub fn result_path(socket: &Path) -> PathBuf {
    socket.with_extension("result.json")
}

/// Lit une ligne de la connexion en cours, ou attend sans fin s'il n'y en a pas.
async fn next_line(lines: &mut Option<Lines<BufReader<OwnedReadHalf>>>) -> Option<String> {
    match lines.as_mut() {
        Some(l) => l.next_line().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

async fn send(writer: &mut Option<OwnedWriteHalf>, backlog: &mut Vec<FromChild>, msg: FromChild) {
    if let Some(w) = writer.as_mut() {
        if w.write_all(line(&msg).as_bytes()).await.is_ok() && w.flush().await.is_ok() {
            return;
        }
        *writer = None;
    }
    backlog.push(msg);
}

/// Côté enfant : la boucle sur le socket, jusqu'à la fin du run ou `max`.
pub async fn serve_socket(
    path: &Path,
    runner: Arc<dyn ChildRun>,
    max: Duration,
) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let listener =
        tokio::net::UnixListener::bind(path).map_err(|e| format!("{} : {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
    let (r, w) = stream.into_split();
    let mut lines = Some(BufReader::new(r).lines());
    let mut writer = Some(w);
    let mut backlog: Vec<FromChild> = Vec::new();
    let first = next_line(&mut lines).await;
    let Some(Ok(ToChild::Init {
        protocol,
        version,
        kind,
        payload,
        ..
    })) = first.map(|l| serde_json::from_str::<ToChild>(&l))
    else {
        return Err("premier message attendu : init".into());
    };
    if protocol != PROTOCOL || (!version.is_empty() && version != VERSION) {
        let error =
            format!("protocole {protocol} ou binaire {version} refusé (ici {PROTOCOL}, {VERSION})");
        send(
            &mut writer,
            &mut backlog,
            FromChild::Failed {
                error: error.clone(),
            },
        )
        .await;
        return Err(error);
    }
    send(
        &mut writer,
        &mut backlog,
        FromChild::Ready {
            protocol: PROTOCOL,
            pid: std::process::id(),
        },
    )
    .await;
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromChild>();
    let relay = Relay::new(out_tx);
    let cancel = CancelToken::new();
    let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<String>();
    let run = {
        let ctx = ChildCtx {
            progress: prog_tx,
            cancel: cancel.clone(),
            relay: relay.clone(),
        };
        tokio::spawn(async move { runner.run(&kind, payload, ctx).await })
    };
    tokio::pin!(run);
    let deadline = tokio::time::sleep(max);
    tokio::pin!(deadline);
    let outcome = loop {
        tokio::select! {
            res = &mut run => break res.map_err(|e| e.to_string()).and_then(|r| r),
            Some(msg) = out_rx.recv() => send(&mut writer, &mut backlog, msg).await,
            Some(text) = prog_rx.recv() => send(&mut writer, &mut backlog, FromChild::Progress { text }).await,
            next = next_line(&mut lines) => match next.map(|l| serde_json::from_str::<ToChild>(&l)) {
                Some(Ok(ToChild::Interrupt)) => cancel.cancel(),
                Some(Ok(ToChild::Heartbeat)) => send(&mut writer, &mut backlog, FromChild::Heartbeat).await,
                Some(Ok(ToChild::McpResult { id, ok, error })) => relay.resolve(id, ok, error),
                Some(_) => {}
                // Superviseur parti : le run continue, le suivant se rattachera.
                None => {
                    lines = None;
                    writer = None;
                }
            },
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let (r, w) = stream.into_split();
                let mut l = BufReader::new(r).lines();
                // Un nouveau superviseur parle d'abord : `resume`.
                if !matches!(l.next_line().await.ok().flatten().map(|x| serde_json::from_str::<ToChild>(&x)), Some(Ok(ToChild::Resume))) {
                    continue;
                }
                lines = Some(l);
                writer = Some(w);
                send(&mut writer, &mut backlog, FromChild::Ready { protocol: PROTOCOL, pid: std::process::id() }).await;
                for f in std::mem::take(&mut backlog).into_iter().filter(|f| !matches!(f, FromChild::McpCall { .. })) {
                    send(&mut writer, &mut backlog, f).await;
                }
                for f in relay.pending_frames() {
                    send(&mut writer, &mut backlog, f).await;
                }
            }
            _ = &mut deadline => cancel.cancel(),
        }
    };
    let last = match outcome {
        Ok(result) => FromChild::Done { result },
        Err(error) => FromChild::Failed { error },
    };
    // Écrit avant d'être envoyé : un daemon qui revient après la fin le lit ici.
    let file = result_path(path);
    let tmp = file.with_extension("tmp");
    if std::fs::write(&tmp, line(&last)).is_ok() {
        let _ = std::fs::rename(&tmp, &file);
    }
    send(&mut writer, &mut backlog, last).await;
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Le résultat que l'enfant a laissé à côté de son socket, s'il a fini.
pub fn read_result(socket: &Path) -> Option<FromChild> {
    let raw = std::fs::read_to_string(result_path(socket)).ok()?;
    serde_json::from_str(raw.trim()).ok()
}

/// Côté daemon : se branche sur l'enfant `pid` par son socket, envoie `first` (`init` au
/// lancement, `resume` après un redémarrage) et le suit jusqu'à sa fin. `started` : quand
/// le run a commencé, pour la borne de durée.
pub async fn supervise_socket(
    pid: u32,
    socket: &Path,
    first: ToChild,
    limits: &Limits,
    cancel: &CancelToken,
    mcp: Option<Arc<dyn McpGateway>>,
    started: Instant,
) -> Report {
    let mut report = Report {
        cause: Cause::Done,
        result: None,
        progress: Vec::new(),
        pid,
        rss_max_kb: 0,
    };
    let finish = |report: &mut Report, last: Option<FromChild>| match last {
        Some(FromChild::Done { result }) => {
            report.result = Some(result);
            report.cause = Cause::Done;
        }
        Some(FromChild::Failed { error }) => report.cause = Cause::Failed { error },
        _ => {
            report.cause = Cause::Exit {
                code: None,
                signal: None,
            }
        }
    };
    // Le socket apparaît quand l'enfant écoute.
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            stream = Some(s);
            break;
        }
        if !process_exists(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(stream) = stream else {
        finish(&mut report, read_result(socket));
        return report;
    };
    let (r, mut w) = stream.into_split();
    let _ = w.write_all(line(&first).as_bytes()).await;
    let _ = w.flush().await;
    let mut lines = BufReader::new(r).lines();
    let mut tick = tokio::time::interval(limits.poll);
    let (reply_tx, mut reply_rx) = mpsc::unbounded_channel::<ToChild>();
    let mut stop: Option<(Cause, Duration)> = None;
    loop {
        tokio::select! {
            Some(reply) = reply_rx.recv() => {
                let _ = w.write_all(line(&reply).as_bytes()).await;
                let _ = w.flush().await;
            }
            next = lines.next_line() => match next.ok().flatten().map(|l| serde_json::from_str::<FromChild>(&l)) {
                Some(Ok(FromChild::Ready { protocol, .. })) if protocol != PROTOCOL => {
                    stop = Some((Cause::Protocol { error: format!("protocole {protocol}") }, limits.grace));
                    break;
                }
                Some(Ok(FromChild::Progress { text })) => report.progress.push(text),
                Some(Ok(last @ (FromChild::Done { .. } | FromChild::Failed { .. }))) => {
                    finish(&mut report, Some(last));
                    return report;
                }
                Some(Ok(FromChild::McpCall { id, op, qualified, args, session_id, origin })) => {
                    let (mcp, tx) = (mcp.clone(), reply_tx.clone());
                    tokio::spawn(async move {
                        let res = serve_mcp(mcp, &op, &qualified, args, session_id, origin).await;
                        let (ok, error) = match res { Ok(v) => (Some(v), None), Err(e) => (None, Some(e)) };
                        let _ = tx.send(ToChild::McpResult { id, ok, error });
                    });
                }
                Some(_) => {}
                // Connexion fermée : l'enfant a fini (son résultat est écrit) ou il est mort.
                None => {
                    for _ in 0..20 {
                        if read_result(socket).is_some() || !process_exists(pid) { break; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    finish(&mut report, read_result(socket));
                    return report;
                }
            },
            _ = tick.tick() => {
                let rss = rss_kb(pid).unwrap_or(0);
                report.rss_max_kb = report.rss_max_kb.max(rss);
                if cancel.is_cancelled() {
                    let _ = w.write_all(line(&ToChild::Interrupt).as_bytes()).await;
                    let _ = w.flush().await;
                    stop = Some((Cause::Interrupted, limits.grace));
                } else if started.elapsed() > limits.max_duration {
                    stop = Some((Cause::Timeout, limits.grace));
                } else if limits.max_rss_kb.is_some_and(|max| rss > max) {
                    stop = Some((Cause::Oom { rss_kb: rss }, Duration::ZERO));
                }
                if stop.is_some() { break; }
            }
        }
    }
    if let Some((cause, grace)) = stop {
        report.cause = cause;
        if report.cause == Cause::Interrupted {
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline && process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        if process_exists(pid) {
            terminate_group(pid, grace).await;
        }
    }
    report
}
