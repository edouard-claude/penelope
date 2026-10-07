//! Rendus lisibles des listes (`session list`, `mcp list`, `schedule list`, `model list`).

use crate::output;
use serde_json::{Value, json};

/// `penelope session list` : titre et date d'abord, l'identifiant pour `switch`.
pub(super) fn render_session_list(v: &Value) -> String {
    let sessions = v.as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        return "Aucune session.".into();
    }
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let when = s["last_activity"]
                .as_str()
                .or_else(|| s["created_at"].as_str())
                .unwrap_or("")
                .replace('T', " ");
            json!({
                "titre": s["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("(sans titre)"),
                "activité": when.chars().take(16).collect::<String>(),
                "état": s["state"],
                "session": s["id"],
            })
        })
        .collect();
    output::table(&rows)
}

/// `penelope mcp list` : un serveur par ligne, puis les déclarations invalides.
pub(super) fn render_mcp_list(v: &Value) -> String {
    let servers = v["servers"].as_array().cloned().unwrap_or_default();
    let mut out = if servers.is_empty() {
        format!(
            "Aucun serveur MCP déclaré dans {}",
            v["dir"].as_str().unwrap_or("mcp.d")
        )
    } else {
        let rows: Vec<Value> = servers
            .iter()
            .map(|s| {
                json!({
                    "serveur": s["name"],
                    "état": s["state"],
                    "outils": s["tools"],
                    "actif": if s["running"].as_bool().unwrap_or(false) { "oui" } else { "non" },
                    "trousseau": keychain_cell(s),
                    "appels": s["calls"],
                    "erreur": s["last_error"].as_str().unwrap_or(""),
                })
            })
            .collect();
        output::table(&rows)
    };
    for bad in v["invalid"].as_array().cloned().unwrap_or_default() {
        out.push_str(&format!(
            "\n⚠️ {} : {}",
            bad["file"].as_str().unwrap_or("?"),
            bad["error"].as_str().unwrap_or("?")
        ));
    }
    out
}

/// `penelope schedule list` : une planification par ligne, avec où elle livre (#124).
pub(super) fn render_schedule_list(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Aucune planification.".into();
    }
    let rows: Vec<Value> = list
        .iter()
        .map(|s| {
            let spec = &s["spec"];
            let quand = match s["kind"].as_str().unwrap_or("?") {
                "cron" => spec["expr"].as_str().unwrap_or("?").to_string(),
                "interval" | "mcp_poll" => format!(
                    "toutes les {} min",
                    spec["every_ms"].as_u64().unwrap_or(0) / 60_000
                ),
                "watch_file" => spec["path"].as_str().unwrap_or("?").to_string(),
                "webhook" => format!("POST {}", spec["path"].as_str().unwrap_or("?")),
                "mcp_subscribe" => format!(
                    "abonnement {} {}",
                    spec["server"].as_str().unwrap_or("?"),
                    spec["uri"].as_str().unwrap_or("?")
                ),
                _ => spec["event"].as_str().unwrap_or("?").to_string(),
            };
            let t = &s["target"];
            let quoi = t["label"]
                .as_str()
                .or(t["prompt"].as_str())
                .or(t["template"].as_str())
                .or(t["workflowId"].as_str())
                .unwrap_or("")
                .chars()
                .take(40)
                .collect::<String>();
            json!({
                "id": s["id"],
                "état": s["state"],
                "quand": quand,
                "quoi": quoi,
                "vers": s["destination"].as_str().unwrap_or(""),
                "prochain": s["next_run"].as_str().unwrap_or(""),
            })
        })
        .collect();
    output::table(&rows)
}

/// Colonne « trousseau » de `penelope mcp list` : un serveur distant n'a pas de processus
/// local, donc rien à dire (issue #122).
pub(super) fn keychain_cell(s: &Value) -> &'static str {
    match (s["transport"].as_str(), s["keychain"].as_bool()) {
        (Some("stdio"), Some(true)) => "ouvert",
        (Some("stdio"), _) => "fermé",
        _ => "",
    }
}

