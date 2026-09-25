//! Contrat RPC figé (lot B, épopée #208, tâche T9 de `design/v1/gel-et-outillage.md`) :
//! une forme JSON par méthode de `penelope_kernel::api::method`, pour que la CLI 0.17 et
//! la 1.0 parlent la même socket pendant la refonte.
//!
//! Chaque fichier `tests/golden/<méthode>.json` porte la requête envoyée (`params`, avec
//! des jetons `$…` à la place des identifiants tirés au vol) et la **forme** de la réponse :
//! les clés et le type de chaque valeur, jamais les valeurs elles-mêmes (identifiants,
//! dates, chemins, montants). Un tableau est décrit par la réunion des formes de ses
//! éléments. Une erreur est décrite par son code JSON-RPC et le type de son message. Une
//! méthode dont la réponse dépend d'un état absent en test (superviseur MCP, réseau,
//! sauvegarde à restaurer) est appelée dans son cas d'erreur documenté.
//!
//! La comparaison est par clés et types : une clé retirée, ajoutée ou dont le type change
//! est rouge. Une valeur `null` ne dit rien de son type : seule la clé est figée.
//!
//! ```bash
//! UPDATE_GOLDEN=1 cargo test -p penelope-evals --test rpc_golden
//! ```

use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::Daemon;
use penelope_daemon::rpc::Rpc;
use penelope_evals::ca_matrix;
use penelope_hitl::{ApprovalKind, RuleScope};
use penelope_kernel::api::{RpcRequest, RpcResponse, method};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_kernel::risk::{PolicyDecision, PolicyWindow, RiskClass};
use penelope_llm::mock::MockProvider;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn golden_dir() -> PathBuf {
    ca_matrix::repo_root().join("crates/penelope-evals/tests/golden")
}

fn golden_path(method: &str) -> PathBuf {
    golden_dir().join(format!("{method}.json"))
}

// ---------------------------------------------------------------- formes

/// Forme d'une valeur : les clés et les types, jamais les valeurs.
fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("bool"),
        Value::Number(n) => json!(if n.is_f64() { "number" } else { "integer" }),
        Value::String(_) => json!("string"),
        Value::Array(items) => {
            let mut merged = Value::Null;
            for item in items {
                merged = merge(merged, shape(item));
            }
            Value::Array(if merged.is_null() {
                vec![]
            } else {
                vec![merged]
            })
        }
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
    }
}

/// Réunion de deux formes : les clés des deux, un type connu plutôt que `null`.
fn merge(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Null, x) | (x, Value::Null) => x,
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                let merged = match a.remove(&k) {
                    Some(prev) => merge(prev, v),
                    None => v,
                };
                a.insert(k, merged);
            }
            Value::Object(a)
        }
        (Value::Array(a), Value::Array(b)) => {
            let merged = a.into_iter().chain(b).fold(Value::Null, merge);
            Value::Array(if merged.is_null() {
                vec![]
            } else {
                vec![merged]
            })
        }
        (Value::String(a), Value::String(b)) if a == "null" => Value::String(b),
        (a, _) => a,
    }
}

/// Chemins où deux formes diffèrent : clé absente d'un côté, type différent. `null`
/// d'un côté ne compte pas.
fn differences(path: &str, golden: &Value, fresh: &Value, out: &mut Vec<String>) {
    match (golden, fresh) {
        (Value::Object(g), Value::Object(f)) => {
            for (k, gv) in g {
                match f.get(k) {
                    Some(fv) => differences(&format!("{path}/{k}"), gv, fv, out),
                    None => out.push(format!("{path}/{k} : clé retirée")),
                }
            }
            for k in f.keys() {
                if !g.contains_key(k) {
                    out.push(format!("{path}/{k} : clé nouvelle"));
                }
            }
        }
        (Value::Array(g), Value::Array(f)) => match (g.first(), f.first()) {
            (Some(gv), Some(fv)) => differences(&format!("{path}[]"), gv, fv, out),
            (None, Some(_)) => out.push(format!("{path}[] : éléments nouveaux")),
            (Some(_), None) => out.push(format!("{path}[] : plus aucun élément")),
            (None, None) => {}
        },
        (Value::String(g), Value::String(f)) if g == "null" || f == "null" || g == f => {}
        (Value::Number(g), Value::Number(f)) if g == f => {}
        (Value::Bool(g), Value::Bool(f)) if g == f => {}
        (Value::Null, Value::Null) => {}
        _ => out.push(format!("{path} : {golden} attendu, {fresh} obtenu")),
    }
}

