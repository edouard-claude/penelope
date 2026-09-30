//! Rattrapage de la mémoire perdue (issue #285) : `penelope mem reclaim`.
//!
//! Deux pertes relevées sur une instance le 30/09 : des candidats rejetés « ni dit ni
//! confirmé par le propriétaire » alors qu'il les avait dits, reformulés par la relecture
//! (défaut corrigé pour l'avenir par #245, mais un rejet est définitif) ; et une fiche
//! source qui est sa parole, l'export de sa propre mémoire, dont les faits n'ont jamais
//! été proposés au tri comme tels.
//!
//! ```text
//!  rejeté « ni dit ni confirmé » ─► messages du propriétaire de son tour ou de son épisode
//!                                   ─► owner_quote retrouve la phrase ? ─► new, owner, phrase
//!                                                                 sinon ─► reste rejeté
//!  --source sources/x.md ─► une puce = un fait ─► candidat `fait`, owner, phrase = le fait
//!
//!  tout repasse par le tri nocturne, qui garde ses portes ; rien n'entre ici dans le profil
//! ```
//!
//! Un passage par clé `kv` : relancée, la commande dit ce qui a déjà été fait et n'écrit
//! rien. `--dry-run` liste sans écrire, ni candidat, ni clé, ni événement.

use penelope_app::services::Services;
use penelope_context::Entry;
use penelope_kernel::event::EventDraft;
use penelope_llm::types::Role;
use penelope_memory::grid::{NOT_ENDORSED, normalized};
use penelope_memory::owner_quote::owner_statement;
use penelope_memory::{Candidate, CandidateType, Origin};
use penelope_store::rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Component, Path};

/// Clé `kv` du passage sur les rejets.
pub const KEY_REJECTED: &str = "mem.reclaim.rejected";
/// Préfixe de la clé `kv` du passage sur une fiche, suivi de son chemin dans le vault.
pub const KEY_SOURCE: &str = "mem.reclaim.source:";
/// Préfixe de la provenance des candidats tirés d'une fiche, suivi de son chemin.
pub const SOURCE_REF: &str = "source:";
/// Importance d'un fait tiré d'une fiche : le tri décide, comme pour les autres.
const SOURCE_IMPORTANCE: u8 = 6;
/// Un fait tient en une phrase ou deux ; au-delà, le texte est scindé en phrases.
const FACT_MAX_CHARS: usize = 300;
/// Un fait dit quelque chose : en dessous, un titre ou un reste de mise en forme.
const FACT_MIN_CHARS: usize = 20;
const FACT_MIN_WORDS: usize = 4;
/// Messages relus avant celui du tour, pour retrouver la réponse finale qui le précède.
const TURN_WINDOW: i64 = 80;

/// Un candidat repassé au tri, et la phrase qui le porte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requeued {
    pub id: String,
    pub text: String,
    pub quote: String,
    pub source_ref: String,
}

/// Le passage sur les rejets « ni dit ni confirmé ».
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rejected {
    /// Candidats relus.
    pub examined: usize,
    /// Repassés au tri, phrase à l'appui.
    pub requeued: usize,
    /// Restés rejetés : phrase non retrouvée…
    pub kept: usize,
    /// … dont ceux sans message relisible (purgé, référence ou session inconnue).
    pub unreadable: usize,
    /// Horodatage du passage déjà fait : rien n'a été relu cette fois.
    pub already: Option<String>,
    pub items: Vec<Requeued>,
}

/// Le passage sur une fiche source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub file: String,
    /// Faits découpés dans la fiche.
    pub facts: usize,
    /// Candidats proposés au tri.
    pub recorded: usize,
    /// Faits écartés : déjà notés depuis cette fiche, secret non rangeable, filtre
    /// d'écriture.
    pub skipped: usize,
    pub already: Option<String>,
    pub items: Vec<String>,
}

/// Ce qu'un passage a fait, ou ferait (`dry_run`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub dry_run: bool,
    pub rejected: Rejected,
    pub source: Option<Source>,
}

impl Report {
    /// Les comptes, pour l'événement `memory.reclaimed`.
    pub fn counts(&self) -> Value {
        json!({
            "dry_run": self.dry_run,
            "rejected": {
                "examined": self.rejected.examined,
                "requeued": self.rejected.requeued,
                "kept": self.rejected.kept,
                "unreadable": self.rejected.unreadable,
                "already": self.rejected.already,
            },
            "source": self.source.as_ref().map(|x| json!({
                "file": x.file,
                "facts": x.facts,
                "recorded": x.recorded,
                "skipped": x.skipped,
                "already": x.already,
            })),
        })
    }

