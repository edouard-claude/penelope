use super::*;
use crate::journal::*;
use crate::tiers::TileMap;
use penelope_llm::types::{Content, Role, ToolCall};
use serde_json::{Value, json};

/// Un journal de session écrit à la main : `seq` croissant, sans base.
#[derive(Default, Clone)]
struct Journal {
    events: Vec<Event>,
}

impl Journal {
    fn raw(&mut self, kind: &str, payload: Value) -> i64 {
        let seq = self.events.last().map_or(1, |e| e.seq + 1);
        self.events.push(Event {
            id: seq,
            session_id: Some("s".into()),
            run_id: None,
            seq,
            ts: "2026-09-24T08:00:00Z".into(),
            kind: kind.into(),
            payload,
            hash: String::new(),
            prev_hash: String::new(),
        });
        seq
    }
    fn conv(&mut self, e: ConvEvent) -> i64 {
        self.raw(e.kind(), e.payload())
    }
    fn observe(&mut self, kind: &str) -> i64 {
        self.raw(kind, json!({"model": "m"}))
    }
    fn user(&mut self, text: &str) -> i64 {
        self.conv(ConvEvent::User(user(text, false)))
    }
    fn assistant(&mut self, text: &str) -> i64 {
        self.conv(ConvEvent::Assistant(Box::new(AssistantPayload {
            content: vec![Content::text(text)],
            ..Default::default()
        })))
    }
    fn call(&mut self, id: &str) -> i64 {
        self.conv(ConvEvent::Assistant(Box::new(AssistantPayload {
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: "fs_read".into(),
                arguments: json!({}),
            }],
            ..Default::default()
        })))
    }
    fn result(&mut self, id: &str, body: &str) -> i64 {
        self.conv(ConvEvent::ToolResult(tool(id, body, SurfaceOp::Append)))
    }
    fn summary(&mut self, from: i64, to: i64, text: &str) -> i64 {
        self.conv(ConvEvent::Summary(SummaryPayload {
            surface: SurfaceOp::Replace { from, to },
            node_id: format!("n_{to}"),
            previous_node_id: None,
            summary: text.into(),
            anchors: vec![],
            verbatim_users: vec![],
            model: None,
            tokens_src: 0,
            tokens_self: 7,
            batches_left: 0,
            trigger: None,
            idempotency_key: None,
        }))
    }
    fn cut(&mut self, after: i64) -> i64 {
        self.conv(ConvEvent::Rewind(RewindPayload {
            surface: SurfaceOp::Cut { after },
            turns: 1,
            archive_session: None,
        }))
    }
    fn system(&mut self, op: SurfaceOp, text: &str) -> i64 {
        self.conv(ConvEvent::System(SystemPayload {
            surface: op,
            hash: format!("h:{text}"),
            rendered: text.into(),
            tiles: TileMap::default(),
            reason: SystemReason::First,
        }))
    }
    fn fork(&mut self, parent: &str, up_to: i64, offset: i64) -> i64 {
        self.conv(ConvEvent::Fork(ForkPayload {
            surface: SurfaceOp::Inherit {
                parent: parent.into(),
                up_to,
                offset,
            },
            parent: parent.into(),
            up_to,
            offset,
        }))
    }
    fn derive(&self) -> Result<Surface, DeriveError> {
        derive(&Sealed::none(), &self.events)
    }
}

fn user(text: &str, mid_turn: bool) -> UserPayload {
    UserPayload {
        surface: SurfaceOp::Append,
        source: UserSource::Owner,
        turn_message_id: None,
        arrived_at: None,
        content: vec![Content::text(text)],
        episode: 0,
        tokens_est: 3,
        mid_turn,
    }
}

fn tool(id: &str, body: &str, surface: SurfaceOp) -> ToolResultPayload {
    ToolResultPayload {
        surface,
        call_id: id.into(),
        tool: "fs_read".into(),
        ok: true,
        content: vec![Content::text(body)],
        tokens_est: 5,
        ..Default::default()
    }
}

