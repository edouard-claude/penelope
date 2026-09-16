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
}

impl CompactionParams {
    pub fn from_config(cfg: &penelope_kernel::config::Config, window: u64, model_id: &str) -> Self {
        CompactionParams {
            window,
            threshold: cfg.compaction_threshold_for(model_id),
            tail_ratio: cfg.context.tail_ratio,
            tail_min_tokens: cfg.context.tail_min_tokens as u64,
            tail_max_tokens: cfg.context.tail_max_tokens as u64,
            min_tail_user_messages: cfg.context.min_tail_user_messages,
            max_tool_result_share: cfg.context.max_tool_result_share,
            large_payload_tokens: cfg.context.large_payload_tokens as u64,
        }
    }

    /// Budget de la **queue verbatim** : 2,5 % de la fenêtre, borné entre 10K et 25K.
    pub fn tail_budget(&self) -> u64 {
        ((self.window as f64 * self.tail_ratio) as u64)
            .clamp(self.tail_min_tokens, self.tail_max_tokens)
    }

    /// Budget d'admission d'un groupe de résultats d'outils.
    pub fn tool_group_budget(&self) -> u64 {
        (self.window as f64 * self.max_tool_result_share) as u64
    }

    pub fn threshold_tokens(&self) -> u64 {
        (self.window as f64 * self.threshold) as u64
    }

