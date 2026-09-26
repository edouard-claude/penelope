//! Ordre de démarrage d'une passerelle (épopée #208, T29 ; issue #12) : `Daemon::run`
//! l'annonce avant de créer le superviseur MCP et ne la démarre qu'après lui.
//!
//! ```text
//!  Daemon::run(Some(gateway))
//!     ├── expect_owner()        un serveur MCP qui se connecte sait qu'on lui répondra
//!     ├── superviseur MCP       hooks.mcp_supervisor rempli
//!     └── gateway.start()       ce que ce test observe
//! ```

use penelope_app::gateway::Gateway;
use penelope_app::services::Services;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::clock::SystemClock;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Passerelle factice : note ce qu'elle voit au démarrage, puis arrête le daemon.
struct Probe {
    d: Arc<Daemon>,
    seen: Mutex<Option<(bool, bool)>>,
}

#[async_trait::async_trait]
impl Gateway for Probe {
    fn name(&self) -> &'static str {
        "sonde"
    }

    async fn start(self: Arc<Self>) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
        let owner = self.d.services.elicitations.reachable();
        let mcp = self.d.hooks.mcp_supervisor().is_some();
        *self.seen.lock().unwrap() = Some((owner, mcp));
        self.d.handle.shutdown();
        Ok(Vec::new())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_is_announced_before_mcp_and_started_after_it() {
    // Chemin court : une socket Unix est limitée à ~104 octets.
    let dir = tempfile::Builder::new()
        .prefix("pen")
        .tempdir_in("/tmp")
        .unwrap();
    let s = Services::for_tests(dir.path().to_path_buf(), Arc::new(SystemClock))
        .await
        .unwrap();
    let d = Arc::new(Daemon::from_services(Arc::new(s)));
    assert!(!d.services.elicitations.reachable(), "personne avant run");
    let probe = Arc::new(Probe {
        d: d.clone(),
        seen: Mutex::new(None),
    });
    tokio::time::timeout(Duration::from_secs(60), d.clone().run(Some(probe.clone())))
        .await
        .expect("run rend la main après l'arrêt")
        .unwrap();
    assert_eq!(
        *probe.seen.lock().unwrap(),
        Some((true, true)),
        "(propriétaire annoncé, superviseur MCP créé) au démarrage de la passerelle"
    );
}
