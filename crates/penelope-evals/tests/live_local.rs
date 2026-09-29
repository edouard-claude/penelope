//! Suite `live-local` (#259) : un vrai tour de Pénélope contre un serveur d'inférence
//! local (mlx_lm.server), et le repli sur OpenRouter quand il est arrêté.
//!
//! ```bash
//! mlx_lm.server --model mlx-community/Qwen3-1.7B-4bit --port 8080 --max-tokens 4096
//! PENELOPE_LIVE_LOCAL_MODEL=local:mlx-community/Qwen3-1.7B-4bit penelope eval live-local
//! ```
//!
//! `PENELOPE_LIVE_LOCAL_URL` change l'adresse (`http://127.0.0.1:8080/v1` par défaut).
//! Les tests du serveur réel sont ignorés par défaut ; celui du repli ne demande aucun
//! serveur (l'adresse d'un port fermé, un faux OpenRouter local, aucune clé réelle) et
//! tourne avec les autres. Les chiffres (temps jusqu'au premier jeton, jetons relus du
//! cache) s'affichent avec `--nocapture`.

use penelope_app::engine::TurnIntake;
use penelope_evals::live;
use penelope_kernel::clock::{SharedClock, SystemClock};
use penelope_llm::provider::{CancelToken, ChunkStream};
use penelope_llm::types::{ChatRequest, StreamChunk};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// La session CLI du daemon, où jouer les tours.
async fn session(d: &penelope_daemon::runtime::Daemon) -> String {
    d.chat_session_for(&penelope_app::bus::Origin::Cli)
        .await
        .expect("session")
}

fn base_url() -> String {
    live::model("PENELOPE_LIVE_LOCAL_URL", "http://127.0.0.1:8080/v1")
}

fn local_model() -> String {
    live::require_env(&["PENELOPE_LIVE_LOCAL_MODEL"]).remove(0)
}

/// Un appel au modèle vu du harnais : taille de la requête, temps jusqu'au premier
/// fragment, jetons d'entrée et jetons relus du cache.
#[derive(Debug, Clone, Default)]
struct Call {
    messages: usize,
    first_token: Option<Duration>,
    total: Option<Duration>,
    prompt: u64,
    cached: u64,
}

/// Le vrai fournisseur local, chronométré : chaque appel du tour passe par lui.
struct Timed {
    inner: Arc<dyn penelope_llm::Provider>,
    calls: Arc<Mutex<Vec<Call>>>,
}

#[async_trait::async_trait]
impl penelope_llm::Provider for Timed {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancelToken,
    ) -> penelope_llm::Result<ChunkStream> {
        let started = Instant::now();
        let index = {
            let mut calls = self.calls.lock().unwrap();
            calls.push(Call {
                messages: req.messages.len(),
                ..Default::default()
            });
            calls.len() - 1
        };
        let mut rx = self.inner.chat_stream(req, cancel).await?;
        let (tx, out) = tokio::sync::mpsc::channel(64);
        let calls = self.calls.clone();
        tokio::spawn(async move {
            while let Some(chunk) = rx.recv().await {
                {
                    let mut calls = calls.lock().unwrap();
                    let call = &mut calls[index];
                    match &chunk {
                        StreamChunk::Delta { .. }
                        | StreamChunk::Reasoning { .. }
                        | StreamChunk::ToolCall(_) => {
                            call.first_token.get_or_insert(started.elapsed());
                        }
                        StreamChunk::Usage(u) => {
                            call.prompt = u.prompt;
                            call.cached = u.cached;
                        }
                        StreamChunk::Done { .. } => call.total = Some(started.elapsed()),
                        _ => {}
                    }
                }
                if tx.send(chunk).await.is_err() {
                    return;
                }
            }
        });
        Ok(out)
    }

    async fn fetch_models(&self) -> penelope_llm::Result<Vec<penelope_llm::ModelInfo>> {
        self.inner.fetch_models().await
    }
}