fn texts(messages: &[penelope_llm::types::ChatMessage]) -> Vec<String> {
    messages.iter().map(|m| m.text()).collect()
}

fn bytes(messages: &[penelope_llm::types::ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect()
}

/// Un append seul étend la requête précédente octet pour octet (§3.1).
#[test]
fn appends_only_extend_the_previous_request() {
    let mut j = Journal::default();
    j.observe(KIND_TURN_STARTED);
    j.system(SurfaceOp::Append, "Tu es Pénélope.");
    let u = j.user("bonjour");
    j.conv(ConvEvent::Context(ContextPayload {
        target: u,
        block: "<contexte>lundi</contexte>\n".into(),
    }));
    j.call("c1");
    j.result("c1", "fichier");
    let first = j.derive().unwrap().request_messages("repli");
    j.assistant("voilà");
    j.observe(KIND_TURN_FINISHED);
    j.user("merci");
    let second = j.derive().unwrap().request_messages("repli");
    assert_eq!(
        bytes(&second[..first.len()]),
        bytes(&first),
        "la requête suivante commence par la précédente"
    );
    assert_eq!(
        texts(&second),
        [
            "Tu es Pénélope.",
            "<contexte>lundi</contexte>\nbonjour",
            "",
            "fichier",
            "voilà",
            "merci"
        ]
    );
}

/// Les événements d'observation consomment des `seq` : les adresses ont des trous.
#[test]
fn observation_kinds_are_ignored_and_leave_holes() {
    let mut j = Journal::default();
    let a = j.user("un");
    let b = j.assistant("deux");
    j.observe(KIND_TURN_FINISHED);
    j.observe("tool.result");
    let c = j.user("trois");
    j.observe("context.compacted");
    j.observe(KIND_TURN_STARTED);
    j.observe("llm.retried");
    let d = j.assistant("quatre");
    assert_eq!((a, b, c, d), (1, 2, 5, 9));
    let s = j.derive().unwrap();
    let seqs: Vec<i64> = s.entries().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2, 5, 9]);
    assert_eq!(s.request_messages("S").len(), 5);
}

#[test]
fn a_purged_payload_is_ignored() {
    let mut j = Journal::default();
    j.user("un");
    j.raw(KIND_USER, json!({"purged": true}));
    j.assistant("trois");
    let s = j.derive().unwrap();
    assert_eq!(texts(&s.request_messages("S")), ["S", "un", "trois"]);
    assert!(s.purged);
}

#[test]
fn a_future_format_is_refused_by_the_fold() {
    let mut j = Journal::default();
    let mut p = ConvEvent::User(user("un", false)).payload();
    p["v"] = json!(2);
    j.raw(KIND_USER, p);
    assert!(matches!(
        j.derive(),
        Err(DeriveError::Format { found: 2, .. })
    ));
}

#[test]
fn a_summary_replaces_its_range_and_masks_it() {
    let mut j = Journal::default();
    let a = j.user("un");
    j.assistant("deux");
    j.observe(KIND_TURN_FINISHED);
    let c = j.user("trois");
    j.assistant("quatre");
    j.summary(a, c, "Résumé A");
    let e = j.user("cinq");
    let s = j.derive().unwrap();
    assert_eq!(
        texts(&s.request_messages("S")),
        [
            "S",
            "Résumé de la conversation antérieure (nœud n_4) :\nRésumé A",
            "quatre",
            "cinq"
        ]
    );
    let compacted: Vec<(i64, bool)> = s.entries().iter().map(|e| (e.seq, e.compacted)).collect();
    assert_eq!(
        compacted,
        [(1, true), (2, true), (4, true), (5, false), (7, false)]
    );
    assert_eq!(
        s.projected_entries()[0].seq,
        0,
        "un résumé a l'adresse 0, comme en V0"
    );
    // Prolongation : le nouveau résumé cite l'ancien par une adresse qu'il couvre.
    j.summary(2, 5, "Résumé B");
    let s = j.derive().unwrap();
    assert_eq!(
        texts(&s.request_messages("S"))[1..],
        [
            "Résumé de la conversation antérieure (nœud n_5) :\nRésumé B".to_string(),
            "cinq".into()
        ]
    );
    let b = s.summaries.values().find(|n| n.node_id == "n_5").unwrap();
    assert_eq!((b.from, b.to), (1, 5));
    assert_eq!(e, 7);
}

