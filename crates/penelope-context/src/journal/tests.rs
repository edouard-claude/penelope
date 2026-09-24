use super::*;
use crate::anchors::{Anchor, AnchorKind};
use crate::compaction::AppliedStep;
use crate::tiers::TileMap;
use penelope_llm::types::{Content, ToolCall};
use serde_json::json;

fn every_kind() -> Vec<ConvEvent> {
    vec![
        ConvEvent::System(SystemPayload {
            surface: SurfaceOp::Replace { from: 3, to: 3 },
            hash: "abc".into(),
            rendered: "Tu es Pénélope.".into(),
            tiles: TileMap::default(),
            reason: SystemReason::Cold,
        }),
        ConvEvent::User(UserPayload {
            surface: SurfaceOp::Append,
            source: UserSource::Merged,
            turn_message_id: Some("t_1".into()),
            arrived_at: Some("2026-09-24T08:00:00Z".into()),
            content: vec![
                Content::text("bonjour"),
                Content::ImageUrl {
                    url: "data:image/png;base64,AA".into(),
                    detail: None,
                },
            ],
            episode: 2,
            tokens_est: 12,
            mid_turn: true,
        }),
        ConvEvent::Context(ContextPayload {
            target: 5,
            block: "<contexte>lundi</contexte>\n".into(),
        }),
        ConvEvent::Assistant(Box::new(AssistantPayload {
            turn: Some("t_1".into()),
            step: 2,
            content: vec![Content::text("je lis")],
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "fs_read".into(),
                arguments: json!({"path": "/tmp/a"}),
            }],
            reasoning: Some("réfléchir".into()),
            reasoning_details: Some(json!([{"type": "reasoning.text"}])),
            model: Some("m".into()),
            usage: Some(TokenUsage {
                prompt: 100,
                completion: 7,
                cached: 80,
                cache_write: 0,
                reasoning: 3,
            }),
            cost_usd: Some(0.000_123),
            llm_request_id: Some("r_1".into()),
            request_hash: Some("h".into()),
            projection: Some(ProjectionTrace {
                levels: vec![0, 2],
                steps: vec![AppliedStep {
                    level: 0,
                    label: "micro".into(),
                    before_tokens: 10,
                    after_tokens: 5,
                    touched: 1,
                }],
            }),
            interrupted: true,
            tokens_est: 9,
            ..Default::default()
        })),
        ConvEvent::ToolResult(ToolResultPayload {
            surface: SurfaceOp::Replace { from: 7, to: 7 },
            call_id: "c1".into(),
            tool: "fs_read".into(),
            ok: true,
            eager: true,
            content: vec![Content::text("[externalisé]")],
            artifact_id: Some("a_1".into()),
            artifact_sha256: Some("sha".into()),
            original_tokens: Some(40_000),
            ..Default::default()
        }),
        ConvEvent::Attempt(AttemptPayload {
            turn: Some("t_1".into()),
            step: 1,
            cause: AttemptCause::EmptyAnswer,
            model: None,
            provider: None,
            upstream: None,
            error: None,
            partial_text: Some("début".into()),
            partial_reasoning: None,
            usage: None,
            cost_usd: None,
            llm_request_id: None,
            retry_prompt: Some("Réponds.".into()),
        }),
        ConvEvent::Summary(SummaryPayload {
            surface: SurfaceOp::Replace { from: 1, to: 9 },
            node_id: "n_2".into(),
            previous_node_id: Some("n_1".into()),
            summary: "Résumé.".into(),
            anchors: vec![Anchor {
                kind: AnchorKind::Path,
                value: "/tmp/a".into(),
            }],
            verbatim_users: vec!["bonjour".into()],
            model: Some("m".into()),
            tokens_src: 900,
            tokens_self: 40,
            batches_left: 1,
            trigger: Some("manual".into()),
            idempotency_key: Some("k".into()),
        }),
        ConvEvent::Rewind(RewindPayload {
            surface: SurfaceOp::Cut { after: 4 },
            turns: 2,
            archive_session: Some("s_arch".into()),
        }),
        ConvEvent::Fork(ForkPayload {
            surface: SurfaceOp::Inherit {
                parent: "s_mere".into(),
                up_to: 12,
                offset: 12,
            },
            parent: "s_mere".into(),
            up_to: 12,
            offset: 12,
        }),
        ConvEvent::Import(ImportPayload {
            surface: SurfaceOp::Seal {
                messages: 548,
                offset: 548,
            },
            messages: 548,
            contexts: 112,
            lcm_active: vec![SealedNode {
                node: "n_1".into(),
                from: 1,
                to: 512,
                superseded_by: None,
            }],
            digest: "d".into(),
        }),
    ]
}

/// Va-et-vient : chaque kind se relit à l'identique et porte `"v": 1`.
#[test]
fn every_kind_roundtrips_with_its_version() {
    let events = every_kind();
    let mut kinds: Vec<&str> = events.iter().map(ConvEvent::kind).collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds.len(), 10, "un exemple par kind");
    for e in events {
        let payload = e.payload();
        assert_eq!(payload["v"], json!(1), "{}", e.kind());
        // Le journal stocke le texte canonique : relire après un passage par le texte.
        let text = penelope_kernel::canonical::canonical_json(&payload);
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            ConvEvent::decode(e.kind(), &back).unwrap(),
            Some(e.clone()),
            "{}",
            e.kind()
        );
    }
}

