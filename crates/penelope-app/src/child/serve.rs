//! Côté enfant : lire `init`, faire le run, rendre `done` ou `failed`. Un stdin fermé
//! (superviseur mort) arrête l'enfant : il ne survit pas orphelin à son daemon.

use super::{FromChild, PROTOCOL, Relay, ToChild, VERSION, line};
use penelope_llm::CancelToken;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// Ce qu'un run reçoit de la boucle : sa progression, son jeton d'arrêt, et le relais
/// vers le host MCP du daemon.
pub struct ChildCtx {
    pub progress: mpsc::UnboundedSender<String>,
    pub cancel: CancelToken,
    pub relay: Relay,
}

/// Un genre de run que l'enfant sait faire.
#[async_trait::async_trait]
pub trait ChildRun: Send + Sync {
    async fn run(&self, kind: &str, payload: Value, ctx: ChildCtx) -> Result<Value, String>;
}

/// Le run d'essai, déterministe et sans réseau, qui sert la suite du protocole :
/// `echo`, `progress`, `sleep_ms`, `alloc_mb`, `exit`, `abort`.
pub struct Probe;

#[async_trait::async_trait]
impl ChildRun for Probe {
    async fn run(&self, kind: &str, payload: Value, ctx: ChildCtx) -> Result<Value, String> {
        let ChildCtx {
            progress,
            cancel,
            relay,
        } = ctx;
        if kind != "probe" {
            return Err(format!("genre de run inconnu : {kind}"));
        }
        for p in payload["progress"].as_array().into_iter().flatten() {
            let _ = progress.send(p.as_str().unwrap_or_default().to_string());
        }
        // Mémoire réellement touchée, pour que la RSS monte.
        // `mcp` : un appel relayé au daemon, son résultat rendu tel quel.
        if let Some(tool) = payload["mcp"].as_str() {
            let out = relay
                .call("call_tool", tool, payload["args"].clone(), None, None)
                .await?;
            return Ok(serde_json::json!({ "mcp": out }));
        }
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

/// La boucle de l'enfant sur stdin et stdout. Un superviseur parti (stdin fermé) arrête
/// l'enfant : il ne survit pas orphelin à son daemon.
pub async fn serve_stdio(runner: Arc<dyn ChildRun>) -> Result<(), String> {
    match serve_io(tokio::io::stdin(), tokio::io::stdout(), runner).await {
        Err(e) if e == GONE => std::process::exit(2),
        other => other,
    }
}

const GONE: &str = "superviseur parti";

/// La boucle de l'enfant sur deux flux quelconques (stdio, ou un tuyau en mémoire pour
/// les tests).
pub async fn serve_io<R, W>(input: R, output: W, runner: Arc<dyn ChildRun>) -> Result<(), String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut lines = BufReader::new(input).lines();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromChild>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(msg) = out_rx.recv().await {
            if output.write_all(line(&msg).as_bytes()).await.is_err() {
                break;
            }
            let _ = output.flush().await;
        }
    });
    let first = lines.next_line().await.map_err(|e| e.to_string())?;
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
    let refused = if protocol != PROTOCOL {
        Some(format!("protocole {protocol} inconnu (attendu {PROTOCOL})"))
    } else if !version.is_empty() && version != VERSION {
        Some(format!(
            "binaire {VERSION}, superviseur {version} : une mise à jour est en cours, le run \
             attend la même version"
        ))
    } else {
        None
    };
    if let Some(error) = refused {
        let _ = out_tx.send(FromChild::Failed {
            error: error.clone(),
        });
        drop(out_tx);
        let _ = writer.await;
        return Err(error);
    }
    let _ = out_tx.send(FromChild::Ready {
        protocol: PROTOCOL,
        pid: std::process::id(),
    });
    let cancel = CancelToken::new();
    let relay = Relay::new(out_tx.clone());
    let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<String>();
    let run = {
        let ctx = ChildCtx {
            progress: prog_tx,
            cancel: cancel.clone(),
            relay: relay.clone(),
        };
        let runner = runner.clone();
        tokio::spawn(async move { runner.run(&kind, payload, ctx).await })
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
                    Ok(ToChild::McpResult { id, ok, error }) => relay.resolve(id, ok, error),
                    _ => {}
                },
                // Superviseur parti : rien à rendre à personne.
                _ => {
                    cancel.cancel();
                    return Err(GONE.into());
                }
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
    drop(relay);
    let _ = writer.await;
    Ok(())
}
