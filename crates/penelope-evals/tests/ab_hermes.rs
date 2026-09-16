//! Suite `ab-hermes` (§20.1, §20.2 point 4, réseau) : les 30 tâches de `ab/tasks.json`
//! rejouées sur Hermes puis sur Pénélope ; réussite, latence, tokens et coût, rapport
//! Markdown. Critère : taux de réussite de Pénélope ≥ Hermes, coût ≤ Hermes.
//!
//! ```bash
//! PENELOPE_AB_HERMES_CMD='hermes chat -Q -q {prompt}' penelope eval ab-hermes
//! ```
//!
//! Commandes lancées sans shell : `{prompt}` devient un argument à part entière.
//! `PENELOPE_AB_PENELOPE_CMD` (défaut `penelope chat {prompt}`), `PENELOPE_AB_TIMEOUT_S`
//! (défaut 180), `PENELOPE_AB_REPORT` (défaut `target/ab-hermes.md`). Coût et tokens :
//! lus dans la sortie quand l'agent les affiche, sinon par différence des totaux que rend
//! `PENELOPE_AB_*_USAGE_CMD` avant et après la tâche : du JSON `[{"costUsd": …,
//! "tokens": …}, …]` sommé ligne à ligne (défaut côté Pénélope :
//! `penelope usage --by day --limit 500 --json`).

use penelope_evals::live;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct Task {
    id: String,
    prompt: String,
    expect: String,
}

#[derive(Debug, Default, Clone)]
struct Outcome {
    ok: bool,
    ms: u128,
    tokens: Option<u64>,
    cost: Option<f64>,
    excerpt: String,
}

fn command(template: &str, prompt: Option<&str>) -> (String, Vec<String>) {
    let mut parts = template.split_whitespace().map(|p| match prompt {
        Some(text) if p == "{prompt}" => text.to_string(),
        _ => p.to_string(),
    });
    let program = parts.next().expect("commande vide");
    (program, parts.collect())
}

async fn run(template: &str, prompt: Option<&str>, timeout: Duration) -> (bool, String) {
    let (program, args) = command(template, prompt);
    let child = tokio::process::Command::new(&program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(timeout, child).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.success(), strip_ansi(&text))
        }
        Ok(Err(e)) => (false, format!("{program} : {e}")),
        Err(_) => (false, format!("délai de {} s dépassé", timeout.as_secs())),
    }
}

fn strip_ansi(s: &str) -> String {
    Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]")
        .unwrap()
        .replace_all(s, "")
        .to_string()
}