/// Daemon sur le serveur réel, chaque appel chronométré.
async fn timed_daemon(
    root: &std::path::Path,
) -> (Arc<penelope_daemon::runtime::Daemon>, Arc<Mutex<Vec<Call>>>) {
    let clock: SharedClock = Arc::new(SystemClock);
    let model = local_model();
    let d = live::local_daemon(root, clock, &base_url(), &model, None).await;
    let inner = d.provider_for(&model).await.expect("fournisseur local");
    // Le catalogue du serveur, comme au démarrage du daemon quand le modèle de
    // conversation est local.
    inner.fetch_models().await.expect("serveur local joignable");
    let calls = Arc::new(Mutex::new(Vec::new()));
    d.set_provider_override(Arc::new(Timed {
        inner,
        calls: calls.clone(),
    }));
    (d, calls)
}

async fn query_i64(d: &penelope_daemon::runtime::Daemon, sql: &'static str) -> i64 {
    d.services
        .store
        .read(move |c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap()
}

fn report(label: &str, calls: &[Call]) {
    for (i, c) in calls.iter().enumerate() {
        eprintln!(
            "{label} appel {} : {} messages, premier jeton {:?}, total {:?}, {} jetons d'entrée \
             dont {} relus du cache",
            i + 1,
            c.messages,
            c.first_token,
            c.total,
            c.prompt,
            c.cached
        );
    }
}

/// Un tour réel : une réponse, comptée à 0 $ sans fausser l'usage (jetons comptés,
/// coût nul, estimé faute de facture).
#[tokio::test]
#[ignore = "serveur local : PENELOPE_LIVE_LOCAL_MODEL, mlx_lm.server lancé"]
async fn a_local_turn_answers_at_zero_cost() {
    let dir = tempfile::tempdir().unwrap();
    let (d, calls) = timed_daemon(dir.path()).await;
    let sid = session(&d).await;
    let answer = live::turn(&d, &sid, "Réponds par un seul mot : bonjour.").await;
    assert!(!answer.trim().is_empty(), "réponse vide");
    report("tour simple", &calls.lock().unwrap());
    let rows = query_i64(
        &d,
        "SELECT COUNT(*) FROM usage WHERE provider = 'openai_compat'",
    )
    .await;
    let prompt = query_i64(&d, "SELECT COALESCE(SUM(prompt), 0) FROM usage").await;
    let cost = d
        .services
        .store
        .read(|c| {
            Ok(
                c.query_row("SELECT COALESCE(SUM(cost_usd), 0) FROM usage", [], |r| {
                    r.get::<_, f64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert!(rows >= 1, "aucun usage local enregistré");
    assert!(prompt > 0, "jetons d'entrée non comptés");
    assert_eq!(cost, 0.0, "un modèle local ne coûte rien");
    eprintln!("réponse : {answer}");
}

/// Un appel d'outil rendu par le modèle local est exécuté par la boucle. L'issue du tour
/// (réponse, ou arrêt du détecteur de boucles quand un petit modèle rappelle l'outil) dit
/// la qualité du modèle, pas celle du harnais : elle est affichée, pas exigée.
#[tokio::test]
#[ignore = "serveur local : PENELOPE_LIVE_LOCAL_MODEL, mlx_lm.server lancé"]
async fn a_local_tool_call_is_executed() {
    let dir = tempfile::tempdir().unwrap();
    let (d, calls) = timed_daemon(dir.path()).await;
    let sid = session(&d).await;
    let outcome = live::turn_outcome(
        &d,
        &sid,
        "Appelle une seule fois l'outil self_status avec section \"model\", puis dis en une \
         phrase quel modèle te fait répondre.",
    )
    .await;
    report("tour outillé", &calls.lock().unwrap());
    let results = d
        .services
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT payload FROM events WHERE kind = 'tool.result'")?;
            let rows = st
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    let executed: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|p| serde_json::from_str(p).ok())
        .filter(|v: &serde_json::Value| v["tool"] == "self_status" && v["ok"] == true)
        .collect();
    eprintln!("résultats d'outil : {executed:?}");
    eprintln!("issue du tour : {outcome:?}");
    assert!(
        !executed.is_empty(),
        "aucun `self_status` exécuté avec succès : {results:?}"
    );
    assert!(
        calls.lock().unwrap().len() >= 2,
        "le résultat de l'outil n'est jamais revenu au modèle"
    );
}

/// Deux tours de la même session : le second relit le préfixe du premier dans le cache
/// du serveur. Mesure le temps jusqu'au premier jeton des deux.
#[tokio::test]
#[ignore = "serveur local : PENELOPE_LIVE_LOCAL_MODEL, mlx_lm.server lancé"]
async fn the_second_turn_reuses_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let (d, calls) = timed_daemon(dir.path()).await;
    let sid = session(&d).await;
    live::turn(&d, &sid, "Dis bonjour en trois mots.").await;
    let first = calls.lock().unwrap().clone();
    live::turn(&d, &sid, "Et maintenant au revoir, en trois mots.").await;
    let all = calls.lock().unwrap().clone();
    let second = &all[first.len()..];
    report("tour 1", &first);
    report("tour 2", second);
    let (a, b) = (&first[0], &second[0]);
    assert!(b.prompt > 0 && a.prompt > 0);
    assert!(
        b.cached > 0,
        "le second tour n'a rien relu du cache : {b:?} (mlx_lm.server lancé avec \
         --prompt-cache-size 0 ?)"
    );
    eprintln!(
        "premier jeton : tour 1 {:?} ({} jetons), tour 2 {:?} ({} jetons dont {} du cache)",
        a.first_token, a.prompt, b.first_token, b.prompt, b.cached
    );
}

/// Faux OpenRouter : rend une réponse courte en SSE et garde les requêtes reçues.
async fn fake_openrouter() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut got = Vec::new();
            let mut buf = vec![0u8; 65536];
            loop {
                let n = sock.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&got).to_string());
            let sse = [
                r#"data: {"id":"gen-repli","model":"faux/repli","provider":"Faux","choices":[{"index":0,"delta":{"role":"assistant","content":"Réponse du repli."},"finish_reason":"stop"}]}"#,
                r#"data: {"id":"gen-repli","model":"faux/repli","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"cost":0.0001}}"#,
                "data: [DONE]",
            ]
            .iter()
            .map(|e| format!("{e}\n\n"))
            .collect::<String>();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{sse}"
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    (format!("http://{addr}/api/v1"), seen)
}

/// #259 : serveur local arrêté, le tour passe au repli OpenRouter de l'alias et y répond.
/// Avant, le repli repartait vers le fournisseur du modèle principal (le serveur local
/// arrêté) avec le nom du modèle de repli, et le tour finissait en erreur.
#[tokio::test]
async fn a_stopped_local_server_falls_back_to_openrouter() {
    let dir = tempfile::tempdir().unwrap();
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stopped = format!("http://{}/v1", closed.local_addr().unwrap());
    drop(closed);
    let (openrouter, seen) = fake_openrouter().await;
    let clock: SharedClock = Arc::new(SystemClock);
    let d = live::local_daemon(
        dir.path(),
        clock,
        &stopped,
        "local:mlx-community/Qwen3-1.7B-4bit",
        Some(&openrouter),
    )
    .await;
    let sid = session(&d).await;
    let answer = tokio::time::timeout(Duration::from_secs(60), live::turn(&d, &sid, "Bonjour ?"))
        .await
        .expect("le tour ne se bloque pas sur le serveur arrêté");
    assert_eq!(answer, "Réponse du repli.");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|r| r.contains("/chat/completions") && r.contains(r#""model":"faux/repli""#)),
        "OpenRouter n'a pas reçu le modèle de repli : {seen:?}"
    );
    let providers = d
        .services
        .store
        .read(|c| {
            let mut st = c.prepare("SELECT provider, model FROM usage")?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert!(
        providers.iter().any(|(p, _)| p == "openrouter"),
        "{providers:?}"
    );
}
