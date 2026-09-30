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

// --- Modes `resume` et `narre` (#273) --------------------------------------------------

use super::narrate::{self, Narrator};
use penelope_kernel::config::Config;

/// Quinze appels de fichiers puis une commande : la ligne du mode `resume` compte par
/// famille et par verbe, dit la commande en cours, puis l'état final.
#[test]
fn resume_counts_by_family_and_verb_and_names_the_running_command() {
    let mut t = Trace::default();
    for i in 0..6 {
        t.call("fs_read", &json!({"path": format!("notes/{i}.md")}));
        t.result("fs_read", true, "");
    }
    for i in 0..7 {
        t.call(
            "fs_search",
            &json!({"pattern": format!("TODO{i}"), "path": "."}),
        );
        t.result("fs_search", true, "");
    }
    t.call("shell_exec", &json!({"command": "cargo test"}));
    assert_eq!(
        t.render(Mode::Resume, false),
        "📄 6 lectures, 7 recherches · 💻 <code>cargo test</code> en cours"
    );
    // En groupe : la commande reste tue, comme l'argument en `compact`.
    assert_eq!(
        t.render(Mode::Resume, true),
        "📄 6 lectures, 7 recherches · 💻 1 commande en cours"
    );
    t.result("shell_exec", true, "ok");
    assert_eq!(
        t.render(Mode::Resume, false),
        "📄 6 lectures, 7 recherches · 💻 <code>cargo test</code> · ✅"
    );
    // `narre` sans phrase rend la même ligne : c'est son repli.
    assert_eq!(t.render(Mode::Narre, false), t.render(Mode::Resume, false));
}

/// Échecs, refus, appels sans réponse et redémarrage : la ligne les compte, les lignes de
/// fin restent celles des autres modes.
#[test]
fn resume_counts_failures_refusals_and_lost_calls() {
    let mut t = Trace::default();
    t.call("fs_read", &json!({"path": "a"}));
    t.result("fs_read", false, "absent");
    t.result("fs_write", false, "refusé");
    t.call("mem_search", &json!({"query": "q"}));
    t.call("mem_note", &json!({"slug": "s"}));
    t.result("mem_search", true, "");
    t.result("mem_note", true, "");
    t.call("http_fetch", &json!({"url": "https://x.org"}));
    t.finish();
    assert_eq!(
        t.render(Mode::Resume, false),
        "📄 1 lecture · 🧠 1 rappel, 1 note · 🌐 1 requête · ❌ 1 échec · 🚫 1 refusé · ⏹ 1 sans réponse"
    );
    let mut t = Trace::default();
    t.call("shell_exec", &json!({"command": "make"}));
    t.call("shell_exec", &json!({"command": "make"}));
    t.result("shell_exec", true, "");
    t.result("shell_exec", true, "");
    t.call("mcp__github__list_issues", &json!({"query": "bug"}));
    t.interrupt();
    assert_eq!(
        t.render(Mode::Resume, false),
        "💻 <code>make</code> (×2) · 🔌 1 appel github · ❔ 1 sans réponse\n⏹ interrompu par un redémarrage"
    );
    let mut t = Trace::default();
    t.result("fs_write", false, "refusé");
    assert_eq!(t.render(Mode::Resume, false), "🚫 1 refusé");
}

/// Le prompt du rôle `trace` ne porte que la liste déjà masquée : un jeton dans une
/// commande, une référence `${SECRET:…}` et un extrait de résultat n'y entrent jamais.
/// Une ligne par groupe, « n. outil argument (×N) état », et rien d'autre : ni phrase
/// précédente ni état du tour à recopier (#280).
#[test]
fn the_trace_role_prompt_never_carries_a_secret_nor_a_result() {
    let key = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
    let mut t = Trace::default();
    t.call(
        "shell_exec",
        &json!({"command": format!("curl -H 'Authorization: {key}' -u ${{SECRET:github_token}} https://api.github.com")}),
    );
    t.result(
        "shell_exec",
        true,
        &format!("contenu privé du fichier {key}"),
    );
    t.call("fs_read", &json!({"path": "notes.txt"}));
    let prompt = narrate::prompt(&t);
    assert!(!prompt.contains("ghp_"), "{prompt}");
    assert!(!prompt.contains("SECRET"), "{prompt}");
    assert!(!prompt.contains("contenu privé"), "{prompt}");
    assert!(prompt.contains("[secret]"), "{prompt}");
    assert!(prompt.starts_with("1. shell_exec curl -H"), "{prompt}");
    assert!(
        prompt.ends_with("\n2. fs_read notes.txt en cours"),
        "{prompt}"
    );
    assert_eq!(prompt.lines().count(), 2, "{prompt}");
    // Un lot groupé, terminé : une seule ligne.
    let mut t = Trace::default();
    for _ in 0..3 {
        t.call("fs_read", &json!({"path": "a"}));
        t.result("fs_read", true, "");
    }
    assert_eq!(narrate::prompt(&t), "1. fs_read a (×3) fini");
    // Une longue trace : les douze derniers groupes seulement, numérotés de 1 à 12.
    let mut t = Trace::default();
    for i in 0..40 {
        t.call("fs_read", &json!({"path": format!("f{i}")}));
        t.result("fs_read", true, "");
    }
    let prompt = narrate::prompt(&t);
    assert_eq!(prompt.lines().count(), 12, "{prompt}");
    assert!(prompt.starts_with("1. fs_read f28 fini\n"), "{prompt}");
    assert!(prompt.ends_with("\n12. fs_read f39 fini"), "{prompt}");
}

