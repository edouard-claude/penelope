//! Vérification externe de l'environnement de dev : chaque contrôle est joué depuis
//! l'extérieur, comme le ferait un utilisateur, et laisse sa preuve (requête, statut,
//! extrait de réponse, durée).
//!
//! `http` et `graphql` passent par le client HTTP de Pénélope. `command` lance l'outil que
//! le projet a déjà (Playwright, Maestro…) à la racine du dépôt, `E2E_BASE_URL` posé :
//! Pénélope n'installe aucun navigateur ni émulateur de son propre chef.

use super::super::*;
use penelope_workflow::delivery::config::{Check, CheckKind, E2ePlan};
use penelope_workflow::delivery::verdicts::{Observed, judge};
use std::path::Path;

const TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(900);
/// Part d'une réponse gardée en preuve.
const EXCERPT_CHARS: usize = 1_000;

/// La preuve d'un contrôle.
#[derive(Debug, Clone)]
pub struct Proof {
    pub ok: bool,
    pub value: Value,
}

fn excerpt(text: &str) -> String {
    let redacted = penelope_observe::redact(text);
    let mut out: String = redacted.chars().take(EXCERPT_CHARS).collect();
    if redacted.chars().count() > EXCERPT_CHARS {
        out.push('…');
    }
    out
}

/// Joue tous les contrôles, dans l'ordre : un contrôle rouge n'empêche pas les suivants,
/// la preuve dit tout ce qui marche et tout ce qui casse.
pub async fn run(ctx: &StepCtx<'_>, plan: &E2ePlan, dir: &Path) -> Vec<Proof> {
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("penelope/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut proofs = Vec::new();
    for check in &plan.checks {
        let started = ctx.s().clock.now_ms();
        let mut proof = match (&http, check.kind) {
            (_, CheckKind::Command) => command(ctx, plan, check, dir).await,
            (Ok(client), _) => request(client, plan, check).await,
            (Err(e), _) => Proof {
                ok: false,
                value: json!({"detail": format!("client HTTP : {e}")}),
            },
        };
        proof.value["check"] = json!(check.label());
        proof.value["kind"] = json!(format!("{:?}", check.kind).to_lowercase());
        proof.value["ok"] = json!(proof.ok);
        proof.value["ms"] = json!(ctx.s().clock.now_ms().saturating_sub(started));
        proofs.push(proof);
    }
    proofs
}

async fn request(client: &reqwest::Client, plan: &E2ePlan, check: &Check) -> Proof {
    let (method, url, builder) = match check.kind {
        CheckKind::Graphql => {
            let path = if check.path.is_empty() {
                "/graphql"
            } else {
                &check.path
            };
            let url = format!("{}{path}", plan.url);
            let body = json!({"query": check.query.clone().unwrap_or_default()});
            (
                "POST".to_string(),
                url.clone(),
                client.post(&url).json(&body),
            )
        }
        _ => {
            let method = check
                .method
                .clone()
                .unwrap_or_else(|| "GET".into())
                .to_ascii_uppercase();
            let url = format!("{}{}", plan.url, check.path);
            let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
            (method, url.clone(), client.request(m, &url))
        }
    };
    let mut value = json!({"request": {"method": method, "url": url}});
    let seen = match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            Observed {
                status,
                body: resp.text().await.unwrap_or_default(),
            }
        }
        Err(e) => {
            value["detail"] = json!(format!("injoignable : {}", e.without_url()));
            return Proof { ok: false, value };
        }
    };
    value["status"] = json!(seen.status);
    value["excerpt"] = json!(excerpt(&seen.body));
    let verdict = judge(check, &seen);
    let ok = verdict.is_ok();
    value["detail"] = json!(verdict.unwrap_or_else(|e| e));
    Proof { ok, value }
}

/// Cite une valeur pour un shell POSIX.
fn quote(v: &str) -> String {
    format!("'{}'", v.replace('\'', "'\\''"))
}

async fn command(ctx: &StepCtx<'_>, plan: &E2ePlan, check: &Check, dir: &Path) -> Proof {
    let s = ctx.s();
    let cfg = s.config.config();
    let raw = check.command.clone().unwrap_or_default();
    let line = format!("E2E_BASE_URL={} {raw}", quote(&plan.url));
    // Un E2E parle à l'environnement de dev : le réseau lui est ouvert, le reste du bac
    // à sable reste celui des commandes de workflow.
    let profile = penelope_tools::shell::profile_with_denied_reads(
        &cfg.sandbox.default_profile,
        dir,
        true,
        &penelope_executor::executor::denied_reads(s),
    );
    let mut value = json!({"request": {"command": raw, "cwd": dir.to_string_lossy()}});
    match penelope_tools::shell::exec(
        &s.platform.processes,
        &line,
        penelope_tools::shell::ExecOptions {
            profile: Some(&profile),
            cwd: Some(dir),
            timeout: COMMAND_TIMEOUT,
            max_output_bytes: cfg.tools.max_output_bytes,
            shell: penelope_executor::executor::shell_override(&cfg.tools.shell),
            cancel: Some(ctx.cancel),
        },
    )
    .await
    {
        Ok(out) => {
            value["exitCode"] = json!(out.exit_code);
            value["excerpt"] = json!(excerpt(&format!("{}\n{}", out.stdout, out.stderr)));
            let ok = out.exit_code == 0;
            value["detail"] = json!(if ok {
                "code de sortie 0".to_string()
            } else {
                format!("code de sortie {}", out.exit_code)
            });
            Proof { ok, value }
        }
        Err(e) => {
            value["detail"] = json!(format!("commande non lancée : {e}"));
            Proof { ok: false, value }
        }
    }
}