// ---------------------------------------------------------------- fixtures

/// Identifiants tirés au vol, désignés par un jeton `$…` dans les paramètres. Un jeton
/// dont la valeur est un entier (`$n…`) devient un nombre.
type Tokens = BTreeMap<&'static str, String>;

fn resolve(params: &Value, tokens: &Tokens) -> Value {
    match params {
        Value::String(s) if s.starts_with('$') => {
            let value = tokens
                .get(s.as_str())
                .unwrap_or_else(|| panic!("jeton inconnu dans les paramètres : {s}"));
            match (s.starts_with("$n_"), value.parse::<u64>()) {
                (true, Ok(n)) => json!(n),
                _ => Value::String(value.clone()),
            }
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| resolve(v, tokens)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.clone(), resolve(v, tokens)))
                .collect(),
        ),
        other => other.clone(),
    }
}

struct World {
    _dir: tempfile::TempDir,
    d: Arc<Daemon>,
    rpc: Rpc,
    p: Arc<MockProvider>,
    tokens: Tokens,
    runners: tokio::task::JoinHandle<()>,
}

impl World {
    async fn call(&self, m: &str, params: Value) -> RpcResponse {
        self.rpc.handle(RpcRequest::new(1, m, params)).await
    }
}

/// Un daemon de test avec de quoi répondre à chaque méthode : une session avec un tour,
/// des sessions à fermer, purger et remonter, deux demandes d'approbation, une règle,
/// des planifications, un secret, des entrées de mémoire, une intention, une séance
/// d'accueil, un vault sous git.
async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let clock: SharedClock = Arc::new(TestClock::default());
    let s = Arc::new(
        Services::for_tests(dir.path().to_path_buf(), clock)
            .await
            .unwrap(),
    );
    let d = Arc::new(Daemon::from_services(s));
    // Sans classifieur : une réponse scriptée par tour, rien d'autre.
    d.publish_config("test", |c| {
        c.models.routing.classifier = false;
        Ok(vec!["models.routing.classifier".into()])
    })
    .unwrap();
    let p = Arc::new(MockProvider::new());
    d.set_provider_override(p.clone());
    let runners = tokio::spawn(penelope_daemon::runner::run_pool(d.clone()));
    let rpc = Rpc::new(d.clone());
    let s = &d.services;
    let mut tokens = Tokens::new();

    // La session courante de la CLI, avec un tour complet.
    let main = d.chat_session_for(&Origin::Cli).await.unwrap();
    p.reply("Bonjour depuis le contrat doré.");
    rpc.handle(RpcRequest::new(
        1,
        method::CHAT_SEND,
        json!({"text": "bonjour, fais le point"}),
    ))
    .await
    .result
    .expect("premier tour répondu");
    tokens.insert("$session", main.clone());

    // Une session par méthode qui la consomme, avec un tour pour celle qu'on remonte.
    for (token, title) in [
        ("$session_close", "à fermer"),
        ("$session_purge", "à purger"),
        ("$session_rewind", "à remonter"),
    ] {
        let sess = s
            .sessions
            .create(
                penelope_kernel::session::SessionKind::Chat,
                Some(title.into()),
            )
            .await
            .unwrap();
        tokens.insert(token, sess.id.to_string());
    }
    p.reply("Un tour à remonter.");
    rpc.handle(RpcRequest::new(
        1,
        method::CHAT_SEND,
        json!({"text": "un message", "session": tokens["$session_rewind"]}),
    ))
    .await
    .result
    .expect("tour de la session à remonter");

    // Deux demandes sans session : la décision n'a aucun tour à reprendre.
    for (token, subject) in [("$approval_ok", "fs_write"), ("$approval_no", "shell_exec")] {
        let a = s
            .approvals
            .create(
                ApprovalKind::ToolCall,
                subject,
                RiskClass::Write,
                json!({"tool": subject, "arguments": {"path": "notes/a.txt"}}),
                vec!["Autoriser".into(), "Refuser".into()],
                None,
                None,
                false,
            )
            .await
            .unwrap();
        tokens.insert(token, a.id.to_string());
    }
    let rule = s
        .policies
        .create_rule(
            RuleScope::Tool,
            Some("fs_read"),
            None,
            None,
            PolicyDecision::Auto,
            PolicyWindow::Always,
            None,
        )
        .await
        .unwrap();
    tokens.insert("$rule", rule.id);

    // Une planification par méthode qui la modifie.
    for token in [
        "$schedule_rm",
        "$schedule_pause",
        "$schedule_resume",
        "$schedule_run",
        "$schedule_move",
    ] {
        let sched = s
            .schedules
            .create(
                penelope_workflow::TriggerKind::Cron,
                json!({"expr": "30 3 * * *"}),
                json!({"type": "prompt", "prompt": "fais le point"}),
                json!({}),
            )
            .await
            .unwrap();
        tokens.insert(token, sched.id);
    }

    rpc.handle(RpcRequest::new(
        1,
        method::SECRET_SET,
        json!({"name": "golden_secret", "value": "valeur"}),
    ))
    .await
    .result
    .expect("secret posé");

    for uid in ["u_show", "u_signals", "u_forget", "u_split"] {
        s.memory
            .upsert(
                &penelope_memory::index::simple_entry(
                    uid,
                    "Le client Martin est basé à Lyon",
                    penelope_memory::Level::Cure,
                    "2026-09-16",
                ),
                &penelope_memory::Provenance::owner("s1", "interactive", "2026-09-16T10:00:00Z"),
            )
            .await
            .unwrap();
    }
    let intent = s
        .intents
        .create("rappeler le devis", vec!["devis".into()], None, 0, 1, None)
        .await
        .unwrap();
    tokens.insert("$intent", intent.id);

    // Skills livrées relues, vault initialisé sous git : `skill.show` et `mem.diff` ont
    // alors quelque chose à rendre.
    rpc.handle(RpcRequest::new(1, method::SKILL_RELOAD, json!({})))
        .await
        .result
        .expect("skills relues");
    rpc.handle(RpcRequest::new(1, method::VAULT_SYNC, json!({})))
        .await
        .result
        .expect("vault synchronisé");

    let sitting = rpc
        .handle(RpcRequest::new(1, method::ONBOARD_NEXT, json!({})))
        .await
        .result
        .expect("séance d'accueil ouverte");
    let rel = sitting["question"]["rel"]
        .as_str()
        .or(sitting["rel"].as_str())
        .expect("identifiant de la séance")
        .to_string();
    tokens.insert("$rel", rel);
    tokens.insert(
        "$n_question",
        sitting["question"]["n"].as_u64().unwrap_or(1).to_string(),
    );

    let shown = rpc
        .handle(RpcRequest::new(
            1,
            method::WF_SHOW,
            json!({"id": "ticket-to-deploy"}),
        ))
        .await
        .result
        .expect("workflow livré");
    tokens.insert("$workflow_json", shown["definition"].to_string());
    tokens.insert(
        "$missing_dir",
        dir.path().join("absent").to_string_lossy().into_owned(),
    );

    World {
        _dir: dir,
        d,
        rpc,
        p,
        tokens,
        runners,
    }
}