#[test]
fn an_invalid_replace_is_refused() {
    // Nœud absent.
    let mut j = Journal::default();
    j.user("un");
    j.summary(1, 40, "x");
    assert!(matches!(j.derive(), Err(DeriveError::Surface { .. })));
    // Ordre inversé.
    let mut j = Journal::default();
    j.user("un");
    j.user("deux");
    j.raw(
        KIND_SUMMARY,
        json!({"v": 1, "surface": {"op": "replace", "from": 2, "to": 1}, "node_id": "n", "summary": "x"}),
    );
    assert!(j.derive().is_err());
    // Système non couvert par un système.
    let mut j = Journal::default();
    j.system(SurfaceOp::Append, "A");
    let u = j.user("un");
    j.system(SurfaceOp::Replace { from: u, to: u }, "B");
    assert!(matches!(j.derive(), Err(DeriveError::Surface { .. })));
    // Un second système en append.
    let mut j = Journal::default();
    j.system(SurfaceOp::Append, "A");
    j.system(SurfaceOp::Append, "B");
    assert!(j.derive().is_err());
    // Niveau 1 sur un nœud qui n'est pas le résultat du même appel.
    let mut j = Journal::default();
    j.call("c1");
    let r = j.result("c1", "gros");
    j.conv(ConvEvent::ToolResult(tool(
        "c2",
        "stub",
        SurfaceOp::Replace { from: r, to: r },
    )));
    assert!(j.derive().is_err());
}

#[test]
fn a_system_replace_moves_the_prefix() {
    let mut j = Journal::default();
    let s1 = j.system(SurfaceOp::Append, "A");
    j.user("un");
    j.system(SurfaceOp::Replace { from: s1, to: s1 }, "B");
    let s = j.derive().unwrap();
    assert_eq!(texts(&s.request_messages("S")), ["B", "un"]);
    assert_eq!(s.system.unwrap().seq, 3);
}

/// Niveau 1 : le corps change, l'adresse et la place restent.
#[test]
fn level1_keeps_the_address_of_the_result() {
    let mut j = Journal::default();
    j.call("c1");
    let r = j.result("c1", "très gros");
    let mut stub = tool("c1", "[externalisé]", SurfaceOp::Replace { from: r, to: r });
    stub.artifact_id = Some("a_1".into());
    j.conv(ConvEvent::ToolResult(stub));
    let s = j.derive().unwrap();
    let e = &s.entries()[1];
    assert_eq!(
        (e.seq, e.message.text(), e.artifact_id.as_deref()),
        (2, "[externalisé]".into(), Some("a_1"))
    );
    assert_eq!(e.message.tool_call_id.as_deref(), Some("c1"));
    assert_eq!(s.nodes, [Slot::Message(1), Slot::Message(2)]);
}

#[test]
fn cut_then_append_keeps_growing_addresses() {
    let mut j = Journal::default();
    let a = j.user("un");
    let b = j.assistant("deux");
    j.user("trois");
    j.assistant("quatre");
    j.cut(b);
    let e = j.user("cinq");
    let s = j.derive().unwrap();
    assert_eq!(texts(&s.request_messages("S")), ["S", "un", "deux", "cinq"]);
    let seqs: Vec<i64> = s.entries().iter().map(|x| x.seq).collect();
    assert_eq!(seqs, [a, b, e]);
    assert!(
        e > b + 1,
        "le nœud suivant ne reprend pas juste après ce qui reste"
    );
}

