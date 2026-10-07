//! Sauvegarde puis restauration de bout en bout (#329) : une instance de test sauvegardée
//! chez un fournisseur `dir`, restaurée dans un `PENELOPE_HOME` vierge, puis un daemon
//! démarré dessus. La session, la mémoire, un secret et la configuration MCP doivent être
//! revenus, et rien de déchiffré ne doit traîner.
//!
//! Les magasins de secrets sont des fichiers chiffrés dans les répertoires de test, jamais
//! le trousseau de la machine.

use penelope_app::services::Services;
use penelope_daemon::rpc::Rpc;
use penelope_daemon::runtime::Daemon;
use penelope_kernel::api::{RpcRequest, method};
use penelope_kernel::clock::{SharedClock, TestClock};
use penelope_ops::backup;
use penelope_platform::secrets::EncryptedFileStore;
use penelope_platform::{Directories, RootedDirs, SecretStore};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

const PASSPHRASE: &str = "six mots tirés au sort pour le test";

fn file_store(root: &Path) -> EncryptedFileStore {
    EncryptedFileStore::with_passphrase(root.join("data/secrets.enc"), "magasin de test").unwrap()
}

async fn services(root: &Path) -> Arc<Services> {
    let clock: SharedClock = Arc::new(TestClock::new(1_789_516_800_000));
    let store_root = root.to_path_buf();
    Arc::new(
        Services::for_tests_with(root.to_path_buf(), clock, move |p| {
            p.secrets = Box::new(file_store(&store_root));
        })
        .await
        .unwrap(),
    )
}

/// L'instance d'origine : une session, un souvenir dans un vault hors de son chemin par
/// défaut, un secret, un serveur MCP, un fournisseur `dir`.
async fn original(root: &Path, provider: &Path) -> Arc<Services> {
    let s = services(root).await;
    let dir = provider.display().to_string();
    s.publish_config("test", move |c| {
        c.memory.vault_path = "{data}/coffre".into();
        c.backup.provider = "dir".into();
        c.backup.dir = dir;
        Ok(vec!["backup.provider".into()])
    })
    .unwrap();
    s.platform
        .secrets
        .set(backup::PASSPHRASE_SECRET, PASSPHRASE)
        .unwrap();
    s.platform
        .secrets
        .set("openrouter_api_key", "sk-or-v1-valeur-de-test")
        .unwrap();
    s.sessions
        .create(
            penelope_kernel::session::SessionKind::Chat,
            Some("Atlas".into()),
        )
        .await
        .unwrap();
    let vault = penelope_app::helpers::vault_dir(&s);
    assert!(vault.ends_with("coffre"), "{}", vault.display());
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::write(
        vault.join("projets.md"),
        "# Projets\n\n## Infrastructure\n\
         - Le serveur Atlas tourne sous Debian. <!-- uid: ATL1 -->\n",
    )
    .unwrap();
    penelope_vault::vault_ops::reindex(&s, &vault)
        .await
        .unwrap();
    let mcp_d = s.platform.dirs.data().join("mcp.d");
    std::fs::create_dir_all(&mcp_d).unwrap();
    std::fs::write(
        mcp_d.join("demo.toml"),
        "command = \"penelope-commande-absente-du-test\"\nargs = [\"--stdio\"]\n",
    )
    .unwrap();
    s
}

/// Rien de déchiffré ni d'extrait ne reste dans `backups/`.
fn no_leftover(dirs: &dyn Directories) {
    let left: Vec<String> = std::fs::read_dir(dirs.data().join("backups"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("restauration-") || n.ends_with(".tar.gz"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_restored_on_a_fresh_home_brings_everything_back() {
    let tmp = tempfile::tempdir().unwrap();
    let provider = tmp.path().join("fournisseur");
    let a = original(&tmp.path().join("origine"), &provider).await;
    let report = backup::run(&a, true, None).await.unwrap();
    assert_eq!(report["pushed"]["provider"], "dir", "{report}");
    let archive = backup::provider::pick_in_dir(&provider, None).unwrap();

    // Machine neuve : un PENELOPE_HOME vierge et son propre magasin.
    let fresh = tmp.path().join("neuve");
    let dirs = RootedDirs::new(&fresh);
    dirs.ensure_all().unwrap();
    let store = file_store(&fresh);
    let which = |c: &str| penelope_platform::process::which(c).is_some();

    // Mauvaise phrase : refus, et rien ne reste.
    let e = backup::restore::restore_archive(&dirs, &archive, "autre", Some(&store), false, &which)
        .unwrap_err();
    assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
    no_leftover(&dirs);

    let dry =
        backup::restore::restore_archive(&dirs, &archive, PASSPHRASE, Some(&store), true, &which)
            .unwrap();
    assert!(
        !dirs.config_file().exists(),
        "--dry-run n'écrit rien : {dry}"
    );
    no_leftover(&dirs);

    let r =
        backup::restore::restore_archive(&dirs, &archive, PASSPHRASE, Some(&store), false, &which)
            .unwrap();
    no_leftover(&dirs);
    let restored: Vec<&str> = r["secrets_restored"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        restored.contains(&"openrouter_api_key") && restored.contains(&backup::PASSPHRASE_SECRET),
        "{r}"
    );
    let todo: Vec<&str> = r["todo"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(todo.len(), 1, "{todo:?}");
    assert!(
        todo[0].contains("penelope-commande-absente-du-test"),
        "{todo:?}"
    );

    // Le secret est dans le magasin, la phrase de passe aussi.
    assert_eq!(
        store.get("openrouter_api_key").unwrap().as_deref(),
        Some("sk-or-v1-valeur-de-test")
    );
    assert_eq!(
        store.get(backup::PASSPHRASE_SECRET).unwrap().as_deref(),
        Some(PASSPHRASE)
    );
    // La configuration et le vault, à son chemin ; la configuration MCP.
    let (cfg, _) = penelope_kernel::config::Config::parse(
        &std::fs::read_to_string(dirs.config_file()).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg.memory.vault_path, "{data}/coffre");
    assert!(fresh.join("data/coffre/projets.md").is_file());
    assert!(!fresh.join("data/vault/projets.md").exists());
    let (servers, errors) = penelope_mcp::config::load_dir(&fresh.join("data/mcp.d"));
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(servers[0].name, "demo");

    // Le daemon démarre sur la machine neuve, avec la configuration restaurée.
    let b = services(&fresh).await;
    b.publish_config("restauration", move |c| {
        *c = cfg;
        Ok(vec!["memory.vault_path".into()])
    })
    .unwrap();
    let d = Arc::new(Daemon::from_services(b));
    let rpc = Rpc::new(d.core.clone());
    let call = |m: &'static str, p: Value| {
        let rpc = &rpc;
        async move {
            rpc.handle(RpcRequest::new(1, m, p))
                .await
                .result
                .unwrap_or_else(|| panic!("{m} en échec"))
        }
    };
    call(method::STATUS, json!({})).await;
    // Le premier passage de maintenance reconstruit les index laissés hors de l'archive.
    assert!(
        backup::rebuild_if_pending(&d.services)
            .await
            .unwrap()
            .is_some()
    );
    let sessions = call(method::SESSION_LIST, json!({})).await;
    assert!(sessions.to_string().contains("Atlas"), "{sessions}");
    let found = call(method::MEM_SEARCH, json!({"query": "Atlas Debian"})).await;
    assert!(found.to_string().contains("ATL1"), "{found}");
    assert_eq!(
        d.services
            .platform
            .secrets
            .get("openrouter_api_key")
            .unwrap()
            .as_deref(),
        Some("sk-or-v1-valeur-de-test")
    );
}