    /// Rendu lisible, pour la CLI.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let verb = if self.dry_run { "ferait" } else { "a fait" };
        out.push_str(&format!(
            "Rattrapage de la mémoire, ce que la passe {verb} :\n"
        ));
        let r = &self.rejected;
        match &r.already {
            Some(at) => out.push_str(&format!(
                "Rejets « ni dit ni confirmé » : déjà rattrapés le {at}, rien à relire.\n"
            )),
            None => {
                out.push_str(&format!(
                    "Rejets « ni dit ni confirmé » : {} relus, {} repassés au tri avec ta phrase, \
                     {} restent rejetés ({} sans message relisible).\n",
                    r.examined, r.requeued, r.kept, r.unreadable
                ));
                for it in &r.items {
                    out.push_str(&format!(
                        "  - « {} » ← « {} »\n",
                        short(&it.text),
                        short(&it.quote)
                    ));
                }
            }
        }
        if let Some(x) = &self.source {
            match &x.already {
                Some(at) => out.push_str(&format!(
                    "Fiche `{}` : déjà proposée au tri le {at}, rien à redoubler.\n",
                    x.file
                )),
                None => {
                    out.push_str(&format!(
                        "Fiche `{}` : {} faits, {} candidats proposés au tri comme ta parole, \
                         {} écartés.\n",
                        x.file, x.facts, x.recorded, x.skipped
                    ));
                    for it in &x.items {
                        out.push_str(&format!("  - {}\n", short(it)));
                    }
                }
            }
        }
        out.push_str(
            "Rien n'entre dans le profil ici : le tri de la nuit décide, avec ses portes.",
        );
        out
    }
}

fn short(text: &str) -> String {
    let t: String = text.chars().take(100).collect();
    if text.chars().count() > 100 {
        format!("{t}…")
    } else {
        t
    }
}

/// Le passage : les rejets, puis la fiche si une est désignée. `dry_run` : rien n'est
/// écrit, ni candidat, ni clé, ni événement.
pub async fn run(s: &Services, dry_run: bool, source: Option<&str>) -> anyhow::Result<Report> {
    // La fiche est lue d'abord : un chemin refusé n'entame pas le passage sur les rejets.
    let fiche = match source {
        Some(path) => Some(read_source(s, path)?),
        None => None,
    };
    let rejected = reclaim_rejected(s, dry_run).await?;
    let source = match fiche {
        Some((file, raw)) => Some(reclaim_source(s, dry_run, &file, &raw).await?),
        None => None,
    };
    let report = Report {
        dry_run,
        rejected,
        source,
    };
    let ran = report.rejected.already.is_none()
        || report.source.as_ref().is_some_and(|x| x.already.is_none());
    if !dry_run && ran {
        let _ = s
            .events
            .append(EventDraft::new("memory.reclaimed", report.counts()))
            .await;
    }
    Ok(report)
}

/// Valeur d'une clé de passage : l'heure et les comptes.
fn stamp(s: &Services, counts: Value) -> String {
    let mut v = counts;
    v["at"] = json!(s.clock.now_rfc3339());
    v.to_string()
}

/// L'heure d'un passage déjà fait, lue dans sa clé.
fn done_at(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v["at"].as_str().map(String::from))
        .unwrap_or_else(|| raw.to_string())
}

// ---------------------------------------------------------------- rejets

async fn reclaim_rejected(s: &Services, dry_run: bool) -> anyhow::Result<Rejected> {
    let mut out = Rejected::default();
    if let Some(done) = s.kv_get(KEY_REJECTED).await? {
        out.already = Some(done_at(&done));
        return Ok(out);
    }
    let candidates = s.candidates.rejected_for(NOT_ENDORSED).await?;
    out.examined = candidates.len();
    let mut quoted: Vec<(String, String)> = Vec::new();
    for c in &candidates {
        let messages = owner_messages(s, c).await;
        if messages.is_empty() {
            out.unreadable += 1;
            out.kept += 1;
            continue;
        }
        match messages.iter().find_map(|m| owner_statement(&c.text, m)) {
            Some(q) => {
                let quote = penelope_observe::redact::redact(&q);
                out.items.push(Requeued {
                    id: c.id.clone(),
                    text: c.text.clone(),
                    quote: quote.clone(),
                    source_ref: c.source_ref.clone().unwrap_or_default(),
                });
                quoted.push((c.id.clone(), quote));
            }
            None => out.kept += 1,
        }
    }
    out.requeued = quoted.len();
    if !dry_run {
        s.candidates.requeue_as_owner(&quoted).await?;
        let counts = json!({"examined": out.examined, "requeued": out.requeued});
        s.kv_set(KEY_REJECTED, &stamp(s, counts)).await?;
    }
    Ok(out)
}

