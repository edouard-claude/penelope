//! Historique canonique et groupes d'outils (§5.4).
//!
//! **Historique canonique ≠ projection de requête.** Les niveaux 0 et 2 ne modifient que
//! la copie envoyée au modèle ; seuls les niveaux 1 et 3 touchent au canonique.
//!
//! **Unité protocolaire = groupe d'outils complet** : un message d'assistant portant des
//! `tool_calls` et *tous* ses résultats. Jamais de coupe au milieu.

use penelope_llm::types::{ChatMessage, Content, Role, ToolCall};
use serde::{Deserialize, Serialize};

/// Une entrée de l'historique canonique.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: i64,
    pub message: ChatMessage,
    /// Résultat d'outil volatil : candidat au niveau 0 (micro-compaction).
    pub eager: bool,
    /// Corps externalisé en artefact (niveau 1).
    pub artifact_id: Option<String>,
    pub tokens: u64,
    pub episode: i64,
    /// Couvert par un nœud de résumé LCM.
    pub compacted: bool,
}

impl Entry {
    pub fn new(seq: i64, message: ChatMessage, tokens: u64) -> Self {
        Entry {
            seq,
            message,
            eager: false,
            artifact_id: None,
            tokens,
            episode: 0,
            compacted: false,
        }
    }
    pub fn eager(mut self, yes: bool) -> Self {
        self.eager = yes;
        self
    }
    pub fn role(&self) -> Role {
        self.message.role
    }
}

/// Un groupe protocolaire : soit un message isolé, soit un appel d'outils et ses résultats.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Indices dans le vecteur d'entrées.
    pub range: std::ops::Range<usize>,
    pub kind: GroupKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    UserMessage,
    AssistantText,
    SystemMessage,
    /// Appel d'outils + résultats.
    ToolGroup,
}

/// Découpe l'historique en groupes protocolaires indivisibles.
pub fn group(entries: &[Entry]) -> Vec<Group> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        match e.role() {
            Role::Assistant if !e.message.tool_calls.is_empty() => {
                let mut j = i + 1;
                while j < entries.len() && entries[j].role() == Role::Tool {
                    j += 1;
                }
                out.push(Group {
                    range: i..j,
                    kind: GroupKind::ToolGroup,
                });
                i = j;
            }
            Role::User => {
                out.push(Group {
                    range: i..i + 1,
                    kind: GroupKind::UserMessage,
                });
                i += 1;
            }
            Role::System => {
                out.push(Group {
                    range: i..i + 1,
                    kind: GroupKind::SystemMessage,
                });
                i += 1;
            }
            Role::Assistant => {
                out.push(Group {
                    range: i..i + 1,
                    kind: GroupKind::AssistantText,
                });
                i += 1;
            }
            Role::Tool => {
                // Résultat orphelin : groupe à lui seul, il sera réparé.
                out.push(Group {
                    range: i..i + 1,
                    kind: GroupKind::ToolGroup,
                });
                i += 1;
            }
        }
    }
    out
}

/// Répare les paires orphelines (§5.4) :
/// - un résultat sans appel est **supprimé** ;
/// - un appel sans résultat est **complété par un stub d'erreur**.
///
/// C'est l'invariant qui empêche un 400 du provider après une coupe de contexte.
pub fn repair_pairs(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    // Passe 1 : identifiants d'appels présents.
    let mut known: std::collections::HashSet<String> = Default::default();
    for m in &messages {
        for tc in &m.tool_calls {
            known.insert(tc.id.clone());
        }
    }

    // Passe 2 : supprime les résultats orphelins.
    let mut kept: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut answered: std::collections::HashSet<String> = Default::default();
    for m in messages {
        if m.role == Role::Tool {
            match &m.tool_call_id {
                Some(id) if known.contains(id) => {
                    answered.insert(id.clone());
                    kept.push(m);
                }
                _ => { /* résultat sans appel : supprimé */ }
            }
        } else {
            kept.push(m);
        }
    }

    // Passe 3 : complète les appels sans résultat.
    let mut out: Vec<ChatMessage> = Vec::with_capacity(kept.len());
    for m in kept {
        let missing: Vec<ToolCall> = m
            .tool_calls
            .iter()
            .filter(|tc| !answered.contains(&tc.id))
            .cloned()
            .collect();
        let has_calls = !m.tool_calls.is_empty();
        out.push(m);
        if has_calls {
            for tc in missing {
                out.push(ChatMessage::tool_result(
                    tc.id.clone(),
                    tc.name.clone(),
                    "[résultat perdu à la compaction : relancer l'outil si nécessaire]",
                ));
            }
        }
    }
    out
}

/// Vrai si la séquence respecte l'appariement appel/résultat.
pub fn pairs_are_valid(messages: &[ChatMessage]) -> bool {
    let mut pending: std::collections::HashSet<String> = Default::default();
    for m in messages {
        match m.role {
            Role::Assistant => {
                for tc in &m.tool_calls {
                    pending.insert(tc.id.clone());
                }
            }
            Role::Tool => match &m.tool_call_id {
                Some(id) => {
                    if !pending.remove(id) {
                        return false; // résultat sans appel
                    }
                }
                None => return false,
            },
            _ => {}
        }
    }
    pending.is_empty()
}

