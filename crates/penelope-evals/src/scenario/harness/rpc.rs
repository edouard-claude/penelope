//! Étape `rpc` (épopée #208, critère 7, règle R10) : une méthode de `api::method` appelée
//! comme la socket locale la sert, `Rpc::handle` ou `handle_streaming` pour `chat.stream`
//! et `tail`. Les tours que l'appel met en file sont joués pendant qu'il attend, un par
//! un, comme le pool de runners les jouerait ; leurs issues entrent dans celle de l'étape.
//!
//! La réponse est gardée entière, ou réduite à des pointeurs (`pick`) quand une partie
//! dépend de la machine, puis normalisée avec le reste du monde. Le contrat de forme de
//! chaque méthode est `rpc_golden` ; ici compte ce que l'appel a fait au monde.
//!
//! Aussi : les relevés `[[observe]]` (requêtes en lecture sur la base après le run) et
//! les serveurs MCP simulés derrière le vrai superviseur (`[[mcp_servers]]`).

use super::journal::sql_json;
use super::{HEARTBEAT, Harness, outcome_json};
use crate::scenario::{McpServer, Observe};
use anyhow::Context as _;
use penelope_app::bus::Origin;
use penelope_app::services::Services;
use penelope_daemon::rpc::Rpc;
use penelope_daemon::{Daemon, runner};
use penelope_kernel::api::{RpcRequest, method};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Au-delà, l'appel est tenu pour bloqué et le scénario échoue en le disant.
const RPC_WAIT: Duration = Duration::from_secs(120);
/// Pas d'attente entre deux regards sur la file des tours.
const POLL: Duration = Duration::from_millis(5);

/// Une étape `rpc`, empruntée au `Spec`.
pub(super) struct Call<'s> {
    pub method: &'s str,
    pub params: &'s toml::Table,
    pub bind: Option<&'s str>,
    pub pick: &'s [String],
    pub mask: &'s [String],
    pub lines: &'s [String],
    pub error: bool,
    pub during: Option<&'s str>,
}

/// Ce que l'appel a rendu.
struct Reply {
    result: Option<Value>,
    error: Option<Value>,
    /// Notifications d'un flux (`chat.stream` : entières ; `tail` : leurs types).
    events: Option<Value>,
}

