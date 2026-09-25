//! Rendu texte des cartes et des listes : approbation, statut, planifications, MCP, routage.

use super::*;

/// Carte d'approbation d'un appel d'outil, lisible par le propriétaire (issue #116).
pub(crate) struct ApprovalCard {
    /// Ce que Pénélope cherche à faire, en une phrase.
    pub intention: String,
    /// L'action telle qu'elle sera faite : la commande exacte, ou l'outil et ses valeurs.
    pub action: String,
    /// Une ligne : qualificatifs, classe de risque, politique.
    pub details: String,
    /// Portée d'une règle « Toujours » : les familles qu'un clic réglerait
    /// (`« yt-dlp », « ffmpeg » (réseau)`). `None` : aucune règle n'est possible, et le
    /// bouton n'est pas posé du tout — la carte dit ce qui l'empêche (issues #141, #150).
    pub always: Option<String>,
}

/// Compose la carte d'une demande d'approbation.
pub(crate) fn approval_card(a: &ApprovalRequest) -> ApprovalCard {
    let args = penelope_agent::without_intention(&a.payload["arguments"]);
    let intention = a.payload["why"]
        .as_str()
        .filter(|w| !w.trim().is_empty())
        .map(String::from)
        .unwrap_or_else(|| format!("Pénélope veut utiliser `{}`.", a.subject));
    let (server, tool) = match penelope_agent::server_of(&a.subject) {
        Some(srv) => {
            let tool = a
                .subject
                .strip_prefix(&format!("mcp__{srv}__"))
                .unwrap_or(&a.subject)
                .to_string();
            (Some(srv), tool)
        }
        None => (None, a.subject.clone()),
    };
    let mut quals: Vec<String> = Vec::new();
    let action = match (a.subject.as_str(), args["command"].as_str()) {
        ("shell_exec", Some(command)) => {
            if penelope_executor::executor::wants_network(&a.subject, &a.payload["arguments"]) {
                quals.push("réseau".into());
            }
            if args["output"].as_str() == Some("full") {
                quals.push("sortie complète".into());
            }
            if let Some(cwd) = args["cwd"].as_str() {
                quals.push(format!("dans `{cwd}`"));
            }
            if let Some(ms) = args["timeout_ms"].as_u64() {
                quals.push(format!("délai {} s", ms / 1_000));
            }
            format!("```\n{}\n```", command.replace("```", "ʼʼʼ"))
        }
        _ => {
            let fields: Vec<String> = args
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| {
                            let v = match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            let v: String = if v.chars().count() > 120 {
                                format!("{}…", v.chars().take(119).collect::<String>())
                            } else {
                                v
                            };
                            format!("{k} = {v}")
                        })
                        .collect()
                })
                .unwrap_or_default();
            if fields.is_empty() {
                format!("`{tool}`")
            } else {
                format!("`{tool}` : {}", fields.join(" · "))
            }
        }
    };
    if let Some(srv) = server {
        quals.push(format!("serveur `{srv}`"));
    }
    // La demande est stockée rédigée : la carte dit combien de valeurs sont masquées,
    // la commande exécutée, elle, reste entière (issue #134).
    let masked = args
        .to_string()
        .matches(penelope_observe::redact::MASK)
        .count();
    if masked > 0 {
        quals.push(format!("🔒 {masked} valeur(s) masquée(s)"));
    }
    quals.push(format!("classe {}", a.risk.as_str()));
    if let Some(reason) = a.payload["reason"].as_str().filter(|r| !r.is_empty()) {
        quals.push(reason.to_string());
    }
    // Le libellé nomme **toutes** les familles qu'un clic autoriserait : une liste
    // `a && b` en crée une par famille, et le propriétaire doit les voir avant (#150).
    let patterns = penelope_agent::arg_patterns(&a.subject, a.payload.get("arguments"));
    let describe = |p: &Value| {
        use penelope_hitl::policy::{CMD_PREFIX_OP, ORIGIN_OP, PATH_PREFIX_OP};
        let op = |k: &str, op: &str| p[k][op].as_str().map(String::from);
        if let Some(family) = op("command", CMD_PREFIX_OP) {
            format!("« {family} »")
        } else if let Some(dir) = op("path", PATH_PREFIX_OP) {
            if dir.is_empty() {
                "ce répertoire".into()
            } else {
                format!("`{dir}`")
            }
        } else if let Some(host) = op("url", ORIGIN_OP) {
            host
        } else {
            penelope_hitl::policy::describe_pattern(p)
        }
    };
    let always = (!patterns.is_empty()).then(|| {
        let network = patterns.iter().any(|p| p["network"] == Value::Bool(true));
        let names: Vec<String> = patterns.iter().map(&describe).collect();
        format!(
            "{}{}",
            names.join(", "),
            if network { " (réseau)" } else { "" }
        )
    });
    // Une commande composée n'a pas de famille : « Toujours » l'autoriserait une fois,
    // sans créer de règle (#111). La carte le dit avant le clic (#141).
    if penelope_agent::always_creates_no_rule(&a.subject, a.payload.get("arguments")) {
        // Dire *ce qui* empêche la règle, et par où sortir : une commande par appel
        // (issue #150). Sans cela, « pas de règle possible » se lit comme une fatalité.
        let why = a.payload["arguments"]["command"]
            .as_str()
            .and_then(penelope_hitl::cmdline::why_composed)
            .unwrap_or_else(|| "commande composée".into());
        quals.push(format!(
            "pas de règle possible ({why}) — demande-lui une commande par appel"
        ));
    }
    ApprovalCard {
        intention,
        action,
        details: quals.join(" · "),
        always,
    }
}