/// Tokens et coût affichés par un agent, s'il le fait.
fn usage_in(text: &str) -> (Option<u64>, Option<f64>) {
    let tokens = Regex::new(r"(?i)(\d[\d\u{202f}\u{a0} ,]*)\s*tokens")
        .unwrap()
        .captures_iter(text)
        .filter_map(|c| {
            c[1].chars()
                .filter(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
        .max();
    let cost = Regex::new(r"(?:\$\s?(\d+[.,]\d+))|(?:(\d+[.,]\d+)\s?\$)")
        .unwrap()
        .captures_iter(text)
        .filter_map(|c| {
            c.get(1)
                .or_else(|| c.get(2))
                .and_then(|m| m.as_str().replace(',', ".").parse::<f64>().ok())
        })
        .last();
    (tokens, cost)
}

/// Totaux (tokens, coût) d'une commande d'usage qui rend un tableau JSON.
async fn usage_totals(command_env: &str, default: Option<&str>) -> Option<(u64, f64)> {
    let template = std::env::var(command_env)
        .ok()
        .or(default.map(String::from))
        .filter(|t| !t.trim().is_empty())?;
    let (ok, out) = run(&template, None, Duration::from_secs(30)).await;
    if !ok {
        return None;
    }
    let rows: Vec<Value> = out
        .find('[')
        .and_then(|i| serde_json::from_str(&out[i..]).ok())?;
    Some(rows.iter().fold((0, 0.0), |(t, c), r| {
        (
            t + r["tokens"].as_u64().unwrap_or(0),
            c + r["costUsd"]
                .as_f64()
                .or_else(|| r["cost_usd"].as_f64())
                .unwrap_or(0.0),
        )
    }))
}

async fn attempt(
    template: &str,
    usage_env: &str,
    usage_default: Option<&str>,
    task: &Task,
    timeout: Duration,
) -> Outcome {
    let before = usage_totals(usage_env, usage_default).await;
    let started = Instant::now();
    let (_, out) = run(template, Some(&task.prompt), timeout).await;
    let ms = started.elapsed().as_millis();
    let re = Regex::new(&format!("(?i){}", task.expect)).expect("expression de la tâche");
    let (mut tokens, mut cost) = usage_in(&out);
    if tokens.is_none()
        && cost.is_none()
        && let (Some((t0, c0)), Some((t1, c1))) =
            (before, usage_totals(usage_env, usage_default).await)
    {
        (tokens, cost) = (Some(t1.saturating_sub(t0)), Some((c1 - c0).max(0.0)));
    }
    Outcome {
        ok: re.is_match(&out),
        ms,
        tokens,
        cost,
        excerpt: out
            .trim()
            .chars()
            .take(80)
            .collect::<String>()
            .replace(['\n', '|'], " "),
    }
}

fn total(outcomes: &[Outcome]) -> (usize, u128, Option<u64>, Option<f64>) {
    let ok = outcomes.iter().filter(|o| o.ok).count();
    let ms = outcomes.iter().map(|o| o.ms).sum();
    let tokens = outcomes
        .iter()
        .map(|o| o.tokens)
        .collect::<Option<Vec<_>>>()
        .map(|v| v.iter().sum());
    let cost = outcomes
        .iter()
        .map(|o| o.cost)
        .collect::<Option<Vec<_>>>()
        .map(|v| v.iter().sum());
    (ok, ms, tokens, cost)
}

fn cell(o: &Outcome) -> String {
    format!(
        "{} {} ms{}{}",
        if o.ok { "✅" } else { "❌" },
        o.ms,
        o.tokens.map(|t| format!(", {t} tok")).unwrap_or_default(),
        o.cost.map(|c| format!(", {c:.4} $")).unwrap_or_default()
    )
}

#[tokio::test]
#[ignore = "réseau : PENELOPE_AB_HERMES_CMD, instance Hermes et daemon Pénélope"]
async fn penelope_does_at_least_as_well_as_hermes_for_no_more_cost() {
    let hermes = live::require_env(&["PENELOPE_AB_HERMES_CMD"]).remove(0);
    let penelope = live::model("PENELOPE_AB_PENELOPE_CMD", "penelope chat {prompt}");
    let timeout = Duration::from_secs(
        std::env::var("PENELOPE_AB_TIMEOUT_S")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(180),
    );
    let tasks: Vec<Task> = serde_json::from_str(include_str!("../ab/tasks.json")).unwrap();
    assert_eq!(tasks.len(), 30);

    let (mut h, mut p) = (Vec::new(), Vec::new());
    for t in &tasks {
        let a = attempt(&hermes, "PENELOPE_AB_HERMES_USAGE_CMD", None, t, timeout).await;
        let b = attempt(
            &penelope,
            "PENELOPE_AB_PENELOPE_USAGE_CMD",
            Some("penelope usage --by day --limit 500 --json"),
            t,
            timeout,
        )
        .await;
        eprintln!("{:<14} Hermes {} · Pénélope {}", t.id, cell(&a), cell(&b));
        h.push(a);
        p.push(b);
    }

    let (h_ok, h_ms, h_tok, h_cost) = total(&h);
    let (p_ok, p_ms, p_tok, p_cost) = total(&p);
    let fmt_tok = |t: Option<u64>| t.map(|v| v.to_string()).unwrap_or_else(|| "n/d".into());
    let fmt_cost = |c: Option<f64>| {
        c.map(|v| format!("{v:.4} $"))
            .unwrap_or_else(|| "n/d".into())
    };
    let mut report = format!(
        "# A/B Hermes · Pénélope\n\n{} tâches, `{hermes}` contre `{penelope}`.\n\n\
         | | Hermes | Pénélope |\n|---|---|---|\n\
         | Réussite | {h_ok}/{n} | {p_ok}/{n} |\n\
         | Latence totale | {} s | {} s |\n\
         | Tokens | {} | {} |\n\
         | Coût | {} | {} |\n\n\
         | Tâche | Hermes | Pénélope | Sortie de Pénélope |\n|---|---|---|---|\n",
        tasks.len(),
        h_ms / 1000,
        p_ms / 1000,
        fmt_tok(h_tok),
        fmt_tok(p_tok),
        fmt_cost(h_cost),
        fmt_cost(p_cost),
        n = tasks.len(),
    );
    for ((t, a), b) in tasks.iter().zip(&h).zip(&p) {
        report.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            t.id,
            cell(a),
            cell(b),
            b.excerpt
        ));
    }
    let cost_ok = match (p_cost, h_cost) {
        (Some(pc), Some(hc)) => Some(pc <= hc),
        _ => None,
    };
    report.push_str(&format!(
        "\nVerdict : réussite {} ; coût {}.\n",
        if p_ok >= h_ok {
            "✅ ≥ Hermes"
        } else {
            "❌ < Hermes"
        },
        match cost_ok {
            Some(true) => "✅ ≤ Hermes",
            Some(false) => "❌ > Hermes",
            None => "non comparable (coût non mesuré d'un côté)",
        }
    ));
    let path = std::env::var("PENELOPE_AB_REPORT").unwrap_or_else(|_| {
        penelope_evals::ca_matrix::repo_root()
            .join("target/ab-hermes.md")
            .to_string_lossy()
            .to_string()
    });
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, &report).expect("rapport");
    eprintln!("rapport : {path}");

    assert!(p_ok >= h_ok, "Pénélope {p_ok}/30 < Hermes {h_ok}/30");
    assert_ne!(cost_ok, Some(false), "Pénélope coûte plus que Hermes");
}

#[test]
fn usage_is_read_from_agent_output() {
    assert_eq!(
        usage_in("Réponse : 42\n1 234 tokens · $0.0031"),
        (Some(1234), Some(0.0031))
    );
    assert_eq!(usage_in("rien"), (None, None));
    let (program, args) = command("hermes chat -q {prompt}", Some("deux mots"));
    assert_eq!(program, "hermes");
    assert_eq!(args, vec!["chat", "-q", "deux mots"]);
    let tasks: Vec<Task> = serde_json::from_str(include_str!("../ab/tasks.json")).unwrap();
    for t in tasks {
        Regex::new(&format!("(?i){}", t.expect)).unwrap_or_else(|e| panic!("{} : {e}", t.id));
    }
}
