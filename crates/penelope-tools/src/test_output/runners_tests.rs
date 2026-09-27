//! Résumés par exécuteur de tests (issue #32) : ce que chaque sortie garde.

use super::*;

/// `n` lignes de remplissage, pour passer le seuil du résumé.
fn filler(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("   Compiling crate-{i} v0.1.0"))
        .collect()
}

fn run(command: &str, code: i32, lines: &[String]) -> String {
    digest(command, code, &lines.join("\n"), "").expect("sortie assez longue pour un résumé")
}

/// Une erreur de compilation n'est pas un test : ses lignes de contexte sont gardées, et
/// la ligne `error: test failed` rejoint le résumé.
#[test]
fn a_cargo_build_error_is_kept_with_its_context() {
    let mut lines = filler(45);
    lines.extend(
        [
            "error[E0425]: cannot find value `x` in this scope",
            " --> src/lib.rs:3:5",
            "  |",
            "3 |     x",
            "  |     ^ not found in this scope",
            "",
            "error: test failed, to rerun pass `--lib`",
        ]
        .map(String::from),
    );
    let t = run("cargo test", 101, &lines);
    assert!(
        t.starts_with("Code de sortie 101 · cargo test · 52 ligne(s)"),
        "{t}"
    );
    assert!(t.contains("error[E0425]"), "{t}");
    assert!(t.contains("not found in this scope"), "{t}");
    assert!(t.contains("Résumé :\nerror: test failed"), "{t}");
    assert!(
        !t.contains("Compiling crate-3 "),
        "le remplissage est filtré : {t}"
    );
}

/// Go : une panique garde sa pile courte, `FAIL` et `ok` font le résumé.
#[test]
fn a_go_panic_keeps_its_short_stack() {
    let mut lines = filler(45);
    lines.extend(
        [
            "panic: runtime error: index out of range [3] with length 3",
            "goroutine 7 [running]:",
            "example.com/x.Parse(...)",
            "FAIL",
            "FAIL\texample.com/x\t0.012s",
        ]
        .map(String::from),
    );
    let t = run("go test ./...", 1, &lines);
    assert!(t.contains("· go test ·"), "{t}");
    assert!(t.contains("panic: runtime error"), "{t}");
    assert!(t.contains("goroutine 7"), "{t}");
    assert!(t.contains("Résumé :"), "{t}");
}

/// Vitest et consorts : les lignes `FAIL` et `×` sont des échecs, les compteurs le
/// résumé.
#[test]
fn javascript_failures_are_kept() {
    let mut lines = filler(45);
    lines.extend(
        [
            " FAIL  src/sum.test.ts > additionne",
            "AssertionError: expected 3 to be 4",
            " × src/mul.test.ts > multiplie",
            " Test Files  2 failed (2)",
            "      Tests  2 failed | 10 passed (12)",
        ]
        .map(String::from),
    );
    let t = run("npx vitest run", 1, &lines);
    assert!(t.contains("· tests JavaScript ·"), "{t}");
    assert!(t.contains("expected 3 to be 4"), "{t}");
    assert!(t.contains("multiplie"), "{t}");
    assert!(t.contains("Test Files  2 failed (2)"), "{t}");
}

/// pytest : chaque cas en échec est une section, séparée par sa bannière `___ nom ___`,
/// et le résumé court est gardé.
#[test]
fn pytest_failures_are_split_by_test() {
    let mut lines = filler(45);
    lines.extend(
        [
            "=================================== FAILURES ===================================",
            "________________________________ test_un ________________________________",
            "    assert 1 == 2",
            "E   assert 1 == 2",
            "________________________________ test_deux ________________________________",
            "E   KeyError: 'x'",
            "=========================== short test summary info ============================",
            "FAILED tests/test_a.py::test_un - assert 1 == 2",
            "========================= 2 failed, 8 passed in 0.12s ==========================",
        ]
        .map(String::from),
    );
    let t = run("python3 -m pytest -q", 1, &lines);
    assert!(t.contains("· pytest ·"), "{t}");
    assert!(t.contains("Échecs (2)"), "{t}");
    assert!(t.contains("KeyError"), "{t}");
    assert!(t.contains("FAILED tests/test_a.py::test_un"), "{t}");
    assert!(t.contains("2 failed, 8 passed in 0.12s"), "{t}");
}

/// `make test` enveloppe un autre exécuteur : le résumé le plus riche gagne.
#[test]
fn make_test_uses_the_wrapped_runner() {
    let mut lines = filler(45);
    lines.extend(
        [
            "---- tests::casse stdout ----",
            "thread 'tests::casse' panicked at src/lib.rs:9:5:",
            "failures:",
            "test result: FAILED. 1 passed; 1 failed",
        ]
        .map(String::from),
    );
    let t = run("make -j4 test", 2, &lines);
    assert!(t.contains("· make test ·"), "{t}");
    assert!(t.contains("panicked at src/lib.rs:9:5"), "{t}");
    assert!(t.contains("test result: FAILED"), "{t}");
}

/// Une section trop longue est coupée et le dit ; un résumé trop long aussi.
#[test]
fn long_sections_and_long_digests_are_cut() {
    let mut lines = filler(45);
    lines.push("---- tests::bavard stdout ----".into());
    lines.extend((0..40).map(|i| format!("détail {i}")));
    let t = run("cargo test", 101, &lines);
    assert!(t.contains("[… 16 ligne(s) de plus]"), "{t}");

    let wide: Vec<String> = (0..300)
        .map(|i| format!("error: échec {i} {}", "x".repeat(60)))
        .collect();
    let t = run("./build.sh", 1, &wide);
    assert!(t.chars().count() < MAX_CHARS + 100, "{}", t.chars().count());
    assert!(t.ends_with("[… résumé coupé : la sortie complète est dans l'artefact]"));
}

/// Une commande réussie qui n'est pas une suite de tests garde sa sortie brute.
#[test]
fn a_successful_plain_command_is_not_digested() {
    let lines = filler(60);
    assert_eq!(digest("ls -la", 0, &lines.join("\n"), ""), None);
    // stdout vide : stderr seul est lu.
    assert!(digest("./build.sh", 1, "", &lines.join("\n")).is_some());
}
