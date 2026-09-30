//! Trace des outils d'un tour (issue #222) : famille, argument principal, regroupement des
//! appels consécutifs et rendu HTML de la bulle. Aucun I/O : la boucle qui envoie et
//! modifie la bulle est dans [`live`] ; la ligne résumée du mode `resume` dans
//! [`resume`], la phrase du mode `narre` dans [`narrate`] (#273).
//!
//! ```text
//! 💻 shell_exec · echo test (×4) ✅
//! 📄 fs_read · src/main.rs ⏳
//! ```

use penelope_kernel::config::ToolTrace as Mode;
use penelope_telegram::render::escape_html;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(super) mod live;
pub(crate) mod narrate;
mod resume;

/// Longueur d'un argument affiché, sur une ligne.
const ARG_CHARS: usize = 80;
/// Longueur de l'extrait d'un résultat en `full`, sur une ligne.
const PREVIEW_CHARS: usize = 120;
/// Caractères visibles d'une bulle : marge sous les 4 096 de Telegram pour les balises
/// qui ne comptent pas et les lignes de fin.
const VISIBLE_BUDGET: usize = 3_800;

/// Famille d'un outil : son icône dans la trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Shell,
    Files,
    Git,
    Web,
    Memory,
    History,
    Planning,
    Workflows,
    Jobs,
    SubAgents,
    Channel,
    Skills,
    Images,
    Himself,
    Tools,
    Mcp,
}

impl Family {
    pub(crate) fn icon(self) -> &'static str {
        match self {
            Family::Shell => "💻",
            Family::Files => "📄",
            Family::Git => "🔀",
            Family::Web => "🌐",
            Family::Memory => "🧠",
            Family::History => "📜",
            Family::Planning => "🗓",
            Family::Workflows => "⚙️",
            // Pas ⏳ : c'est le statut d'un appel en cours.
            Family::Jobs => "🧵",
            Family::SubAgents => "🤖",
            Family::Channel => "💬",
            Family::Skills => "🧩",
            Family::Images => "🎨",
            Family::Himself => "🪞",
            Family::Tools => "🔎",
            Family::Mcp => "🔌",
        }
    }

    /// Clés dont la première chaîne présente est l'argument principal. Les textes libres
    /// (`texte`, `content`, `prompt`, `text`, `body`) n'y sont jamais.
    fn keys(self, name: &str) -> &'static [&'static str] {
        match (self, name) {
            (Family::Shell, _) => &["command"],
            (Family::Files, "fs_search") => &["pattern", "path"],
            (Family::Files, "artifact_read") => &["id"],
            (Family::Files, _) => &["path"],
            (Family::Git, _) => &["message", "url", "branch", "name", "path"],
            (Family::Web, _) => &["url"],
            (Family::Memory, _) => &["query", "slug", "uid", "key"],
            (Family::History, _) => &["query", "node_id", "question"],
            (Family::Planning, _) => &["spec", "id"],
            (Family::Workflows, _) => &["id", "run_id"],
            (Family::Jobs, _) => &["job"],
            (Family::SubAgents, _) => &["kind"],
            (Family::Channel, "send_file") => &["path"],
            (Family::Channel, _) => &[],
            (Family::Skills, _) => &["query", "name"],
            (Family::Images, _) => &["path"],
            (Family::Himself, _) => &["section", "query", "path"],
            (Family::Tools, _) => &["query", "name"],
            (Family::Mcp, _) => &["command", "path", "url", "query", "id"],
        }
    }
}

/// Famille d'un outil. `None` : un outil natif que la table ne connaît pas encore (un
/// test parcourt le catalogue pour qu'il n'y en ait pas).
pub(crate) fn family_of(name: &str) -> Option<Family> {
    if name.starts_with("mcp__") {
        return Some(Family::Mcp);
    }
    Some(match name {
        "shell_exec" => Family::Shell,
        "fs_read" | "fs_list" | "fs_write" | "fs_edit" | "fs_search" | "artifact_read" => {
            Family::Files
        }
        "git_status" | "git_diff" | "git_branch" | "git_commit" | "git_clone" | "git_push" => {
            Family::Git
        }
        "http_fetch" => Family::Web,
        "mem_search" | "mem_get" | "mem_neighbors" | "mem_note" | "mem_remember" | "mem_forget"
        | "session_notes" | "session_metadata" => Family::Memory,
        "history_grep" | "history_describe" | "history_expand" | "history_expand_query" => {
            Family::History
        }
        "schedule_create" | "schedule_list" | "schedule_delete" | "schedule_move" | "time_now"
        | "intent_create" | "intent_list" | "intent_cancel" => Family::Planning,
        "workflow_list" | "workflow_describe" | "workflow_plan" | "workflow_start"
        | "workflow_status" | "workflow_control" | "workflow_author" | "step_done"
        | "return_value" => Family::Workflows,
        "job_list" | "job_status" | "job_wait" | "job_cancel" => Family::Jobs,
        "sub_agent_spawn" => Family::SubAgents,
        "send_message" | "send_voice" | "send_file" | "ask_user" => Family::Channel,
        "skill_search" | "skill_load" | "skill_propose" | "skill_patch" => Family::Skills,
        "image_inspect" | "image_generate" => Family::Images,
        "self_status" | "self_docs" | "env_explore" | "config_set" => Family::Himself,
        "tool_search" | "tool_describe" => Family::Tools,
        _ => return None,
    })
}