/// Remplace le contenu d'un message par un stub, en gardant son rôle et ses métadonnées
/// protocolaires (les niveaux 0 et 2 ne changent **que** la projection).
pub fn stub_message(m: &ChatMessage, note: &str) -> ChatMessage {
    ChatMessage {
        role: m.role,
        content: vec![Content::text(note)],
        tool_calls: m.tool_calls.clone(),
        tool_call_id: m.tool_call_id.clone(),
        name: m.name.clone(),
        cache_marker: false,
        reasoning: None,
        reasoning_details: None,
    }
}

/// Aperçu tête/queue d'un texte volumineux.
pub fn head_tail(text: &str, head_chars: usize, tail_chars: usize) -> (String, String) {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= head_chars + tail_chars {
        return (text.to_string(), String::new());
    }
    let head: String = chars[..head_chars].iter().collect();
    let tail: String = chars[chars.len() - tail_chars..].iter().collect();
    (head, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}),
        }
    }

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new(1, ChatMessage::user("fais le point"), 10),
            Entry::new(
                2,
                ChatMessage::assistant("je regarde")
                    .with_tool_calls(vec![call("c1", "fs_read"), call("c2", "fs_list")]),
                20,
            ),
            Entry::new(3, ChatMessage::tool_result("c1", "fs_read", "contenu"), 50),
            Entry::new(4, ChatMessage::tool_result("c2", "fs_list", "a\nb"), 30),
            Entry::new(5, ChatMessage::assistant("voici le point"), 15),
        ]
    }

    #[test]
    fn grouping_keeps_tool_calls_with_their_results() {
        let g = group(&entries());
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].kind, GroupKind::UserMessage);
        assert_eq!(g[1].kind, GroupKind::ToolGroup);
        assert_eq!(
            g[1].range,
            1..4,
            "l'appel et ses deux résultats sont indivisibles"
        );
        assert_eq!(g[2].kind, GroupKind::AssistantText);
    }

    #[test]
    fn valid_sequence_is_recognised() {
        let msgs: Vec<ChatMessage> = entries().into_iter().map(|e| e.message).collect();
        assert!(pairs_are_valid(&msgs));
    }

    #[test]
    fn orphan_result_is_removed() {
        let msgs = vec![
            ChatMessage::user("a"),
            ChatMessage::tool_result("inconnu", "fs_read", "contenu"),
            ChatMessage::assistant("b"),
        ];
        assert!(!pairs_are_valid(&msgs));
        let fixed = repair_pairs(msgs);
        assert_eq!(fixed.len(), 2);
        assert!(pairs_are_valid(&fixed));
    }

    #[test]
    fn call_without_result_gets_an_error_stub() {
        let msgs = vec![
            ChatMessage::user("a"),
            ChatMessage::assistant("j'appelle").with_tool_calls(vec![call("c1", "fs_read")]),
            ChatMessage::assistant("suite"),
        ];
        assert!(!pairs_are_valid(&msgs));
        let fixed = repair_pairs(msgs);
        assert!(pairs_are_valid(&fixed));
        assert_eq!(fixed[2].role, Role::Tool);
        assert!(fixed[2].text().contains("perdu à la compaction"));
    }

    #[test]
    fn repair_handles_partial_group() {
        // Deux appels, un seul résultat : le second reçoit un stub.
        let msgs = vec![
            ChatMessage::assistant("").with_tool_calls(vec![call("c1", "a"), call("c2", "b")]),
            ChatMessage::tool_result("c1", "a", "ok"),
        ];
        let fixed = repair_pairs(msgs);
        assert!(pairs_are_valid(&fixed));
        assert_eq!(fixed.len(), 3);
    }

    #[test]
    fn repair_is_idempotent() {
        let msgs = vec![ChatMessage::assistant("").with_tool_calls(vec![call("c1", "a")])];
        let once = repair_pairs(msgs);
        let twice = repair_pairs(once.clone());
        assert_eq!(once, twice);
    }

    #[test]
    fn stub_preserves_protocol_fields() {
        let m = ChatMessage::tool_result("c1", "fs_read", "un très long contenu");
        let s = stub_message(&m, "[élidé]");
        assert_eq!(s.role, Role::Tool);
        assert_eq!(s.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(s.name.as_deref(), Some("fs_read"));
        assert_eq!(s.text(), "[élidé]");
    }

    #[test]
    fn head_tail_splits_long_text() {
        let t: String = (0..1000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let (h, tail) = head_tail(&t, 10, 10);
        assert_eq!(h.chars().count(), 10);
        assert_eq!(tail.chars().count(), 10);
        assert!(t.starts_with(&h));
        assert!(t.ends_with(&tail));
    }

    #[test]
    fn head_tail_returns_whole_short_text() {
        let (h, t) = head_tail("court", 10, 10);
        assert_eq!(h, "court");
        assert!(t.is_empty());
    }
}
