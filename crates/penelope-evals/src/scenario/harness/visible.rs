//! Ce que le modèle lit est journalisé, et le journal ne réécrit rien au milieu d'un tour
//! (épopée #208, T22 ; `design/v1/source-de-verite.md` §2.1 invariant 1 et §3.2).
//!
//! Deux contrôles sur la base que le scénario laisse, avant la refonte de `journal.rs` :
//!
//! 1. **Chaque appel est le pliage du journal.** Pour chaque réponse du modèle
//!    (`conv.assistant`) et chaque tentative sans réponse (`conv.attempt`), la requête
//!    reçue par le fournisseur, retrouvée par son empreinte (`request_hash`), est
//!    comparée octet pour octet, corps « chat completions » compris, à
//!    `derive_until(journal, avant l'événement).request_messages()`. Le pliage est refait
//!    ici avec les seules fonctions pures de `penelope-context` (préfixe d'un fork
//!    compris), sans passer par la lecture du daemon qu'il contrôle. Seul le marqueur
//!    `cache_control` est retiré de la requête : il se déplace d'un appel à l'autre sans
//!    être du contenu, et l'empreinte l'ignore déjà. Un appel dont la réponse porte
//!    `projection` (niveaux 0, 2 ou 4, fonctions pures non journalisées) est compté à
//!    part, pas comparé.
//! 2. **Aucun remplacement entre deux appels d'un même tour**, sauf deux cas où le cache
//!    ne perd rien : le niveau 1 d'un résultat d'outil ajouté depuis l'appel précédent
//!    (jamais envoyé), et le résumé d'un dépassement prouvé après une tentative refusée
//!    avant le flux (la requête refusée n'a rien mis en cache).

use penelope_context::derive::{Sealed, derive_until};
use penelope_daemon::Services;
use penelope_daemon::cache_audit::Fingerprint;
use penelope_kernel::event::Event;
use penelope_llm::provider::to_openai_body;
use penelope_llm::types::ChatRequest;
use penelope_store::rusqlite;
use serde_json::Value;
use std::collections::BTreeMap;

/// Ce que les contrôles ont vu, pour qu'un critère d'acceptation vérifie qu'il n'a pas
/// réussi à vide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Visible {
    /// Appels comparés octet pour octet à leur pliage.
    pub compared: usize,
    /// Appels dont la requête a été réduite par un niveau 0, 2 ou 4 (non comparés).
    pub transformed: usize,
    /// Remplacements admis entre deux appels d'un même tour, par kind.
    pub within_turn: BTreeMap<String, usize>,
}

const ASSISTANT: &str = "conv.assistant";
const ATTEMPT: &str = "conv.attempt";

/// Les événements de pliage de chaque session, et l'empreinte de chaque requête LLM.
type Journal = (BTreeMap<String, Vec<Event>>, BTreeMap<String, String>);

async fn journal(s: &Services) -> anyhow::Result<Journal> {
    Ok(s.store
        .read(|c| {
            let mut sessions: BTreeMap<String, Vec<Event>> = BTreeMap::new();
            let mut st = c.prepare(
                "SELECT id, session_id, run_id, seq, ts, kind, payload, hash, prev_hash
                 FROM events
                 WHERE session_id IS NOT NULL
                   AND (kind LIKE 'conv.%' OR kind IN ('turn.started', 'turn.finished'))
                 ORDER BY session_id, seq",
            )?;
            let rows = st.query_map([], |r| {
                let payload: String = r.get(6)?;
                Ok(Event {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    run_id: r.get(2)?,
                    seq: r.get(3)?,
                    ts: r.get(4)?,
                    kind: r.get(5)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                    hash: r.get(7)?,
                    prev_hash: r.get(8)?,
                })
            })?;
            for e in rows {
                let e = e?;
                let sid = e.session_id.clone().unwrap_or_default();
                sessions.entry(sid).or_default().push(e);
            }
            let mut st = c.prepare(
                "SELECT id, request_hash FROM llm_requests WHERE request_hash IS NOT NULL",
            )?;
            let hashes = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<BTreeMap<String, String>>>()?;
            Ok((sessions, hashes))
        })
        .await?)
}

/// Le préfixe hérité d'une session et son offset : rien, ou la mère d'un fork repliée
/// jusqu'à `up_to`, récursivement.
fn prefix(
    sessions: &BTreeMap<String, Vec<Event>>,
    sid: &str,
    depth: usize,
) -> anyhow::Result<(Sealed, i64)> {
    anyhow::ensure!(depth < 16, "chaîne de forks trop longue sous {sid}");
    let events = sessions.get(sid).map(Vec::as_slice).unwrap_or_default();
    let Some(head) = events.iter().find(|e| e.kind.starts_with("conv.")) else {
        return Ok((Sealed::none(), 0));
    };
    match head.kind.as_str() {
        "conv.fork" => {
            let p = &head.payload;
            let parent = p["parent"].as_str().unwrap_or_default();
            let (up_to, offset) = (p["up_to"].as_i64(), p["surface"]["offset"].as_i64());
            let (Some(up_to), Some(offset)) = (up_to, offset) else {
                anyhow::bail!("session {sid} : `conv.fork` sans `up_to` ni offset : {p}");
            };
            let (grand, _) = prefix(sessions, parent, depth + 1)?;
            let parent_events = sessions.get(parent).map(Vec::as_slice).unwrap_or_default();
            Ok((Sealed::fork(parent, &grand, parent_events, up_to)?, offset))
        }
        "conv.import" => anyhow::bail!(
            "session {sid} : préfixe scellé, que ce contrôle ne sait pas replier (aucun \
             scénario n'en a)"
        ),
        _ => Ok((Sealed::none(), 0)),
    }
}

