//! Reconstitution d'une requête (issue #205).
//!
//! `penelope audit show --turn <id>` remonte ce que le modèle avait sous les yeux : le
//! prompt système depuis son instantané, les messages depuis le transcript, les outils
//! depuis leur empreinte. Ce qui manque est dit, jamais comblé : un prompt reconstitué
//! n'est pas la requête envoyée si l'historique a été réécrit depuis (§5.4 niveaux 1 et 3),
//! et l'outil doit le déclarer plutôt que présenter un résultat faux comme exact.

use crate::prompt_snapshot;
use penelope_app::services::Services;
use penelope_kernel::budget::{miss_label, tile_label};
use penelope_observe::redact::redact;
use penelope_store::rusqlite::OptionalExtension;
use serde_json::{Value, json};

/// Un appel au modèle, tel que `usage` l'a gardé.
#[derive(Debug, Clone)]
struct Call {
    ts: String,
    session_id: Option<String>,
    model: String,
    prompt: i64,
    cached: i64,
    msg_count: Option<i64>,
    system_hash: Option<String>,
    tools_hash: Option<String>,
    request_hash: Option<String>,
    miss_cause: Option<String>,
}

/// Reconstitue le ou les appels d'un tour.
pub async fn show(s: &Services, turn_id: &str) -> anyhow::Result<Value> {
    let calls = calls_of_turn(s, turn_id).await?;
    if calls.is_empty() {
        anyhow::bail!("tour inconnu, ou consommation déjà purgée : {turn_id}");
    }
    // Un tour peut porter des appels auxiliaires (routage, titre) sans empreinte : c'est
    // le dernier appel de conversation qui dit ce que le modèle a lu.
    let last = calls
        .iter()
        .rfind(|c| c.system_hash.is_some())
        .cloned()
        .unwrap_or_else(|| calls[calls.len() - 1].clone());
    let session_id = last.session_id.clone();

    let mut reserves: Vec<String> = Vec::new();
    let system = match &last.system_hash {
        Some(hash) => match prompt_snapshot::get(s, hash).await? {
            Some(snap) => rendered_prompt(&snap, hash, &mut reserves),
            None => {
                reserves.push(format!(
                    "aucun instantané pour le prompt système {} : il a été purgé, ou l'appel \
                     est antérieur à la 0.17.59",
                    &hash[..hash.len().min(12)]
                ));
                json!({"empreinte": hash, "exact": false})
            }
        },
        None => {
            reserves.push(
                "cet appel ne porte pas d'empreinte de prompt système : rien à relire".into(),
            );
            Value::Null
        }
    };

    let messages = match &session_id {
        Some(sid) => transcript(s, sid, &last, &mut reserves).await?,
        None => {
            reserves.push("appel hors session : pas de transcript à relire".into());
            Vec::new()
        }
    };

    let mut out = json!({
        "tour": turn_id,
        "session": session_id,
        "appels": calls.iter().map(call_json).collect::<Vec<_>>(),
        "prompt_systeme": system,
        "messages": messages,
        "outils": {
            "empreinte": last.tools_hash,
            "liste": "non conservée : seule l'empreinte dit si la liste a changé",
        },
    });
    out["exact"] = json!(reserves.is_empty());
    if !reserves.is_empty() {
        out["reserves"] = json!(reserves);
    }
    Ok(out)
}

fn call_json(c: &Call) -> Value {
    json!({
        "ts": c.ts,
        "modele": c.model,
        "tokens_entree": c.prompt,
        "tokens_en_cache": c.cached,
        "messages": c.msg_count,
        "system_hash": c.system_hash,
        "tools_hash": c.tools_hash,
        "request_hash": c.request_hash,
        "cache": c.miss_cause.as_deref().map(|m| json!({
            "cause": m,
            "libelle": miss_label(m),
        })),
    })
}