#[test]
fn a_future_version_is_refused() {
    let mut p = every_kind()[1].payload();
    p["v"] = json!(2);
    assert_eq!(
        ConvEvent::decode(KIND_USER, &p).unwrap_err(),
        DeriveError::Format {
            kind: KIND_USER.into(),
            found: 2,
            known: 1
        }
    );
    p.as_object_mut().unwrap().remove("v");
    assert!(matches!(
        ConvEvent::decode(KIND_USER, &p),
        Err(DeriveError::Format { found: 0, .. })
    ));
}

#[test]
fn an_unknown_content_kind_is_refused_unless_ignorable() {
    let p = json!({"v": 1, "x": 1});
    assert_eq!(
        ConvEvent::decode("conv.inconnu", &p).unwrap_err(),
        DeriveError::UnknownKind {
            kind: "conv.inconnu".into()
        }
    );
    let p = json!({"v": 7, "ignorable": true});
    assert_eq!(ConvEvent::decode("conv.inconnu", &p).unwrap(), None);
}

#[test]
fn observation_kinds_and_purged_payloads_are_not_folded() {
    let p = json!({"model": "m"});
    assert_eq!(ConvEvent::decode("turn.started", &p).unwrap(), None);
    assert_eq!(ConvEvent::decode("tool.result", &p).unwrap(), None);
    let purged = json!({"purged": true});
    assert_eq!(ConvEvent::decode(KIND_USER, &purged).unwrap(), None);
}

#[test]
fn a_surface_operation_that_does_not_fit_its_kind_is_refused() {
    let wrong = [
        (KIND_USER, json!({"op": "cut", "after": 1})),
        (
            KIND_TOOL_RESULT,
            json!({"op": "replace", "from": 1, "to": 2}),
        ),
        (KIND_SUMMARY, json!({"op": "replace", "from": 5, "to": 2})),
        (KIND_SUMMARY, json!({"op": "append"})),
    ];
    for (kind, op) in wrong {
        let mut p = every_kind()
            .into_iter()
            .find(|e| e.kind() == kind)
            .unwrap()
            .payload();
        p["surface"] = op.clone();
        assert!(
            matches!(
                ConvEvent::decode(kind, &p),
                Err(DeriveError::Payload { .. })
            ),
            "{kind} {op}"
        );
    }
    // Le fork répète ses bornes dans l'opération : les deux doivent dire la même chose.
    let mut p = every_kind()[8].payload();
    p["up_to"] = json!(3);
    assert!(ConvEvent::decode(KIND_FORK, &p).is_err());
}

#[test]
fn a_malformed_payload_is_named() {
    let e = ConvEvent::decode(KIND_CONTEXT, &json!({"v": 1, "target": "x"})).unwrap_err();
    assert!(
        matches!(e, DeriveError::Payload { ref kind, .. } if kind == KIND_CONTEXT),
        "{e}"
    );
}

#[test]
fn upgrade_is_the_identity_in_v1() {
    let p = json!({"v": 1, "a": [1, 2]});
    assert_eq!(upgrade_payload(KIND_USER, 1, p.clone()).unwrap(), p);
    assert!(upgrade_payload(KIND_USER, 3, p).is_err());
}

#[test]
fn attempt_causes_are_named_as_they_are_serialised() {
    for cause in [
        AttemptCause::StreamCut,
        AttemptCause::BeforeStream,
        AttemptCause::EmptyAnswer,
        AttemptCause::Fallback,
    ] {
        assert_eq!(serde_json::to_value(cause).unwrap(), cause.as_str());
        let p = AttemptPayload::new(cause);
        let back = ConvEvent::decode(KIND_ATTEMPT, &ConvEvent::Attempt(p.clone()).payload());
        assert_eq!(back.unwrap(), Some(ConvEvent::Attempt(p)));
    }
}

/// T14 : relu du journal, un message assistant a les octets de sa ligne V0. Les clés
/// des arguments et des `reasoning_details` gardent l'ordre du fournisseur, un flottant
/// entier garde son `.0` ; une valeur déjà canonique n'est pas doublée.
#[test]
fn free_json_values_keep_the_bytes_the_provider_sent() {
    let args: serde_json::Value =
        serde_json::from_str(r#"{"path":"a.md","content":"x","ratio":1.0}"#).unwrap();
    let details: serde_json::Value =
        serde_json::from_str(r#"[{"type":"reasoning.text","text":"…","index":0}]"#).unwrap();
    let message = penelope_llm::types::ChatMessage {
        tool_calls: vec![
            ToolCall {
                id: "c1".into(),
                name: "fs_write".into(),
                arguments: args,
            },
            ToolCall {
                id: "c2".into(),
                name: "fs_read".into(),
                arguments: json!({"path": "b.md"}),
            },
        ],
        reasoning_details: Some(details),
        ..penelope_llm::types::ChatMessage::assistant("j'écris")
    };
    let Some(event) = message_event(&message, 10, 1, false, &Provenance::default()) else {
        panic!("un message assistant a son événement");
    };
    let text = penelope_kernel::canonical::canonical_json(&event.payload());
    assert!(text.contains(r#""arguments":{"content":"x","path":"a.md","ratio":1}"#));
    let back: serde_json::Value = serde_json::from_str(&text).unwrap();
    let Some(ConvEvent::Assistant(p)) = ConvEvent::decode(KIND_ASSISTANT, &back).unwrap() else {
        panic!("conv.assistant relu");
    };
    assert_eq!(
        p.verbatim.keys().collect::<Vec<_>>(),
        ["reasoning_details", "tool_calls.0.arguments"],
        "l'appel c2, déjà canonique, n'est pas doublé"
    );
    let relu = crate::derive::assistant_node(*p).message;
    assert_eq!(
        serde_json::to_string(&relu).unwrap(),
        serde_json::to_string(&message).unwrap()
    );
}