#[test]
fn a_cut_through_a_summary_is_refused() {
    let mut j = Journal::default();
    let a = j.user("un");
    j.assistant("deux");
    let c = j.user("trois");
    j.summary(a, c, "R");
    j.assistant("quatre");
    j.cut(0);
    assert!(matches!(j.derive(), Err(DeriveError::Surface { .. })));
}

/// Fork par référence, sur deux niveaux : la petite-fille voit la grand-mère jusqu'au
/// point du premier fork, la mère jusqu'au second, puis ses propres messages.
#[test]
fn inherit_is_recursive_over_two_levels() {
    let mut root = Journal::default();
    root.system(SurfaceOp::Append, "S");
    root.user("r1");
    let up_to_root = root.assistant("r2");
    root.user("après le fork, invisible");

    let mut child = Journal::default();
    child.fork("root", up_to_root, up_to_root);
    child.observe(KIND_TURN_STARTED);
    let c1 = child.user("c1");
    let up_to_child = child.assistant("c2") + up_to_root;
    assert_eq!(
        c1, 3,
        "adresse dans le journal de la fille : 3 + offset {up_to_root}"
    );

    let mut grand = Journal::default();
    grand.fork("child", up_to_child, up_to_child);
    grand.user("g1");

    let root_prefix = Sealed::none();
    let child_prefix = Sealed::fork("root", &root_prefix, &root.events, up_to_root).unwrap();
    let grand_prefix = Sealed::fork("child", &child_prefix, &child.events, up_to_child).unwrap();
    let s = derive(&grand_prefix, &grand.events).unwrap();
    assert_eq!(
        texts(&s.request_messages("?")),
        ["S", "r1", "r2", "c1", "c2", "g1"]
    );
    let seqs: Vec<i64> = s.entries().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [2, 3, 6, 7, 9]);
}

#[test]
fn a_prefix_must_match_the_inherit_event() {
    let mut root = Journal::default();
    let up = root.user("r1");
    let prefix = Sealed::fork("root", &Sealed::none(), &root.events, up).unwrap();
    let mut other = Journal::default();
    other.fork("autre", up, up);
    assert!(derive(&prefix, &other.events).is_err());
    let mut late = Journal::default();
    late.user("x");
    late.fork("root", up, up);
    assert!(
        derive(&prefix, &late.events).is_err(),
        "héritage seulement en tête"
    );
    let mut none = Journal::default();
    none.user("x");
    assert!(
        derive(&prefix, &none.events).is_err(),
        "préfixe sans héritage"
    );
}

/// Mère purgée : le fork perd son début, ses remplacements masquent ce qui reste.
#[test]
fn a_purged_parent_leaves_a_lenient_fork() {
    let mut root = Journal::default();
    root.raw(KIND_USER, json!({"purged": true}));
    let up = root.raw(KIND_ASSISTANT, json!({"purged": true}));
    let prefix = Sealed::fork("root", &Sealed::none(), &root.events, up).unwrap();
    let mut child = Journal::default();
    child.fork("root", up, up);
    child.user("c1");
    child.summary(1, 3, "R");
    let s = derive(&prefix, &child.events).unwrap();
    assert!(s.purged);
    assert_eq!(
        texts(&s.request_messages("S"))[1],
        "Résumé de la conversation antérieure (nœud n_3) :\nR"
    );
}

#[test]
fn an_attempt_adds_the_retry_prompt_until_an_answer() {
    let mut j = Journal::default();
    j.observe(KIND_TURN_STARTED);
    j.user("un");
    j.conv(ConvEvent::Attempt(AttemptPayload {
        turn: None,
        step: 1,
        cause: AttemptCause::EmptyAnswer,
        model: None,
        provider: None,
        upstream: None,
        error: None,
        partial_text: None,
        partial_reasoning: None,
        usage: None,
        cost_usd: None,
        llm_request_id: None,
        retry_prompt: Some("Relance.".into()),
    }));
    let m = j.derive().unwrap().request_messages("S");
    assert_eq!(texts(&m), ["S", "un", "Relance."]);
    assert_eq!(m[2].role, Role::User);
    j.assistant("deux");
    assert_eq!(
        texts(&j.derive().unwrap().request_messages("S")),
        ["S", "un", "deux"]
    );
}

