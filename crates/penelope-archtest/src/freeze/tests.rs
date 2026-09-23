//! Tests des règles de gel : chaque règle est tenue sur le workspace réel, et son
//! détecteur est éprouvé sur des fichiers fictifs (comme
//! `the_pattern_detector_actually_detects`).
//!
//! Deux fixtures sont assemblées avec `concat!` pour que ce fichier ne se compte pas
//! lui-même : un `fn ca_…` en clair serait un critère d'acceptation aux yeux de
//! `ca_names` (et de `docs/ca-matrix.md`), un allow de `too_many_lines` en clair
//! compterait dans R7.

use super::*;
use crate::snapshot::workspace_snapshot;

fn workspace_budget() -> Budget {
    Budget::load().unwrap_or_else(|e| panic!("{e}"))
}

fn lines(n: usize) -> String {
    "x\n".repeat(n)
}

fn budget() -> Budget {
    Budget {
        ceiling: 1000,
        test_ceiling: 1500,
        test_modules: ["crates/x/src/e2e.rs".to_string()].into(),
        oversized: [("crates/x/src/big.rs".to_string(), 1200)].into(),
        crates: [("penelope-daemon".to_string(), 100)].into(),
        daemon_modules: ["agent".to_string()].into(),
        impl_daemon: ["runtime.rs".to_string()].into(),
        daemon_users: [
            ("scheduler.rs".to_string(), 2),
            ("runtime.rs".to_string(), 1),
        ]
        .into(),
        allow_too_many_lines: 1,
        ca_required: ["ca_5_4_each_request_extends_the_previous_one".to_string()].into(),
        channel_allowed: [("crates/penelope-daemon/src/bus.rs".to_string(), 1)].into(),
    }
}

fn daemon(rel: &str, raw: &str) -> SourceFile {
    SourceFile::new(
        "penelope-daemon",
        &format!("crates/penelope-daemon/src/{rel}"),
        raw,
    )
}

fn rules(v: &[Violation]) -> Vec<&'static str> {
    v.iter().map(|x| x.rule).collect()
}

fn show(v: &[Violation]) -> String {
    v.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------------------
// Le workspace réel
// ---------------------------------------------------------------------------------------

#[test]
fn the_budget_file_is_readable() {
    let b = workspace_budget();
    assert_eq!(b.ceiling, 1000);
    assert_eq!(b.test_ceiling, 1500);
    assert!(
        b.oversized.len() >= 30,
        "liste de référence : {}",
        b.oversized.len()
    );
    assert!(
        b.ca_required.len() >= 70,
        "critères : {}",
        b.ca_required.len()
    );
}

/// R1, R2 : aucun fichier hors liste ne dépasse son plafond.
#[test]
fn no_source_file_exceeds_the_ceiling() {
    let v: Vec<Violation> = size_violations(workspace_snapshot(), &workspace_budget())
        .into_iter()
        .filter(|x| x.rule == "plafond de taille" || x.rule == "plafond des tests")
        .collect();
    assert!(v.is_empty(), "plafonds dépassés :\n{}", show(&v));
}

/// R3 : la liste de référence ne peut que décroître (hausse, entrée périmée).
#[test]
fn oversized_files_only_shrink() {
    let v: Vec<Violation> = size_violations(workspace_snapshot(), &workspace_budget())
        .into_iter()
        .filter(|x| x.rule == "liste de référence" || x.rule == "entrée périmée")
        .collect();
    assert!(v.is_empty(), "liste de référence :\n{}", show(&v));
}

/// R4.
#[test]
fn crates_stay_under_their_ceiling() {
    let v = crate_size_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "{}", v.join("\n"));
}

/// R5.
#[test]
fn daemon_modules_are_whitelisted() {
    let v = daemon_module_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "modules hors liste :\n{}", show(&v));
}

/// R6.
#[test]
fn daemon_coupling_never_grows() {
    let v = daemon_coupling_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "couplage au Daemon :\n{}", show(&v));
}

/// R7.
#[test]
fn too_many_lines_allows_never_grow() {
    let v = too_many_lines_allow_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "{}", v.join("\n"));
}

/// R8.
#[test]
fn acceptance_tests_never_disappear() {
    let v = acceptance_test_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "{}", v.join("\n"));
}

/// R8 du découpage.
#[test]
fn channel_agnostic_surfaces_do_not_name_a_channel() {
    let v = channel_violations(workspace_snapshot(), &workspace_budget());
    assert!(v.is_empty(), "frontière canal/cœur :\n{}", show(&v));
}