    /// Seuil de déclenchement de la compaction de fond (seuil moins 10 points).
    pub fn background_threshold_tokens(&self, margin: f64) -> u64 {
        (self.window as f64 * (self.threshold - margin).max(0.1)) as u64
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

    // Toujours laisser quelque chose à résumer, sinon la compaction ne sert à rien.
    if boundary == 0 && entries.len() > 2 {
        if let Some(g) = groups.first() {
            boundary = g.range.end;
            tail_tokens = entries[boundary..].iter().map(|e| e.tokens).sum();
        }
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

/// Schéma JSON du résumé demandé au modèle (sortie validée).
pub fn summary_schema() -> serde_json::Value {
    let props: serde_json::Map<String, serde_json::Value> = SUMMARY_SECTIONS
        .iter()
        .map(|s| {
            (
                section_key(s),
                serde_json::json!({"type": "string", "maxLength": 4000}),
            )
        })
        .collect();
    serde_json::json!({
        "type": "object",
        "properties": props,
        "required": ["objectif", "fait", "en_cours", "prochaines_etapes"],
        "additionalProperties": false
    })
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
    let mut s = String::from("## Résumé des tours compactés\n");
    for section in SUMMARY_SECTIONS {
        let key = section_key(section);
        if let Some(t) = v.get(&key).and_then(|x| x.as_str()) {
            if !t.trim().is_empty() {
                s.push_str(&format!("\n### {section}\n{}\n", t.trim()));
            }
        }
    }
    if !verbatim_users.is_empty() {
        s.push_str("\n### Messages utilisateur conservés verbatim\n");
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
mod tests {
    use super::*;
    use penelope_llm::types::ToolCall;
    use serde_json::json;

    fn params(window: u64) -> CompactionParams {
        CompactionParams {
            window,
            threshold: 0.70,
            tail_ratio: 0.025,
            tail_min_tokens: 10_000,
            tail_max_tokens: 25_000,
            min_tail_user_messages: 2,
            max_tool_result_share: 0.25,
            large_payload_tokens: 25_000,
        }
    }

    fn est(m: &ChatMessage) -> u64 {
        (m.text().chars().count() as u64 / 4).max(1)
    }
    fn est_all(ms: &[ChatMessage]) -> u64 {
        ms.iter().map(est).sum()
    }

    #[test]
    fn tail_budget_is_clamped() {
        assert_eq!(params(100_000).tail_budget(), 10_000, "plancher à 10K");
        assert_eq!(params(1_000_000).tail_budget(), 25_000, "plafond à 25K");
        assert_eq!(params(600_000).tail_budget(), 15_000, "2,5 % au milieu");
    }

    #[test]
    fn tool_group_budget_is_a_share_of_the_window() {
        assert_eq!(params(200_000).tool_group_budget(), 50_000);
    }

    #[test]
    fn level0_only_touches_eager_tool_results() {
        let entries = vec![
            Entry::new(1, ChatMessage::user("a"), 5),
            Entry::new(
                2,
                ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                    id: "c1".into(),
                    name: "fs_read".into(),
                    arguments: json!({}),
                }]),
                5,
            ),
            Entry::new(
                3,
                ChatMessage::tool_result("c1", "fs_read", "gros contenu"),
                900,
            )
            .eager(true),
            Entry::new(4, ChatMessage::assistant("fini"), 5),
        ];
        let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
        let n = level0_micro(&entries, &mut proj, 4);
        assert_eq!(n, 1);
        assert!(proj[2].text().contains("history_expand"));
        assert_eq!(proj[0].text(), "a", "les autres messages sont intacts");
        // Le canonique n'a pas bougé.
        assert_eq!(entries[2].message.text(), "gros contenu");
    }

    #[test]
    fn level0_protects_recent_entries() {
        let entries = vec![Entry::new(1, ChatMessage::tool_result("c", "t", "x"), 100).eager(true)];
        let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
        assert_eq!(level0_micro(&entries, &mut proj, 0), 0);
    }

    #[test]
    fn level1_keeps_small_groups_untouched() {
        let p = params(100_000);
        let r = vec![(0, "petit".to_string(), 100), (1, "aussi".to_string(), 200)];
        let out = level1_admission(&r, &p);
        assert!(out.iter().all(|(_, a)| *a == Admission::Keep));
    }

    #[test]
    fn level1_externalises_only_the_oversized_result() {
        let p = params(100_000); // budget de groupe : 25 000
        let big: String = "x".repeat(400_000);
        let r = vec![(0, "petit".to_string(), 50), (1, big.clone(), 100_000)];
        let out = level1_admission(&r, &p);
        assert_eq!(out[0].1, Admission::Keep, "le petit passe entier");
        match &out[1].1 {
            Admission::Externalise {
                head,
                tail,
                original_tokens,
            } => {
                assert_eq!(*original_tokens, 100_000);
                assert!(!head.is_empty() && !tail.is_empty());
                assert!(head.chars().count() < big.chars().count());
            }
            other => panic!("attendu une externalisation, obtenu {other:?}"),
        }
    }

    #[test]
    fn externalised_body_carries_the_pointer() {
        let b = externalised_body("art_1", "début", "fin", 50_000, "log");
        assert!(b.contains("artifact_read(\"art_1\")"));
        assert!(b.contains("50000 tokens"));
        assert!(b.contains("début") && b.contains("fin"));
    }

    #[test]
    fn level2_degrades_until_it_fits() {
        let long: String = "a".repeat(4000);
        let entries: Vec<Entry> = (0..6)
            .map(|i| {
                Entry::new(
                    i,
                    ChatMessage::tool_result(format!("c{i}"), "t", long.clone()),
                    1000,
                )
            })
            .collect();
        let mut proj: Vec<ChatMessage> = entries.iter().map(|e| e.message.clone()).collect();
        let before = est_all(&proj);
        let (after, touched) = level2_degrade(&entries, &mut proj, 5, before / 2, before, &est);
        assert!(after < before, "{after} doit être < {before}");
        assert!(touched > 0);
        assert_eq!(
            proj[5].text().chars().count(),
            4000,
            "le groupe protégé reste intact"
        );
    }

    #[test]
    fn split_keeps_at_least_two_user_messages_in_the_tail() {
        let p = params(100_000); // queue : 10 000 tokens
        let mut entries = Vec::new();
        for i in 0..20 {
            entries.push(Entry::new(i * 2, ChatMessage::user(format!("q{i}")), 3000));
            entries.push(Entry::new(i * 2 + 1, ChatMessage::assistant("r"), 3000));
        }
        let (boundary, tail) = split_for_summary(&entries, &p);
        let users_in_tail = entries[boundary..]
            .iter()
            .filter(|e| e.role() == Role::User)
            .count();
        assert!(
            users_in_tail >= 2,
            "queue : {users_in_tail} messages utilisateur"
        );
        assert!(boundary > 0, "il doit rester quelque chose à résumer");
        assert!(tail > 0);
    }

    #[test]
    fn split_never_cuts_inside_a_tool_group() {
        let p = params(100_000);
        let entries = vec![
            Entry::new(1, ChatMessage::user("a"), 6000),
            Entry::new(2, ChatMessage::user("b"), 6000),
            Entry::new(
                3,
                ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                    id: "c1".into(),
                    name: "t".into(),
                    arguments: json!({}),
                }]),
                100,
            ),
            Entry::new(4, ChatMessage::tool_result("c1", "t", "r"), 4000),
            Entry::new(5, ChatMessage::user("c"), 100),
        ];
        let (boundary, _) = split_for_summary(&entries, &p);
        assert_ne!(
            boundary, 4,
            "la frontière ne tombe jamais entre l'appel et son résultat"
        );
    }

    #[test]
    fn verbatim_users_are_most_recent_first_under_budget() {
        let entries: Vec<Entry> = (0..10)
            .map(|i| Entry::new(i, ChatMessage::user(format!("message {i}")), 100))
            .collect();
        let kept = select_verbatim_users(&entries, 350);
        assert_eq!(kept.len(), 3);
        assert_eq!(
            kept.last().unwrap(),
            "message 9",
            "les plus récents d'abord"
        );
    }

    #[test]
    fn summary_schema_and_render() {
        let s = summary_schema();
        let good = json!({
            "objectif": "corriger le bug de TVA",
            "fait": "localisé dans facture.rs",
            "en_cours": "écriture du test",
            "prochaines_etapes": "ouvrir la PR"
        });
        assert!(penelope_kernel::schema::validate(&s, &good).is_empty());
        let bad = json!({"objectif": "x"});
        assert!(!penelope_kernel::schema::validate(&s, &bad).is_empty());

        let txt = render_summary(
            &good,
            "Ancres :\n- chemin : facture.rs\n",
            &["où en est-on ?".into()],
        );
        assert!(txt.contains("### Objectif"));
        assert!(txt.contains("### Prochaines étapes"));
        assert!(txt.contains("verbatim"));
        assert!(txt.contains("facture.rs"));
    }

    /// CA 5 : niveau 4, la requête est prouvée conforme **avant** envoi.
    #[test]
    fn ca_5_1_level4_proves_it_fits() {
        let long = "x".repeat(40_000);
        let mut msgs = vec![ChatMessage::system("règles")];
        for i in 0..8 {
            msgs.push(ChatMessage::assistant("").with_tool_calls(vec![ToolCall {
                id: format!("c{i}"),
                name: "t".into(),
                arguments: json!({}),
            }]));
            msgs.push(ChatMessage::tool_result(format!("c{i}"), "t", long.clone()));
        }
        msgs.push(ChatMessage::user("et maintenant ?"));

        let limit = 5_000;
        let (out, fits) = level4_emergency(msgs, limit, &est_all);
        assert!(fits, "le niveau 4 doit converger");
        let tokens = proof_of_fit(&out, limit, &est_all).expect("preuve locale");
        assert!(tokens <= limit);
        assert!(
            out.iter().any(|m| m.text() == "et maintenant ?"),
            "le dernier message utilisateur est conservé"
        );
        assert!(out.iter().any(|m| m.role == Role::System));
    }

    #[test]
    fn proof_rejects_broken_pairs() {
        let msgs = vec![ChatMessage::tool_result("orphelin", "t", "x")];
        assert!(proof_of_fit(&msgs, 1_000_000, &est_all).is_err());
    }

    #[test]
    fn cooldown_escalates_then_clears() {
        let steps = [60_000u64, 300_000, 900_000];
        let mut c = Cooldown::default();
        assert!(!c.is_active(0));
        c.record_failure(0, &steps);
        assert!(c.is_active(59_000));
        assert!(!c.is_active(61_000));
        c.record_failure(61_000, &steps);
        assert_eq!(c.until_ms, 61_000 + 300_000);
        c.record_failure(400_000, &steps);
        assert_eq!(c.until_ms, 400_000 + 900_000);
        c.record_failure(2_000_000, &steps);
        assert_eq!(
            c.until_ms,
            2_000_000 + 900_000,
            "plafonné au dernier palier"
        );
        c.clear();
        assert!(!c.is_active(0));
        assert_eq!(c.failures, 0);
    }

    #[test]
    fn plan_levels_follows_the_prd_triggers() {
        let p = params(100_000);
        assert_eq!(plan_levels(10_000, &p, true), Vec::<u8>::new());
        assert_eq!(plan_levels(10_000, &p, false), vec![0, 2]);
        assert_eq!(plan_levels(80_000, &p, true), vec![3]);
        assert_eq!(plan_levels(80_000, &p, false), vec![0, 2, 3]);
    }

    #[test]
    fn background_threshold_is_ten_points_lower() {
        let p = params(100_000);
        assert_eq!(p.threshold_tokens(), 70_000);
        assert_eq!(p.background_threshold_tokens(0.10), 60_000);
    }
}