/// Paramètres envoyés à chaque méthode. Un jeton `$…` désigne un identifiant tiré au vol.
fn params_of(m: &str) -> Value {
    match m {
        method::CHAT_SEND => json!({"text": "bonjour, fais le point"}),
        method::CHAT_STOP => json!({"session": "$session"}),
        method::SESSION_NEW => json!({"title": "Contrat doré"}),
        method::SESSION_SWITCH => json!({"session": "$session"}),
        method::SESSION_CLOSE => json!({"session": "$session_close"}),
        method::SESSION_TITLE => json!({"session": "$session", "title": "Titre posé"}),
        method::SESSION_FORK => json!({"session": "$session", "title": "Branche"}),
        method::SESSION_REWIND => json!({"session": "$session_rewind", "turns": 1}),
        method::SESSION_COMPACT => json!({"session": "$session"}),
        method::SESSION_EXPORT => json!({"session": "$session"}),
        method::SESSION_PURGE => json!({"session": "$session_purge", "reason": "test"}),
        method::SESSION_PURGE_PREVIEW => json!({"session": "$session"}),
        method::SESSION_MODEL => json!({"session": "$session"}),
        method::SESSION_MODE => json!({"session": "$session", "mode": "reads"}),
        method::SESSION_PROJECT => json!({"session": "$session", "project": "penelope"}),
        method::SESSION_BUDGET => json!({"session": "$session", "usd": 2.5}),
        method::CONFIG_GET => json!({}),
        method::CONFIG_SET => json!({"path": "budget.daily_usd", "value": 42.0}),
        method::SECRET_SET => json!({"name": "golden_other", "value": "valeur"}),
        method::SECRET_RM => json!({"name": "golden_secret"}),
        method::MODEL_SET => {
            json!({"alias": "main", "model": "openrouter:anthropic/claude-sonnet-4"})
        }
        method::MODEL_ROUTE_TEST => json!({"text": "explique-moi la relativité générale"}),
        method::MODEL_AUTH => json!({"provider": "codex", "action": "status"}),
        method::MODEL_LIST => json!({"filter": "claude"}),
        // Aucun superviseur MCP dans un daemon de test : le cas d'erreur documenté.
        method::MCP_SHOW
        | method::MCP_RM
        | method::MCP_ENABLE
        | method::MCP_DISABLE
        | method::MCP_RESTART
        | method::MCP_TEST
        | method::MCP_AUTH
        | method::MCP_LOGS => json!({"name": "forge"}),
        method::MCP_ADD => json!({"name": "forge", "toml": "command = \"forge-mcp\""}),
        method::MCP_EDIT => json!({"name": "forge", "patch": {"enabled": false}}),
        method::SKILL_SHOW | method::SKILL_ROLLBACK => json!({"name": "wiki-markdown"}),
        // Sans `source` : refusé avant tout accès au réseau.
        method::SKILL_INSTALL => json!({}),
        method::WF_SHOW => json!({"id": "ticket-to-deploy"}),
        method::WF_VALIDATE => json!({"json": "$workflow_json", "name": "ticket-to-deploy"}),
        method::WF_RUN => json!({"id": "inconnu", "params": {}}),
        method::WF_TRACE => json!({"run": "r_inconnu"}),
        method::WF_CONTROL => json!({"run": "r_inconnu", "op": "pause"}),
        method::SCHEDULE_ADD => json!({
            "kind": "cron",
            "spec": {"expr": "0 9 * * 1"},
            "target": {"type": "prompt", "prompt": "digest de la semaine"},
        }),
        method::SCHEDULE_RM => json!({"id": "$schedule_rm"}),
        method::SCHEDULE_PAUSE => json!({"id": "$schedule_pause"}),
        method::SCHEDULE_RESUME => json!({"id": "$schedule_resume"}),
        method::SCHEDULE_RUN_NOW => json!({"id": "$schedule_run"}),
        method::SCHEDULE_MOVE => json!({"id": "$schedule_move", "private": true}),
        method::APPROVE => json!({"id": "$approval_ok"}),
        method::DENY => json!({"id": "$approval_no", "reason": "pas maintenant"}),
        method::POLICY_REVOKE => json!({"id": "$rule"}),
        method::QUIET => json!({"range": "22:00-07:00"}),
        method::MEM_SEARCH => json!({"query": "Martin"}),
        method::MEM_SHOW => json!({"uid": "u_show"}),
        method::MEM_SIGNALS => json!({"uid": "u_signals"}),
        method::MEM_FORGET => json!({"uid": "u_forget"}),
        // Une entrée courte : « rien à découper », sans appel au modèle.
        method::MEM_SPLIT => json!({"uid": "u_split"}),
        method::MEM_RESTORE => json!({"id": 1}),
        method::MEM_DREAM => json!({"dry_run": true}),
        method::MEM_LEARNED => json!({"days": 7}),
        method::MEM_REINDEX => json!({}),
        method::MEM_DIFF => json!({}),
        method::INTENT_CANCEL => json!({"id": "$intent"}),
        method::ONBOARD_ANSWER => json!({"rel": "$rel", "n": "$n_question", "answer": "Édouard"}),
        method::ONBOARD_WRITE => json!({"rel": "$rel"}),
        // Un répertoire absent : le cas d'erreur, sans lire le vrai répertoire personnel.
        method::IMPORT_HERMES => json!({"path": "$missing_dir"}),
        method::EXPORT => json!({"what": "session", "id": "$session"}),
        method::AUDIT_SHOW => json!({"session": "$session"}),
        method::USAGE => json!({"by": "session", "limit": 5}),
        // Le retour arrière manuel est local : sans version précédente, il le dit.
        method::UPGRADE => json!({"rollback": true}),
        _ => json!({}),
    }
}

