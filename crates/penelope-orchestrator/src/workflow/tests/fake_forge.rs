//! Faux forgeurs GitHub et GitLab (#192) : un serveur HTTP local qui sert le peu d'API
//! qu'emploie la livraison (PR, CI d'un commit) et l'environnement de dev, avec un état
//! que le test fait évoluer (CI en attente puis verte, forgeur en panne, PR déjà
//! ouverte par une vie antérieure du daemon, PR dev fusionnée ou avancée d'un commit).

use penelope_workflow::delivery::config::ForgeKind;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// Dépôt servi, sous le même chemin sur les deux forgeurs.
pub const REPO: &str = "equipe/service";
/// Jeton attendu : une requête sans lui est refusée comme par le vrai forgeur.
pub const TOKEN: &str = "jeton-du-forgeur";

#[derive(Default)]
pub struct World {
    pub prs: Vec<Value>,
    /// GitHub : check runs et statuts du commit ; GitLab : ses pipelines.
    pub check_runs: Value,
    pub statuses: Value,
    pub pipelines: Value,
    /// Toute requête d'API rend 503.
    pub down: bool,
    /// Environnement de dev : chemin → (statut, corps).
    pub dev: BTreeMap<String, (u16, String)>,
    /// Requêtes reçues : `MÉTHODE /chemin?requête`.
    pub requests: Vec<String>,
}

pub struct FakeForge {
    pub kind: ForgeKind,
    pub base: String,
    pub world: Arc<Mutex<World>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeForge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeForge {
    pub async fn start(kind: ForgeKind) -> FakeForge {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let world = Arc::new(Mutex::new(World {
            check_runs: json!({"check_runs": []}),
            statuses: json!({"statuses": []}),
            pipelines: json!([]),
            ..World::default()
        }));
        let (w, b) = (world.clone(), base.clone());
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (w, b) = (w.clone(), b.clone());
                tokio::spawn(async move {
                    let _ = serve(stream, kind, &w, &b).await;
                });
            }
        });
        FakeForge {
            kind,
            base,
            world,
            task,
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        f(&mut self.world.lock().unwrap())
    }

    /// Les requêtes reçues qui commencent par `prefix` (`POST`, `GET /repos`…).
    pub fn requests(&self, prefix: &str) -> Vec<String> {
        self.with(|w| {
            w.requests
                .iter()
                .filter(|r| r.starts_with(prefix))
                .cloned()
                .collect()
        })
    }

    /// Une PR déjà ouverte de `head` vers `base`, comme si un POST avait abouti avant un
    /// arrêt brutal du daemon.
    pub fn open(&self, head: &str, base: &str) {
        let base_url = self.base.clone();
        let kind = self.kind;
        self.with(|w| {
            let n = w.prs.len() as u64 + 1;
            w.prs.push(pr(kind, &base_url, n, head, base, ""));
        });
    }

    /// La PR `n` porte maintenant le commit `sha` (#193).
    pub fn head(&self, n: u64, sha: &str) {
        let kind = self.kind;
        self.with(|w| {
            let p = &mut w.prs[n as usize - 1];
            match kind {
                ForgeKind::GitHub => p["head"]["sha"] = json!(sha),
                ForgeKind::GitLab => p["sha"] = json!(sha),
            }
        });
    }

    /// Le propriétaire fusionne la PR `n`, qui laisse le commit de fusion `merge` sur la
    /// branche cible (#193).
    pub fn merge(&self, n: u64, merge: &str) {
        let kind = self.kind;
        self.with(|w| {
            let p = &mut w.prs[n as usize - 1];
            p["merge_commit_sha"] = json!(merge);
            match kind {
                ForgeKind::GitHub => {
                    p["state"] = json!("closed");
                    p["merged_at"] = json!("2026-09-29T10:00:00Z");
                }
                ForgeKind::GitLab => p["state"] = json!("merged"),
            }
        });
    }

    /// Les PR vers `base`, dans tous leurs états.
    pub fn prs_to(&self, base: &str) -> Vec<Value> {
        let kind = self.kind;
        self.with(|w| {
            w.prs
                .iter()
                .filter(|p| base_ref(kind, p) == base)
                .cloned()
                .collect()
        })
    }

    /// CI d'un commit, comme un forgeur la rend : `pending`, `green` ou `red`.
    pub fn ci(&self, state: &str) {
        let kind = self.kind;
        self.with(|w| match kind {
            ForgeKind::GitHub => {
                let (status, conclusion) = match state {
                    "pending" => ("in_progress", Value::Null),
                    "green" => ("completed", json!("success")),
                    _ => ("completed", json!("failure")),
                };
                w.check_runs = json!({"check_runs": [
                    {"name": "tests", "status": status, "conclusion": conclusion}
                ]});
            }
            ForgeKind::GitLab => {
                let status = match state {
                    "pending" => "running",
                    "green" => "success",
                    _ => "failed",
                };
                w.pipelines = json!([{"id": 41, "status": status}]);
            }
        });
    }
}