/// État d'une connexion de fournisseur à compte, en une bulle (issue #142).
pub(super) fn codex_status_text(v: &Value) -> String {
    let status = &v["status"];
    if status.is_null() {
        return "🔌 Aucun compte ChatGPT connecté. `/model auth codex` pour le faire.".into();
    }
    if let Some(why) = status["disconnected"].as_str() {
        return format!(
            "🔌 Compte ChatGPT déconnecté ({why}). `/model auth codex` pour reconnecter."
        );
    }
    format!(
        "✅ Compte ChatGPT connecté : plan {}, compte {}{}.",
        shown(&status["plan"]),
        shown(&status["account"]),
        if v["enabled"] == Value::Bool(true) {
            ""
        } else {
            " — fournisseur éteint (`providers.codex.enabled`)"
        }
    )
}

/// Mention des messages en attente abandonnés par la fermeture d'une session.
pub(super) fn cancelled_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n⏹ 1 message en attente dans la session a été abandonné.".into(),
        n => format!("\n⏹ {n} messages en attente dans la session ont été abandonnés."),
    }
}

/// Mention du travail que la session quittée poursuit en fond (issue #112).
pub(super) fn background_note(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "\n⏳ L'ancienne session continue en fond (1 tour en file) : sa réponse t'attend \
              à ton retour."
            .into(),
        n => format!(
            "\n⏳ L'ancienne session continue en fond ({n} tours en file) : ses réponses \
             t'attendent à ton retour."
        ),
    }
}

/// Montant en dollars à la française : `5,02`, `20`.
pub(super) fn fmt_usd(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    s.replace('.', ",")
}

/// Taille du contexte d'une session, seuil de la compaction de fond et dernière
/// compaction (issue #40).
pub(super) fn context_line(view: &Value) -> String {
    let prompt = view["last_prompt_tokens"]
        .as_i64()
        .map(|p| format!("{} k tokens au dernier appel", p / 1000))
        .unwrap_or_else(|| "aucun appel encore".into());
    let last = view["last_compaction"]
        .as_str()
        .map(|t| t.chars().take(16).collect::<String>().replace('T', " "))
        .unwrap_or_else(|| "jamais".into());
    let failing = match view["compaction_failures"].as_u64().unwrap_or(0) {
        0 => String::new(),
        n => format!(
            " ; ⚠️ le résumé échoue ({n} fois de suite){}",
            view["cost_per_turn_usd"]
                .as_f64()
                .map(|c| format!(", {c:.3} $ par tour"))
                .unwrap_or_default()
        ),
    };
    format!(
        "Contexte : {prompt}, compaction de fond vers {} k, dernière compaction : {last}{failing}",
        view["background_compaction_at"].as_u64().unwrap_or(0) / 1000
    )
}