impl Harness<'_> {
    pub(super) async fn rpc(&mut self, c: Call<'_>) -> anyhow::Result<Value> {
        let params = self.resolve(serde_json::to_value(c.params)?)?;
        let d = self.daemon()?;
        let rpc = Rpc::new(d.clone());
        let req = RpcRequest::new(1, c.method, params);
        let streaming = matches!(c.method, method::CHAT_STREAM | method::TAIL);
        let (done, close) = (AtomicBool::new(false), Notify::new());
        let call = async {
            let reply = if streaming {
                stream(&rpc, req, &close).await
            } else {
                let r = rpc.handle(req).await;
                Reply {
                    result: r.result,
                    error: r
                        .error
                        .map(|e| json!({"code": e.code, "message": e.message})),
                    events: None,
                }
            };
            done.store(true, Ordering::SeqCst);
            reply
        };
        let tail = (c.method == method::TAIL).then_some(&close);
        let drain = self.drain(&d, &done, c.during, tail);
        let (reply, drained) = tokio::time::timeout(RPC_WAIT, async { tokio::join!(call, drain) })
            .await
            .with_context(|| format!("`{}` sans réponse en {RPC_WAIT:?}", c.method))?;
        let drained = drained?;

        if let Some(e) = &reply.error
            && !c.error
        {
            anyhow::bail!(
                "`{}` a échoué ({e}) ; `error = true` si l'échec est attendu",
                c.method
            );
        }
        if reply.error.is_none() && c.error {
            anyhow::bail!("`{}` devait échouer (`error = true`) et a réussi", c.method);
        }
        if let (Some(name), Some(result)) = (c.bind, &reply.result) {
            self.bound.insert(name.to_string(), result.clone());
        }
        let mut out = json!({});
        if let Some(result) = reply.result {
            let keep = c
                .lines
                .iter()
                .map(|l| regex::Regex::new(l).with_context(|| format!("motif `{l}`")))
                .collect::<anyhow::Result<Vec<_>>>()?;
            out["result"] = project(filter_lines(result, &keep), c.pick, c.mask);
        }
        if let Some(error) = reply.error {
            out["error"] = error;
        }
        if let Some(events) = reply.events {
            out["events"] = events;
        }
        if !drained.is_empty() {
            out["drained"] = json!(drained);
        }
        if d.handle.is_shutting_down() {
            out["shutting_down"] = json!(true);
            out["wants_restart"] = json!(d.handle.wants_restart());
        }
        Ok(out)
    }

    /// Joue les tours mis en file pendant l'appel, jusqu'à ce qu'il réponde et que la
    /// file soit vide. Pour `tail`, joue `during` puis ferme le flux.
    async fn drain(
        &self,
        d: &Arc<Daemon>,
        done: &AtomicBool,
        during: Option<&str>,
        tail: Option<&Notify>,
    ) -> anyhow::Result<Vec<Value>> {
        if let Some(text) = during {
            // Le flux s'abonne au premier passage : il est ouvert avant ce message.
            tokio::task::yield_now().await;
            d.enqueue_message(&self.session, text, &Origin::Cli, None)
                .await?
                .context("tour non créé")?;
        }
        let mut out = Vec::new();
        let mut closed = false;
        loop {
            if let Some(turn) = self.claim().await? {
                out.push(outcome_json(&runner::process(d, turn, HEARTBEAT).await));
                continue;
            }
            if done.load(Ordering::SeqCst) {
                return Ok(out);
            }
            if let Some(close) = tail
                && !closed
            {
                // Le flux lit ce qui reste sur le bus avant d'être fermé.
                for _ in 0..50 {
                    tokio::task::yield_now().await;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                close.notify_one();
                closed = true;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Remplace `$session` et `$nom.chemin` par leur valeur, partout dans les paramètres.
    fn resolve(&self, v: Value) -> anyhow::Result<Value> {
        Ok(match v {
            Value::String(s) if s.starts_with('$') => {
                // `$json:nom.chemin` : la valeur sérialisée, pour un paramètre qui attend
                // un texte JSON (`wf.validate`).
                let (raw, as_text) = match s[1..].strip_prefix("json:") {
                    Some(rest) => (rest, true),
                    None => (&s[1..], false),
                };
                let (name, path) = raw.split_once('.').unwrap_or((raw, ""));
                let base = if name == "session" {
                    json!(self.session)
                } else {
                    self.bound
                        .get(name)
                        .cloned()
                        .with_context(|| format!("`{s}` : aucune réponse liée à `{name}`"))?
                };
                let v = path
                    .split('.')
                    .filter(|p| !p.is_empty())
                    .try_fold(base, |v, seg| step_into(&v, seg))
                    .with_context(|| format!("`{s}` : chemin absent de la réponse"))?;
                if as_text {
                    Value::String(v.to_string())
                } else {
                    v
                }
            }
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|x| self.resolve(x))
                    .collect::<anyhow::Result<_>>()?,
            ),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, x)| Ok((k, self.resolve(x)?)))
                    .collect::<anyhow::Result<_>>()?,
            ),
            other => other,
        })
    }
}

fn step_into(v: &Value, seg: &str) -> Option<Value> {
    match v {
        Value::Array(a) => a.get(seg.parse::<usize>().ok()?).cloned(),
        Value::Object(o) => o.get(seg).cloned(),
        _ => None,
    }
}