/// Messages du propriétaire relus pour un candidat : ceux de son épisode pour une
/// référence `episode:<session>:<n>`, ceux de son tour pour `turn:<id>`. Vide si la
/// session, la référence ou les messages manquent (purge, session absente).
async fn owner_messages(s: &Services, c: &Candidate) -> Vec<String> {
    let (Some(sid), Some(source_ref)) = (&c.session_id, &c.source_ref) else {
        return Vec::new();
    };
    let entries = if let Some(rest) = source_ref.strip_prefix("episode:") {
        match rest
            .rsplit_once(':')
            .and_then(|(_, n)| n.parse::<i64>().ok())
        {
            Some(n) => s
                .context
                .history
                .load_episode(sid, n)
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        }
    } else if let Some(turn) = source_ref.strip_prefix("turn:") {
        match turn_seq(s, sid, turn, &c.observed_at).await {
            Some(seq) => {
                let from = (seq - TURN_WINDOW).max(0);
                let entries = s.context.history.load(sid, from).await.unwrap_or_default();
                turn_window(entries, seq)
            }
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    entries
        .iter()
        .filter(|e| e.message.role == Role::User)
        .map(|e| e.message.text())
        .filter(|t| is_owner_text(t))
        .collect()
}

/// Le message du propriétaire d'un tour : celui qui porte l'identifiant du tour
/// (migration 0016) ; avant, le dernier message du propriétaire avant la relecture, qui
/// suit la réponse du tour.
async fn turn_seq(s: &Services, sid: &str, turn: &str, observed_at: &str) -> Option<i64> {
    let (sid, turn, at) = (sid.to_string(), turn.to_string(), observed_at.to_string());
    s.store
        .read(move |c| {
            let by_turn = c
                .query_row(
                    "SELECT seq FROM messages
                     WHERE session_id = ?1 AND source_turn_id = ?2 AND sealed != 2",
                    params![sid, turn],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?;
            if by_turn.is_some() {
                return Ok(by_turn);
            }
            Ok(c.query_row(
                "SELECT seq FROM messages
                 WHERE session_id = ?1 AND role = 'user' AND ts <= ?2 AND sealed != 2
                 ORDER BY seq DESC LIMIT 1",
                params![sid, at],
                |r| r.get::<_, i64>(0),
            )
            .optional()?)
        })
        .await
        .ok()
        .flatten()
}

/// Les messages du tour dont `seq` est le message du propriétaire : de la réponse finale
/// précédente (exclue) à la réponse finale suivante (exclue), messages absorbés compris.
fn turn_window(entries: Vec<Entry>, seq: i64) -> Vec<Entry> {
    let Some(i) = entries.iter().position(|e| e.seq == seq) else {
        return Vec::new();
    };
    let is_final = |e: &Entry| e.message.role == Role::Assistant && e.message.tool_calls.is_empty();
    let start = entries[..i]
        .iter()
        .rposition(is_final)
        .map(|p| p + 1)
        .unwrap_or(0);
    let end = entries[i + 1..]
        .iter()
        .position(is_final)
        .map(|p| i + 1 + p)
        .unwrap_or(entries.len());
    entries.into_iter().skip(start).take(end - start).collect()
}

/// Un déclencheur, une relance ou un contenu transféré n'est pas la parole du
/// propriétaire (#245).
fn is_owner_text(text: &str) -> bool {
    !(text.starts_with("[déclencheur")
        || text.starts_with("[relance]")
        || text.contains("<<<DONNÉES NON FIABLES"))
}

// ---------------------------------------------------------------- fiche

/// Lit la fiche désignée : un chemin relatif au vault, sans remontée, qui existe. Renvoie
/// le chemin normalisé et le contenu.
fn read_source(s: &Services, path: &str) -> anyhow::Result<(String, String)> {
    let rel = path.trim().trim_start_matches("./");
    let p = Path::new(rel);
    if rel.is_empty()
        || p.is_absolute()
        || p.components().any(|c| !matches!(c, Component::Normal(_)))
    {
        anyhow::bail!("`--source` attend un chemin relatif au vault, sans `..` : `{path}`");
    }
    let vault = penelope_app::helpers::vault_dir(s);
    let raw = std::fs::read_to_string(vault.join(p))
        .map_err(|e| anyhow::anyhow!("fiche `{rel}` illisible dans le vault : {e}"))?;
    Ok((rel.to_string(), raw))
}

async fn reclaim_source(
    s: &Services,
    dry_run: bool,
    file: &str,
    raw: &str,
) -> anyhow::Result<Source> {
    let mut out = Source {
        file: file.to_string(),
        ..Default::default()
    };
    let key = format!("{KEY_SOURCE}{file}");
    if let Some(done) = s.kv_get(&key).await? {
        out.already = Some(done_at(&done));
        return Ok(out);
    }
    let facts = facts_of(raw);
    out.facts = facts.len();
    let source_ref = format!("{SOURCE_REF}{file}");
    let known = s.candidates.texts_from(&source_ref).await?;
    let now = s.clock.now_rfc3339();
    let mut candidates = Vec::new();
    for fact in facts {
        // Un secret part dans le magasin ; le fait n'en garde que la référence (#37). À
        // blanc, rien n'est rangé : le fait est montré avec la référence qu'il aurait.
        let text = if dry_run {
            crate::secret_shelf::masked(&fact)
        } else {
            match crate::secret_shelf::shelve(s, &fact) {
                Ok((text, _)) => text,
                Err(e) => {
                    tracing::warn!(error = %e, "fait avec un secret non rangé : écarté");
                    out.skipped += 1;
                    continue;
                }
            }
        };
        if crate::vault_ops::write_filter(&text).is_err() || known.contains(&text) {
            out.skipped += 1;
            continue;
        }
        let mut c = Candidate::new(
            CandidateType::Fait,
            &text,
            Origin::Owner,
            "interactive",
            &now,
        )
        .with_importance(SOURCE_IMPORTANCE)
        .said_by_owner(&text);
        c.source_ref = Some(source_ref.clone());
        out.items.push(text);
        candidates.push(c);
    }
    out.recorded = candidates.len();
    if !dry_run {
        let n = candidates.len();
        s.candidates.record(candidates, n).await?;
        let counts = json!({"facts": out.facts, "recorded": out.recorded});
        s.kv_set(&key, &stamp(s, counts)).await?;
    }
    Ok(out)
}

/// Faits d'une fiche, dans l'ordre : une puce ou une ligne numérotée par fait ; sans
/// puce, un paragraphe par fait. Un texte plus long que [`FACT_MAX_CHARS`] est scindé en
/// phrases. En-têtes, tableaux, blocs de code, lignes trop courtes et doublons écartés ;
/// le préfixe daté d'un export (`[2024-03-01] - `) et la graisse retirés. D'une fiche
/// `source`, seul le contenu compte : le résumé est celui de Pénélope.
pub fn facts_of(raw: &str) -> Vec<String> {
    let text = match penelope_memory::ingest::parse_source(raw) {
        Some(p) => p.text,
        None => penelope_kernel::frontmatter::parse(raw)
            .map(|f| f.body)
            .unwrap_or_else(|_| raw.to_string()),
    };
    let bullets: Vec<String> = text.lines().filter_map(bullet).collect();
    let items = if bullets.is_empty() {
        paragraphs(&text)
    } else {
        bullets
    };
    let mut seen = BTreeSet::new();
    items
        .iter()
        .map(|t| clean(t))
        .flat_map(|t| {
            if t.chars().count() > FACT_MAX_CHARS {
                sentences(&t)
            } else {
                vec![t]
            }
        })
        .filter(|t| is_fact(t))
        .filter(|t| seen.insert(normalized(t)))
        .collect()
}

/// Le texte d'une puce (`-`, `*`, `•`, `+`) ou d'une ligne numérotée (`3.`, `3)`).
fn bullet(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "• ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return Some(rest.to_string());
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &t[digits..];
        if let Some(rest) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Paragraphes d'un texte sans puces, en-têtes, tableaux et blocs de code écartés.
fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_code = false;
    for para in text.split("\n\n") {
        let mut lines = Vec::new();
        for l in para.lines().map(str::trim) {
            if l.starts_with("```") {
                in_code = !in_code;
                continue;
            }
            if in_code || l.is_empty() || l.starts_with('#') || l.starts_with('|') {
                continue;
            }
            lines.push(l);
        }
        if !lines.is_empty() {
            out.push(lines.join(" "));
        }
    }
    out
}

/// Phrases d'un texte : coupé après `.`, `!` ou `?` suivi d'un blanc.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        cur.push(c);
        if matches!(c, '.' | '!' | '?') && chars.peek().is_none_or(|n| n.is_whitespace()) {
            let t = cur.trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
            cur.clear();
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// Préfixe daté d'un export (`[2024-03-01] - `, `[unknown] - `) et graisse retirés,
/// blancs ramenés à un.
fn clean(t: &str) -> String {
    let mut t = t.trim();
    if let Some(rest) = t.strip_prefix('[')
        && let Some(end) = rest.find(']')
        && end <= 12
    {
        t = rest[end + 1..]
            .trim_start()
            .trim_start_matches(['-', '–', ':'])
            .trim_start();
    }
    let t = t.replace("**", "").replace("__", "");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Un fait dit quelque chose : ni un titre, ni une ligne de tableau, ni un mot.
fn is_fact(t: &str) -> bool {
    t.chars().count() >= FACT_MIN_CHARS
        && t.split_whitespace().count() >= FACT_MIN_WORDS
        && !t.starts_with('#')
        && !t.starts_with('|')
}

#[cfg(test)]
mod tests;
