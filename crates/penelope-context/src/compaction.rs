//! Compaction en 5 niveaux (§5.4).
//!
//! | Niveau | Nom | Modifie | LLM |
//! |---|---|---|---|
//! | 0 | Micro | projection | non |
//! | 1 | Admission | **canonique** | non |
//! | 2 | Dégradation | projection | non |
//! | 3 | Résumé LCM | **canonique** | oui |
//! | 4 | Urgence | projection | non |

use crate::transcript::{
    Entry, Group, GroupKind, group, head_tail, pairs_are_valid, repair_pairs, stub_message,
};
use penelope_llm::types::{ChatMessage, Role};
use serde::{Deserialize, Serialize};

/// Paramètres effectifs, dérivés de la configuration et de la fenêtre du modèle.
#[derive(Debug, Clone, Copy)]
pub struct CompactionParams {
    pub window: u64,
    pub threshold: f64,
    pub tail_ratio: f64,
    pub tail_min_tokens: u64,
    pub tail_max_tokens: u64,
    pub min_tail_user_messages: usize,
    pub max_tool_result_share: f64,
    pub large_payload_tokens: u64,
    /// Marge sous le seuil à partir de laquelle la compaction de fond démarre.
    pub background_margin: f64,
    /// Plafond de coût du prompt (`context.max_prompt_tokens`), 0 : aucun.
    pub max_prompt_tokens: u64,
}

/// Tokens réservés à la réponse : on ne remplit jamais la fenêtre jusqu'au bord (10 % de
/// la fenêtre, entre 1 000 et 32 000).
pub fn reserved_output(window: u64) -> u64 {
    (window / 10).clamp(1_000, 32_000)
}

impl CompactionParams {
    /// Paramètres d'un modèle. Sans seuil propre au modèle (`context.model_thresholds`),
    /// le seuil est ramené à [`CompactionParams::headroom_threshold`] quand la fenêtre est
    /// trop courte pour lui (issue #107).
    pub fn from_config(cfg: &penelope_kernel::config::Config, window: u64, model_id: &str) -> Self {
        let mut p = CompactionParams {
            window,
            threshold: cfg.compaction_threshold_for(model_id),
            tail_ratio: cfg.context.tail_ratio,
            tail_min_tokens: cfg.context.tail_min_tokens as u64,
            tail_max_tokens: cfg.context.tail_max_tokens as u64,
            min_tail_user_messages: cfg.context.min_tail_user_messages,
            max_tool_result_share: cfg.context.max_tool_result_share,
            large_payload_tokens: cfg.context.large_payload_tokens as u64,
            background_margin: cfg.context.background_compaction_margin,
            max_prompt_tokens: cfg.context.max_prompt_tokens as u64,
        };
        if cfg.model_threshold(model_id).is_none() {
            p.threshold = p.threshold.min(p.headroom_threshold()).max(0.1);
        }
        p
    }

    /// Seuil le plus haut qui laisse toujours la place, au-dessus de lui, d'un groupe de
    /// résultats d'outils admis entier et de la réserve de réponse. Sur 32 k tokens, 70 %
    /// plus un résultat d'un quart de fenêtre plus 10 % de réserve débordent : le seuil
    /// descend à 65 %. Sur 128 k, le résultat est plafonné à 25 k : 70 % tient.
    pub fn headroom_threshold(&self) -> f64 {
        let w = self.window.max(1) as f64;
        1.0 - reserved_output(self.window) as f64 / w - self.tool_group_budget() as f64 / w
    }

    /// Fenêtre de travail : celle du modèle, réduite pour que le seuil de compaction ne
    /// dépasse pas `max_prompt_tokens`. Sur un modèle à 1,3 M de tokens et 120 k de
    /// plafond, la compaction part vers 103 k au lieu de 917 k (issue #18).
    pub fn budget_window(&self) -> u64 {
        if self.max_prompt_tokens == 0 || self.threshold <= 0.0 {
            return self.window;
        }
        self.window
            .min((self.max_prompt_tokens as f64 / self.threshold) as u64)
    }

