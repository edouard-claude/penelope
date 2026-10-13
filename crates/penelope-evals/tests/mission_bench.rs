//! Suite `mission-bench` (#369, réseau) : trois stories réelles rejouées sur le dépôt
//! témoin `fixtures/mission/atelier`, avec le vrai modèle, pour mesurer ce que vaut une
//! session de code longue avant et après chaque chantier de l'épopée #370.
//!
//! ```bash
//! OPENROUTER_API_KEY=… penelope eval mission-bench
//! ```
//!
//! Chaque story part d'une copie fraîche du témoin (dépôt git initialisé), reçoit sa
//! consigne dans une session CLI, et tourne jusqu'à ce que la réponse dise `TERMINÉ`. Un
//! tour arrêté par son plafond d'appels (`CALLS_EXHAUSTED`) est relancé par « Continue » :
//! chaque relance compte comme une **intervention**, ce que le mode mission (#364) doit
//! ramener à zéro. La recette est un fichier de tests caché (`fixtures/mission/acceptance`)
//! copié dans le témoin après coup : la story réussit si `cargo test` le passe.
//!
//! Mesures par story : réussite, tours, itérations, appels d'outils, interventions,
//! compactions, tokens (dont lus en cache), coût, durée. Rapport Markdown dans
//! `PENELOPE_MISSION_REPORT` (défaut `target/mission-bench.md`) ; `UPDATE_DOCS=1` l'écrit
//! aussi dans `docs/evals/mission-<version>.md`. `PENELOPE_MISSION_STORIES=rapport,prix`
//! restreint la passe ; `PENELOPE_MISSION_TIMEOUT_S` (défaut 1 800) borne chaque story.
//! Trois passes conseillées, médiane retenue : le modèle varie d'une passe à l'autre.
//!
//! La suite ne juge pas : la référence 1.0.59 doit produire un rapport, même mauvais.
//! `PENELOPE_MISSION_REQUIRE=1` exige trois réussites sans intervention (définition de
//! fini de l'épopée).

use penelope_agent::{CALLS_EXHAUSTED, TurnOutcome};
use penelope_evals::live;
use penelope_kernel::clock::{SharedClock, SystemClock};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct Story {
    id: String,
    title: String,
    acceptance: String,
    prompt: String,
}

#[derive(Debug, Default)]
struct Measure {
    id: String,
    title: String,
    ok: bool,
    /// Pourquoi la story s'est arrêtée (`terminé`, `relances épuisées`, `boucle`…).
    end: String,
    turns: u32,
    iterations: u32,
    tool_calls: usize,
    interventions: u32,
    compactions: usize,
    tokens: u64,
    cached: u64,
    cost_usd: f64,
    seconds: u64,
    evidence: Option<PathBuf>,
}

/// Relances au plus par story : au-delà, la story est comptée perdue.
const MAX_CONTINUES: u32 = 40;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mission")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        if name == "target" {
            continue;
        }
        let dest = to.join(&name);
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Témoin",
            "-c",
            "user.email=temoin@example.invalid",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Un témoin frais : copie du dépôt, `git init`, premier commit.
fn fresh_workspace(root: &Path) -> PathBuf {
    let ws = root.join("atelier");
    copy_dir(&fixtures().join("atelier"), &ws);
    assert!(git(&ws, &["init", "-q"]), "git init");
    assert!(git(&ws, &["add", "-A"]), "git add");
    assert!(git(&ws, &["commit", "-q", "-m", "Témoin"]), "git commit");
    ws
}

/// La recette, copiée dans le témoin après la mission, puis `cargo test` dessus.
async fn acceptance(ws: &Path, file: &str) -> (bool, String) {
    let name = file.trim_end_matches(".rs");
    std::fs::copy(
        fixtures().join("acceptance").join(file),
        ws.join("tests").join(file),
    )
    .unwrap();
    let out = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new("cargo")
            .args(["test", "-q", "--test", name])
            .current_dir(ws)
            .output(),
    )
    .await;
    match out {
        Ok(Ok(o)) => {
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text)
        }
        Ok(Err(e)) => (false, format!("cargo : {e}")),
        Err(_) => (false, "cargo test : délai dépassé".into()),
    }
}