/// `chat.stream` et `tail` sur un tampon : les notifications, puis la réponse finale.
async fn stream(rpc: &Rpc, req: RpcRequest, close: &Notify) -> Reply {
    let tail = req.method == method::TAIL;
    let mut out: Vec<u8> = Vec::new();
    let served = rpc.handle_streaming(req, &mut out, close.notified()).await;
    let lines: Vec<Value> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let notes: Vec<Value> = lines
        .iter()
        .filter(|l| l["method"] == "event")
        .map(|l| l["params"].clone())
        .collect();
    let events = if tail {
        // L'ordre de lecture du bus n'est pas celui du tour à l'événement près : les
        // types vus, dans l'ordre de leur première apparition.
        let mut kinds: Vec<Value> = Vec::new();
        for n in &notes {
            if !kinds.contains(&n["type"]) {
                kinds.push(n["type"].clone());
            }
        }
        json!(kinds)
    } else {
        // `done` n'arrive que si l'issue du tour gagne la course contre le dernier
        // fragment (`handle_streaming` saute `Finished` dans sa boucle, pas dans la
        // vidange finale) : la réponse finale porte déjà l'issue.
        json!(
            notes
                .into_iter()
                .filter(|n| n["type"] != "done")
                .collect::<Vec<_>>()
        )
    };
    let last = lines.iter().rev().find(|l| l.get("id").is_some());
    Reply {
        result: last.and_then(|l| l.get("result").cloned()),
        error: match served {
            Err(e) => Some(json!({"code": null, "message": e.to_string()})),
            Ok(()) => last
                .and_then(|l| l.get("error").cloned())
                .map(|e| json!({"code": e["code"], "message": e["message"]})),
        },
        events: Some(events),
    }
}

/// Chaque texte de la réponse réduit à ses lignes qui satisfont un des motifs ; sans
/// motif, la réponse telle quelle.
fn filter_lines(v: Value, keep: &[regex::Regex]) -> Value {
    if keep.is_empty() {
        return v;
    }
    match v {
        Value::String(s) => Value::String(
            s.lines()
                .filter(|l| keep.iter().any(|r| r.is_match(l)))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(|x| filter_lines(x, keep)).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, x)| (k, filter_lines(x, keep)))
                .collect(),
        ),
        other => other,
    }
}

/// Segments d'un pointeur `/a/*/b` ; `*` est chaque élément d'un tableau ou d'un objet.
fn segments(pointer: &str) -> Vec<&str> {
    pointer.split('/').filter(|s| !s.is_empty()).collect()
}

fn get_at(v: &Value, segs: &[&str]) -> Value {
    let Some((head, rest)) = segs.split_first() else {
        return v.clone();
    };
    if *head == "*" {
        let items: Vec<Value> = match v {
            Value::Array(a) => a.iter().map(|x| get_at(x, rest)).collect(),
            Value::Object(o) => o.values().map(|x| get_at(x, rest)).collect(),
            _ => Vec::new(),
        };
        return Value::Array(items);
    }
    step_into(v, head).map_or(Value::Null, |x| get_at(&x, rest))
}

fn mask_at(v: &mut Value, segs: &[&str]) {
    let Some((head, rest)) = segs.split_first() else {
        *v = json!("{{masked}}");
        return;
    };
    let children: Vec<&mut Value> = match (v, *head) {
        (Value::Array(a), "*") => a.iter_mut().collect(),
        (Value::Object(o), "*") => o.values_mut().collect(),
        (Value::Array(a), i) => i
            .parse::<usize>()
            .ok()
            .and_then(|i| a.get_mut(i))
            .into_iter()
            .collect(),
        (Value::Object(o), k) => o.get_mut(k).into_iter().collect(),
        _ => Vec::new(),
    };
    for c in children {
        mask_at(c, rest);
    }
}

/// La réponse gardée : masquée, puis réduite aux pointeurs de `pick` s'il y en a.
pub(super) fn project(mut v: Value, pick: &[String], mask: &[String]) -> Value {
    for m in mask {
        mask_at(&mut v, &segments(m));
    }
    if pick.is_empty() {
        return v;
    }
    Value::Object(
        pick.iter()
            .map(|p| (p.clone(), get_at(&v, &segments(p))))
            .collect(),
    )
}

