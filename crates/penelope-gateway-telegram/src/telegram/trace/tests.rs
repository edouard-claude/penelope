//! Trace des outils (issue #222) : familles, argument principal, regroupement, rendu.

use super::*;
use serde_json::json;

fn compact(t: &Trace) -> String {
    t.render(Mode::Compact, false)
}

/// Tout outil natif du catalogue a une famille : un outil ajouté sans la sienne fait
/// échouer ce test au lieu de s'afficher sous l'icône générique.
#[test]
fn every_native_tool_has_a_family() {
    let missing: Vec<&str> = penelope_tools::spec::all()
        .iter()
        .map(|s| s.name)
        .filter(|n| family_of(n).is_none())
        .collect();
    assert!(missing.is_empty(), "outils sans famille : {missing:?}");
    for meta in ["tool_search", "tool_describe"] {
        assert_eq!(family_of(meta), Some(Family::Tools));
    }
    assert_eq!(family_of("mcp__github__list_issues"), Some(Family::Mcp));
    assert_eq!(family_of("inconnu"), None);
}

#[test]
fn the_main_argument_follows_the_family() {
    let cases: &[(&str, Value, Option<&str>)] = &[
        (
            "shell_exec",
            json!({"command": "echo test"}),
            Some("echo test"),
        ),
        (
            "fs_read",
            json!({"path": "src/main.rs"}),
            Some("src/main.rs"),
        ),
        (
            "fs_search",
            json!({"pattern": "TODO", "path": "src"}),
            Some("TODO"),
        ),
        ("artifact_read", json!({"id": "a1"}), Some("a1")),
        ("git_commit", json!({"message": "corrige"}), Some("corrige")),
        (
            "http_fetch",
            json!({"url": "https://x.org"}),
            Some("https://x.org"),
        ),
        (
            "http_fetch",
            json!({"url": "https://x.org", "method": "post"}),
            Some("POST https://x.org"),
        ),
        (
            "mem_note",
            json!({"slug": "projet", "texte": "long texte privé"}),
            Some("projet"),
        ),
        ("history_expand", json!({"node_id": "n4"}), Some("n4")),
        (
            "schedule_create",
            json!({"spec": "0 9 * * *"}),
            Some("0 9 * * *"),
        ),
        (
            "workflow_control",
            json!({"op": "pause", "run_id": "r1"}),
            Some("pause r1"),
        ),
        ("job_wait", json!({"job": "j1"}), Some("j1")),
        (
            "sub_agent_spawn",
            json!({"kind": "code", "prompt": "…"}),
            Some("code"),
        ),
        ("send_message", json!({"text": "coucou"}), None),
        (
            "send_file",
            json!({"path": "rapport.pdf"}),
            Some("rapport.pdf"),
        ),
        ("skill_load", json!({"name": "revue"}), Some("revue")),
        (
            "image_generate",
            json!({"prompt": "un chat", "path": "chat.png"}),
            Some("chat.png"),
        ),
        (
            "self_docs",
            json!({"query": "brouillons"}),
            Some("brouillons"),
        ),
        ("tool_search", json!({"query": "pdf"}), Some("pdf")),
        ("mcp__gh__search", json!({"query": "bug"}), Some("bug")),
        ("time_now", json!({}), None),
    ];
    for (name, args, want) in cases {
        assert_eq!(main_arg(name, args).as_deref(), *want, "{name}");
    }
}

/// `tool_call` est déballé : l'outil appelé s'affiche, pas le méta-outil.
#[test]
fn tool_call_is_unwrapped() {
    let (n, a) = unwrap_call(
        "tool_call",
        &json!({"name": "git_clone", "args": {"url": "https://g.it/r"}}),
    );
    assert_eq!(
        (n.as_str(), a["url"].as_str()),
        ("git_clone", Some("https://g.it/r"))
    );
    let (n, a) = unwrap_call(
        "tool_call",
        &json!({"name": "mcp__gh__issue", "args_json": "{\"id\":\"7\"}"}),
    );
    assert_eq!(
        (n.as_str(), a["id"].as_str()),
        ("mcp__gh__issue", Some("7"))
    );

    let mut t = Trace::default();
    t.call(
        "tool_call",
        &json!({"name": "mcp__github__list_issues", "args": {"query": "bug"}}),
    );
    t.result("tool_call", true, "3 issues");
    assert_eq!(compact(&t), "🔌 github · list_issues · <code>bug</code> ✅");
}

/// Hermes : quatre `echo test` consécutifs font une ligne ; un appel séparé par un autre
/// outil n'est pas regroupé à distance.
#[test]
fn consecutive_calls_group_and_distant_ones_do_not() {
    let mut t = Trace::default();
    for _ in 0..4 {
        t.call("shell_exec", &json!({"command": "echo test"}));
        t.result("shell_exec", true, "test");
    }
    t.call("fs_read", &json!({"path": "a.rs"}));
    assert_eq!(
        compact(&t),
        "💻 shell_exec · <code>echo test</code> (×4) ✅\n📄 fs_read · <code>a.rs</code> ⏳"
    );
    t.result("fs_read", true, "");
    t.call("shell_exec", &json!({"command": "echo test"}));
    t.result("shell_exec", true, "");
    assert_eq!(compact(&t).lines().count(), 3, "{}", compact(&t));
}

