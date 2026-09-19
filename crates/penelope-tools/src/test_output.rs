//! Sorties de commandes filtrées pour le modèle (issue #32).
//!
//! ```text
//! cargo test, go test, npm|pnpm|yarn test, jest, vitest, pytest, make test
//!   └─► résumé (compteurs) + sections d'échec (nom, assertion, pile courte)
//! toute autre commande en échec, longue
//!   └─► tête, queue, lignes error|failed|panic|FAIL|Traceback
//! sortie complète ─► artefact, relisible par pages (`artifact_read`)
//! ```
//!
//! Une suite qui échoue sur 3 cas sur 1 200 ne fait plus entrer 1 197 lignes de succès
//! dans le contexte, et la relancer ne rejoue pas ce coût.

use regex::Regex;
use std::sync::OnceLock;

/// Exécuteur de tests reconnu dans une commande.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    Cargo,
    Go,
    Node,
    Pytest,
    Make,
}

impl Runner {
    pub fn as_str(&self) -> &'static str {
        match self {
            Runner::Cargo => "cargo test",
            Runner::Go => "go test",
            Runner::Node => "tests JavaScript",
            Runner::Pytest => "pytest",
            Runner::Make => "make test",
        }
    }
}

/// En dessous, la sortie est rendue telle quelle : un résumé ne ferait rien gagner.
pub const MIN_LINES: usize = 40;
/// Taille maximale du texte filtré (environ 1 500 tokens).
pub const MAX_CHARS: usize = 6_000;
/// Lignes gardées par section d'échec.
const SECTION_LINES: usize = 25;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("regex valide"))
}

/// Exécuteur de tests invoqué par la commande, s'il y en a un.
pub fn runner_of(command: &str) -> Option<Runner> {
    static CARGO: OnceLock<Regex> = OnceLock::new();
    static GO: OnceLock<Regex> = OnceLock::new();
    static NODE: OnceLock<Regex> = OnceLock::new();
    static PYTEST: OnceLock<Regex> = OnceLock::new();
    static MAKE: OnceLock<Regex> = OnceLock::new();
    let c = command.to_lowercase();
    if re(&CARGO, r"\bcargo\s+(\+\S+\s+)?(test|nextest)\b").is_match(&c) {
        Some(Runner::Cargo)
    } else if re(&GO, r"\bgo\s+test\b").is_match(&c) {
        Some(Runner::Go)
    } else if re(
        &NODE,
        r"\b(npm|pnpm|yarn|bun)\s+(run\s+)?test\b|\b(npx\s+)?(jest|vitest)\b",
    )
    .is_match(&c)
    {
        Some(Runner::Node)
    } else if re(&PYTEST, r"\bpytest\b|\bpython\d?(\.\d+)?\s+-m\s+pytest\b").is_match(&c) {
        Some(Runner::Pytest)
    } else if re(&MAKE, r"\bmake\b(\s+-\S+)*(\s+\S+)*\s+test\b").is_match(&c) {
        Some(Runner::Make)
    } else {
        None
    }
}

fn push_section(out: &mut Vec<String>, lines: &[&str]) {
    let mut kept: Vec<String> = lines
        .iter()
        .take(SECTION_LINES)
        .map(|l| l.to_string())
        .collect();
    if lines.len() > SECTION_LINES {
        kept.push(format!(
            "[… {} ligne(s) de plus]",
            lines.len() - SECTION_LINES
        ));
    }
    out.push(kept.join("\n"));
}

/// `cargo test` : lignes `test result:`, blocs `---- nom stdout ----`, erreurs de compilation.
fn cargo(lines: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut summary = Vec::new();
    let mut failures = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        if t.starts_with("test result:") {
            summary.push(t.to_string());
        } else if t.starts_with("---- ") && t.ends_with(" ----") {
            let start = i;
            i += 1;
            while i < lines.len() {
                let n = lines[i].trim_start();
                if (n.starts_with("---- ") && n.ends_with(" ----")) || n == "failures:" {
                    break;
                }
                i += 1;
            }
            let block: Vec<&str> = lines[start..i]
                .iter()
                .copied()
                .filter(|x| !x.trim().is_empty())
                .collect();
            push_section(&mut failures, &block);
            continue;
        } else if t.starts_with("error: test failed") {
            summary.push(t.to_string());
        } else if t.starts_with("error[") || t.starts_with("error:") {
            let end = (i + 6).min(lines.len());
            push_section(&mut failures, &lines[i..end]);
        } else if t.starts_with("test ") && t.ends_with("FAILED") && failures.is_empty() {
            summary.push(t.to_string());
        }
        i += 1;
    }
    (summary, failures)
}