/// La requête du rôle `trace` (#280) : le système donne la liste des emojis d'activité et
/// leur sens, sans phrase d'exemple à recopier ; quatre tours d'exemple alternent
/// propriétaire et assistant, chaque réponse d'exemple passe `clean` telle quelle ; le
/// dernier message est la liste seule.
#[test]
fn the_trace_role_request_is_a_system_four_shots_and_the_list() {
    use penelope_llm::types::Role;
    let mut t = Trace::default();
    t.call("fs_read", &json!({"path": "notes.txt"}));
    let m = narrate::messages(&t);
    assert_eq!(m.len(), 10, "système, quatre paires, la liste");
    assert_eq!(m[0].role, Role::System);
    let system = m[0].text();
    for (e, sense) in narrate::EMOJIS {
        assert!(system.contains(&format!("{e} {sense}")), "{system}");
    }
    assert!(system.contains("💬 message envoyé"), "{system}");
    for (e, _) in narrate::STATE_EMOJIS {
        assert!(
            !system.contains(e),
            "un emoji d'état n'est pas proposé : {system}"
        );
    }
    assert!(
        !system.contains("Relecture"),
        "pas de phrase à recopier : {system}"
    );
    assert!(!system.contains("précédente"), "{system}");
    for pair in m[1..9].chunks(2) {
        assert_eq!(pair[0].role, Role::User);
        assert_eq!(pair[1].role, Role::Assistant);
        let user = pair[0].text();
        assert!(
            user.lines()
                .enumerate()
                .all(|(i, l)| l.starts_with(&format!("{}. ", i + 1))
                    && (l.ends_with(" fini") || l.ends_with(" en cours"))),
            "l'exemple a la forme de la liste : {user}"
        );
        let answer = pair[1].text();
        assert_eq!(
            narrate::clean(&answer, "📄").as_deref(),
            Some(answer.as_str()),
            "l'exemple respecte la forme demandée"
        );
    }
    assert_eq!(m[9].role, Role::User);
    assert_eq!(m[9].text(), "1. fs_read notes.txt en cours");
    assert_eq!(m[9].text(), narrate::prompt(&t));
}

/// La phrase rendue : un emoji de la liste fermée en tête (celui du modèle s'il est
/// admis, sinon celui de l'activité), les autres pictogrammes retirés, guillemets et point
/// final ôtés ; vide, trop longue ou porteuse d'un secret, elle est refusée.
#[test]
fn the_narrated_phrase_is_cleaned_and_bounded() {
    let c = |raw: &str| narrate::clean(raw, "📄");
    assert_eq!(
        c("« 🔎 Recherche des devis ACME. »\n").as_deref(),
        Some("🔎 Recherche des devis ACME")
    );
    assert_eq!(
        c("🚀 Relecture des notes du jour").as_deref(),
        Some("📄 Relecture des notes du jour")
    );
    assert_eq!(
        c("Relecture des notes 🎉 du jour ✨").as_deref(),
        Some("📄 Relecture des notes du jour")
    );
    assert_eq!(
        c("Relecture des notes 🧠 du jour").as_deref(),
        Some("🧠 Relecture des notes du jour")
    );
    assert_eq!(
        c("✍ Écriture du rapport").as_deref(),
        Some("✍️ Écriture du rapport")
    );
    assert_eq!(
        c("⏸ Attente d'approbation").as_deref(),
        Some("⏸️ Attente d'approbation")
    );
    assert_eq!(
        c("💬 Réponse envoyée au propriétaire").as_deref(),
        Some("💬 Réponse envoyée au propriétaire"),
        "💬 est admis (#280)"
    );
    // Qwen3 qui réfléchit : la première ligne est `<think>`, un seul mot, la phrase est
    // rejetée ; d'où `penelope local install --no-think`.
    assert!(c("<think>\nLe propriétaire veut…\n</think>\n📄 Lecture des notes").is_none());
    assert!(c("  \n ").is_none());
    assert!(c("Bonjour").is_none(), "un seul mot");
    assert!(c(&"mot ".repeat(13)).is_none(), "trop de mots");
    assert!(c("sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef clé").is_none());
}

/// Le modèle du rôle `trace` : `models.roles.trace`, sinon l'alias `local`, sinon le
/// premier alias `local:` qui ne sert ni la voix, ni les images, ni les embeddings ; rien
/// sans alias local.
#[test]
fn the_trace_role_defaults_to_a_local_text_alias() {
    let mut cfg = Config::sample(1);
    assert_eq!(narrate::role_model(&cfg), None);
    cfg.models
        .aliases
        .insert("voix".into(), "local:mlx-community/whisper".into());
    cfg.models.roles.insert("stt".into(), "voix".into());
    assert_eq!(
        narrate::role_model(&cfg),
        None,
        "un alias de voix n'est pas un narrateur"
    );
    cfg.models
        .aliases
        .insert("petit".into(), "local:mlx-community/Qwen3-1.7B-4bit".into());
    cfg.models
        .aliases
        .insert("gros".into(), "local:mlx-community/Qwen3-8B-4bit".into());
    assert_eq!(
        narrate::role_model(&cfg),
        Some(Narrator {
            alias: "gros".into(),
            model: "local:mlx-community/Qwen3-8B-4bit".into()
        }),
        "le premier alias local par ordre alphabétique"
    );
    cfg.models
        .aliases
        .insert("local".into(), "local:mlx-community/Qwen3-4B-4bit".into());
    assert_eq!(
        narrate::role_model(&cfg).map(|n| n.alias),
        Some("local".into())
    );
    cfg.models.roles.insert("trace".into(), "fast".into());
    let n = narrate::role_model(&cfg).unwrap();
    assert_eq!(n.alias, "fast");
    assert!(!n.is_local(), "{n:?}");
    assert!(
        Narrator {
            alias: "x".into(),
            model: "local:m".into()
        }
        .is_local()
    );
}