#[test]
fn every_crate_is_on_one_side_of_the_channel_boundary() {
    let v = undeclared_channel_crates(workspace_snapshot());
    assert!(v.is_empty(), "{}", v.join("\n"));
}

// ---------------------------------------------------------------------------------------
// Les détecteurs, sur des fichiers fictifs
// ---------------------------------------------------------------------------------------

#[test]
fn the_size_detector_actually_detects() {
    let b = budget();
    let snap = |n: usize| {
        Snapshot::of(vec![
            SourceFile::new("penelope-x", "crates/x/src/media.rs", lines(n)),
            SourceFile::new("penelope-x", "crates/x/src/big.rs", lines(1100)),
        ])
    };
    let v = size_violations(&snap(1001), &b);
    assert_eq!(rules(&v), ["plafond de taille"], "{}", show(&v));
    assert_eq!(
        v[0].to_string(),
        "[penelope-x] crates/x/src/media.rs:1 — plafond de taille : 1 001 lignes, plafond \
         1 000 (hors liste de référence) : découper, ou déplacer les tests dans media/tests.rs \
         (tests.rs, tests/, *_tests.rs et testing.rs sont des fichiers de tests, plafond 1 500)"
    );
    assert!(size_violations(&snap(999), &b).is_empty());
    assert!(size_violations(&snap(1000), &b).is_empty());
}

#[test]
fn the_reference_list_detector_reports_its_three_failures() {
    let b = budget();
    let with_big = |n: usize| {
        Snapshot::of(vec![SourceFile::new(
            "penelope-x",
            "crates/x/src/big.rs",
            lines(n),
        )])
    };
    // Hausse au-dessus de la borne inscrite.
    let v = size_violations(&with_big(1201), &b);
    assert_eq!(rules(&v), ["liste de référence"], "{}", show(&v));
    assert!(
        v[0].text
            .contains("1 201 lignes, la liste de référence lui en accorde 1 200")
    );
    // Une baisse sans passage sous le plafond est tolérée.
    assert!(size_violations(&with_big(1050), &b).is_empty());
    // Entrée périmée : repassé sous le plafond.
    let v = size_violations(&with_big(987), &b);
    assert_eq!(rules(&v), ["entrée périmée"], "{}", show(&v));
    assert!(v[0].text.contains("987 lignes, plafond 1 000"));
    assert!(v[0].text.contains("UPDATE_BUDGET=1"));
    // Entrée périmée : fichier disparu.
    let v = size_violations(&Snapshot::of(vec![]), &b);
    assert_eq!(rules(&v), ["entrée périmée"]);
    assert!(v[0].text.contains("fichier disparu"));
    assert_eq!(v[0].crate_name, "x");
}

#[test]
fn test_files_have_their_own_ceiling() {
    let b = budget();
    let one = |rel: &str, n: usize| {
        size_violations(
            &Snapshot::of(vec![
                SourceFile::new("penelope-x", rel, lines(n)),
                SourceFile::new("penelope-x", "crates/x/src/big.rs", lines(1100)),
            ]),
            &b,
        )
    };
    let v = one("crates/x/tests/docs.rs", 1501);
    assert_eq!(rules(&v), ["plafond des tests"], "{}", show(&v));
    assert_eq!(
        v[0].to_string(),
        "[penelope-x] crates/x/tests/docs.rs:1 — plafond des tests : 1 501 lignes, plafond \
         1 500 : scinder le fichier de tests"
    );
    for rel in [
        "crates/x/tests/docs.rs",
        "crates/x/src/e2e.rs",
        "crates/x/src/foo/tests.rs",
        "crates/x/src/foo_tests.rs",
        "crates/x/src/telegram/tests/mod.rs",
        "crates/x/src/telegram/tests/commands.rs",
        "crates/x/src/agent/clone_policy_tests.rs",
        "crates/x/src/mcp/testing.rs",
    ] {
        assert!(one(rel, 1400).is_empty(), "{rel} est un fichier de tests");
    }
    assert_eq!(
        rules(&one("crates/x/examples/demo.rs", 1001)),
        ["plafond de taille"]
    );
    assert_eq!(
        rules(&one("crates/x/src/foo/tests_helpers.rs", 1001)),
        ["plafond de taille"]
    );
}