/// Le prompt système relu, vérifié par l'empreinte déjà enregistrée, et découpé en tuiles
/// quand la découpe est connue. Le texte sort **rédigé** : un prompt peut porter un
/// secret écrit à la main dans le vault (régression de #134).
fn rendered_prompt(
    snap: &prompt_snapshot::Snapshot,
    hash: &str,
    reserves: &mut Vec<String>,
) -> Value {
    let exact = penelope_kernel::canonical::sha256_hex(snap.rendered.as_bytes()) == hash;
    if !exact {
        reserves.push("l'instantané ne retombe pas sur son empreinte".into());
    }
    let tiles = snap.tiles.as_ref().map(|map| {
        ["T0", "T1", "T2"]
            .into_iter()
            .map(|name| {
                json!({
                    "nom": name,
                    "libelle": tile_label(name),
                    "texte": map.slice(&snap.rendered, name).map(redact),
                })
            })
            .collect::<Vec<_>>()
    });
    json!({
        "empreinte": hash,
        "exact": exact,
        "octets": snap.rendered.len(),
        "appels": snap.uses,
        "vu_la_premiere_fois": snap.first_seen_at,
        "vu_la_derniere_fois": snap.last_seen_at,
        "rendu": redact(&snap.rendered),
        "tuiles": tiles,
    })
}