/// Les relevés `[[observe]]` : une ligne `observed` par ligne de résultat.
pub(super) async fn observe(s: &Services, wanted: &[Observe]) -> anyhow::Result<Vec<Value>> {
    let mut out = Vec::new();
    for o in wanted {
        let sql = o.sql.clone();
        let rows = s
            .store
            .read(move |c| {
                let mut st = c.prepare(&sql)?;
                let names: Vec<String> = st.column_names().iter().map(|n| n.to_string()).collect();
                let rows = st.query_map([], |r| {
                    let mut row = serde_json::Map::new();
                    for (i, n) in names.iter().enumerate() {
                        row.insert(n.clone(), readable(sql_json(r.get(i)?)));
                    }
                    Ok(Value::Object(row))
                })?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .await
            .with_context(|| format!("relevé `{}`", o.name))?;
        out.extend(
            rows.into_iter()
                .map(|row| json!({"type": "observed", "name": o.name, "row": row})),
        );
    }
    Ok(out)
}

/// Un texte JSON (objet ou tableau) est relevé comme du JSON : lisible et normalisable.
fn readable(v: Value) -> Value {
    match v.as_str().map(str::trim_start) {
        Some(t) if t.starts_with('{') || t.starts_with('[') => serde_json::from_str(t).unwrap_or(v),
        _ => v,
    }
}

/// Le vrai superviseur MCP, branché sur un connecteur de test : aucun processus lancé,
/// chaque serveur servi rend ses outils. Relit `mcp.d/` à chaque vie.
pub(super) async fn install_supervisor(
    daemon: &Daemon,
    services: &Arc<Services>,
    servers: &[McpServer],
) {
    use penelope_mcp_host::testing::{FakeConnector, server, tool};
    let fake = Arc::new(FakeConnector::default());
    for srv in servers {
        let tools: Vec<Value> = srv.tools.iter().map(|t| tool(t, json!({}))).collect();
        fake.serve(&srv.name, server(Arc::new(std::sync::Mutex::new(tools))));
    }
    let sup = penelope_mcp_host::testing::supervisor(services.clone(), fake);
    sup.reload().await;
    daemon.hooks.set_mcp(sup);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_and_mask_follow_pointers_and_wildcards() {
        let v = json!({"servers": [{"name": "a", "pid": 12}, {"name": "b", "pid": 13}],
                       "uptime": 7});
        let masked = project(v.clone(), &[], &["/servers/*/pid".into(), "/uptime".into()]);
        assert_eq!(masked["servers"][1]["pid"], "{{masked}}");
        assert_eq!(masked["uptime"], "{{masked}}");
        assert_eq!(masked["servers"][0]["name"], "a");
        let picked = project(v, &["/servers/*/name".into(), "/absent".into()], &[]);
        assert_eq!(
            picked,
            json!({"/servers/*/name": ["a", "b"], "/absent": null})
        );
    }

    #[test]
    fn only_the_matching_lines_of_a_text_are_kept() {
        let keep = [regex::Regex::new("^# TYPE penelope_approvals").unwrap()];
        let v = json!({"text": "# TYPE penelope_turns_total counter\n# TYPE penelope_approvals_pending gauge\npenelope_approvals_pending 0"});
        assert_eq!(
            filter_lines(v, &keep),
            json!({"text": "# TYPE penelope_approvals_pending gauge"})
        );
        assert_eq!(filter_lines(json!("x"), &[]), json!("x"));
    }

    #[test]
    fn a_json_column_is_read_as_json() {
        assert_eq!(readable(json!("{\"a\": 1}")), json!({"a": 1}));
        assert_eq!(readable(json!("texte")), json!("texte"));
        assert_eq!(readable(json!("{pas du json")), json!("{pas du json"));
    }
}