#[test]
fn the_crate_ceiling_detector_counts_src_only() {
    let b = budget();
    let snap = |src: usize, tests: usize| {
        Snapshot::of(vec![
            daemon("a.rs", &lines(src / 2)),
            daemon("b.rs", &lines(src - src / 2)),
            SourceFile::new(
                "penelope-daemon",
                "crates/penelope-daemon/tests/chat_socket.rs",
                lines(tests),
            ),
        ])
    };
    let v = crate_size_violations(&snap(101, 0), &b);
    assert_eq!(
        v,
        [
            "penelope-daemon/src : 101 lignes, plafond 100 (budget.toml [crates]) : la 0.17 \
          ne grossit plus, la fonctionnalité va dans la branche v1"
        ]
    );
    assert!(crate_size_violations(&snap(100, 500), &b).is_empty());
}

#[test]
fn the_module_whitelist_detector_actually_detects() {
    let b = budget();
    let lib = "pub mod agent;\npub mod notifications;\n#[cfg(test)]\nmod tests;\npub(crate) mod helpers;\n";
    let v = daemon_module_violations(&Snapshot::of(vec![daemon("lib.rs", lib)]), &b);
    assert_eq!(rules(&v), ["liste blanche des modules"; 2], "{}", show(&v));
    assert_eq!(
        v[0].to_string(),
        "[penelope-daemon] crates/penelope-daemon/src/lib.rs:2 — liste blanche des modules \
         : module « notifications » absent de la liste blanche (budget.toml \
         [daemon].modules) : un nouveau module du daemon se fait dans v1, pas dans la 0.17"
    );
    assert_eq!(v[1].line, 5);
    // Un sous-module d'un `mod tests` scindé, un module en ligne, une déclaration après
    // le module de tests, un commentaire, un autre crate : rien.
    let quiet = Snapshot::of(vec![
        daemon("telegram/tests.rs", "mod commands;\nmod cards;\n"),
        daemon(
            "a.rs",
            "mod inline {\n}\n// mod ghost;\n#[cfg(test)]\nmod tests {\n    mod sub;\n}\n",
        ),
        SourceFile::new(
            "penelope-kernel",
            "crates/penelope-kernel/src/lib.rs",
            "pub mod anything;\n",
        ),
    ]);
    assert!(daemon_module_violations(&quiet, &b).is_empty());
}

#[test]
fn the_daemon_coupling_detector_actually_detects() {
    let b = budget();
    let v = daemon_coupling_violations(
        &Snapshot::of(vec![daemon(
            "media.rs",
            "pub async fn transcribe(d: &Daemon) {}\n",
        )]),
        &b,
    );
    assert_eq!(
        show(&v),
        "[penelope-daemon] crates/penelope-daemon/src/media.rs:1 — couplage au Daemon : \
         media.rs nomme le type Daemon 1 fois hors tests, budget 0 (budget.toml \
         [daemon.daemon_users]) : prendre &Services ou un trait, pas le daemon entier"
    );
    // Au budget, rien ; une de plus, la ligne du dépassement est nommée.
    let sched = |extra: &str| {
        daemon(
            "scheduler.rs",
            &format!("fn a(d: &Daemon) {{}}\nfn b(d: Arc<Daemon>) {{}}\n{extra}"),
        )
    };
    assert!(daemon_coupling_violations(&Snapshot::of(vec![sched("")]), &b).is_empty());
    let v = daemon_coupling_violations(&Snapshot::of(vec![sched("fn c(d: &Daemon) {}\n")]), &b);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 3);
    assert!(v[0].text.contains("3 fois hors tests, budget 2"));
    // Frontière de mot, commentaires, module de tests, fichiers de tests : rien.
    let quiet = Snapshot::of(vec![
        daemon(
            "voice.rs",
            "fn a(h: &DaemonHandle, t: DaemonTokens) {}\n// Daemon\n/// Daemon\n#[cfg(test)]\nmod tests {\n    fn t(d: &Daemon) {}\n}\n",
        ),
        SourceFile::new(
            "penelope-daemon",
            "crates/penelope-daemon/tests/e2e.rs",
            "fn t(d: &Daemon) {}\n",
        ),
    ]);
    assert!(daemon_coupling_violations(&quiet, &b).is_empty());
}