fn consigne(ws: &Path, story: &Story) -> String {
    format!(
        "Tu travailles dans le dépôt `{}` (Rust, voir son CLAUDE.md). Fais la story \
         ci-dessous de bout en bout, sans me poser de question : lis le code, modifie-le, \
         ajoute les tests, lance `cargo test` et `cargo fmt` dans ce dépôt (donne toujours \
         `cwd`), corrige jusqu'au vert, puis commite. Quand tout est fait et vérifié, réponds \
         par un court compte rendu qui se termine par le mot TERMINÉ. Si tu es bloqué pour de \
         bon, dis-le et termine par BLOQUÉ.\n\n{}",
        ws.display(),
        story.prompt
    )
}

async fn tokens_of(s: &penelope_app::services::Services, session: &str) -> (u64, u64) {
    let sid = session.to_string();
    s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(SUM(prompt + completion + reasoning), 0),
                        COALESCE(SUM(cached), 0)
                 FROM usage WHERE session_id = ?1",
                [sid],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )?)
        })
        .await
        .unwrap_or((0, 0))
}

/// Ce qu'une story laisse pour être relue : réponses, appels d'outils, recette.
async fn keep_evidence(
    root: &Path,
    s: &penelope_app::services::Services,
    session: &str,
    answers: &[String],
    recette: &str,
) -> PathBuf {
    let out = root.join("preuves");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("reponses.txt"), answers.join("\n\n---\n\n")).unwrap();
    std::fs::write(out.join("recette.txt"), recette).unwrap();
    let mut calls = String::new();
    for e in s
        .events
        .session_events(session, 0)
        .await
        .unwrap_or_default()
    {
        if e.kind == "runtime.tool" {
            let p = &e.payload;
            let line = serde_json::json!({
                "outil": p["tool"], "ok": p["ok"], "ms": p["duration_ms"],
                "arguments": p["args"], "resultat": p["result"],
            });
            calls.push_str(&format!("{line}\n"));
        }
    }
    std::fs::write(out.join("appels.jsonl"), calls).unwrap();
    out
}

async fn run_story(story: &Story, timeout: Duration) -> Measure {
    let dir = tempfile::tempdir().unwrap();
    let ws = fresh_workspace(dir.path());
    let clock: SharedClock = Arc::new(SystemClock);
    let d = live::daemon(&dir.path().join("penelope"), clock).await;
    let ws_cfg = ws.to_string_lossy().into_owned();
    d.publish_config("mission-bench", move |c| {
        c.sandbox.workspaces = vec![ws_cfg];
        // Le témoin compile avec `cargo` : pas de bac à sable, comme sur la machine du
        // propriétaire.
        c.sandbox.default_profile = "full".into();
        Ok(vec![
            "sandbox.workspaces".into(),
            "sandbox.default_profile".into(),
        ])
    })
    .expect("configuration");
    let s = d.services.clone();
    let session = format!("mission-{}", story.id);
    let mut m = Measure {
        id: story.id.clone(),
        title: story.title.clone(),
        ..Default::default()
    };
    let started = Instant::now();
    let mut answers = Vec::new();
    let mut text = consigne(&ws, story);
    loop {
        if started.elapsed() > timeout {
            m.end = "délai dépassé".into();
            break;
        }
        let outcome = live::turn_outcome(&d, &session, &text).await;
        m.turns += 1;
        match outcome {
            TurnOutcome::Answered {
                text: answer,
                iterations,
                ..
            } => {
                m.iterations += iterations;
                let exhausted = answer.starts_with(CALLS_EXHAUSTED);
                answers.push(answer.clone());
                if answer.contains("TERMINÉ") {
                    m.end = "terminé".into();
                    break;
                }
                if answer.contains("BLOQUÉ") {
                    m.end = "bloqué".into();
                    break;
                }
                if m.interventions >= MAX_CONTINUES {
                    m.end = "relances épuisées".into();
                    break;
                }
                m.interventions += 1;
                text = if exhausted {
                    "Continue.".into()
                } else {
                    "Continue jusqu'au bout de la story ; termine par TERMINÉ quand tout est \
                     vérifié et commité."
                        .into()
                };
            }
            TurnOutcome::AwaitingApproval { approval_id } => {
                m.end = format!("approbation demandée ({approval_id})");
                break;
            }
            TurnOutcome::LoopAborted { report, .. } => {
                answers.push(report);
                m.end = "boucle détectée".into();
                break;
            }
            TurnOutcome::Cancelled => {
                m.end = "annulé".into();
                break;
            }
            TurnOutcome::BudgetExceeded { scope, .. } => {
                m.end = format!("budget {scope} épuisé");
                break;
            }
            TurnOutcome::Failed { error } => {
                m.end = format!("échec : {error}");
                break;
            }
        }
    }
    m.seconds = started.elapsed().as_secs();
    let (ok, recette) = acceptance(&ws, &story.acceptance).await;
    m.ok = ok;
    m.tool_calls = s
        .events
        .session_events_of_kind(&session, "runtime.tool")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    m.compactions = s
        .events
        .session_events_of_kind(&session, "context.compacted")
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    (m.tokens, m.cached) = tokens_of(&s, &session).await;
    m.cost_usd = s.budget.spent_session(&session).await.unwrap_or(0.0);
    let root = dir.keep();
    m.evidence = Some(keep_evidence(&root, &s, &session, &answers, &recette).await);
    m
}