/// L'empreinte de la requête qui a produit l'événement, si c'est un appel.
fn call_hash<'a>(e: &'a Event, hashes: &'a BTreeMap<String, String>) -> Option<&'a str> {
    match e.kind.as_str() {
        ASSISTANT => e.payload["request_hash"].as_str(),
        ATTEMPT => e.payload["llm_request_id"]
            .as_str()
            .and_then(|id| hashes.get(id))
            .map(String::as_str),
        _ => None,
    }
}

/// Les messages d'un corps « chat completions », tels qu'ils partent sur le fil.
fn wire(req: &ChatRequest) -> Vec<String> {
    to_openai_body(req)["messages"]
        .as_array()
        .map(|ms| ms.iter().map(Value::to_string).collect())
        .unwrap_or_default()
}

pub(super) async fn check(
    s: &Services,
    seen: &[ChatRequest],
    name: &str,
) -> anyhow::Result<Visible> {
    let (sessions, hashes) = journal(s).await?;
    let sent: BTreeMap<String, &ChatRequest> = seen
        .iter()
        .filter_map(|r| {
            Fingerprint::of(&r.messages, &r.tools)
                .request_hash()
                .map(|h| (h, r))
        })
        .collect();
    let mut out = Visible::default();
    for (sid, events) in &sessions {
        let (sealed, offset) = prefix(&sessions, sid, 0)?;
        for e in events {
            let Some(hash) = call_hash(e, &hashes) else {
                continue;
            };
            let Some(req) = sent.get(hash) else {
                anyhow::bail!(
                    "scénario {name}, session {sid} : l'appel de `{}` (seq {}) cite une requête \
                     que le fournisseur n'a jamais reçue",
                    e.kind,
                    e.seq
                );
            };
            if e.payload.get("projection").is_some() {
                out.transformed += 1;
                continue;
            }
            let surface = derive_until(&sealed, events, offset + e.seq - 1)?;
            let derived = ChatRequest {
                messages: surface.request_messages(""),
                ..(*req).clone()
            };
            let mut sent = (*req).clone();
            for m in &mut sent.messages {
                m.cache_marker = false;
            }
            let (want, got) = (wire(&derived), wire(&sent));
            if want != got {
                let at = want.iter().zip(&got).take_while(|(a, b)| a == b).count();
                anyhow::bail!(
                    "scénario {name}, session {sid} : la requête de `{}` (seq {}) n'est pas le \
                     pliage du journal ({} messages dérivés, {} envoyés), premier écart au \
                     message {at} :\n  journal {}\n  envoyé  {}",
                    e.kind,
                    e.seq,
                    want.len(),
                    got.len(),
                    want.get(at).map_or("(rien)", String::as_str),
                    got.get(at).map_or("(rien)", String::as_str),
                );
            }
            out.compared += 1;
        }
        for (kind, n) in replaces_within_turns(events, offset, &hashes)
            .map_err(|e| anyhow::anyhow!("scénario {name}, session {sid} : {e}"))?
        {
            *out.within_turn.entry(kind).or_default() += n;
        }
    }
    Ok(out)
}

/// Les remplacements écrits entre deux appels d'un même tour (`turn.started` …
/// `turn.finished`), par kind ; une erreur pour tout remplacement qui n'est pas l'un des
/// deux cas admis (voir l'en-tête).
pub(super) fn replaces_within_turns(
    events: &[Event],
    offset: i64,
    hashes: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, usize>, String> {
    let mut out = BTreeMap::new();
    let mut in_turn = false;
    // Le dernier appel du tour, et les remplacements écrits depuis.
    let mut last_call: Option<&Event> = None;
    let mut pending: Vec<&Event> = Vec::new();
    for e in events {
        match e.kind.as_str() {
            "turn.started" => {
                (in_turn, last_call) = (true, None);
                pending.clear();
            }
            "turn.finished" => {
                (in_turn, last_call) = (false, None);
                pending.clear();
            }
            _ if in_turn && call_hash(e, hashes).is_some() => {
                if let Some(previous) = last_call {
                    for r in pending.drain(..) {
                        admitted(r, previous, offset)?;
                        *out.entry(r.kind.clone()).or_default() += 1;
                    }
                }
                last_call = Some(e);
            }
            _ if in_turn && last_call.is_some() && rewrites(e) => pending.push(e),
            _ => {}
        }
    }
    Ok(out)
}

/// L'événement réécrit-il la surface (remplacement, coupe, héritage, scellement) ?
fn rewrites(e: &Event) -> bool {
    e.kind.starts_with("conv.")
        && e.payload["surface"]["op"]
            .as_str()
            .is_some_and(|op| op != "append")
}

/// Un remplacement entre l'appel `previous` et le suivant est-il l'un des cas admis ?
fn admitted(r: &Event, previous: &Event, offset: i64) -> Result<(), String> {
    let surface = &r.payload["surface"];
    let (from, to) = (surface["from"].as_i64(), surface["to"].as_i64());
    let level_one = r.kind == "conv.tool_result"
        && surface["op"] == "replace"
        && from.is_some()
        && from == to
        && from.is_some_and(|a| a > offset + previous.seq);
    let proven_overflow = r.kind == "conv.summary"
        && r.payload["trigger"] == "overflow"
        && previous.kind == ATTEMPT
        && previous.payload["cause"] == "before_stream";
    if level_one || proven_overflow {
        return Ok(());
    }
    Err(format!(
        "`{}` (seq {}) réécrit la surface entre deux appels du même tour (après `{}` seq {}) : \
         {}",
        r.kind, r.seq, previous.kind, previous.seq, r.payload["surface"]
    ))
}
