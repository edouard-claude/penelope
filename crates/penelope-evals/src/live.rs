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
use penelope_app::services::Services;
use penelope_daemon::Daemon;
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
        Ok(paths)
    })
    .expect("configuration");
    d
}

/// Un tour complet de conversation CLI ; renvoie le texte de la réponse.
pub async fn turn(d: &Arc<Daemon>, session: &str, text: &str) -> String {
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
    match penelope_daemon::runner::process(d, turn, Duration::from_secs(30)).await {
        penelope_agent::TurnOutcome::Answered { text, .. } => text,
        other => panic!("tour sans réponse : {other:?}"),
    }
}