    /// Budget de la **queue verbatim** : 2,5 % de la fenêtre, borné entre 10K et 25K, et
    /// jamais plus du quart du seuil de fond : sous un plafond de coût comme sur une fenêtre
    /// courte, une queue plus grosse couvrirait tout ce qu'il faudrait résumer (sur 8 k
    /// tokens, 10 k de queue interdisaient tout résumé, issue #107).
    pub fn tail_budget(&self) -> u64 {
        ((self.window as f64 * self.tail_ratio) as u64)
            .clamp(self.tail_min_tokens, self.tail_max_tokens)
            .min(self.background_threshold_tokens(self.background_margin) / 4)
    }

    /// Budget d'admission d'un groupe de résultats d'outils : une part de la fenêtre,
    /// plafonnée en valeur absolue. Sans plafond, un modèle à 1,3 M de tokens gardait
    /// entiers des résultats de 175 k (issue #8).
    pub fn tool_group_budget(&self) -> u64 {
        let share = (self.budget_window() as f64 * self.max_tool_result_share) as u64;
        if self.large_payload_tokens == 0 {
            share
        } else {
            share.min(self.large_payload_tokens)
        }
    }

    pub fn threshold_tokens(&self) -> u64 {
        let t = (self.window as f64 * self.threshold) as u64;
        match self.max_prompt_tokens {
            0 => t,
            cap => t.min(cap),
        }
    }

    /// Seuil de déclenchement de la compaction de fond (seuil moins 10 points).
    pub fn background_threshold_tokens(&self, margin: f64) -> u64 {
        (self.budget_window() as f64 * (self.threshold - margin).max(0.1)) as u64
    }
}

/// Trace d'une étape de compaction appliquée à une requête.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedStep {
    pub level: u8,
    pub label: String,
    pub before_tokens: u64,
    pub after_tokens: u64,
    pub touched: usize,
}

/// Résultat de la construction d'une projection de requête.
#[derive(Debug, Clone)]
pub struct Projection {
    pub messages: Vec<ChatMessage>,
    pub tokens: u64,
    pub steps: Vec<AppliedStep>,
    /// Vrai si la requête tient dans la fenêtre après application.
    pub fits: bool,
}

// ------------------------------------------------------------------ niveau 0

/// **Micro** : remplace les vieux résultats d'outils volatils (`eager`) par un stub avec
/// pointeur de récupération. Ne touche **que** la projection.
pub fn level0_micro(entries: &[Entry], projection: &mut [ChatMessage], keep_from: usize) -> usize {
    let mut touched = 0;
    for (i, e) in entries.iter().enumerate() {
        if i >= keep_from || !e.eager || e.role() != Role::Tool {
            continue;
        }
        if let Some(m) = projection.get_mut(i) {
            if m.text().starts_with("[résultat volatil élidé") {
                continue;
            }
            let pointer = e
                .artifact_id
                .clone()
                .unwrap_or_else(|| format!("seq:{}", e.seq));
            *m = stub_message(
                m,
                &format!("[résultat volatil élidé — récupérable via history_expand({pointer})]"),
            );
            touched += 1;
        }
    }
    touched
}

// ------------------------------------------------------------------ niveau 1

/// Décision d'admission d'un résultat d'outil (§5.4 niveau 1).
#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    /// Le résultat tient : conservé tel quel.
    Keep,
    /// Le résultat dépasse : externalisé en artefact, remplacé par un aperçu.
    Externalise {
        head: String,
        tail: String,
        original_tokens: u64,
    },
}

