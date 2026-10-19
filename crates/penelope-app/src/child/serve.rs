//! Côté enfant : lire `init`, faire le run, rendre `done` ou `failed`. Un stdin fermé
//! (superviseur mort) arrête l'enfant : il ne survit pas orphelin à son daemon.

use super::{FromChild, PROTOCOL, ToChild, line};
use penelope_llm::CancelToken;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Un genre de run que l'enfant sait faire.
#[async_trait::async_trait]
pub trait ChildRun: Send + Sync {
    async fn run(
        &self,
        kind: &str,
        payload: Value,
        progress: mpsc::UnboundedSender<String>,
        cancel: CancelToken,
    ) -> Result<Value, String>;
}

/// Le run d'essai, déterministe et sans réseau, qui sert la suite du protocole :
/// `echo`, `progress`, `sleep_ms`, `alloc_mb`, `exit`, `abort`.
pub struct Probe;

#[async_trait::async_trait]
impl ChildRun for Probe {
    async fn run(
        &self,
        kind: &str,
        payload: Value,
        progress: mpsc::UnboundedSender<String>,
        cancel: CancelToken,
    ) -> Result<Value, String> {
        if kind != "probe" {
            return Err(format!("genre de run inconnu : {kind}"));
        }
        for p in payload["progress"].as_array().into_iter().flatten() {
            let _ = progress.send(p.as_str().unwrap_or_default().to_string());
        }
        // Mémoire réellement touchée, pour que la RSS monte.
        let held: Vec<u8> = vec![1u8; (payload["alloc_mb"].as_u64().unwrap_or(0) as usize) << 20];
        if let Some(code) = payload["exit"].as_i64() {
            std::process::exit(code as i32);
        }
        if payload["abort"].as_bool() == Some(true) {
            std::process::abort();
        }
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(payload["sleep_ms"].as_u64().unwrap_or(0));
        while std::time::Instant::now() < deadline {
            if cancel.is_cancelled() {
                return Err("interrompu".into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        Ok(serde_json::json!({ "echo": payload["echo"], "held": held.len() }))
    }
}

/// La boucle de l'enfant sur stdin et stdout.
pub async fn serve_stdio(runner: Arc<dyn ChildRun>) -> Result<(), String> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromChild>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            if stdout.write_all(line(&msg).as_bytes()).await.is_err() {
                break;
            }
            let _ = stdout.flush().await;
        }
    });
    let first = lines.next_line().await.map_err(|e| e.to_string())?;
    let Some(Ok(ToChild::Init {
        protocol,
        kind,
        payload,
        ..
    })) = first.map(|l| serde_json::from_str::<ToChild>(&l))
    else {
        return Err("premier message attendu : init".into());
    };
    if protocol != PROTOCOL {
        let _ = out_tx.send(FromChild::Failed {
            error: format!("protocole {protocol} inconnu (attendu {PROTOCOL})"),
        });
        drop(out_tx);
        let _ = writer.await;
        return Err("protocole incompatible".into());
    }
    let _ = out_tx.send(FromChild::Ready {
        protocol: PROTOCOL,
        pid: std::process::id(),
    });
    let cancel = CancelToken::new();
    let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<String>();
    let run = {
        let (runner, cancel) = (runner.clone(), cancel.clone());
        tokio::spawn(async move { runner.run(&kind, payload, prog_tx, cancel).await })
    };
    tokio::pin!(run);
    let outcome = loop {
        tokio::select! {
            res = &mut run => break res.map_err(|e| e.to_string()).and_then(|r| r),
            Some(text) = prog_rx.recv() => { let _ = out_tx.send(FromChild::Progress { text }); }
            next = lines.next_line() => match next {
                Ok(Some(l)) => match serde_json::from_str::<ToChild>(&l) {
                    Ok(ToChild::Interrupt) => cancel.cancel(),
                    Ok(ToChild::Heartbeat) => { let _ = out_tx.send(FromChild::Heartbeat); }
                    _ => {}
                },
                // Superviseur parti : rien à rendre à personne.
                _ => std::process::exit(2),
            },
        }
    };
    while let Ok(text) = prog_rx.try_recv() {
        let _ = out_tx.send(FromChild::Progress { text });
    }
    let _ = out_tx.send(match outcome {
        Ok(result) => FromChild::Done { result },
        Err(error) => FromChild::Failed { error },
    });
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
