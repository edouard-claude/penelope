//! #259 : le serveur d'inférence locale, contre un faux serveur qui rend le catalogue de
//! mlx_lm.server tel qu'il a été capturé.

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Catalogue réel de mlx_lm.server 0.31.3 : le cache Hugging Face, sans fenêtre.
const MLX_MODELS: &str = r#"{"object": "list", "data": [{"id": "mlx-community/Qwen3-1.7B-4bit", "object": "model", "created": 1790674667}, {"id": "mlx-community/Llama-3.2-1B-Instruct-4bit", "object": "model", "created": 1790674667}]}"#;

const QWEN: &str = "mlx-community/Qwen3-1.7B-4bit";

/// Faux serveur qui rend `body` à toute requête.
async fn serving(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    format!("http://{addr}/v1")
}

/// Une adresse où rien n'écoute : le serveur arrêté.
async fn stopped() -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    format!("http://{addr}/v1")
}

fn local_main(s: &Services, base_url: String, model: &str) {
    let model = format!("local:{model}");
    s.publish_config("test", move |c| {
        c.providers.local.enabled = true;
        c.providers.local.base_url = base_url;
        c.models.aliases.insert("main".into(), model);
        Ok(vec!["providers.local".into()])
    })
    .unwrap();
}

/// Sans endpoint local actif, aucun contrôle : la sortie de `doctor` ne change pas pour
/// une instance qui n'en a pas. Un endpoint qui ne sert que la voix n'est pas sondé non
/// plus (whisper-server n'a pas de `GET /models`).
#[tokio::test]
async fn no_local_text_alias_means_no_probe() {
    let (_d, s) = services().await;
    assert!(local_inference_checks(&s).await.is_empty());
    s.publish_config("test", |c| {
        c.providers.local.enabled = true;
        Ok(vec!["providers.local.enabled".into()])
    })
    .unwrap();
    assert!(
        s.config.config().models.aliases["tts"].starts_with("openai_compat:"),
        "le défaut vise un serveur local pour la voix"
    );
    assert!(local_inference_checks(&s).await.is_empty());
}

/// Serveur arrêté : signalé, avec les alias qui passent au repli et la commande qui le
/// remet en route.
#[tokio::test]
async fn a_stopped_server_is_reported_with_its_fix() {
    let (_d, s) = services().await;
    local_main(&s, stopped().await, QWEN);
    let checks = local_inference_checks(&s).await;
    assert_eq!(checks.len(), 1);
    let c = &checks[0];
    assert_eq!(c.id, "local.local");
    assert!(!c.ok);
    assert!(c.detail.contains("serveur injoignable"), "{}", c.detail);
    assert!(c.detail.contains("`main`"), "{}", c.detail);
    let fix = c.fix.as_deref().unwrap();
    assert!(
        fix == format!("penelope local install {QWEN} --endpoint local")
            || fix.starts_with("launchctl kickstart -k gui/$(id -u)/com.penelope.inference.local"),
        "{fix}"
    );
    // Toute la passe le porte.
    assert!(run(&s).await.iter().any(|x| x.id == "local.local" && !x.ok));
}

/// Serveur sans modèle, ou sans le modèle visé : signalé avec ce qu'il sert.
#[tokio::test]
async fn a_server_without_the_model_is_reported() {
    let (_d, s) = services().await;
    local_main(&s, serving(r#"{"object":"list","data":[]}"#).await, QWEN);
    let c = &local_inference_checks(&s).await[0];
    assert!(!c.ok && c.detail.contains("ne sert aucun modèle"), "{c:?}");

    local_main(&s, serving(MLX_MODELS).await, "mlx-community/Qwen3-8B-4bit");
    let c = &local_inference_checks(&s).await[0];
    assert!(!c.ok, "{c:?}");
    assert!(
        c.detail
            .contains("`main` → `mlx-community/Qwen3-8B-4bit` non servi(s)"),
        "{}",
        c.detail
    );
    assert!(c.detail.contains(QWEN), "les modèles servis sont nommés");
    assert!(c.fix.as_deref().unwrap().contains("hf download"));
}

/// Tout va : les modèles servis, la fenêtre configurée faute d'annonce, et la part du
/// préfixe relue du cache, mesurée sur l'usage des sept derniers jours.
#[tokio::test]
async fn a_healthy_server_reports_window_and_prefix_cache() {
    let (_d, s) = services().await;
    local_main(&s, serving(MLX_MODELS).await, QWEN);
    let c = &local_inference_checks(&s).await[0];
    assert!(c.ok, "{c:?}");
    assert!(c.detail.contains("2 modèle(s) servi(s)"), "{}", c.detail);
    assert!(
        c.detail
            .contains("fenêtre 32768 (configurée, le serveur ne l'annonce pas)"),
        "{}",
        c.detail
    );
    assert!(c.detail.contains("pas encore mesuré"), "{}", c.detail);

    let now = s.clock.now_rfc3339();
    s.store
        .write(move |tx| {
            for (prompt, cached) in [(4_000, 0), (4_200, 4_000)] {
                tx.execute(
                    "INSERT INTO usage(ts, day, model, provider, prompt, completion, cached)
                     VALUES(?1, '2026-09-29', ?2, 'openai_compat', ?3, 10, ?4)",
                    penelope_store::rusqlite::params![now, QWEN, prompt, cached],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let c = &local_inference_checks(&s).await[0];
    assert!(
        c.detail
            .contains("49 % de l'entrée relue du cache sur 7 jours (2 appel(s))"),
        "{}",
        c.detail
    );
}

/// Un endpoint de `providers.extra` qui liste le modèle est sondé sous son nom, même
/// avec `providers.local` éteint.
#[tokio::test]
async fn an_extra_endpoint_is_probed_under_its_name() {
    let (_d, s) = services().await;
    let url = serving(MLX_MODELS).await;
    s.publish_config("test", move |c| {
        c.providers.extra.insert(
            "mlx".into(),
            penelope_kernel::config::LocalProvider {
                enabled: true,
                base_url: url,
                models: vec![QWEN.into()],
                ..Default::default()
            },
        );
        c.models
            .aliases
            .insert("main".into(), format!("local:{QWEN}"));
        Ok(vec!["providers.extra".into()])
    })
    .unwrap();
    let checks = local_inference_checks(&s).await;
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].id, "local.mlx");
    assert!(checks[0].ok, "{:?}", checks[0]);
}