/// Admet un groupe de résultats d'outils sous budget.
///
/// Le surplus est externalisé avec un aperçu tête/queue et un identifiant : c'est la
/// seule modification du **canonique** faite sans appeler le modèle.
pub fn level1_admission(
    results: &[(usize, String, u64)],
    params: &CompactionParams,
) -> Vec<(usize, Admission)> {
    let budget = params.tool_group_budget();
    let total: u64 = results.iter().map(|(_, _, t)| *t).sum();
    if total <= budget {
        return results
            .iter()
            .map(|(i, _, _)| (*i, Admission::Keep))
            .collect();
    }

    // Part égale par résultat, puis redistribution de ce que les petits n'utilisent pas.
    let n = results.len().max(1) as u64;
    let mut share = budget / n;
    let unused: u64 = results
        .iter()
        .map(|(_, _, t)| share.saturating_sub(*t))
        .sum();
    // Aucun dépassement : rien à redistribuer, et surtout pas de division par zéro.
    let over = results.iter().filter(|(_, _, t)| *t >= share).count() as u64;
    share += unused.checked_div(over).unwrap_or(0);

    results
        .iter()
        .map(|(i, text, tokens)| {
            if *tokens <= share {
                (*i, Admission::Keep)
            } else {
                // ~3,6 caractères par token : on convertit le budget en caractères,
                // 60 % en tête et 40 % en queue.
                let chars = (share as f64 * 3.6) as usize;
                let (h, t) = head_tail(text, (chars * 6) / 10, (chars * 4) / 10);
                (
                    *i,
                    Admission::Externalise {
                        head: h,
                        tail: t,
                        original_tokens: *tokens,
                    },
                )
            }
        })
        .collect()
}

/// Rendu d'un résultat externalisé, tel qu'il apparaît dans le canonique.
pub fn externalised_body(
    artifact_id: &str,
    head: &str,
    tail: &str,
    original_tokens: u64,
    kind: &str,
) -> String {
    let mut s = format!(
        "[résultat externalisé — artefact {artifact_id} · type {kind} · {original_tokens} tokens · \
         lire avec artifact_read(\"{artifact_id}\")]\n--- début ---\n{head}"
    );
    if !tail.is_empty() {
        s.push_str(&format!("\n--- … ---\n{tail}\n--- fin ---"));
    } else {
        s.push_str("\n--- fin ---");
    }
    s
}

// ------------------------------------------------------------------ niveau 2

/// **Dégradation** : réduit les anciens résultats d'outils dans la projection
/// seulement — aperçu, puis stub. S'arrête dès que la requête tient.
pub fn level2_degrade(
    entries: &[Entry],
    projection: &mut [ChatMessage],
    protect_from: usize,
    target_tokens: u64,
    current_tokens: u64,
    estimate: &dyn Fn(&ChatMessage) -> u64,
) -> (u64, usize) {
    let mut tokens = current_tokens;
    let mut touched = 0;

    // Deux passes : d'abord l'aperçu, puis le stub complet, du plus ancien au plus récent.
    for pass in 0..2 {
        for (i, e) in entries.iter().enumerate() {
            if tokens <= target_tokens {
                return (tokens, touched);
            }
            if i >= protect_from || e.role() != Role::Tool {
                continue;
            }
            let Some(m) = projection.get_mut(i) else {
                continue;
            };
            let before = estimate(m);
            let text = m.text();
            let replacement = if pass == 0 {
                if text.chars().count() <= 400 {
                    continue;
                }
                let (h, t) = head_tail(&text, 200, 100);
                format!("{h}\n[…élidé…]\n{t}")
            } else {
                if text.starts_with("[résultat élidé") {
                    continue;
                }
                format!(
                    "[résultat élidé — récupérable via history_expand(seq:{})]",
                    e.seq
                )
            };
            *m = stub_message(m, &replacement);
            let after = estimate(m);
            tokens = tokens.saturating_sub(before.saturating_sub(after));
            touched += 1;
        }
    }
    (tokens, touched)
}

// ------------------------------------------------------------------ niveau 3

/// Découpe l'historique : ce qui est résumable, et la queue verbatim conservée.
///
/// La frontière tombe toujours sur une frontière de **groupe**, et la queue contient au
/// moins `min_tail_user_messages` messages utilisateur.
pub fn split_for_summary(entries: &[Entry], params: &CompactionParams) -> (usize, u64) {
    let (mut boundary, mut tail_tokens) = natural_split(entries, params);

    // Toujours laisser quelque chose à résumer, sinon la compaction ne sert à rien.
    if boundary == 0
        && entries.len() > 2
        && let Some(g) = group(entries).first()
    {
        boundary = g.range.end;
        tail_tokens = entries[boundary..].iter().map(|e| e.tokens).sum();
    }
    (boundary, tail_tokens)
}

