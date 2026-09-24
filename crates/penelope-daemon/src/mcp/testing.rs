use super::*;
use penelope_mcp::transport::LoopbackTransport;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Superviseur de test, sans nommer son type hors de `mcp/` (épopée #208, T09).
pub fn supervisor(services: Arc<Services>, connector: Arc<dyn Connector>) -> Arc<McpSupervisor> {
    McpSupervisor::new(services, connector)
}

pub type Handler = Arc<dyn Fn(&str, &Value) -> penelope_mcp::Result<Value> + Send + Sync>;

/// Connecteur qui ouvre des boucles locales, un gestionnaire par serveur.
#[derive(Default)]
pub struct FakeConnector {
    pub handlers: Mutex<BTreeMap<String, Handler>>,
    pub opened: Mutex<Vec<String>>,
    pub fail_open: Mutex<BTreeMap<String, String>>,
    pub transports: Mutex<Vec<(String, Arc<LoopbackTransport>)>>,
    /// Serveurs qui meurent quand on leur envoie la sonde `server/discover`.
    pub dies_on_probe: Mutex<Vec<String>>,
    /// Lenteur simulée à l'ouverture (issue #73).
    pub open_delay: Mutex<Option<std::time::Duration>>,
}

impl FakeConnector {
    pub fn serve(&self, name: &str, handler: Handler) {
        self.handlers
            .lock()
            .unwrap()
            .insert(name.to_string(), handler);
    }
    /// Fait traîner l'ouverture : de quoi vérifier qu'un clic n'attend pas (#73).
    pub fn set_open_delay(&self, d: std::time::Duration) {
        *self.open_delay.lock().unwrap() = Some(d);
    }
    pub fn opened(&self, name: &str) -> usize {
        self.opened
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == name)
            .count()
    }
    pub fn last_transport(&self, name: &str) -> Arc<LoopbackTransport> {
        self.transports
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.clone())
            .expect("transport ouvert")
    }
}

#[async_trait::async_trait]
impl Connector for FakeConnector {
    async fn open(&self, cfg: &ServerConfig) -> Result<Arc<dyn Transport>, String> {
        self.opened.lock().unwrap().push(cfg.name.clone());
        let delay = *self.open_delay.lock().unwrap();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        if let Some(e) = self.fail_open.lock().unwrap().get(&cfg.name) {
            return Err(e.clone());
        }
        let h = self
            .handlers
            .lock()
            .unwrap()
            .get(&cfg.name)
            .cloned()
            .ok_or_else(|| "aucun faux serveur".to_string())?;
        let dies = self.dies_on_probe.lock().unwrap().contains(&cfg.name);
        let dead = Arc::new(AtomicBool::new(false));
        let t = LoopbackTransport::new("stdio", move |m, p| {
            if dead.load(Ordering::SeqCst) {
                return Err(McpError::Transport(
                    "le serveur a fermé la connexion".into(),
                ));
            }
            if dies && m == "server/discover" {
                dead.store(true, Ordering::SeqCst);
                return Err(McpError::Transport(
                    "le serveur a fermé la connexion".into(),
                ));
            }
            h(m, p)
        });
        self.transports
            .lock()
            .unwrap()
            .push((cfg.name.clone(), t.clone()));
        Ok(t)
    }
}

/// Serveur d'avant 2026 : pas de `server/discover`, `initialize`, outils donnés.
pub fn server(tools: Arc<Mutex<Vec<Value>>>) -> Handler {
    Arc::new(move |m, p| match m {
        "server/discover" => Err(McpError::Rpc {
            code: penelope_mcp::protocol::METHOD_NOT_FOUND,
            message: "Method not found".into(),
            data: None,
        }),
        "initialize" => Ok(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": true}},
            "serverInfo": {"name": "faux", "version": "1.0"}
        })),
        "tools/list" => Ok(json!({"tools": tools.lock().unwrap().clone()})),
        "tools/call" => Ok(json!({
            "content": [{"type": "text", "text": format!("{} {}", p["name"].as_str().unwrap_or(""), p["arguments"])}]
        })),
        _ => Ok(json!({})),
    })
}

pub fn tool(name: &str, annotations: Value) -> Value {
    json!({
        "name": name,
        "description": format!("outil {name}"),
        "inputSchema": {"type": "object", "properties": {"project": {"type": "string"}}},
        "annotations": annotations
    })
}

pub fn declare(sup: &McpSupervisor, name: &str, extra: &str) {
    std::fs::create_dir_all(sup.dir()).unwrap();
    std::fs::write(
        sup.dir().join(format!("{name}.toml")),
        format!("command = \"/opt/mcp/{name}\"\n{extra}"),
    )
    .unwrap();
}