#[test]
fn an_impl_daemon_block_is_only_allowed_in_the_listed_files() {
    let b = budget();
    let v = daemon_coupling_violations(
        &Snapshot::of(vec![daemon("dream.rs", "fn x() {}\nimpl Daemon {\n}\n")]),
        &b,
    );
    assert_eq!(
        rules(&v),
        ["couplage au Daemon", "impl Daemon"],
        "{}",
        show(&v)
    );
    assert_eq!(v[1].line, 2);
    assert!(v[1].text.contains("impl_daemon = runtime.rs"));
    let v = daemon_coupling_violations(
        &Snapshot::of(vec![daemon("dream.rs", "impl Foo for Daemon {\n}\n")]),
        &b,
    );
    assert!(rules(&v).contains(&"impl Daemon"));
    let v = daemon_coupling_violations(
        &Snapshot::of(vec![daemon("runtime.rs", "impl Daemon {\n}\n")]),
        &b,
    );
    assert!(v.is_empty(), "{}", show(&v));
}

#[test]
fn the_allow_counter_actually_counts() {
    let b = budget();
    let allow = concat!("#[allow(clippy::", "too_many_lines)]");
    let one = SourceFile::new(
        "penelope-x",
        "crates/x/src/a.rs",
        format!("{allow}\nfn f() {{}}\n"),
    );
    assert!(too_many_lines_allow_violations(&Snapshot::of(vec![one.clone()]), &b).is_empty());
    let two = SourceFile::new(
        "penelope-x",
        "crates/x/tests/t.rs",
        format!(
            "// {allow} en commentaire, ne compte pas\n#![allow(dead_code, clippy::{})]\n",
            "too_many_lines"
        ),
    );
    let v = too_many_lines_allow_violations(&Snapshot::of(vec![one, two]), &b);
    assert_eq!(
        v[0],
        format!(
            "2 {allow} dans les sources, budget 1 (budget.toml [lints]) : découper la fonction"
        )
    );
    assert_eq!(v[1..], ["  crates/x/src/a.rs:1", "  crates/x/tests/t.rs:2"]);
    let expect = SourceFile::new(
        "penelope-x",
        "crates/x/src/b.rs",
        concat!("#[expect(clippy::", "too_many_lines)]\n"),
    );
    assert_eq!(too_many_lines_allows(&Snapshot::of(vec![expect])).len(), 1);
}

#[test]
fn the_acceptance_test_detector_actually_detects() {
    let b = budget();
    let name = "ca_5_4_each_request_extends_the_previous_one";
    let present = SourceFile::new(
        "penelope-x",
        "crates/x/tests/other_file.rs",
        concat!(
            "#[test]\nfn ",
            "ca_5_4_each_request_extends_the_previous_one() {}\n"
        ),
    );
    assert!(acceptance_test_violations(&Snapshot::of(vec![present]), &b).is_empty());
    let renamed = SourceFile::new(
        "penelope-x",
        "crates/x/tests/other_file.rs",
        concat!(
            "#[test]\nfn ",
            "each_request_extends_the_previous_one() {}\n"
        ),
    );
    let v = acceptance_test_violations(&Snapshot::of(vec![renamed]), &b);
    assert_eq!(
        v,
        [format!(
            "critère d'acceptation disparu : {name} (budget.toml [ca].required le cite, \
             aucune fonction ne le porte) : le renommer est interdit, le déplacer est permis"
        )]
    );
}

#[test]
fn the_channel_detector_actually_detects() {
    let b = budget();
    let v = channel_violations(
        &Snapshot::of(vec![daemon(
            "media.rs",
            "fn f(origin: &Origin) -> bool {\n    matches!(origin, Origin::Telegram { .. })\n}\n",
        )]),
        &b,
    );
    assert_eq!(
        show(&v),
        "[penelope-daemon] crates/penelope-daemon/src/media.rs:2 — frontière canal/cœur : \
         media.rs nomme le canal 1 fois hors tests (« Telegram » ligne 2), budget 0 \
         (budget.toml [channel.allowed]) : le cœur ne connaît un canal que par \
         ChannelDelivery, Messenger ou OwnerChannel"
    );
    for (src, hit) in [
        ("let id = chat_id;\n", "chat_id"),
        ("let t = topic_id;\n", "topic_id"),
        ("sql(\"DELETE FROM tg_outbox\");\n", "tg_outbox"),
        ("let d = callback_data;\n", "callback_data"),
        ("store.find_by_topic(1);\n", "find_by_topic"),
        ("use penelope_telegram::Template;\n", "telegram"),
        ("let s = \"canal Telegram indisponible\";\n", "Telegram"),
    ] {
        let v = channel_violations(&Snapshot::of(vec![daemon("media.rs", src)]), &b);
        assert_eq!(v.len(), 1, "{src}");
        assert!(v[0].text.contains(&format!("(« {hit} »")), "{}", v[0].text);
    }
    // Au budget, rien ; une de plus, la ligne du dépassement est nommée.
    let bus = |extra: &str| daemon("bus.rs", &format!("Origin::Telegram\n{extra}"));
    assert!(channel_violations(&Snapshot::of(vec![bus("")]), &b).is_empty());
    let v = channel_violations(&Snapshot::of(vec![bus("let c = chat_id;\n")]), &b);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].line, 2);
    assert!(
        v[0].text
            .contains("2 fois hors tests (« chat_id » ligne 2), budget 1")
    );
}

