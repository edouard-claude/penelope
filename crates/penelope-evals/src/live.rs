//! Outillage des suites réseau du §20.1 : `ctx-recall`, `mem-longitudinal`,
//! `live-openrouter`, `live-telegram`, `ab-hermes`.
//!
//! Elles parlent à de vrais services (modèles facturés, bot réel, instance Hermes) : leurs
//! tests sont ignorés par défaut et se lancent explicitement.
//!
//! ```text
//! penelope eval live-openrouter
//!   └─ cargo test -p penelope-evals --test live_openrouter -- --ignored
//!        └─ variables d'environnement vérifiées d'entrée, message clair si l'une manque
//! ```
//!
//! Les secrets viennent de l'environnement du processus de test, jamais d'un argument.

use penelope_app::bus::Origin;
use penelope_app::engine::TurnIntake;
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::clock::SharedClock;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Valeurs des variables demandées ; échoue en les nommant toutes si l'une manque.
pub fn require_env(names: &[&str]) -> Vec<String> {
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| {
            std::env::var(n)
                .map(|v| v.trim().is_empty())
                .unwrap_or(true)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "suite réseau : variable(s) d'environnement absente(s) : {}",
        missing.join(", ")
    );
    names
        .iter()
        .map(|n| std::env::var(n).unwrap_or_default())
        .collect()
}

/// Identifiant de modèle : variable d'environnement, sinon valeur par défaut.
pub fn model(env: &str, default: &str) -> String {
    std::env::var(env)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Daemon de test branché sur OpenRouter (`OPENROUTER_API_KEY`), revue de mémoire
/// active. Les alias `main`, `fast`, `reasoning` et `summarizer` suivent
/// `PENELOPE_LIVE_MODEL` (un seul modèle, le moins cher suffit), sauf surcharge
/// `PENELOPE_LIVE_<ALIAS>`.
pub async fn daemon(root: &Path, clock: SharedClock) -> Arc<Daemon> {
    let key = require_env(&["OPENROUTER_API_KEY"]).remove(0);
    let s = Services::for_tests(root.to_path_buf(), clock)
        .await
        .expect("services");
    s.platform
        .secrets
        .set("openrouter_api_key", &key)
        .expect("secret");
    let d = Arc::new(Daemon::from_services(Arc::new(s)));
    let base = model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    d.publish_config("live", |c| {
        let mut paths = Vec::new();
        for alias in ["main", "fast", "reasoning", "summarizer"] {
            let v = model(&format!("PENELOPE_LIVE_{}", alias.to_uppercase()), &base);
            c.models.aliases.insert(alias.to_string(), v);
            paths.push(format!("models.aliases.{alias}"));
        }
        c.memory.review_max_candidates = 5;
        paths.push("memory.review_max_candidates".into());
        // Personne ne répond aux cartes pendant une suite réseau : tout sauf le destructif
        // passe sans approbation, sinon le premier outil demandé arrête le tour.
        c.tools.approval_mode = "auto".into();
        paths.push("tools.approval_mode".into());
        Ok(paths)
    })
    .expect("configuration");
    d
}

/// Un tour complet de conversation CLI ; renvoie le texte de la réponse.
pub async fn turn(d: &Arc<Daemon>, session: &str, text: &str) -> String {
    match turn_outcome(d, session, text).await {
        penelope_agent::TurnOutcome::Answered { text, .. } => text,
        other => panic!("tour sans réponse : {other:?}"),
    }
}

/// Un tour complet de conversation CLI, et son issue telle quelle.
pub async fn turn_outcome(
    d: &Arc<Daemon>,
    session: &str,
    text: &str,
) -> penelope_agent::TurnOutcome {
    let id = d
        .enqueue_message(session, text, &Origin::Cli, None)
        .await
        .expect("mise en file")
        .expect("tour créé");
    let turn = loop {
        match d.services.turns.claim("live").await.expect("réclamation") {
            Some(t) if t.id == id => break t,
            Some(other) => {
                // Un tour d'une autre nature (relance) : traité tel quel.
                let _ = penelope_daemon::runner::process(d, other, Duration::from_secs(30)).await;
            }
            None => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    };
    penelope_daemon::runner::process(d, turn, Duration::from_secs(30)).await
}

/// Daemon de test dont les alias de conversation visent un serveur d'inférence local
/// (#259) : `providers.local` sur `base_url`, OpenRouter éteint sauf si `openrouter`
/// donne l'adresse d'un faux OpenRouter, qui sert alors l'alias de repli `cloud`. Le
/// classifieur est éteint : chaque tour ne fait que ses propres appels.
pub async fn local_daemon(
    root: &Path,
    clock: SharedClock,
    base_url: &str,
    model: &str,
    openrouter: Option<&str>,
) -> Arc<Daemon> {
    let s = Services::for_tests(root.to_path_buf(), clock)
        .await
        .expect("services");
    if openrouter.is_some() {
        s.platform
            .secrets
            .set("openrouter_api_key", "sk-or-v1-faux-openrouter-local")
            .expect("secret");
    }
    let d = Arc::new(Daemon::from_services(Arc::new(s)));
    let (base_url, model) = (base_url.to_string(), model.to_string());
    let openrouter = openrouter.map(String::from);
    d.publish_config("live-local", move |c| {
        c.providers.local.enabled = true;
        c.providers.local.base_url = base_url;
        c.providers.openrouter.enabled = openrouter.is_some();
        if let Some(url) = openrouter {
            c.providers.openrouter.base_url = url;
            c.models
                .aliases
                .insert("cloud".into(), "openrouter:faux/repli".into());
            c.models
                .routing
                .fallback
                .insert("main".into(), vec!["cloud".into()]);
        }
        for alias in ["main", "fast", "reasoning", "summarizer"] {
            c.models.aliases.insert(alias.to_string(), model.clone());
        }
        c.models.routing.classifier = false;
        c.tools.approval_mode = "auto".into();
        Ok(vec![
            "providers".into(),
            "models.aliases".into(),
            "models.routing".into(),
            "tools.approval_mode".into(),
        ])
    })
    .expect("configuration");
    d
}
