//! Transcript du résumeur : rendu des messages et découpage en lots (§5.4).
//!
//! Sorti de `engine.rs` tel quel (épopée #208, lot E) pour que le moteur reste sous sa
//! borne de lignes.

use crate::transcript::Entry;
use penelope_llm::types::Role;

/// Au-delà, un message est échantillonné (tête et queue) pour le résumeur ; les ancres
/// sont extraites du texte complet.
pub(crate) const TOOL_RESULT_MAX_CHARS: usize = 4_000;
const MESSAGE_MAX_CHARS: usize = 12_000;

/// Met le transcript en forme pour le résumeur.
pub fn render_transcript(entries: &[Entry]) -> String {
    entries.iter().map(render_entry).collect()
}

/// Une ligne de transcript : rôle, numéro, appels d'outils, texte échantillonné au-delà
/// d'une taille raisonnable (tête et queue, le nombre de caractères élidés est dit).
pub fn render_entry(e: &Entry) -> String {
    let who = match e.message.role {
        Role::User => "UTILISATEUR",
        Role::Assistant => "ASSISTANT",
        Role::Tool => "OUTIL",
        Role::System => "SYSTÈME",
    };
    let mut s = format!("[{} #{}] ", who, e.seq);
    if !e.message.tool_calls.is_empty() {
        let names: Vec<&str> = e
            .message
            .tool_calls
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        s.push_str(&format!("(appelle {}) ", names.join(", ")));
    }
    let max = if e.message.role == Role::Tool {
        TOOL_RESULT_MAX_CHARS
    } else {
        MESSAGE_MAX_CHARS
    };
    s.push_str(&sample(&e.message.text(), max));
    s.push('\n');
    s
}

/// Tête et queue d'un texte trop long pour le résumeur.
fn sample(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max * 2 / 3).collect();
    let tail: String = text.chars().skip(n - max / 3).collect();
    format!(
        "{head}\n[… {} caractères non montrés au résumeur …]\n{tail}",
        n - head.chars().count() - tail.chars().count()
    )
}

/// Découpe les candidats en lots qui tiennent dans la fenêtre du résumeur, sans
/// jamais séparer un appel d'outils de ses résultats. Un groupe plus gros que le budget
/// forme un lot à lui seul (ses messages sont déjà échantillonnés).
pub(crate) fn plan_batches(entries: &[Entry], sizes: &[u64], budget: u64) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut start: Option<i64> = None;
    let mut end = 0i64;
    let mut acc = 0u64;
    for g in crate::transcript::group(entries) {
        let g_tokens: u64 = sizes[g.range.clone()].iter().sum();
        let (g_from, g_to) = (entries[g.range.start].seq, entries[g.range.end - 1].seq);
        if let Some(s) = start
            && acc + g_tokens > budget
        {
            out.push((s, end));
            start = None;
            acc = 0;
        }
        start.get_or_insert(g_from);
        end = g_to;
        acc += g_tokens;
    }
    if let Some(s) = start {
        out.push((s, end));
    }
    out
}