fn report(model: &str, measures: &[Measure]) -> String {
    let mut out = format!(
        "# Banc d'essai mission, Pénélope {}\n\nModèle : `{model}`. Une passe ; trois \
         conseillées, médiane retenue.\n\n| Story | Réussite | Fin | Tours | Itérations | \
         Appels d'outils | Interventions | Compactions | Tokens | dont cache | Coût | Durée |\n\
         |---|---|---|---|---|---|---|---|---|---|---|---|\n",
        env!("CARGO_PKG_VERSION")
    );
    for m in measures {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3} $ | {} min {:02} s |\n",
            m.title,
            if m.ok { "✅" } else { "❌" },
            m.end,
            m.turns,
            m.iterations,
            m.tool_calls,
            m.interventions,
            m.compactions,
            m.tokens,
            m.cached,
            m.cost_usd,
            m.seconds / 60,
            m.seconds % 60
        ));
    }
    let passed = measures.iter().filter(|m| m.ok).count();
    let clean = measures
        .iter()
        .filter(|m| m.ok && m.interventions == 0)
        .count();
    out.push_str(&format!(
        "\n{passed}/{} réussies, {clean} sans intervention. Preuves :\n",
        measures.len()
    ));
    for m in measures {
        if let Some(p) = &m.evidence {
            out.push_str(&format!("- {} : `{}`\n", m.id, p.display()));
        }
    }
    out
}

fn stories() -> Vec<Story> {
    let all: Vec<Story> =
        serde_json::from_str(include_str!("../fixtures/mission/stories.json")).unwrap();
    let Ok(only) = std::env::var("PENELOPE_MISSION_STORIES") else {
        return all;
    };
    let wanted: Vec<&str> = only.split(',').map(str::trim).collect();
    all.into_iter()
        .filter(|s| wanted.contains(&s.id.as_str()))
        .collect()
}

#[tokio::test]
#[ignore = "réseau : OPENROUTER_API_KEY"]
async fn three_stories_on_the_witness_repository() {
    let timeout = std::env::var("PENELOPE_MISSION_TIMEOUT_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1_800));
    let model = live::model(
        "PENELOPE_LIVE_MODEL",
        "openrouter:deepseek/deepseek-v4-flash",
    );
    let mut measures = Vec::new();
    for story in stories() {
        eprintln!("▶ {} ({})", story.title, story.id);
        let m = run_story(&story, timeout).await;
        eprintln!(
            "  {} {} · {} tours, {} itérations, {} appels, {} interventions, {} tokens, \
             {:.3} $, {} s",
            if m.ok { "✅" } else { "❌" },
            m.end,
            m.turns,
            m.iterations,
            m.tool_calls,
            m.interventions,
            m.tokens,
            m.cost_usd,
            m.seconds
        );
        measures.push(m);
    }
    let text = report(&model, &measures);
    eprintln!("\n{text}");
    let path = std::env::var("PENELOPE_MISSION_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/mission-bench.md")
        });
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, &text).unwrap();
    eprintln!("rapport : {}", path.display());
    if std::env::var("UPDATE_DOCS").is_ok() {
        let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../docs/evals/mission-{}.md",
            env!("CARGO_PKG_VERSION")
        ));
        let _ = std::fs::create_dir_all(docs.parent().unwrap());
        std::fs::write(&docs, &text).unwrap();
        eprintln!("rapport : {}", docs.display());
    }
    if std::env::var("PENELOPE_MISSION_REQUIRE").is_ok() {
        assert!(
            measures.iter().all(|m| m.ok && m.interventions == 0),
            "toutes les stories doivent réussir sans intervention"
        );
    }
}