/// Réponse d'une méthode, sous sa forme.
fn response_shape(r: &RpcResponse) -> Value {
    match (&r.result, &r.error) {
        (Some(v), _) => json!({"result": shape(v)}),
        (None, Some(e)) => json!({"error": {"code": e.code, "message": shape(&json!(e.message))}}),
        (None, None) => json!({"result": "null"}),
    }
}

fn golden_of(m: &str, response: Value) -> Value {
    json!({"method": m, "params": params_of(m), "response": response})
}

fn read_golden(m: &str) -> Option<Value> {
    let raw = std::fs::read_to_string(golden_path(m)).ok()?;
    Some(serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{m}.json illisible : {e}")))
}

fn write_golden(m: &str, v: &Value) {
    let mut raw = serde_json::to_string_pretty(v).unwrap();
    raw.push('\n');
    std::fs::write(golden_path(m), raw).unwrap();
}

// ---------------------------------------------------------------- tests

/// Chaque méthode a son fichier doré, chaque fichier doré a sa méthode.
#[test]
fn every_method_has_a_golden_file_and_every_file_a_method() {
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        return;
    }
    let declared: BTreeSet<&str> = method::ALL.iter().copied().collect();
    let files: BTreeSet<String> = std::fs::read_dir(golden_dir())
        .expect("tests/golden existe")
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".json").map(String::from)
        })
        .collect();
    let files: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let missing: Vec<&&str> = declared.difference(&files).collect();
    let stale: Vec<&&str> = files.difference(&declared).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "fichiers dorés manquants : {missing:?} ; sans méthode : {stale:?}. Régénérer :\n\
         UPDATE_GOLDEN=1 cargo test -p penelope-evals --test rpc_golden"
    );
}