/// L'outil réellement appelé : `tool_call` porte `{name, args}` (ou `args_json`), il est
/// déballé avant tout le reste.
pub(crate) fn unwrap_call(name: &str, args: &Value) -> (String, Value) {
    if name != "tool_call" {
        return (name.to_string(), args.clone());
    }
    let inner = args["name"].as_str().unwrap_or("tool_call").to_string();
    let object = args.get("args").filter(|v| v.is_object()).cloned();
    let parsed = || {
        args.get("args_json")
            .or_else(|| args.get("args"))
            .and_then(|v| v.as_str())
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
    };
    (inner, object.or_else(parsed).unwrap_or(Value::Null))
}

/// Nom affiché : l'outil natif tel quel, un outil MCP en `serveur · outil`.
fn display_name(name: &str) -> String {
    match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        Some((server, tool)) => format!("{server} · {tool}"),
        None => name.to_string(),
    }
}

/// Argument principal d'un appel déjà déballé : la commande, le chemin, la requête…
/// Brut : chaque affichage le met sur une ligne et le coupe à sa largeur.
pub(crate) fn main_arg(name: &str, args: &Value) -> Option<String> {
    let keys: &[&str] = match family_of(name) {
        Some(f) => f.keys(name),
        None => &["command", "path", "query", "url", "name", "id"],
    };
    let found = keys
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.trim().is_empty())
        .map(String::from);
    match name {
        "http_fetch" => {
            let method = args["method"].as_str().unwrap_or("GET");
            found.map(|u| {
                if method.eq_ignore_ascii_case("GET") {
                    u
                } else {
                    format!("{} {u}", method.to_uppercase())
                }
            })
        }
        "workflow_control" => match (args["op"].as_str(), found) {
            (Some(op), Some(id)) => Some(format!("{op} {id}")),
            (op, id) => id.or(op.map(String::from)),
        },
        _ => found,
    }
}

/// Un texte sur une ligne, coupé à `max` caractères.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    } else {
        s
    }
}

/// Appels consécutifs d'un même outil sur le même argument.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Group {
    /// Nom porté par l'événement (`tool_call` compris) : les résultats s'apparient dessus.
    raw: String,
    icon: String,
    name: String,
    /// Argument principal caviardé, sur une ligne.
    arg: Option<String>,
    count: u32,
    ok: u32,
    err: u32,
    /// Appels sans résultat encore.
    open: u32,
    /// Appels restés sans résultat à la fin du tour.
    lost: u32,
    /// Résultat sans appel annoncé : refusé ou invalide avant d'être exécuté.
    refused: bool,
    /// Extrait caviardé du dernier résultat.
    preview: Option<String>,
}

impl Group {
    fn status(&self, unknown: bool) -> String {
        if self.refused {
            return String::new();
        }
        if self.open > 0 {
            return "⏳".into();
        }
        if self.lost > 0 {
            return if unknown { "❔" } else { "⏹" }.into();
        }
        match (self.ok, self.err) {
            (_, 0) => "✅".into(),
            (0, _) => "❌".into(),
            (_, e) => format!("❌ {e}/{}", self.count),
        }
    }

    /// La ligne du groupe : (caractères visibles, HTML).
    fn render(&self, mode: Mode, group_chat: bool, unknown: bool) -> (usize, String) {
        let icon = if self.refused {
            "🚫"
        } else {
            self.icon.as_str()
        };
        let mut plain = format!("{icon} {}", self.name);
        let mut html = format!("{icon} {}", escape_html(&self.name));
        let show_arg = mode == Mode::Full || !group_chat;
        if let Some(a) = self.arg.as_ref().filter(|_| show_arg) {
            plain.push_str(&format!(" · {a}"));
            html.push_str(&format!(" · <code>{}</code>", escape_html(a)));
        }
        if self.count > 1 {
            let n = format!(" (×{})", self.count);
            plain.push_str(&n);
            html.push_str(&n);
        }
        let status = self.status(unknown);
        if !status.is_empty() {
            plain.push_str(&format!(" {status}"));
            html.push_str(&format!(" {status}"));
        }
        if mode == Mode::Full
            && !group_chat
            && let Some(p) = &self.preview
        {
            plain.push_str(&format!("\n   ↳ {p}"));
            html.push_str(&format!("\n   ↳ <i>{}</i>", escape_html(p)));
        }
        (plain.chars().count(), html)
    }
}

/// La trace d'un tour : ses groupes, dans l'ordre des appels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Trace {
    groups: Vec<Group>,
    /// Des événements ont été perdus (le bus a débordé) : un appel resté ouvert est
    /// inconnu, pas arrêté.
    incomplete: bool,
    /// Le daemon a redémarré en plein tour.
    interrupted: bool,
}