/// Frontière dessinée par le seul budget de la queue : `0` quand toute l'entrée tient
/// dans la queue verbatim. La compaction de fond s'en contente ; `/compact` force.
pub fn natural_split(entries: &[Entry], params: &CompactionParams) -> (usize, u64) {
    let groups = group(entries);
    let budget = params.tail_budget();
    let mut tail_tokens = 0u64;
    let mut user_messages = 0usize;
    let mut boundary = entries.len();

    for g in groups.iter().rev() {
        let g_tokens: u64 = entries[g.range.clone()].iter().map(|e| e.tokens).sum();
        let g_users = entries[g.range.clone()]
            .iter()
            .filter(|e| e.role() == Role::User)
            .count();

        let enough_users = user_messages >= params.min_tail_user_messages;
        if tail_tokens + g_tokens > budget && enough_users {
            break;
        }
        tail_tokens += g_tokens;
        user_messages += g_users;
        boundary = g.range.start;
    }
    (boundary, tail_tokens)
}

/// Gabarit de résumé structuré (§5.4).
pub const SUMMARY_SECTIONS: &[&str] = &[
    "Objectif",
    "Contraintes et préférences",
    "Fait",
    "En cours",
    "Bloqué",
    "Décisions clés",
    "Fichiers et ressources",
    "Prochaines étapes",
    "Contexte critique",
];

/// Longueur maximale d'une section du résumé, en caractères.
pub const SECTION_MAX_CHARS: usize = 4_000;

/// Sections obligatoires : un résumé qui les laisse toutes vides est rejeté.
pub const REQUIRED_SECTIONS: &[&str] = &["objectif", "fait", "en_cours", "prochaines_etapes"];

/// Clés JSON des sections, dans l'ordre du gabarit.
pub fn section_keys() -> Vec<String> {
    SUMMARY_SECTIONS.iter().map(|s| section_key(s)).collect()
}

/// Schéma JSON du résumé demandé au modèle (sortie validée).
pub fn summary_schema() -> serde_json::Value {
    let props: serde_json::Map<String, serde_json::Value> = SUMMARY_SECTIONS
        .iter()
        .map(|s| {
            (
                section_key(s),
                serde_json::json!({"type": "string", "maxLength": SECTION_MAX_CHARS}),
            )
        })
        .collect();
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": REQUIRED_SECTIONS,
        "additionalProperties": false
    })
}

/// `response_format` envoyé au résumeur : sortie structurée stricte, toutes les clés
/// présentes (une section sans objet est une chaîne vide). Les longueurs, que le mode
/// strict ne sait pas toujours exprimer, sont imposées à la validation.
pub fn summary_response_format() -> serde_json::Value {
    let keys = section_keys();
    let props: serde_json::Map<String, serde_json::Value> = keys
        .iter()
        .map(|k| (k.clone(), serde_json::json!({"type": "string"})))
        .collect();
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "resume",
            "strict": true,
            "schema": {
                "type": "object",
                "properties": props,
                "required": keys,
                "additionalProperties": false
            }
        }
    })
}