/// `/schedules` : un déclencheur par ligne, prochain passage et cible.
pub(super) fn schedules_text(v: &Value) -> String {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "Aucun déclencheur planifié.".into();
    }
    let mut t = String::from("**Déclencheurs**\n\n");
    for sc in &list {
        let state = sc["state"].as_str().unwrap_or("?");
        let icon = match state {
            "active" => "🟢",
            "paused" => "⏸",
            "done" => "✅",
            _ => "⚪",
        };
        let spec = &sc["spec"];
        let when = match sc["kind"].as_str().unwrap_or("?") {
            "cron" => format!(
                "cron `{}`{}",
                spec["expr"].as_str().unwrap_or("?"),
                if spec["once"].as_bool() == Some(true) {
                    " (une fois)"
                } else {
                    ""
                }
            ),
            "interval" | "mcp_poll" => format!(
                "{} toutes les {} min",
                sc["kind"].as_str().unwrap_or("?"),
                spec["every_ms"].as_u64().unwrap_or(0) / 60_000
            ),
            "watch_file" => format!("fichier `{}`", spec["path"].as_str().unwrap_or("?")),
            other => format!("{other} `{}`", spec["event"].as_str().unwrap_or("?")),
        };
        let target = &sc["target"];
        let what = match target["type"].as_str().unwrap_or("?") {
            "notify" => target["template"].as_str().unwrap_or("").to_string(),
            "prompt" => target["prompt"].as_str().unwrap_or("").to_string(),
            "workflow" => format!("workflow {}", target["workflowId"].as_str().unwrap_or("?")),
            other => other.to_string(),
        };
        t.push_str(&format!(
            "{icon} `{}` · {when} · {}\n",
            sc["id"].as_str().unwrap_or("?"),
            what.chars().take(80).collect::<String>()
        ));
        if let Some(to) = sc["destination"].as_str() {
            t.push_str(&format!("   ↳ vers : {to}\n"));
        }
        if let Some(next) = sc["next_run"].as_str().filter(|_| state == "active") {
            t.push_str(&format!("   ↳ prochain : {next}\n"));
        }
        if let Some(e) = sc["last_error"].as_str() {
            t.push_str(&format!(
                "   ↳ erreur : {}\n",
                e.chars().take(160).collect::<String>()
            ));
        }
    }
    t.push_str(
        "\n`/schedules pause|resume|rm|run <id>` ; `/schedules ici <id>` la fait livrer ici",
    );
    t
}

pub(super) fn mcp_state_icon(state: &str) -> &'static str {
    match state {
        "ready" => "🟢",
        "degraded" => "🟡",
        "connecting" => "🔄",
        "failed" => "🔴",
        "disabled" => "⏸",
        "auth_required" => "🔐",
        _ => "⚪",
    }
}

/// `/mcp` : un serveur par ligne, puis les déclarations invalides.
pub(super) fn mcp_list_text(v: &Value) -> String {
    let servers = v["servers"].as_array().cloned().unwrap_or_default();
    let mut t = if servers.is_empty() {
        "Aucun serveur MCP déclaré.".to_string()
    } else {
        let mut t = String::from("**Serveurs MCP**\n\n");
        for srv in &servers {
            let state = srv["state"].as_str().unwrap_or("?");
            let label = match state {
                "configured" => "démarre au premier appel",
                "ready" => "prêt",
                "degraded" => "dégradé",
                "connecting" => "connexion",
                "failed" => "en panne",
                "disabled" => "désactivé",
                "auth_required" => "autorisation requise",
                other => other,
            };
            t.push_str(&format!(
                "{} `{}` · {} outil(s) · {label}\n",
                mcp_state_icon(state),
                srv["name"].as_str().unwrap_or("?"),
                shown(&srv["tools"])
            ));
            if let Some(e) = srv["last_error"].as_str().filter(|_| state != "ready") {
                t.push_str(&format!(
                    "   ↳ {}\n",
                    e.chars().take(200).collect::<String>()
                ));
            }
        }
        t
    };
    for bad in v["invalid"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "\n⚠️ `{}` : {}",
            bad["file"].as_str().unwrap_or("?"),
            bad["error"].as_str().unwrap_or("?")
        ));
    }
    t.push_str("\n\nDétail : `/mcp <serveur>` ; `/mcp restart|logs|test <serveur>`");
    t
}