impl Trace {
    /// Un appel annoncé. Il ne rejoint que le **dernier** groupe : deux `fs_read` du même
    /// fichier séparés par autre chose font deux lignes.
    pub(crate) fn call(&mut self, raw: &str, args: &Value) {
        let (name, inner) = unwrap_call(raw, args);
        let arg =
            main_arg(&name, &inner).map(|a| one_line(&penelope_observe::redact(&a), ARG_CHARS));
        let icon = family_of(&name).map_or("⚙️", Family::icon).to_string();
        let name = display_name(&name);
        if let Some(last) = self.groups.last_mut()
            && !last.refused
            && last.raw == raw
            && last.name == name
            && last.arg == arg
        {
            last.count += 1;
            last.open += 1;
            return;
        }
        self.groups.push(Group {
            raw: raw.to_string(),
            icon,
            name,
            arg,
            count: 1,
            open: 1,
            ..Default::default()
        });
    }

    /// Un résultat. `ToolResult` n'a pas d'identifiant d'appel : il va au plus ancien
    /// appel ouvert du même nom, ce qui est juste parce qu'un lot parallèle émet ses N
    /// appels puis ses N résultats dans le même ordre.
    pub(crate) fn result(&mut self, raw: &str, ok: bool, preview: &str) {
        let preview = Some(one_line(&penelope_observe::redact(preview), PREVIEW_CHARS))
            .filter(|p| !p.is_empty());
        if let Some(g) = self.groups.iter_mut().find(|g| g.open > 0 && g.raw == raw) {
            g.open -= 1;
            if ok {
                g.ok += 1;
            } else {
                g.err += 1;
            }
            g.preview = preview;
            return;
        }
        let name = display_name(&unwrap_call(raw, &Value::Null).0);
        if let Some(last) = self.groups.last_mut()
            && last.refused
            && last.name == name
        {
            last.count += 1;
            return;
        }
        self.groups.push(Group {
            raw: raw.to_string(),
            name,
            count: 1,
            err: 1,
            refused: true,
            preview,
            ..Default::default()
        });
    }

    /// Fin du tour : un appel encore ouvert ne rendra plus rien.
    pub(crate) fn finish(&mut self) {
        for g in &mut self.groups {
            g.lost += g.open;
            g.open = 0;
        }
    }

    /// Des événements ont été perdus en route.
    pub(crate) fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    /// Redémarrage du daemon en plein tour.
    pub(crate) fn interrupt(&mut self) {
        self.incomplete = true;
        self.interrupted = true;
        self.finish();
    }

    /// Les lignes de fin : des événements perdus, un redémarrage en plein tour.
    fn tail(&self) -> Vec<String> {
        let mut tail: Vec<String> = Vec::new();
        if self.incomplete && !self.interrupted {
            tail.push("❔ trace incomplète".into());
        }
        if self.interrupted {
            tail.push("⏹ interrompu par un redémarrage".into());
        }
        tail
    }

    /// La bulle du mode `narre` : la phrase du modèle, échappée, puis les lignes de fin.
    pub(crate) fn narrated(&self, phrase: &str) -> String {
        let mut out = vec![escape_html(phrase)];
        out.extend(self.tail());
        out.join("\n")
    }

    /// La bulle, en HTML. En `resume` et `narre` (sans phrase), la ligne résumée. Sinon la
    /// liste : au-delà du budget, les premiers groupes, une ligne qui dit combien d'appels
    /// manquent, puis le groupe **courant** : l'appel en cours reste visible. Aucune ligne
    /// ne commence par ❌ (`shorten_failure` couperait la bulle).
    pub(crate) fn render(&self, mode: Mode, group_chat: bool) -> String {
        if mode.summarises() {
            let mut out = vec![self.resume(group_chat)];
            out.extend(self.tail());
            return out.join("\n");
        }
        let lines: Vec<(usize, String)> = self
            .groups
            .iter()
            .map(|g| g.render(mode, group_chat, self.incomplete))
            .collect();
        let tail = self.tail();
        let total: usize = lines.iter().map(|(n, _)| n + 1).sum();
        let mut out: Vec<String> = if total <= VISIBLE_BUDGET || lines.len() < 2 {
            lines.into_iter().map(|(_, h)| h).collect()
        } else {
            let (last_len, last) = lines.last().cloned().unwrap_or_default();
            let room = VISIBLE_BUDGET.saturating_sub(last_len + 40);
            let (mut used, mut kept) = (0, Vec::new());
            for (n, h) in &lines[..lines.len() - 1] {
                if used + n + 1 > room {
                    break;
                }
                used += n + 1;
                kept.push(h.clone());
            }
            let skipped: u32 = self.groups[kept.len()..self.groups.len() - 1]
                .iter()
                .map(|g| g.count)
                .sum();
            kept.push(format!("… et {skipped} appel(s) de plus"));
            kept.push(last);
            kept
        };
        out.extend(tail);
        out.join("\n")
    }
}

#[cfg(test)]
mod tests;