/// Valide la sortie du résumeur et la ramène au gabarit.
///
/// Tolère un bloc de code autour du JSON, une section rendue en liste ou absente ;
/// tronque chaque section à [`SECTION_MAX_CHARS`]. Rejette ce qui n'est pas un objet
/// JSON ou un résumé dont toutes les sections obligatoires sont vides.
pub fn validate_summary(raw: &str) -> Result<serde_json::Value, String> {
    let start = raw
        .find('{')
        .ok_or("le résumeur n'a pas rendu d'objet JSON")?;
    let end = raw
        .rfind('}')
        .filter(|e| *e > start)
        .ok_or("le résumeur n'a pas rendu d'objet JSON complet")?;
    let parsed: serde_json::Value = serde_json::from_str(&raw[start..=end])
        .map_err(|e| format!("JSON du résumé invalide : {e}"))?;
    let obj = parsed
        .as_object()
        .ok_or("le résumé doit être un objet JSON")?;

    let mut out = serde_json::Map::new();
    let mut known = 0usize;
    for key in section_keys() {
        let text = match obj.get(&key) {
            None | Some(serde_json::Value::Null) => String::new(),
            Some(serde_json::Value::String(t)) => t.trim().to_string(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .map(|i| match i {
                    serde_json::Value::String(t) => format!("- {}", t.trim()),
                    other => format!("- {other}"),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Some(other) => other.to_string(),
        };
        if obj.contains_key(&key) {
            known += 1;
        }
        out.insert(
            key,
            serde_json::Value::String(text.chars().take(SECTION_MAX_CHARS).collect()),
        );
    }
    if known == 0 {
        return Err("le résumé ne contient aucune section attendue".into());
    }
    let filled = REQUIRED_SECTIONS.iter().any(|k| {
        out.get(*k)
            .and_then(|v| v.as_str())
            .map(|t| !t.is_empty())
            .unwrap_or(false)
    });
    if !filled {
        return Err("résumé vide : aucune section obligatoire n'est remplie".into());
    }
    Ok(serde_json::Value::Object(out))
}

/// En-tête du résumé rendu.
pub const SUMMARY_HEADER: &str = "## Résumé des tours compactés";
/// Titre du bloc des messages utilisateur conservés tels quels.
pub const VERBATIM_HEADER: &str = "### Messages utilisateur conservés verbatim";

/// Sections rédigées d'un résumé rendu, sans l'en-tête ni les blocs ajoutés
/// mécaniquement (messages verbatim, ancres) : c'est ce que le résumeur met à jour.
pub fn summary_sections_only(rendered: &str) -> String {
    let body = rendered.strip_prefix(SUMMARY_HEADER).unwrap_or(rendered);
    let mut cut = body.len();
    for marker in [format!("\n{VERBATIM_HEADER}"), "\nAncres :\n".to_string()] {
        if let Some(i) = body.find(&marker) {
            cut = cut.min(i);
        }
    }
    body[..cut].trim().to_string()
}

pub fn section_key(section: &str) -> String {
    section
        .to_lowercase()
        .replace([' ', '\''], "_")
        .replace(['é', 'è'], "e")
        .replace('ç', "c")
}

/// Rend un résumé validé en texte injectable.
pub fn render_summary(v: &serde_json::Value, anchors: &str, verbatim_users: &[String]) -> String {
    let mut s = format!("{SUMMARY_HEADER}\n");
    for section in SUMMARY_SECTIONS {
        let key = section_key(section);
        if let Some(t) = v.get(&key).and_then(|x| x.as_str())
            && !t.trim().is_empty()
        {
            s.push_str(&format!("\n### {section}\n{}\n", t.trim()));
        }
    }
    if !verbatim_users.is_empty() {
        s.push_str(&format!("\n{VERBATIM_HEADER}\n"));
        for u in verbatim_users {
            s.push_str(&format!("- « {} »\n", u.replace('\n', " ")));
        }
    }
    if !anchors.is_empty() {
        s.push('\n');
        s.push_str(anchors);
    }
    s
}

/// Sélectionne les messages utilisateur à conserver verbatim, les plus récents d'abord,
/// sous budget (§5.4).
pub fn select_verbatim_users(entries: &[Entry], budget_tokens: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = 0u64;
    for e in entries.iter().rev() {
        if e.role() != Role::User {
            continue;
        }
        let t = e.tokens.max(1);
        if used + t > budget_tokens {
            continue;
        }
        used += t;
        out.push(e.message.text());
    }
    out.reverse();
    out
}

// ------------------------------------------------------------------ niveau 4

/// **Urgence** : sur erreur `context_length` du provider.
///
/// Supprime les corps d'outils anciens, conserve le dernier message utilisateur et les
/// paires valides, puis **prouve localement** que la requête tient avant envoi.
pub fn level4_emergency(
    messages: Vec<ChatMessage>,
    limit_tokens: u64,
    estimate: &dyn Fn(&[ChatMessage]) -> u64,
) -> (Vec<ChatMessage>, bool) {
    let mut msgs = messages;

    // 1. Vider tous les corps de résultats d'outils, sauf le dernier groupe.
    let last_tool = msgs.iter().rposition(|m| m.role == Role::Tool);
    for (i, m) in msgs.iter_mut().enumerate() {
        if m.role == Role::Tool && Some(i) != last_tool {
            *m = stub_message(m, "[corps supprimé — compaction d'urgence]");
        }
    }
    if estimate(&msgs) <= limit_tokens {
        return (repair_pairs(msgs), true);
    }

    // 2. Retirer les groupes les plus anciens, en gardant système + dernier utilisateur.
    let last_user = msgs.iter().rposition(|m| m.role == Role::User);
    let mut keep_from = 0usize;
    while keep_from < msgs.len() {
        let candidate: Vec<ChatMessage> = msgs
            .iter()
            .enumerate()
            .filter(|(i, m)| m.role == Role::System || *i >= keep_from || Some(*i) == last_user)
            .map(|(_, m)| m.clone())
            .collect();
        let repaired = repair_pairs(candidate);
        if estimate(&repaired) <= limit_tokens {
            return (repaired, true);
        }
        keep_from += 1;
    }

    // 3. Dernier recours : système + dernier message utilisateur, tronqué.
    let mut minimal: Vec<ChatMessage> = msgs
        .iter()
        .filter(|m| m.role == Role::System)
        .cloned()
        .collect();
    if let Some(i) = last_user {
        minimal.push(msgs[i].clone());
    }
    let fits = estimate(&minimal) <= limit_tokens;
    (minimal, fits)
}

/// Vérifie l'invariant du niveau 4 : paires valides **et** taille prouvée.
pub fn proof_of_fit(
    messages: &[ChatMessage],
    limit_tokens: u64,
    estimate: &dyn Fn(&[ChatMessage]) -> u64,
) -> Result<u64, String> {
    if !pairs_are_valid(messages) {
        return Err("paires appel/résultat invalides après compaction".into());
    }
    let t = estimate(messages);
    if t > limit_tokens {
        return Err(format!(
            "la requête pèse encore {t} tokens pour une limite de {limit_tokens}"
        ));
    }
    Ok(t)
}

// ------------------------------------------------------------------ cooldown

/// Cooldown d'échec de compaction : 60 s, puis 300 s, puis 900 s (§5.4), persisté.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Cooldown {
    pub failures: u32,
    pub until_ms: i64,
}

impl Cooldown {
    pub fn is_active(&self, now_ms: i64) -> bool {
        now_ms < self.until_ms
    }

    pub fn record_failure(&mut self, now_ms: i64, steps_ms: &[u64]) {
        let idx = (self.failures as usize).min(steps_ms.len().saturating_sub(1));
        let wait = steps_ms.get(idx).copied().unwrap_or(900_000) as i64;
        self.failures = self.failures.saturating_add(1);
        self.until_ms = now_ms + wait;
    }

    /// Levé par `/compact` ou par une erreur de dépassement prouvée par le provider.
    pub fn clear(&mut self) {
        self.failures = 0;
        self.until_ms = 0;
    }
}

/// Niveau qu'il faut appliquer, compte tenu de l'usage courant.
pub fn plan_levels(usage_tokens: u64, params: &CompactionParams, request_fits: bool) -> Vec<u8> {
    let mut levels = Vec::new();
    if !request_fits {
        levels.push(0);
        levels.push(2);
    }
    if usage_tokens >= params.threshold_tokens() {
        levels.push(3);
    }
    levels
}

/// Vrai si `group` est un groupe d'outils (utilisé par la sélection de résumé).
pub fn is_tool_group(g: &Group) -> bool {
    g.kind == GroupKind::ToolGroup
}

#[cfg(test)]
mod tests;