/// `go test` : blocs `--- FAIL:` et leurs lignes indentées, lignes `ok`/`FAIL` de paquet.
fn go(lines: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut summary = Vec::new();
    let mut failures = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with("--- FAIL:") {
            let start = i;
            i += 1;
            while i < lines.len()
                && (lines[i].starts_with(' ') || lines[i].starts_with('\t'))
                && !lines[i].trim_start().starts_with("--- ")
            {
                i += 1;
            }
            push_section(&mut failures, &lines[start..i]);
            continue;
        }
        if t.starts_with("ok ")
            || t.starts_with("ok\t")
            || t.starts_with("FAIL\t")
            || t == "FAIL"
            || t == "PASS"
        {
            summary.push(t.to_string());
        } else if t.starts_with("panic:") {
            let end = (i + 8).min(lines.len());
            push_section(&mut failures, &lines[i..end]);
        }
        i += 1;
    }
    (summary, failures)
}

/// Jest et Vitest : lignes `Tests:`, `Test Suites:`, `Test Files`, blocs `●` ou `FAIL`.
fn node(lines: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut summary = Vec::new();
    let mut failures = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with("Tests:")
            || t.starts_with("Test Suites:")
            || t.starts_with("Test Files")
            || t.starts_with("Tests ")
        {
            summary.push(t.to_string());
        } else if t.starts_with("● ") && !t.starts_with("● Console") {
            let start = i;
            i += 1;
            while i < lines.len() && {
                let n = lines[i].trim_start();
                !n.starts_with("● ") && !n.starts_with("Tests:") && !n.starts_with("Test Suites:")
            } {
                i += 1;
            }
            let block: Vec<&str> = lines[start..i]
                .iter()
                .copied()
                .filter(|x| !x.trim().is_empty())
                .collect();
            push_section(&mut failures, &block);
            continue;
        } else if (t.starts_with("FAIL ") || t.starts_with("× ") || t.starts_with("✗ "))
            && failures.len() < 20
        {
            let end = (i + 8).min(lines.len());
            push_section(&mut failures, &lines[i..end]);
        }
        i += 1;
    }
    (summary, failures)
}

/// pytest : section `FAILURES` découpée par `____ nom ____`, résumé court et ligne finale.
fn pytest(lines: &[&str]) -> (Vec<String>, Vec<String>) {
    static BANNER: OnceLock<Regex> = OnceLock::new();
    let banner = re(&BANNER, r"^=+ (.*?) =+$");
    let mut summary = Vec::new();
    let mut failures = Vec::new();
    let mut section = String::new();
    let mut block: Vec<&str> = Vec::new();
    for l in lines {
        let t = l.trim();
        if let Some(c) = banner.captures(t) {
            if !block.is_empty() {
                push_section(&mut failures, &block);
                block.clear();
            }
            section = c[1].to_lowercase();
            if section.contains("passed")
                || section.contains("failed")
                || section.contains("error") && section.contains(" in ")
            {
                summary.push(t.to_string());
            }
            continue;
        }
        if section == "failures" || section == "errors" {
            if t.starts_with('_') && t.ends_with('_') && t.len() > 4 && !block.is_empty() {
                push_section(&mut failures, &block);
                block.clear();
            }
            if !t.is_empty() {
                block.push(l);
            }
        } else if section.starts_with("short test summary") && !t.is_empty() {
            summary.push(t.to_string());
        }
    }
    if !block.is_empty() {
        push_section(&mut failures, &block);
    }
    (summary, failures)
}

fn error_lines(lines: &[&str]) -> Vec<String> {
    static ERR: OnceLock<Regex> = OnceLock::new();
    let err = re(
        &ERR,
        r"(?i)\b(error|errors|failed|failure|panic|panicked|traceback|exception)\b|\bFAIL\b",
    );
    let mut seen = std::collections::BTreeSet::new();
    lines
        .iter()
        .filter(|l| err.is_match(l))
        .filter(|l| seen.insert(l.trim().to_string()))
        .take(60)
        .map(|l| l.to_string())
        .collect()
}

fn generic(lines: &[&str]) -> String {
    // Tête et queue de 30 lignes : sous 60 lignes, elles se recouvrent et il n'y a pas de
    // milieu. `lines[30..len - 30]` paniquait de 31 à 59 lignes (« slice index starts at
    // 30 but ends at 11 » pour 41 lignes, issue #130) : la sortie part entière.
    if lines.len() <= 60 {
        return format!("--- sortie ---\n{}", lines.join("\n"));
    }
    let head: Vec<&str> = lines.iter().take(30).copied().collect();
    let tail: Vec<&str> = lines
        .iter()
        .skip(lines.len().saturating_sub(30))
        .copied()
        .collect();
    let errors = error_lines(&lines[30..lines.len() - 30]);
    let mut t = format!("--- début ---\n{}\n", head.join("\n"));
    if !errors.is_empty() {
        t.push_str(&format!(
            "--- lignes d'erreur du milieu ({}) ---\n{}\n",
            errors.len(),
            errors.join("\n")
        ));
    }
    t.push_str(&format!("--- fin ---\n{}", tail.join("\n")));
    t
}

