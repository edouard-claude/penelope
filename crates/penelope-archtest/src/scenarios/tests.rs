//! Tests de R10 : la règle tenue sur le workspace réel, et chaque détecteur éprouvé sur
//! des fichiers fictifs.

use super::*;
use crate::snapshot::{SourceFile, workspace_snapshot};

const COMMANDS: &str = r#"
use penelope_kernel::api::method as m;

pub fn all() -> Vec<Command> {
    vec![
        c(
            "new",
            "Session",
            "Nouvelle session",
            "/new",
            m::SESSION_NEW,
        ),
        c("compact", "Contexte", "Compacte", "/compact", m::SESSION_COMPACT),
    ]
}

#[cfg(test)]
mod tests {
    fn autre() { c("pas_une_commande", "", "", "", ""); }
}
"#;

const TOOLS: &str = r#"
pub(super) fn files() -> Vec<ToolSpec> {
    vec![
        spec(
            "fs_read",
            RiskClass::Read,
            "Lit un fichier.",
            obj(json!({}), &[]),
            true,
            false,
            false,
        ),
        spec("fs_write", RiskClass::Write, "Écrit.", obj(json!({}), &[]), false, false, false),
    ]
}
"#;

const API: &str = r#"
pub mod method {
    pub const STATUS: &str = "status";
    pub const SESSION_NEW: &str = "session.new";
    pub const ALL: &[&str] = &[STATUS, SESSION_NEW];
}

pub const NOT_A_METHOD: &str = "ailleurs";
"#;

fn snapshot() -> Snapshot {
    Snapshot::of(vec![
        SourceFile::new("penelope-telegram", COMMANDS_FILE, COMMANDS),
        SourceFile::new(
            "penelope-tools",
            "crates/penelope-tools/src/spec/system.rs",
            TOOLS,
        ),
        SourceFile::new(
            "penelope-tools",
            "crates/penelope-tools/src/spec/tests.rs",
            "spec(\"outil_de_test\", …)",
        ),
        SourceFile::new("penelope-kernel", API_FILE, API),
    ])
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn scenario(spec: &str, model: &str) -> ScenarioFiles {
    ScenarioFiles {
        name: "essai".into(),
        spec: spec.into(),
        model: model.into(),
    }
}

#[test]
fn the_catalog_reads_commands_tools_and_methods() {
    assert_eq!(
        catalog(&snapshot()),
        set(&[
            "/compact",
            "/new",
            "outil:fs_read",
            "outil:fs_write",
            "rpc:session.new",
            "rpc:status",
        ])
    );
}

#[test]
fn a_scenario_exercises_its_commands_and_the_tools_the_model_calls() {
    let s = scenario(
        r#"name = "essai"
[[steps]]
kind = "message"
text = "/new ne compte pas : c'est un message"
[[steps]]
kind = "command"
command = "/compact"
[[steps]]
kind = "rpc"
method = "schedule.add"
params = { kind = "cron", note = "method = \"session.new\" ne compte pas" }
"#,
        r#"{"tool_calls": {"text": "", "calls": [{"id": "c1", "name": "fs_read", "arguments": {"path": "a"}}]}}
{"text": "le mot \"name\": \"fs_write\" dans une réponse ne compte pas"}
{"tool_calls": {"text": "", "calls": [{"id": "c2", "name": "tool_call", "arguments": {"name": "config_set"}}]}}
"#,
    );
    assert_eq!(
        exercised(&s).unwrap(),
        set(&[
            "/compact",
            "outil:config_set",
            "outil:fs_read",
            "outil:tool_call",
            "rpc:schedule.add"
        ])
    );
}

/// L'étape `telegram` exerce la commande de son `text` ; un message ou un clic, rien.
#[test]
fn a_telegram_step_exercises_its_command() {
    let s = scenario(
        r#"name = "essai"
[[steps]]
kind = "telegram"
text = "/status maintenant"
[[steps]]
kind = "telegram"
text = "bonjour, pas une commande"
[[steps]]
kind = "telegram"
click = "Approuver"
"#,
        "",
    );
    assert_eq!(exercised(&s).unwrap(), set(&["/status"]));
}

#[test]
fn an_unreadable_scenario_is_an_error_not_a_pass() {
    let err = exercised(&scenario("steps = [", "")).unwrap_err();
    assert!(err.contains("scénario essai"), "{err}");
}

#[test]
fn uncovered_surfaces_must_be_listed_and_listed_ones_must_be_uncovered() {
    let cov = coverage(
        &snapshot(),
        &[scenario(
            "[[steps]]\nkind = \"command\"\ncommand = \"/compact\"\n",
            "",
        )],
    )
    .unwrap();
    assert!(!cov.uncovered.contains("/compact"));

    // Liste exacte : rien à dire.
    assert!(violations(&cov, &cov.uncovered).is_empty());

    // Une surface nouvelle sans scénario, absente de la liste.
    let mut missing = cov.uncovered.clone();
    missing.remove("/new");
    let v = violations(&cov, &missing);
    assert_eq!(
        v,
        ["commande Telegram /new sans scénario de composition \
          (crates/penelope-evals/scenarios/) et absente de budget.toml [scenarios].missing"]
    );

    // Une entrée dormante : couverte, ou disparue du code.
    let mut missing = cov.uncovered.clone();
    missing.insert("/compact".into());
    missing.insert("rpc:disparue".into());
    let v = violations(&cov, &missing);
    assert_eq!(v.len(), 2, "{v:?}");
    assert!(
        v[0].contains("« /compact » a maintenant un scénario"),
        "{}",
        v[0]
    );
    assert!(v[1].contains("« rpc:disparue » n'existe plus"), "{}", v[1]);
}

#[test]
fn tightening_only_removes_entries() {
    let raw = "# en-tête\n[scenarios]   # R10\nmissing = [\n    \"/compact\",\n    \"/new\",\n    \"rpc:status\",\n]\n";
    let out = tighten(raw, &set(&["/new", "rpc:status", "outil:fs_read"])).unwrap();
    assert_eq!(missing_in(&out).unwrap(), set(&["/new", "rpc:status"]));
    assert!(out.starts_with("# en-tête\n[scenarios]   # R10\n"), "{out}");
    // Sans table, rien n'est créé.
    assert_eq!(tighten("[files]\n", &set(&["/new"])).unwrap(), "[files]\n");
    assert!(missing_in("[files]\n").unwrap().is_empty());
}

/// Le détecteur lit bien les trois catalogues réels : un motif qui ne trouverait plus
/// rien rendrait R10 vide, donc toujours verte.
#[test]
fn the_real_catalogs_are_read() {
    let cat = catalog(workspace_snapshot());
    let count = |p: &str| cat.iter().filter(|s| s.starts_with(p)).count();
    assert!(count("/") >= 40, "commandes : {}", count("/"));
    assert!(count("outil:") >= 50, "outils : {}", count("outil:"));
    assert!(count("rpc:") >= 100, "méthodes : {}", count("rpc:"));
    for known in ["/new", "/compact", "outil:fs_read", "rpc:session.new"] {
        assert!(cat.contains(known), "{known} absent du catalogue lu");
    }
}

/// R10.
#[test]
fn every_visible_surface_has_a_scenario() {
    let cov = workspace_coverage(workspace_snapshot()).unwrap_or_else(|e| panic!("{e}"));
    let missing = workspace_missing().unwrap_or_else(|e| panic!("{e}"));
    let v = violations(&cov, &missing);
    assert!(v.is_empty(), "R10 :\n{}", v.join("\n"));
}