/// Chaque méthode répond avec la forme figée. `UPDATE_GOLDEN=1` réécrit les fichiers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_method_answers_with_its_golden_shape() {
    let w = world().await;
    let update = std::env::var("UPDATE_GOLDEN").is_ok();
    if update {
        std::fs::create_dir_all(golden_dir()).unwrap();
    }
    let mut failures: Vec<String> = Vec::new();

    // `shutdown` et `restart` arrêtent le pool de runners : en dernier.
    let last = [method::SHUTDOWN, method::RESTART];
    let ordered = method::ALL
        .iter()
        .copied()
        .filter(|m| !last.contains(m))
        .chain(last);
    for m in ordered {
        if m == method::CHAT_SEND {
            w.p.reply("Réponse du contrat doré.");
        }
        if m == method::SESSION_COMPACT {
            // Le résumeur attend un objet JSON aux sections du gabarit.
            w.p.reply(
                r#"{"objectif": "Faire le point", "fait": "Un tour", "en_cours": "", "prochaines_etapes": "Continuer"}"#,
            );
        }
        let params = resolve(&params_of(m), &w.tokens);
        let response = tokio::time::timeout(Duration::from_secs(120), w.call(m, params))
            .await
            .unwrap_or_else(|_| panic!("`{m}` ne répond pas en deux minutes"));
        let fresh = golden_of(m, response_shape(&response));
        if update {
            // La régénération dit quelles méthodes sont figées dans leur cas d'erreur, et
            // pourquoi : à relire avant de commiter (`--nocapture`).
            if let Some(e) = &response.error {
                println!("{m} : erreur {} : {}", e.code, e.message);
            }
            write_golden(m, &fresh);
            continue;
        }
        match read_golden(m) {
            None => failures.push(format!("{m} : aucun fichier doré")),
            Some(golden) => {
                let mut diffs = Vec::new();
                differences("", &golden, &fresh, &mut diffs);
                if !diffs.is_empty() {
                    failures.push(format!("{m} :\n    {}", diffs.join("\n    ")));
                }
            }
        }
    }
    if update {
        // Un fichier sans méthode ne survit pas à la régénération.
        let declared: BTreeSet<&str> = method::ALL.iter().copied().collect();
        for e in std::fs::read_dir(golden_dir()).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".json")
                && !declared.contains(stem)
            {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
    }
    w.d.handle.shutdown();
    let _ = tokio::time::timeout(Duration::from_secs(5), w.runners).await;
    assert!(
        failures.is_empty(),
        "contrat RPC changé pour {} méthode(s) :\n{}\n\nSi le changement est voulu : \
         UPDATE_GOLDEN=1 cargo test -p penelope-evals --test rpc_golden",
        failures.len(),
        failures.join("\n")
    );
}