/// `/mcp <serveur>` : état, outils, dernière erreur.
pub(super) fn mcp_show_text(v: &Value) -> String {
    let st = &v["status"];
    let name = st["name"].as_str().unwrap_or("?");
    let state = st["state"].as_str().unwrap_or("?");
    let mut t = format!(
        "{} **{name}** · {} · {} outil(s) · {} appel(s), {} erreur(s)\n",
        mcp_state_icon(state),
        state,
        shown(&st["tool_count"]),
        shown(&st["calls"]),
        shown(&st["errors"])
    );
    if let Some(p) = st["protocol"].as_str() {
        t.push_str(&format!(
            "Protocole {p} · transport {}\n",
            st["transport"].as_str().unwrap_or("?")
        ));
    }
    if let Some(e) = st["last_error"].as_str() {
        t.push_str(&format!(
            "Dernière erreur : {}\n",
            e.chars().take(300).collect::<String>()
        ));
    }
    let tools = v["tools"].as_array().cloned().unwrap_or_default();
    if !tools.is_empty() {
        t.push('\n');
        for tool in tools.iter().take(25) {
            t.push_str(&format!(
                "- `{}` ({})\n",
                tool["title"].as_str().unwrap_or("?"),
                tool["risk"].as_str().unwrap_or("?")
            ));
        }
        if tools.len() > 25 {
            t.push_str(&format!("… et {} autres\n", tools.len() - 25));
        }
    }
    t
}

/// Alias et routage, tels que `model.list` les décrit./// Alias et routage, tels que `model.list` les décrit.
pub(super) fn routing_text(v: &Value) -> String {
    let mut t = String::from("**Alias**\n\n");
    for a in v["aliases"].as_array().cloned().unwrap_or_default() {
        t.push_str(&format!(
            "- `{}` → `{}`\n",
            a["alias"].as_str().unwrap_or("?"),
            a["model"].as_str().unwrap_or("?")
        ));
    }
    let r = &v["routing"];
    if r.is_object() {
        let step = |k: &str| {
            format!(
                "`{}` (`{}`)",
                r[k]["alias"].as_str().unwrap_or("?"),
                r[k]["model"].as_str().unwrap_or("?")
            )
        };
        t.push_str("\n**Routage**\n\n");
        if r["classifier"].as_bool().unwrap_or(false) {
            t.push_str(&format!(
                "Adaptatif (classifieur `{}`) :\n- simple → {}\n- ordinaire → {}\n- difficile → {}\n",
                r["classifier_model"].as_str().unwrap_or("?"),
                step("low"),
                step("medium"),
                step("high"),
            ));
            t.push_str("Tout sur `main` : `/model auto off`\n");
        } else {
            t.push_str(&format!(
                "Fixe : tout passe par {} (adaptatif : `/model auto on`)\n",
                step("default")
            ));
        }
        if let Some(fb) = r["fallback"].as_object().filter(|f| !f.is_empty()) {
            let chains: Vec<String> = fb
                .iter()
                .map(|(from, to)| {
                    let to: Vec<&str> = to
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    format!("`{from}` → `{}`", to.join("`, `"))
                })
                .collect();
            t.push_str(&format!("Replis sur panne : {}\n", chains.join(" ; ")));
        }
    }
    t
}

/// Rend une valeur RPC en Markdown lisible dans une conversation.
pub fn render_value(v: &Value) -> String {
    fn scalar(v: &Value) -> String {
        match v {
            Value::String(s) => {
                let s: String = s.chars().take(120).collect();
                s
            }
            Value::Null => "—".into(),
            other => {
                let s = other.to_string();
                s.chars().take(120).collect()
            }
        }
    }
    let out = match v {
        Value::Array(items) if items.is_empty() => "(vide)".to_string(),
        Value::Array(items) => {
            let mut s = String::new();
            for it in items.iter().take(40) {
                match it {
                    Value::Object(o) => {
                        let line = o
                            .iter()
                            .filter(|(_, v)| !v.is_null() && !v.is_object() && !v.is_array())
                            .take(4)
                            .map(|(k, v)| format!("{k} : {}", scalar(v)))
                            .collect::<Vec<_>>()
                            .join(" · ");
                        s.push_str(&format!("- {line}\n"));
                    }
                    other => s.push_str(&format!("- {}\n", scalar(other))),
                }
            }
            if items.len() > 40 {
                s.push_str(&format!("… et {} de plus\n", items.len() - 40));
            }
            s
        }
        Value::Object(o) => {
            let mut s = String::new();
            for (k, v) in o {
                match v {
                    Value::Object(_) | Value::Array(_) => {
                        let compact = v.to_string();
                        let compact: String = compact.chars().take(200).collect();
                        s.push_str(&format!("**{k}** : `{compact}`\n"));
                    }
                    other => s.push_str(&format!("**{k}** : {}\n", scalar(other))),
                }
            }
            s
        }
        other => scalar(other),
    };
    out.chars().take(3_500).collect()
}