fn cap(text: String) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text;
    }
    let kept: String = text.chars().take(MAX_CHARS).collect();
    format!("{kept}\n[… résumé coupé : la sortie complète est dans l'artefact]")
}

/// Texte filtré d'une commande pour le modèle, ou `None` s'il faut rendre la sortie brute :
/// sortie courte, ou commande réussie qui n'est pas une suite de tests.
pub fn digest(command: &str, exit_code: i32, stdout: &str, stderr: &str) -> Option<String> {
    let combined = if stderr.trim().is_empty() {
        stdout.to_string()
    } else if stdout.trim().is_empty() {
        stderr.to_string()
    } else {
        format!("{stdout}\n{stderr}")
    };
    let lines: Vec<&str> = combined.lines().collect();
    if lines.len() < MIN_LINES {
        return None;
    }
    let runner = runner_of(command);
    let head = format!(
        "Code de sortie {exit_code}{} · {} ligne(s) de sortie, filtrées.",
        runner
            .map(|r| format!(" · {}", r.as_str()))
            .unwrap_or_default(),
        lines.len()
    );
    let Some(runner) = runner else {
        return (exit_code != 0).then(|| cap(format!("{head}\n{}", generic(&lines))));
    };
    let (summary, failures) = match runner {
        Runner::Cargo => cargo(&lines),
        Runner::Go => go(&lines),
        Runner::Node => node(&lines),
        Runner::Pytest => pytest(&lines),
        Runner::Make => {
            // `make test` enveloppe souvent un autre exécuteur : on essaie chacun.
            let mut best = (Vec::new(), Vec::new());
            for parse in [cargo, go, node, pytest] {
                let got = parse(&lines);
                if got.0.len() + got.1.len() > best.0.len() + best.1.len() {
                    best = got;
                }
            }
            best
        }
    };
    if summary.is_empty() && failures.is_empty() {
        return Some(cap(format!("{head}\n{}", generic(&lines))));
    }
    let mut t = head;
    if !summary.is_empty() {
        t.push_str(&format!("\n\nRésumé :\n{}", summary.join("\n")));
    }
    if failures.is_empty() {
        if exit_code != 0 {
            let errors = error_lines(&lines);
            if !errors.is_empty() {
                t.push_str(&format!("\n\nLignes d'erreur :\n{}", errors.join("\n")));
            }
        }
    } else {
        t.push_str(&format!(
            "\n\nÉchecs ({}) :\n{}",
            failures.len(),
            failures.join("\n\n")
        ));
    }
    Some(cap(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #130 : une commande en échec de 40 à 60 lignes, hors suite de tests, ne fait plus
    /// paniquer le résumé ; au-delà, tête, lignes d'erreur du milieu et queue.
    #[test]
    fn a_failing_output_of_any_length_is_digested_without_panic() {
        for n in [40, 41, 59, 60, 61, 100] {
            let out: String = (1..=n)
                .map(|i| {
                    if i == n / 2 + 1 {
                        format!("Traceback ligne {i}")
                    } else {
                        format!("ligne {i}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let d = digest("python3 - <<'PYEOF'\nprint(1)\nPYEOF", 1, &out, "").expect("résumé");
            assert!(d.contains(&format!("ligne {n}")), "{n} : fin présente");
            if n <= 60 {
                assert!(
                    d.contains(&format!("Traceback ligne {}", n / 2 + 1)),
                    "{n} : tout y est"
                );
            } else {
                assert!(d.contains("lignes d'erreur du milieu"), "{n} : {d}");
            }
        }
    }

    #[test]
    fn test_runners_are_recognised() {
        assert_eq!(runner_of("cargo test --workspace"), Some(Runner::Cargo));
        assert_eq!(
            runner_of("cd api && cargo +nightly nextest run"),
            Some(Runner::Cargo)
        );
        assert_eq!(runner_of("go test ./..."), Some(Runner::Go));
        assert_eq!(runner_of("pnpm run test -- --ci"), Some(Runner::Node));
        assert_eq!(runner_of("npx vitest run"), Some(Runner::Node));
        assert_eq!(runner_of("python3 -m pytest -x"), Some(Runner::Pytest));
        assert_eq!(runner_of("make -j4 test"), Some(Runner::Make));
        assert_eq!(runner_of("cargo build"), None);
        assert_eq!(runner_of("ls -la"), None);
    }

    /// `cargo test` avec 2 échecs sur 500 : les 2 échecs et le résumé, moins de 2 000
    /// tokens.
    #[test]
    fn a_cargo_run_keeps_only_the_failures_and_the_summary() {
        let mut out = String::from("running 500 tests\n");
        for i in 0..498 {
            out.push_str(&format!("test module::tests::case_{i} ... ok\n"));
        }
        out.push_str("test module::tests::parses_dates ... FAILED\n");
        out.push_str("test module::tests::rounds_totals ... FAILED\n\nfailures:\n\n");
        out.push_str(
            "---- module::tests::parses_dates stdout ----\n\
             thread 'module::tests::parses_dates' panicked at src/dates.rs:42:9:\n\
             assertion `left == right` failed\n  left: \"2026-09-17\"\n right: \"17/09/2026\"\n\n\
             ---- module::tests::rounds_totals stdout ----\n\
             thread 'module::tests::rounds_totals' panicked at src/totals.rs:7:5:\n\
             attendu 12.50, obtenu 12.49\n\nfailures:\n    module::tests::parses_dates\n    \
             module::tests::rounds_totals\n\n\
             test result: FAILED. 498 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; \
             finished in 1.20s\n",
        );
        let d = digest(
            "cargo test",
            101,
            &out,
            "error: test failed, to rerun pass `--lib`",
        )
        .unwrap();
        assert!(
            d.contains("test result: FAILED. 498 passed; 2 failed"),
            "{d}"
        );
        assert!(d.contains("Échecs (2)"), "{d}");
        assert!(
            d.contains("src/dates.rs:42:9") && d.contains("12.49"),
            "{d}"
        );
        assert!(!d.contains("case_17 ... ok"), "pas de lignes de succès");
        assert!(
            d.chars().count() / 4 < 2_000,
            "{} caractères",
            d.chars().count()
        );
    }

    #[test]
    fn go_jest_and_pytest_failures_are_extracted() {
        let pad = |s: &str| {
            format!(
                "{}{s}",
                "=== RUN   TestOk\n--- PASS: TestOk (0.00s)\n".repeat(30)
            )
        };
        let go_out = pad(
            "--- FAIL: TestTotal (0.01s)\n    total_test.go:12: attendu 3, obtenu 4\nFAIL\nFAIL\texample.com/facture\t0.02s\n",
        );
        let d = digest("go test ./...", 1, &go_out, "").unwrap();
        assert!(
            d.contains("--- FAIL: TestTotal") && d.contains("attendu 3, obtenu 4"),
            "{d}"
        );
        assert!(d.contains("FAIL\texample.com/facture"), "{d}");

        let jest = format!(
            "{}  ● Facture › arrondit le total\n\n    expect(received).toBe(expected)\n    Expected: 12.5\n    Received: 12.49\n\nTests:       1 failed, 120 passed, 121 total\n",
            "  ✓ passe (3 ms)\n".repeat(50)
        );
        let d = digest("npm test", 1, &jest, "").unwrap();
        assert!(
            d.contains("● Facture › arrondit le total") && d.contains("Received: 12.49"),
            "{d}"
        );
        assert!(d.contains("Tests:       1 failed, 120 passed"), "{d}");

        let py = format!(
            "{}=================================== FAILURES ===================================\n\
             ________________________________ test_total ________________________________\n\
             \n    def test_total():\n>       assert total() == 3\nE       assert 4 == 3\n\ntests/test_total.py:5: AssertionError\n\
             =========================== short test summary info ============================\n\
             FAILED tests/test_total.py::test_total - assert 4 == 3\n\
             ========================= 1 failed, 99 passed in 0.42s =========================\n",
            "tests/test_a.py .....\n".repeat(45)
        );
        let d = digest("pytest", 1, &py, "").unwrap();
        assert!(d.contains("E       assert 4 == 3"), "{d}");
        assert!(d.contains("FAILED tests/test_total.py::test_total"), "{d}");
        assert!(d.contains("1 failed, 99 passed in 0.42s"), "{d}");
    }

    #[test]
    fn other_long_failures_keep_head_tail_and_error_lines() {
        let mut out = String::new();
        for i in 0..200 {
            out.push_str(&format!("étape {i} en cours\n"));
            if i == 100 {
                out.push_str("Error: connexion refusée à la base\n");
            }
        }
        let d = digest("./deploy.sh", 2, &out, "").unwrap();
        assert!(
            d.contains("étape 0 en cours") && d.contains("étape 199 en cours"),
            "{d}"
        );
        assert!(d.contains("Error: connexion refusée à la base"), "{d}");
        assert!(!d.contains("étape 60 en cours"));
        assert!(
            digest("./deploy.sh", 0, &out, "").is_none(),
            "succès hors tests : brut"
        );
        assert!(
            digest("cargo test", 101, "court\n", "").is_none(),
            "sortie courte : brute"
        );
    }
}
