//! Réponses en flux (`chat.stream`, `tail`) et issue d'un tour pour la CLI.

use super::*;
use penelope_agent::TurnOutcome;
use penelope_app::bus::BusKind;
use tokio::io::AsyncWriteExt;

impl Rpc {
    /// Méthodes à réponse en flux : `chat.stream` et `tail`. Les événements partent en
    /// notifications JSON-RPC (sans `id`), la réponse finale clôt l'échange.
    /// Le client d'un tour CLI est parti : le tour s'arrête, qu'il tourne déjà ou attende
    /// encore son tour (#100). Un tour Telegram n'est jamais concerné : il n'a pas de
    /// client de flux.
    async fn abandon(&self, session_id: &str, turn_id: &str) {
        if !self.daemon.bus.cancel_turn(session_id, turn_id) {
            let _ = self.daemon.services.turns.cancel_if_pending(turn_id).await;
        }
        tracing::info!(
            session = session_id,
            turn = turn_id,
            "client parti : tour annulé"
        );
    }

    /// Flux d'un tour (`chat.stream`) ou de tous (`tail`). `closed` se résout quand le
    /// client ferme sa connexion : un tour lancé par la CLI que personne ne lira plus est
    /// annulé, outils compris, au lieu de tourner et de facturer (issue #100).
    pub async fn handle_streaming<W, C>(
        &self,
        req: RpcRequest,
        out: &mut W,
        closed: C,
    ) -> anyhow::Result<()>
    where
        W: tokio::io::AsyncWrite + Unpin,
        C: std::future::Future<Output = ()>,
    {
        let params = req.params.clone().unwrap_or(json!({}));
        let mut rx = self.daemon.bus.subscribe();
        tokio::pin!(closed);
        match req.method.as_str() {
            method::TAIL => loop {
                let ev = tokio::select! {
                    ev = rx.recv() => ev,
                    _ = &mut closed => return Ok(()),
                };
                let ev = match ev {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => return Ok(()),
                };
                if let Some(se) = to_stream_event(&ev) {
                    write_line(out, &notification(&se)).await?;
                }
            },
            _ => {
                let text = required_str(&params, "text")?;
                let sid = self.session_param(&params).await?;
                let id = self
                    .daemon
                    .enqueue_message(&sid, &text, &Origin::Cli, None)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("tour non créé"))?;
                let mut done = self.daemon.bus.wait_for(id.as_str());
                loop {
                    tokio::select! {
                        outcome = &mut done => {
                            // Les derniers fragments éventuels, puis la réponse.
                            while let Ok(ev) = rx.try_recv() {
                                if ev.turn_id == id.as_str()
                                    && let Some(se) = to_stream_event(&ev) {
                                        write_line(out, &notification(&se)).await?;
                                    }
                            }
                            let outcome = outcome?;
                            let resp = RpcResponse::ok(req.id.clone(), outcome_json(&sid, id.as_str(), &outcome));
                            write_line(out, &serde_json::to_value(resp)?).await?;
                            return Ok(());
                        }
                        _ = &mut closed => {
                            self.abandon(&sid, id.as_str()).await;
                            return Ok(());
                        }
                        ev = rx.recv() => {
                            match ev {
                                Ok(ev) if ev.turn_id == id.as_str() => {
                                    if matches!(ev.kind, BusKind::Finished(_)) {
                                        continue;
                                    }
                                    if let Some(se) = to_stream_event(&ev)
                                        && let Err(e) = write_line(out, &notification(&se)).await
                                    {
                                        // Client parti entre deux fragments.
                                        self.abandon(&sid, id.as_str()).await;
                                        return Err(e);
                                    }
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                Err(_) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Issue d'un tour, pour la CLI.
pub fn outcome_json(session_id: &str, turn_id: &str, o: &TurnOutcome) -> Value {
    let (kind, extra) = match o {
        TurnOutcome::Answered {
            text,
            iterations,
            cost_usd,
        } => (
            "answered",
            json!({"text": text, "iterations": iterations, "cost_usd": cost_usd}),
        ),
        TurnOutcome::AwaitingApproval { approval_id } => {
            ("awaiting_approval", json!({"approval_id": approval_id}))
        }
        TurnOutcome::LoopAborted {
            report,
            answer,
            choices,
        } => (
            "loop_aborted",
            json!({"report": report, "answer": answer, "choices": choices}),
        ),
        TurnOutcome::Cancelled => ("cancelled", json!({})),
        TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        } => (
            "budget_exceeded",
            json!({
                "scope": scope,
                "spent_usd": spent_usd,
                "limit_usd": limit_usd,
                "text": penelope_agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
            }),
        ),
        TurnOutcome::Failed { error } => ("failed", json!({"error": error})),
    };
    let mut v = json!({"session": session_id, "turn": turn_id, "outcome": kind});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    v
}

fn to_stream_event(ev: &penelope_app::bus::BusEvent) -> Option<StreamEvent> {
    use penelope_agent::TurnEvent as T;
    let session_id = ev.session_id.clone();
    Some(match &ev.kind {
        BusKind::Event(T::Delta(text)) => StreamEvent::Delta {
            session_id,
            text: text.clone(),
        },
        BusKind::Event(T::Reasoning(text)) => StreamEvent::Reasoning {
            session_id,
            text: text.clone(),
        },
        BusKind::Event(T::ToolCall { name, args }) => StreamEvent::ToolCall {
            session_id,
            name: name.clone(),
            args: args.clone(),
        },
        BusKind::Event(T::ToolResult { name, ok, preview }) => StreamEvent::ToolResult {
            session_id,
            name: name.clone(),
            ok: *ok,
            preview: preview.clone(),
        },
        BusKind::Event(T::Approval { id, tool, risk, .. }) => StreamEvent::Approval {
            id: id.clone(),
            kind: "tool_call".into(),
            subject: tool.clone(),
            risk: risk.as_str().into(),
        },
        BusKind::Finished(TurnOutcome::Answered { text, .. }) => StreamEvent::Done {
            session_id,
            text: text.clone(),
        },
        BusKind::Finished(TurnOutcome::Failed { error }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: error.clone(),
        },
        // L'événement d'approbation est déjà parti pendant le tour.
        BusKind::Finished(TurnOutcome::AwaitingApproval { .. }) => return None,
        BusKind::Finished(TurnOutcome::Cancelled) => StreamEvent::Error {
            session_id: Some(session_id),
            message: "génération arrêtée".into(),
        },
        // Le rapport technique reste dans les événements et les journaux (issue #31).
        BusKind::Finished(TurnOutcome::LoopAborted {
            answer, choices, ..
        }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: format!("{answer}\n\nSuites possibles : {}", choices.join(" · ")),
        },
        BusKind::Finished(TurnOutcome::BudgetExceeded {
            scope,
            spent_usd,
            limit_usd,
        }) => StreamEvent::Error {
            session_id: Some(session_id),
            message: penelope_agent::budget_exceeded_text(scope, *spent_usd, *limit_usd),
        },
        _ => return None,
    })
}

fn notification(se: &StreamEvent) -> Value {
    json!({"jsonrpc": JSONRPC, "method": "event", "params": se})
}

pub(super) async fn write_line<W: tokio::io::AsyncWrite + Unpin>(
    out: &mut W,
    v: &Value,
) -> anyhow::Result<()> {
    let mut body = serde_json::to_string(v)?;
    body.push('\n');
    out.write_all(body.as_bytes()).await?;
    out.flush().await?;
    Ok(())
}