fn pr(kind: ForgeKind, base_url: &str, n: u64, head: &str, base: &str, body: &str) -> Value {
    match kind {
        ForgeKind::GitHub => json!({
            "number": n, "state": "open",
            "html_url": format!("{base_url}/{REPO}/pull/{n}"),
            "head": {"ref": head, "sha": null}, "base": {"ref": base},
            "merged_at": null, "body": body,
        }),
        ForgeKind::GitLab => json!({
            "iid": n, "state": "opened",
            "web_url": format!("{base_url}/{REPO}/-/merge_requests/{n}"),
            "source_branch": head, "target_branch": base, "sha": null,
            "description": body,
        }),
    }
}

fn base_ref(kind: ForgeKind, p: &Value) -> String {
    match kind {
        ForgeKind::GitHub => p["base"]["ref"].as_str().unwrap_or_default().to_string(),
        ForgeKind::GitLab => p["target_branch"].as_str().unwrap_or_default().to_string(),
    }
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(raw: &str) -> BTreeMap<String, String> {
    raw.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

async fn serve(
    mut stream: TcpStream,
    kind: ForgeKind,
    world: &Mutex<World>,
    base_url: &str,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_string();
    let target = first.next().unwrap_or_default().to_string();
    let headers: BTreeMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
    let (path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    let (status, reply) = {
        let mut w = world.lock().unwrap();
        w.requests.push(format!("{method} {target}"));
        route(
            &mut w,
            kind,
            base_url,
            &method,
            path,
            &query(raw_query),
            &headers,
            &body,
        )
    };
    let response = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{reply}",
        reply.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

#[allow(clippy::too_many_arguments)]
fn route(
    w: &mut World,
    kind: ForgeKind,
    base_url: &str,
    method: &str,
    path: &str,
    q: &BTreeMap<String, String>,
    headers: &BTreeMap<String, String>,
    body: &Value,
) -> (u16, String) {
    if let Some((status, body)) = w.dev.get(path) {
        return (*status, body.clone());
    }
    let (project, authorised) = match kind {
        ForgeKind::GitHub => (
            format!("/repos/{REPO}"),
            headers.get("authorization").map(String::as_str) == Some(&format!("Bearer {TOKEN}")),
        ),
        ForgeKind::GitLab => (
            "/projects/equipe%2Fservice".to_string(),
            headers.get("private-token").map(String::as_str) == Some(TOKEN),
        ),
    };
    let Some(rest) = path.strip_prefix(&project) else {
        return (404, json!({"message": "Not Found"}).to_string());
    };
    if w.down {
        return (503, "{}".into());
    }
    if !authorised {
        return (401, json!({"message": "Bad credentials"}).to_string());
    }
    let (head_key, base_key) = match kind {
        ForgeKind::GitHub => ("head", "base"),
        ForgeKind::GitLab => ("source_branch", "target_branch"),
    };
    let wanted_head = |p: &Value| match kind {
        ForgeKind::GitHub => format!(
            "{}:{}",
            REPO.split('/').next().unwrap(),
            p["head"]["ref"].as_str().unwrap_or_default()
        ),
        ForgeKind::GitLab => p["source_branch"].as_str().unwrap_or_default().to_string(),
    };
    let base_of = |p: &Value| base_ref(kind, p);
    match (method, rest) {
        ("GET", "/pulls" | "/merge_requests") => {
            let found: Vec<Value> = w
                .prs
                .iter()
                .filter(|p| Some(&wanted_head(p)) == q.get(head_key))
                .filter(|p| Some(&base_of(p)) == q.get(base_key))
                .cloned()
                .collect();
            (200, Value::Array(found).to_string())
        }
        ("POST", "/pulls" | "/merge_requests") => {
            let n = w.prs.len() as u64 + 1;
            let head = body[head_key].as_str().unwrap_or_default();
            let base = body[base_key].as_str().unwrap_or_default();
            let text = body["body"]
                .as_str()
                .or(body["description"].as_str())
                .unwrap_or_default();
            let created = pr(kind, base_url, n, head, base, text);
            w.prs.push(created.clone());
            (201, created.to_string())
        }
        ("GET", "/pipelines") => (200, w.pipelines.to_string()),
        ("GET", r) if r.ends_with("/check-runs") => (200, w.check_runs.to_string()),
        ("GET", r) if r.ends_with("/status") => (200, w.statuses.to_string()),
        _ => (404, json!({"message": "Not Found"}).to_string()),
    }
}