/// La comparaison voit une clé retirée, une clé ajoutée, un type changé ; elle ignore un
/// `null` et compare les tableaux par la réunion de leurs éléments.
#[test]
fn a_removed_key_or_a_changed_type_is_detected() {
    let golden = json!({
        "params": {"dry_run": true, "limit": 5, "none": null},
        "result": {"a": "integer", "b": "string", "c": "null", "d": ["string"]},
    });
    let same = json!({
        "params": {"dry_run": true, "limit": 5, "none": null},
        "result": {"a": "integer", "b": "string", "c": "bool", "d": ["string"]},
    });
    let mut diffs = Vec::new();
    differences("", &golden, &same, &mut diffs);
    assert!(diffs.is_empty(), "{diffs:?}");

    let changed = json!({
        "params": {"dry_run": false, "limit": 5, "none": null},
        "result": {"a": "string", "c": "null", "d": [], "e": "bool"},
    });
    let mut diffs = Vec::new();
    differences("", &golden, &changed, &mut diffs);
    assert_eq!(
        diffs,
        vec![
            "/params/dry_run : true attendu, false obtenu".to_string(),
            "/result/a : \"integer\" attendu, \"string\" obtenu".to_string(),
            "/result/b : clé retirée".to_string(),
            "/result/d[] : plus aucun élément".to_string(),
            "/result/e : clé nouvelle".to_string(),
        ]
    );

    let mixed = shape(&json!([{"a": 1}, {"a": null, "b": 2.5}, {"b": null, "c": true}]));
    assert_eq!(mixed, json!([{"a": "integer", "b": "number", "c": "bool"}]));
    let error = RpcResponse::err(None, -32602, "paramètre `x` manquant");
    assert_eq!(
        response_shape(&error),
        json!({"error": {"code": -32602, "message": "string"}})
    );
}