/// Les messages tels qu'ils étaient au moment de l'appel, avec le contexte volatil figé
/// avec chacun (#17). Ce qui a été résumé depuis est signalé, pas reconstruit.
async fn transcript(
    s: &Services,
    session_id: &str,
    call: &Call,
    reserves: &mut Vec<String>,
) -> anyhow::Result<Vec<Value>> {
    if let Some(hash) = &call.request_hash
        && let Some(view) = s
            .context
            .history
            .call_view(session_id, hash)
            .await?
            .map_err(anyhow::Error::msg)?
    {
        return Ok(replayed(&view, call, reserves));
    }
    let entries = s.context.history.load(session_id, 0).await?;
    let sid = session_id.to_string();
    let frozen: std::collections::HashMap<i64, String> = s
        .store
        .read(move |c| {
            let mut st =
                c.prepare("SELECT seq, context FROM message_context WHERE session_id = ?1")?;
            let rows = st.query_map([&sid], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut m = std::collections::HashMap::new();
            for r in rows {
                let (seq, ctx) = r?;
                m.insert(seq, ctx);
            }
            Ok(m)
        })
        .await?;

    // La projection de l'appel s'arrête où `msg_count` le dit : ce qui a été écrit
    // après (la réponse du modèle, le tour suivant) n'en faisait pas partie.
    let mut entries = entries;
    if let Some(n) = call.msg_count {
        let want = (n - 1).max(0) as usize;
        if entries.len() < want {
            reserves.push(format!(
                "l'appel portait {n} messages, le transcript n'en garde que {} : des \
                 messages ont été effacés depuis",
                entries.len() + 1
            ));
        }
        entries.truncate(want);
    } else {
        reserves.push("l'appel ne dit pas combien de messages il portait".into());
    }

    if entries.iter().any(|e| e.compacted) {
        reserves.push(
            "des messages de cette session ont été résumés depuis : le transcript relu n'est \
             plus celui de l'appel"
                .into(),
        );
    }
    let out: Vec<Value> = entries
        .iter()
        .map(|e| {
            json!({
                "seq": e.seq,
                "role": e.message.role.as_str(),
                "texte": redact(&e.message.text()),
                "contexte_fige": frozen.get(&e.seq).map(|c| redact(c)),
                "resume_depuis": e.compacted,
            })
        })
        .collect();
    Ok(out)
}

/// Les messages de l'appel repliés depuis le journal jusqu'à sa réponse (T15) : les
/// résumés de l'époque, pas ceux d'aujourd'hui ; ce qui a été résumé depuis y est en clair.
fn replayed(
    view: &penelope_context::CallView,
    call: &Call,
    reserves: &mut Vec<String>,
) -> Vec<Value> {
    if let Some(n) = call.msg_count
        && n != view.messages as i64
    {
        reserves.push(format!(
            "l'appel portait {n} messages, le journal en replie {}",
            view.messages
        ));
    }
    view.nodes
        .iter()
        .map(|n| {
            json!({
                "seq": n.seq,
                "role": n.message.role.as_str(),
                "texte": redact(&n.message.text()),
                "contexte_fige": n.context.as_deref().map(redact),
                "resume_depuis": false,
            })
        })
        .collect()
}

async fn calls_of_turn(s: &Services, turn_id: &str) -> anyhow::Result<Vec<Call>> {
    let turn = turn_id.to_string();
    Ok(s.store
        .read(move |c| {
            let mut st = c.prepare(
                "SELECT ts, session_id, model, prompt, cached, msg_count, system_hash,
                        tools_hash, request_hash, miss_cause
                 FROM usage WHERE turn_id = ?1 ORDER BY ts, rowid",
            )?;
            let rows = st.query_map([&turn], |r| {
                Ok(Call {
                    ts: r.get(0)?,
                    session_id: r.get(1)?,
                    model: r.get(2)?,
                    prompt: r.get(3)?,
                    cached: r.get(4)?,
                    msg_count: r.get(5)?,
                    system_hash: r.get(6)?,
                    tools_hash: r.get(7)?,
                    request_hash: r.get(8)?,
                    miss_cause: r.get(9)?,
                })
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            Ok(v)
        })
        .await?)
}

/// Dernier tour d'une session, pour `--turn` omis.
pub async fn last_turn(s: &Services, session_id: &str) -> anyhow::Result<Option<String>> {
    let sid = session_id.to_string();
    Ok(s.store
        .read(move |c| {
            Ok(c.query_row(
                "SELECT turn_id FROM usage
                  WHERE session_id = ?1 AND turn_id IS NOT NULL
                  ORDER BY ts DESC, rowid DESC LIMIT 1",
                [sid],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Daemon;
    use penelope_app::bus::Origin;
    use penelope_kernel::clock::TestClock;
    use penelope_llm::mock::MockProvider;
    use penelope_llm::types::ChatMessage;
    use std::sync::Arc;

    async fn one_turn(text: &str) -> (tempfile::TempDir, Arc<Daemon>, String) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Voilà.");
        d.enqueue_message(&sid, text, &Origin::Cli, None)
            .await
            .unwrap();
        let t = s.turns.claim("test").await.unwrap().unwrap();
        d.run_turn(&t).await;
        s.turns.complete(&t).await.unwrap();
        (dir, d, sid)
    }

    /// #205 : le prompt système relu est celui qui est parti, prouvé par l'empreinte déjà
    /// enregistrée ; les messages et la découpe en tuiles l'accompagnent.
    #[tokio::test]
    async fn a_turn_is_replayed_from_its_fingerprint() {
        let (_dir, d, sid) = one_turn("où en es-tu ?").await;
        let s = &d.services;
        let turn = last_turn(s, &sid).await.unwrap().expect("un tour");
        let v = show(s, &turn).await.unwrap();

        assert_eq!(v["exact"], true, "{}", v["reserves"]);
        let prompt = &v["prompt_systeme"];
        assert_eq!(prompt["exact"], true);
        let rendu = prompt["rendu"].as_str().unwrap();
        assert!(rendu.contains("agent personnel autonome"), "{rendu}");
        assert_eq!(
            penelope_kernel::canonical::sha256_hex(rendu.as_bytes()),
            prompt["empreinte"].as_str().unwrap(),
            "le prompt rendu retombe octet pour octet sur son empreinte"
        );
        let tuiles = prompt["tuiles"].as_array().unwrap();
        assert_eq!(tuiles.len(), 3);
        assert_eq!(tuiles[0]["nom"], "T0");
        assert!(
            tuiles[0]["texte"]
                .as_str()
                .unwrap()
                .contains("agent personnel autonome")
        );
        let messages = v["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["texte"].as_str().unwrap().contains("où en es-tu ?")),
            "{messages:?}"
        );
        let appels = v["appels"].as_array().unwrap();
        assert!(
            appels.iter().any(|a| a["request_hash"].is_string()),
            "{appels:?}"
        );
    }

    /// T15 : après une compaction, la requête d'un tour passé se replie depuis le journal
    /// jusqu'à l'appel : les messages résumés depuis y sont en clair, sans le résumé
    /// d'aujourd'hui, et la reconstitution reste exacte.
    #[tokio::test]
    async fn a_turn_is_replayed_exactly_after_a_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(TestClock::default());
        let s = Arc::new(
            Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        let d = Arc::new(Daemon::from_services(s.clone()));
        let p = Arc::new(MockProvider::new());
        d.set_provider_override(p.clone());
        let sid = d.chat_session_for(&Origin::Cli).await.unwrap();
        let h = &s.context.history;
        for i in 0..20 {
            let q = format!("question {i} sur PROJ-7 : {}", "détail ".repeat(340));
            let a = format!("réponse {i} : {}", "analyse ".repeat(300));
            h.append(&sid, &ChatMessage::user(q), 600, 0, false, None)
                .await
                .unwrap();
            h.append(&sid, &ChatMessage::assistant(a), 600, 0, false, None)
                .await
                .unwrap();
        }
        p.reply(r#"{"complexity":"medium"}"#);
        p.reply("Voilà.");
        d.enqueue_message(&sid, "où en es-tu ?", &Origin::Cli, None)
            .await
            .unwrap();
        let t = s.turns.claim("test").await.unwrap().unwrap();
        d.run_turn(&t).await;
        s.turns.complete(&t).await.unwrap();
        let turn = last_turn(&s, &sid).await.unwrap().expect("un tour");
        let before = show(&s, &turn).await.unwrap();
        assert_eq!(before["exact"], true, "{}", before["reserves"]);

        p.reply(r#"{"objectif": "résumé d'aujourd'hui"}"#);
        let r = penelope_conversation::compaction::compact(
            &crate::compaction::context_of(&d),
            &sid,
            penelope_conversation::compaction::Trigger::Manual,
            None,
        )
        .await
        .unwrap();
        assert_eq!(r.published, 1, "{r:?}");
        let history = h.load(&sid, 0).await.unwrap();
        assert!(
            history.iter().any(|e| e.compacted),
            "la compaction a masqué"
        );

        let v = show(&s, &turn).await.unwrap();
        assert_eq!(v["exact"], true, "{}", v["reserves"]);
        assert_eq!(v["messages"], before["messages"], "la requête de l'époque");
        let text = v["messages"].to_string();
        assert!(text.contains("question 0 sur PROJ-7"), "{text}");
        assert!(!text.contains("résumé d'aujourd'hui"), "{text}");
    }

    /// Un tour inconnu ne rend pas une reconstitution vide : il le dit.
    #[tokio::test]
    async fn an_unknown_turn_is_an_error_not_an_empty_answer() {
        let (_dir, d, _sid) = one_turn("bonjour").await;
        let e = show(&d.services, "t_inconnu").await.unwrap_err();
        assert!(e.to_string().contains("tour inconnu"), "{e}");
    }

    /// Prompt purgé : la reconstitution le dit au lieu de rendre un texte qui n'est pas
    /// celui qu'a lu le modèle.
    #[tokio::test]
    async fn a_purged_prompt_is_announced_not_invented() {
        let (_dir, d, sid) = one_turn("bonjour").await;
        let s = &d.services;
        s.store
            .write(|tx| {
                tx.execute("DELETE FROM prompt_snapshots", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let turn = last_turn(s, &sid).await.unwrap().unwrap();
        let v = show(s, &turn).await.unwrap();
        assert_eq!(v["exact"], false);
        assert!(
            v["reserves"][0]
                .as_str()
                .unwrap()
                .contains("aucun instantané"),
            "{v}"
        );
    }

    /// #134 : un secret écrit à la main dans le prompt ne ressort pas en clair.
    #[tokio::test]
    async fn a_secret_inside_the_prompt_never_comes_back_in_the_clear() {
        let (_dir, d, sid) = one_turn("bonjour").await;
        let s = &d.services;
        let token = "sk-ant-api03-0123456789abcdefghijklmnopqrstuvwxyz0123456789";
        let rendered = format!("Tu es Pénélope. Jeton : {token}");
        let hash = penelope_kernel::canonical::sha256_hex(rendered.as_bytes());
        let (h, r) = (hash.clone(), rendered.clone());
        s.store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO prompt_snapshots(hash, rendered, first_seen_at, last_seen_at,
                        uses) VALUES(?1,?2,'2026-09-01T00:00:00Z','2026-09-01T00:00:00Z',1)",
                    penelope_store::rusqlite::params![h, r],
                )?;
                tx.execute("UPDATE usage SET system_hash = ?1", [&h])?;
                Ok(())
            })
            .await
            .unwrap();
        let turn = last_turn(s, &sid).await.unwrap().unwrap();
        let v = show(s, &turn).await.unwrap();
        let text = v.to_string();
        assert!(!text.contains(token), "secret en clair : {text}");
        assert!(text.contains("Tu es Pénélope."));
    }
}