/// Le profil actif, les autres, la table des rôles et les écarts des 24 h.
fn render_profiles(v: &Value) -> String {
    let Some(name) = v["profile"].as_str() else {
        return String::new();
    };
    let mut out = format!(
        "Profil actif : « {name} » · principal {} ({}) · garde Codex : {}\n",
        v["primary"]["label"].as_str().unwrap_or("?"),
        v["primary"]["model"].as_str().unwrap_or("?"),
        v["codex_background"].as_str().unwrap_or("deny"),
    );
    let profiles: Vec<String> = v["profiles"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    let mark = if p["active"].as_bool().unwrap_or(false) {
                        "●"
                    } else {
                        "○"
                    };
                    let derived = if p["derived"].as_bool().unwrap_or(false) {
                        " (déduit des clés d'avant 1.0.47)"
                    } else {
                        ""
                    };
                    format!("{mark} {}{derived}", p["name"].as_str().unwrap_or("?"))
                })
                .collect()
        })
        .unwrap_or_default();
    out.push_str(&format!("Profils : {}\n\nRôles\n", profiles.join(" · ")));
    for r in v["roles"].as_array().cloned().unwrap_or_default() {
        let guarded = r["guarded"]
            .as_str()
            .map(|g| format!(" → {g} sous la garde"))
            .unwrap_or_default();
        out.push_str(&format!(
            "  {} {:<16} {}{guarded} ({})\n",
            r["icon"].as_str().unwrap_or(" "),
            r["role"].as_str().unwrap_or("?"),
            r["model"].as_str().unwrap_or("aucun"),
            r["why"].as_str().unwrap_or(""),
        ));
    }
    let deviations = v["deviations"].as_array().cloned().unwrap_or_default();
    if !deviations.is_empty() {
        out.push_str("\nÉcarts en cours (24 h)\n");
        for d in deviations {
            out.push_str(&format!("  {}\n", d["text"].as_str().unwrap_or("?")));
        }
    }
    out.push('\n');
    out
}

/// `penelope model list` : profil actif, rôle par rôle le modèle effectif et sa raison,
/// écarts en cours, alias, routage, puis recherche au catalogue (#334).
pub(super) fn render_model_list(v: &Value) -> String {
    let mut out = render_profiles(v);
    out.push_str("Alias\n");
    if let Some(a) = v["aliases"].as_array() {
        out.push_str(&output::table(a));
    }
    let r = &v["routing"];
    if r.is_object() {
        let step = |k: &str| {
            format!(
                "{} ({})",
                r[k]["alias"].as_str().unwrap_or("?"),
                r[k]["model"].as_str().unwrap_or("?")
            )
        };
        out.push_str("\n\nRoutage\n");
        if r["classifier"].as_bool().unwrap_or(false) {
            out.push_str(&format!(
                "adaptatif, classifieur {}\n  simple    → {}\n  ordinaire → {}\n  difficile → {}\n",
                r["classifier_model"].as_str().unwrap_or("?"),
                step("low"),
                step("medium"),
                step("high")
            ));
            out.push_str("tout sur main : penelope config set models.routing.classifier false");
        } else {
            out.push_str(&format!(
                "fixe : tout passe par {}\nadaptatif : penelope config set models.routing.classifier true",
                step("default")
            ));
        }
        if let Some(fb) = r["fallback"].as_object().filter(|f| !f.is_empty()) {
            out.push_str("\nreplis sur panne :");
            for (from, to) in fb {
                let to: Vec<&str> = to
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                    .unwrap_or_default();
                out.push_str(&format!(" {from} → {} ;", to.join(", ")));
            }
            out.pop();
        }
    }
    if let Some(m) = v["models"].as_array().filter(|m| !m.is_empty()) {
        out.push_str("\n\nCatalogue\n");
        out.push_str(&output::table(m));
    }
    if let Some(note) = v["note"].as_str().filter(|n| !n.is_empty()) {
        out.push_str(&format!("\n\n{note}"));
    }
    out
}