#[test]
fn the_channel_detector_ignores_the_gateway_tests_comments_and_channel_crates() {
    let b = budget();
    let noisy = "Origin::Telegram chat_id topic_id\n";
    let quiet = Snapshot::of(vec![
        daemon("telegram.rs", noisy),
        daemon("telegram/screens.rs", noisy),
        daemon("telegram/commands.rs", noisy),
        daemon(
            "media.rs",
            "// Telegram\n/// chat_id\n#[cfg(test)]\nmod tests {\n    Origin::Telegram\n}\n",
        ),
        daemon("tests.rs", noisy),
        SourceFile::new(
            "penelope-daemon",
            "crates/penelope-daemon/tests/chat_socket.rs",
            noisy,
        ),
        SourceFile::new("penelope-cli", "crates/penelope-cli/src/commands.rs", noisy),
        SourceFile::new(
            "penelope-telegram",
            "crates/penelope-telegram/src/api.rs",
            noisy,
        ),
        SourceFile::new(
            "penelope-evals",
            "crates/penelope-evals/src/suites.rs",
            noisy,
        ),
        SourceFile::new(
            "penelope-kernel",
            "crates/penelope-kernel/src/x.rs",
            "let chat = 1; let topic = 2; let tag = 3; let x = a_tg_b;\n",
        ),
    ]);
    let v = channel_violations(&quiet, &b);
    assert!(v.is_empty(), "{}", show(&v));
    // Une crate future est protégée dès sa création ; une crate inconnue doit se déclarer.
    let app = SourceFile::new("penelope-app", "crates/penelope-app/src/lib.rs", noisy);
    assert_eq!(channel_violations(&Snapshot::of(vec![app]), &b).len(), 1);
    let unknown = SourceFile::new("penelope-new", "crates/penelope-new/src/lib.rs", "");
    let v = undeclared_channel_crates(&Snapshot::of(vec![unknown]));
    assert_eq!(v.len(), 1);
    assert!(v[0].contains("`penelope-new`"));
}

#[test]
fn measure_reports_only_what_counts_against_the_budget() {
    let b = budget();
    let snap = Snapshot::of(vec![
        SourceFile::new("penelope-x", "crates/x/src/big.rs", lines(1150)),
        SourceFile::new("penelope-x", "crates/x/src/small.rs", lines(1000)),
        SourceFile::new("penelope-x", "crates/x/tests/t.rs", lines(1501)),
        daemon(
            "lib.rs",
            "pub mod agent;\npub mod fresh;\n#[cfg(test)]\nmod tests;\n",
        ),
        daemon("runtime.rs", "impl Daemon {\n}\n"),
        daemon("quiet.rs", "fn f(h: &DaemonHandle) {}\n"),
        daemon("bus.rs", "Origin::Telegram\n"),
        SourceFile::new(
            "penelope-x",
            "crates/x/tests/ca.rs",
            concat!("fn ", "ca_9_9_something() {}\n"),
        ),
    ]);
    let m = measure(&snap, &b);
    assert_eq!(
        m.oversized,
        [
            ("crates/x/src/big.rs".to_string(), 1150),
            ("crates/x/tests/t.rs".to_string(), 1501)
        ]
        .into()
    );
    assert_eq!(
        m.daemon_modules,
        ["agent".to_string(), "fresh".to_string()].into()
    );
    assert_eq!(m.impl_daemon, ["runtime.rs".to_string()].into());
    assert_eq!(m.daemon_users, [("runtime.rs".to_string(), 1)].into());
    assert_eq!(m.allow_too_many_lines, 0);
    assert_eq!(m.ca_present, ["ca_9_9_something".to_string()].into());
    assert_eq!(
        m.channel,
        [("crates/penelope-daemon/src/bus.rs".to_string(), 1)].into()
    );
}

#[test]
fn thousands_are_spaced_like_the_issues() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1000), "1 000");
    assert_eq!(thousands(13716), "13 716");
    assert_eq!(thousands(1234567), "1 234 567");
}
