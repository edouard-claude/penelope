//! Mode `resume` de la trace des outils (issue #273) : la liste des appels tient en une
//! ligne, par famille et par verbe, sans appel de modèle. C'est aussi le repli du mode
//! `narre` quand le modèle du rôle `trace` manque, tarde ou déraille.
//!
//! ```text
//! 📄 6 lectures, 7 recherches · 💻 cargo test en cours
//! 📄 6 lectures, 7 recherches · 💻 cargo test ✅
//! ```

use super::{Family, Group, Trace, family_of, one_line, unwrap_call};
use penelope_telegram::render::escape_html;

/// Largeur d'une commande dans le résumé : la ligne doit rester une ligne.
const COMMAND_CHARS: usize = 40;

/// Verbe d'un outil, au singulier et au pluriel : « 3 lectures », « 1 commit ».
fn verb(family: Option<Family>, name: &str) -> (String, String) {
    let (s, p) = match (family, name) {
        (Some(Family::Files), "fs_search") => ("recherche", "recherches"),
        (Some(Family::Files), "fs_write" | "fs_edit") => ("écriture", "écritures"),
        (Some(Family::Files), _) => ("lecture", "lectures"),
        (Some(Family::Git), "git_commit") => ("commit", "commits"),
        (Some(Family::Git), "git_push") => ("envoi", "envois"),
        (Some(Family::Git), "git_clone") => ("clone", "clones"),
        (Some(Family::Git), _) => ("lecture git", "lectures git"),
        (Some(Family::Web), _) => ("requête", "requêtes"),
        (
            Some(Family::Memory),
            "mem_note" | "mem_remember" | "session_notes" | "session_metadata",
        ) => ("note", "notes"),
        (Some(Family::Memory), "mem_forget") => ("oubli", "oublis"),
        (Some(Family::Memory), _) => ("rappel", "rappels"),
        (Some(Family::History), _) => ("relecture", "relectures"),
        (Some(Family::Planning), "time_now") => ("heure", "heures"),
        (Some(Family::Planning), n) if n.starts_with("intent_") => ("intention", "intentions"),
        (Some(Family::Planning), _) => ("planification", "planifications"),
        (Some(Family::Workflows), _) => ("workflow", "workflows"),
        (Some(Family::Jobs), _) => ("job", "jobs"),
        (Some(Family::SubAgents), _) => ("sous-agent", "sous-agents"),
        (Some(Family::Channel), _) => ("message", "messages"),
        (Some(Family::Skills), _) => ("skill", "skills"),
        (Some(Family::Images), _) => ("image", "images"),
        (Some(Family::Himself), _) => ("consultation", "consultations"),
        (Some(Family::Tools), _) => ("recherche d'outil", "recherches d'outil"),
        (Some(Family::Mcp), _) => {
            // `serveur · outil` : le serveur suffit au résumé.
            let server = name
                .strip_prefix("mcp__")
                .and_then(|r| r.split_once("__"))
                .map_or("MCP", |(s, _)| s);
            return (format!("appel {server}"), format!("appels {server}"));
        }
        (Some(Family::Shell), _) | (None, _) => ("appel", "appels"),
    };
    (s.to_string(), p.to_string())
}

/// Les appels d'une famille : ses verbes comptés, ses commandes, un appel encore ouvert.
#[derive(Default)]
struct Part {
    icon: String,
    /// (singulier, pluriel, nombre), dans l'ordre d'apparition.
    verbs: Vec<(String, String, u32)>,
    /// Commandes distinctes (famille shell), caviardées, sur une ligne.
    commands: Vec<String>,
    count: u32,
    open: bool,
}

impl Part {
    fn add(&mut self, g: &Group, name: &str, family: Option<Family>) {
        self.count += g.count;
        self.open |= g.open > 0;
        if family == Some(Family::Shell) {
            if let Some(c) = &g.arg {
                let c = one_line(c, COMMAND_CHARS);
                if !self.commands.contains(&c) {
                    self.commands.push(c);
                }
            }
            return;
        }
        let (s, p) = verb(family, name);
        match self.verbs.iter_mut().find(|(a, _, _)| *a == s) {
            Some(v) => v.2 += g.count,
            None => self.verbs.push((s, p, g.count)),
        }
    }

    /// `📄 6 lectures, 7 recherches`, `💻 <code>cargo test</code>`, `💻 3 commandes`.
    fn render(&self, group_chat: bool) -> String {
        let mut out = self.icon.clone();
        if self.verbs.is_empty() {
            match self.commands.as_slice() {
                [one] if !group_chat => {
                    out.push_str(&format!(" <code>{}</code>", escape_html(one)));
                    if self.count > 1 {
                        out.push_str(&format!(" (×{})", self.count));
                    }
                }
                _ => out.push_str(&format!(" {}", plural(self.count, "commande", "commandes"))),
            }
        } else {
            let words: Vec<String> = self
                .verbs
                .iter()
                .map(|(s, p, n)| plural(*n, s, p))
                .collect();
            out.push(' ');
            out.push_str(&words.join(", "));
        }
        if self.open {
            out.push_str(" en cours");
        }
        out
    }
}

fn plural(n: u32, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("1 {singular}")
    } else {
        format!("{n} {plural}")
    }
}

impl Trace {
    /// La ligne du mode `resume` : une part par famille dans l'ordre d'apparition, puis
    /// l'état final quand plus rien ne tourne (`✅`, `❌ 2 échecs`, `🚫 1 refusé`,
    /// `⏹ 1 sans réponse`, `❔` si des événements ont été perdus).
    pub(crate) fn resume(&self, group_chat: bool) -> String {
        let mut parts: Vec<Part> = Vec::new();
        let (mut err, mut refused, mut lost) = (0u32, 0u32, 0u32);
        for g in &self.groups {
            if g.refused {
                refused += g.count;
                continue;
            }
            err += g.err;
            lost += g.lost;
            let (name, _) = unwrap_call(&g.raw, &serde_json::Value::Null);
            let family = family_of(&name);
            let part = match parts.iter_mut().find(|p| p.icon == g.icon) {
                Some(p) => p,
                None => {
                    parts.push(Part {
                        icon: g.icon.clone(),
                        ..Default::default()
                    });
                    parts.last_mut().expect("part poussée")
                }
            };
            part.add(g, &name, family);
        }
        let mut pieces: Vec<String> = parts.iter().map(|p| p.render(group_chat)).collect();
        let running = parts.iter().any(|p| p.open);
        if !running && !parts.is_empty() {
            let mut status = Vec::new();
            if err > 0 {
                status.push(format!("❌ {}", plural(err, "échec", "échecs")));
            }
            if refused > 0 {
                status.push(format!("🚫 {}", plural(refused, "refusé", "refusés")));
            }
            if lost > 0 {
                let mark = if self.incomplete { "❔" } else { "⏹" };
                status.push(format!(
                    "{mark} {}",
                    plural(lost, "sans réponse", "sans réponse")
                ));
            }
            if status.is_empty() {
                pieces.push("✅".into());
            } else {
                pieces.extend(status);
            }
        } else if parts.is_empty() && refused > 0 {
            pieces.push(format!("🚫 {}", plural(refused, "refusé", "refusés")));
        }
        pieces.join(" · ")
    }
}