/// La note de fusion suit les messages système de tête, résumés compris, et disparaît
/// au tour suivant.
#[test]
fn the_merge_note_follows_the_leading_system_messages() {
    let mut j = Journal::default();
    let a = j.user("un");
    let b = j.assistant("deux");
    j.summary(a, b, "R");
    j.observe(KIND_TURN_STARTED);
    j.user("trois");
    j.conv(ConvEvent::User(user("pendant", true)));
    let m = j.derive().unwrap().request_messages("S");
    assert_eq!(m[2].text(), MERGE_NOTE);
    assert_eq!(texts(&m)[3..], ["trois".to_string(), "pendant".into()]);
    j.observe(KIND_TURN_FINISHED);
    assert!(!j.derive().unwrap().merge_note);
}

#[test]
fn a_sealed_prefix_comes_first_with_its_summaries() {
    use crate::transcript::Entry;
    use penelope_llm::types::ChatMessage;
    let mut e1 = Entry::new(1, ChatMessage::user("v0 un"), 3);
    e1.compacted = true;
    let e2 = Entry::new(2, ChatMessage::assistant("v0 deux"), 3);
    let e3 = Entry::new(3, ChatMessage::user("v0 trois"), 3);
    let contexts = [(3, "<c>".to_string())].into_iter().collect();
    let prefix = Sealed::import(
        vec![e1, e2, e3],
        contexts,
        vec![SealedSummary {
            node_id: "n_v0".into(),
            summary: "ancien".into(),
            tokens_self: 5,
            from: 1,
            to: 1,
        }],
    );
    let mut j = Journal::default();
    j.observe(KIND_TURN_STARTED);
    j.conv(ConvEvent::Import(ImportPayload {
        surface: SurfaceOp::Seal {
            messages: 3,
            offset: 3,
        },
        messages: 3,
        contexts: 1,
        lcm_active: vec![],
        digest: "d".into(),
    }));
    let u = j.user("v1");
    j.summary(1, 2, "prolongé");
    let s = derive(&prefix, &j.events).unwrap();
    assert_eq!(u, 3, "seq de l'événement");
    assert_eq!(
        texts(&s.request_messages("S")),
        [
            "S",
            "Résumé de la conversation antérieure (nœud n_2) :\nprolongé",
            "<c>v0 trois",
            "v1"
        ]
    );
    let seqs: Vec<i64> = s.entries().iter().map(|e| e.seq).collect();
    assert_eq!(
        seqs,
        [1, 2, 3, 6],
        "le premier nœud du journal suit le préfixe"
    );
}

#[test]
fn events_out_of_order_are_refused() {
    let mut j = Journal::default();
    j.user("un");
    j.user("deux");
    j.events.swap(0, 1);
    assert!(j.derive().is_err());
}

/// T14 : une fille hérite des messages de sa mère sans leur contexte figé, comme la
/// copie V0 ; les contextes de ses propres messages restent.
#[test]
fn a_fork_inherits_messages_without_their_frozen_context() {
    let mut root = Journal::default();
    let r1 = root.user("r1");
    root.conv(ConvEvent::Context(ContextPayload {
        target: r1,
        block: "<ctx mère>".into(),
    }));
    let up = root.assistant("r2");
    let mut child = Journal::default();
    child.fork("root", up, up);
    let c1 = child.user("c1") + up;
    child.conv(ConvEvent::Context(ContextPayload {
        target: c1,
        block: "<ctx fille>".into(),
    }));
    let prefix = Sealed::fork("root", &Sealed::none(), &root.events, up).unwrap();
    assert!(prefix.surface().contexts.is_empty());
    let s = derive(&prefix, &child.events).unwrap();
    assert_eq!(
        texts(&s.request_messages("S")),
        ["S", "r1", "r2", "<ctx fille>c1"]
    );
}