#[test]
fn a_group_says_all_ok_all_failed_or_how_many_failed() {
    let mut t = Trace::default();
    for ok in [true, false, true, false] {
        t.call("shell_exec", &json!({"command": "make"}));
        t.result("shell_exec", ok, "");
    }
    assert!(compact(&t).ends_with("(×4) ❌ 2/4"), "{}", compact(&t));
    let mut t = Trace::default();
    t.call("shell_exec", &json!({"command": "false"}));
    t.result("shell_exec", false, "code 1");
    assert!(compact(&t).ends_with("</code> ❌"), "{}", compact(&t));
    assert!(!compact(&t).starts_with('❌'));
}

/// Un lot parallèle émet ses appels puis ses résultats dans le même ordre : chaque
/// résultat va au plus ancien appel ouvert du même nom.
#[test]
fn a_parallel_batch_pairs_results_in_order() {
    let mut t = Trace::default();
    t.call("fs_read", &json!({"path": "a"}));
    t.call("fs_read", &json!({"path": "b"}));
    t.call("mem_search", &json!({"query": "q"}));
    t.result("fs_read", true, "");
    t.result("fs_read", false, "absent");
    t.result("mem_search", true, "");
    assert_eq!(
        compact(&t),
        "📄 fs_read · <code>a</code> ✅\n📄 fs_read · <code>b</code> ❌\n🧠 mem_search · <code>q</code> ✅"
    );
}

/// Un résultat sans appel annoncé (appel refusé ou invalide) donne une ligne 🚫 ; un
/// appel resté ouvert devient ⏹ à la fin du tour, ❔ si des événements ont été perdus.
#[test]
fn refused_stopped_and_unknown_calls_are_named() {
    let mut t = Trace::default();
    t.result("fs_write", false, "refusé");
    t.call("shell_exec", &json!({"command": "sleep 99"}));
    t.finish();
    assert_eq!(
        compact(&t),
        "🚫 fs_write\n💻 shell_exec · <code>sleep 99</code> ⏹"
    );
    let mut t = Trace::default();
    t.call("shell_exec", &json!({"command": "sleep 99"}));
    t.mark_incomplete();
    t.finish();
    assert_eq!(
        compact(&t),
        "💻 shell_exec · <code>sleep 99</code> ❔\n❔ trace incomplète"
    );
    let mut t = Trace::default();
    t.call("time_now", &json!({}));
    t.interrupt();
    assert_eq!(
        compact(&t),
        "🗓 time_now ❔\n⏹ interrompu par un redémarrage"
    );
}

/// `full` ajoute l'extrait du résultat en privé ; dans un groupe, `compact` tait
/// l'argument et `full` le montre sans extrait.
#[test]
fn the_modes_and_group_chats_show_more_or_less() {
    let mut t = Trace::default();
    t.call("shell_exec", &json!({"command": "ls"}));
    t.result("shell_exec", true, "a.rs\nb.rs");
    assert_eq!(
        t.render(Mode::Full, false),
        "💻 shell_exec · <code>ls</code> ✅\n   ↳ <i>a.rs b.rs</i>"
    );
    assert_eq!(t.render(Mode::Compact, true), "💻 shell_exec ✅");
    assert_eq!(
        t.render(Mode::Full, true),
        "💻 shell_exec · <code>ls</code> ✅"
    );
}

/// Une commande avec `<`, `&` et des accents graves reste du HTML valide.
#[test]
fn a_command_is_escaped_not_interpreted() {
    let mut t = Trace::default();
    t.call(
        "shell_exec",
        &json!({"command": "cat <a && echo `date` > b"}),
    );
    assert_eq!(
        compact(&t),
        "💻 shell_exec · <code>cat &lt;a &amp;&amp; echo `date` &gt; b</code> ⏳"
    );
}

/// Un secret dans une commande ou dans un extrait ne s'affiche pas.
#[test]
fn secrets_are_redacted_in_arguments_and_previews() {
    let key = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
    let mut t = Trace::default();
    t.call(
        "shell_exec",
        &json!({"command": format!("curl -H 'x-api-key: {key}' x")}),
    );
    t.result("shell_exec", true, &format!("clé : {key}"));
    let full = t.render(Mode::Full, false);
    assert!(!full.contains("AbCdEfGhIjKlMnOp"), "{full}");
}

/// Une longue trace tient dans une bulle et garde l'appel courant visible.
#[test]
fn a_long_trace_fits_and_keeps_the_current_call() {
    let mut t = Trace::default();
    for i in 0..200 {
        t.call(
            "fs_read",
            &json!({"path": format!("src/module_{i:03}/fichier_long.rs")}),
        );
        t.result("fs_read", true, "");
    }
    t.call("shell_exec", &json!({"command": "cargo test"}));
    let out = compact(&t);
    let visible = penelope_telegram::html_to_plain(&out);
    assert!(
        visible.chars().count() <= VISIBLE_BUDGET,
        "{}",
        visible.len()
    );
    // Telegram compte ses 4 096 caractères après lecture des entités : les balises
    // n'en sont pas.
    assert!(out.contains("appel(s) de plus"), "{out}");
    assert!(
        out.ends_with("💻 shell_exec · <code>cargo test</code> ⏳"),
        "{out}"
    );
    let skipped: u32 = out
        .lines()
        .find_map(|l| l.strip_prefix("… et "))
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    let shown = out.lines().filter(|l| l.starts_with("📄")).count() as u32;
    assert_eq!(shown + skipped, 200);
}
