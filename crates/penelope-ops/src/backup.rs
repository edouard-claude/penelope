//! Sauvegarde complète chiffrée (issue #42), vers **un** fournisseur (#327).
//!
//! ```text
//!  base (VACUUM INTO) ─┐
//!  vault/, skills/,    │
//!  workflows/, mcp.d/, ├─► tar.gz ─► chiffré (phrase de passe) ─► fournisseur : S3,
//!  workspace/,         │            + MANIFEST.json              dossier ou iCloud
//!  mcp-data/, config   │
//!  secrets ─► 2e couche┘
//! ```
//!
//! Une seule archive, un seul fournisseur : sur une machine neuve, `penelope restore` la
//! tire et tout repart. Les **valeurs** des secrets y sont, chiffrées une seconde fois
//! par une clé dérivée de la même phrase de passe ([`secrets`]). N'y sont pas : les
//! modèles locaux (`data/models`, rechargeables), les artefacts et les médias reçus (sauf
//! `--media`), les journaux, et les index dérivés de la base ([`DERIVED_TABLES`], #289) :
//! l'instantané les vide et porte la marque [`REBUILD_PENDING_KEY`], que
//! [`rebuild_if_pending`] honore au premier passage de maintenance qui suit une
//! restauration. Le manifeste dit ce qui est inclus, exclu, et pourquoi.
//!
//! GitHub n'est plus une destination (#327) : l'archive dépassait la limite de 100 Mo
//! d'un fichier, et l'historique git gardait toutes les archives retirées.

use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod alert;
mod doctor;
pub mod inventory;
pub mod kit;
pub mod provider;
pub mod restore;
pub mod s3;
pub mod secrets;
pub mod sigv4;

pub use doctor::{doctor_check, doctor_checks, status};

/// Nom du secret qui porte la phrase de passe des sauvegardes.
pub const PASSPHRASE_SECRET: &str = "backup_passphrase";
use penelope_app::backup_state::{LAST_ERROR_KEY, LAST_KEY};

/// Tables laissées hors de l'archive (#289) : chacune se recalcule depuis sa source,
/// `messages` pour `messages_fts`, `mem_entries` pour `mem_fts`, `mcp_tools` pour
/// `mcp_tools_fts`, le rôle `embedding` pour les trois tables de vecteurs et leur cache.
/// Sur une instance de 349 Mo, elles en pesaient plus de 110.
pub const DERIVED_TABLES: &[&str] = &[
    "messages_fts",
    "mem_fts",
    "mcp_tools_fts",
    "mem_vec",
    "intent_vec",
    "mcp_tools_vec",
    "embeddings_cache",
];

/// Clé `kv` posée dans l'instantané : la base restaurée demande ses index.
pub const REBUILD_PENDING_KEY: &str = "store.rebuild_pending";

/// Ce qui entre dans l'archive, en plus de l'instantané de la base et des secrets.
fn entries(s: &Services, media: bool) -> Vec<(PathBuf, String)> {
    let dirs = &s.platform.dirs;
    let mut v: Vec<(PathBuf, String)> = vec![
        (crate::helpers::vault_dir(s), "vault".into()),
        (dirs.skills(), "skills".into()),
        (dirs.data().join("workflows"), "workflows".into()),
        (dirs.data().join("templates"), "templates".into()),
        (dirs.data().join("mcp.d"), "mcp.d".into()),
        (dirs.config_file(), "config.toml".into()),
        // Le dossier de travail des outils et l'état des serveurs MCP (session d'un pont
        // de messagerie, jetons locaux) : sans eux, rien ne « repart comme hier » (#327).
        (dirs.data().join("workspace"), "workspace".into()),
        (dirs.data().join("mcp-data"), "mcp-data".into()),
    ];
    if media {
        v.push((dirs.artifacts(), "artifacts".into()));
        v.push((dirs.data().join("media"), "media".into()));
    }
    v.retain(|(src, _)| src.exists());
    v
}

/// Ce qui reste dehors, et pourquoi : le manifeste le dit (#327).
fn excluded(s: &Services, media: bool) -> Vec<Value> {
    let data = s.platform.dirs.data();
    let models: Vec<String> = std::fs::read_dir(data.join("models"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let mut v = vec![
        json!({"name": "models", "why": "modèles locaux, rechargeables : à retélécharger \
               après restauration", "items": models}),
        json!({"name": "logs", "why": "journaux de la machine, sans valeur ailleurs"}),
        json!({"name": "index", "why": "index dérivés de la base, reconstruits au premier \
               passage de maintenance", "items": DERIVED_TABLES}),
    ];
    if !media {
        v.push(
            json!({"name": "artifacts, media", "why": "lourds et reconstructibles : \
                       `backup.include_media` ou `--media` les incluent"}),
        );
    }
    v
}

/// Construit l'archive chiffrée. Renvoie son chemin, sa taille et son manifeste.
///
/// Tout le travail lourd (instantané de la base, copies, tar, Argon2id, chiffrement,
/// somme) s'exécute sur un thread bloquant, et l'instantané ne prend pas l'écrivain : les
/// tours, les battements de bail et Telegram continuent pendant la sauvegarde (#77).
pub async fn build(s: &Services, media: bool) -> anyhow::Result<(PathBuf, Value)> {
    let now = s.clock.now_rfc3339();
    let job = BuildJob {
        store: s.store.clone(),
        platform: s.platform.clone(),
        entries: entries(s, media),
        excluded: excluded(s, media),
        mcp_commands: inventory::mcp_commands(&s.platform.dirs.data().join("mcp.d")),
        // Les LaunchAgents de la machine ; en test (`Discovery::none`), elle n'est pas lue.
        services: inventory::services(
            s.platform
                .discovery
                .hardware
                .then(penelope_platform::dirs::home_dir)
                .flatten()
                .as_deref(),
        ),
        out_dir: s.platform.dirs.data().join("backups"),
        stamp: now.replace([':', '.'], "-"),
        day: now.chars().take(10).collect(),
        created_at: now,
        media,
    };
    let started = std::time::Instant::now();
    let (sealed, mut report, snapshot) = tokio::task::spawn_blocking(move || job.run())
        .await
        .map_err(|e| anyhow::anyhow!("sauvegarde interrompue : {e}"))??;
    let total = started.elapsed();
    report["snapshot_ms"] = json!(snapshot.as_millis() as u64);
    report["duration_ms"] = json!(total.as_millis() as u64);
    let _ = s
        .events
        .append(EventDraft::new(
            "store.backup",
            json!({
                "snapshot_ms": snapshot.as_millis() as u64,
                "duration_ms": total.as_millis() as u64,
                "db_bytes": report["manifest"]["db_bytes"],
            }),
        ))
        .await;
    Ok((sealed, report))
}

/// Ce qu'il faut pour construire l'archive hors du runtime async.
struct BuildJob {
    store: penelope_store::Store,
    platform: std::sync::Arc<penelope_platform::Platform>,
    entries: Vec<(PathBuf, String)>,
    excluded: Vec<Value>,
    mcp_commands: Vec<Value>,
    services: Vec<Value>,
    out_dir: PathBuf,
    stamp: String,
    day: String,
    created_at: String,
    media: bool,
}

impl BuildJob {
    fn run(self) -> anyhow::Result<(PathBuf, Value, std::time::Duration)> {
        // Sans phrase de passe, rien n'est commencé.
        let passphrase = secret_passphrase(&self.platform)?;
        std::fs::create_dir_all(&self.out_dir)?;
        // Répertoire de travail : tout y est copié, puis l'archive est faite d'un bloc.
        let work = self.out_dir.join(format!("work-{}", self.stamp));
        let result = self.assemble(&work, &passphrase);
        let _ = std::fs::remove_dir_all(&work);
        result
    }

    fn assemble(
        &self,
        work: &Path,
        passphrase: &str,
    ) -> anyhow::Result<(PathBuf, Value, std::time::Duration)> {
        let root = work.join("penelope");
        std::fs::create_dir_all(&root)?;

        // Instantané cohérent de la base (§15), jamais une copie à chaud ; allégé de ses
        // index dérivés, qui reviendront d'eux-mêmes (#289).
        let db = root.join("penelope.db");
        let t = std::time::Instant::now();
        self.store.backup_to(&db)?;
        let full_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        strip_derived(&db, &self.created_at)
            .map_err(|e| anyhow::anyhow!("allègement de l'instantané : {e}"))?;
        let snapshot = t.elapsed();

        // Collecte bornée : les liens symboliques ne sont pas suivis (rien n'entre de
        // hors des racines déclarées, aucune boucle), et la taille est comptée en route.
        let mut collect = Collect::bounded(MAX_COLLECTED_BYTES);
        collect.bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
        let mut contents: Vec<Value> = Vec::new();
        for (src, name) in &self.entries {
            let dst = root.join(name);
            let before = collect.bytes;
            collect.copy_root(src, &dst, name)?;
            contents.push(json!({"name": name, "bytes": collect.bytes - before}));
        }

        // Les valeurs des secrets, sous leur seconde couche ; le manifeste n'en porte que
        // les noms, et ceux qui n'ont pas pu être lus (à ressaisir).
        let dump = secrets::dump(self.platform.secrets.as_ref());
        std::fs::write(
            root.join(secrets::FILE),
            secrets::seal(&dump.values, passphrase)?,
        )?;
        let manifest = json!({
            "version": crate::VERSION,
            "created_at": self.created_at,
            "day": self.day,
            "db_bytes": std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0),
            "db_full_bytes": full_bytes,
            "derived_excluded": DERIVED_TABLES,
            "contents": contents,
            "excluded": self.excluded,
            "links_skipped": collect.skipped,
            "media_included": self.media,
            "secrets_included": dump.values.keys().collect::<Vec<_>>(),
            "secrets_expected": dump.unreadable,
            "mcp_commands": self.mcp_commands,
            "services": self.services,
        });
        std::fs::write(
            root.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;

        let tar = work.join(format!("penelope-{}.tar.gz", self.stamp));
        penelope_platform::process::create_tar_gz(&tar, work, &["penelope".into()])
            .map_err(|e| anyhow::anyhow!("archive : {e}"))?;

        let sealed = self
            .out_dir
            .join(format!("penelope-{}.tar.gz.enc", self.stamp));
        let sealed_bytes = penelope_platform::archive::seal(&tar, &sealed, passphrase)
            .map_err(|e| anyhow::anyhow!("chiffrement : {e}"))?;

        let verified = verify(&sealed, passphrase).inspect_err(|_| {
            let _ = std::fs::remove_file(&sealed);
        })?;
        let report = json!({
            "archive": sealed.file_name().map(|f| f.to_string_lossy().to_string()),
            "bytes": sealed_bytes,
            "sha256": sha256_of(&sealed)?,
            "manifest": manifest,
            "verified": {"at": self.created_at, "entries": verified},
        });
        Ok((sealed, report, snapshot))
    }
}

/// Contrôle de chaque sauvegarde (#328) : l'archive qui vient d'être écrite se déchiffre
/// avec la phrase de passe, et son contenu se liste (sans rien extraire) avec la base, le
/// manifeste et les secrets. Renvoie le nombre d'entrées.
fn verify(sealed: &Path, passphrase: &str) -> anyhow::Result<usize> {
    let fail = |e: String| anyhow::anyhow!("contrôle de déchiffrement en échec : {e}");
    let plain = penelope_platform::archive::open_to_vec(sealed, passphrase)
        .map_err(|e| fail(e.to_string()))?;
    let entries =
        penelope_platform::process::list_tar_gz_bytes(&plain).map_err(|e| fail(e.to_string()))?;
    for needed in [
        "penelope/penelope.db",
        "penelope/MANIFEST.json",
        "penelope/secrets.enc",
    ] {
        if !entries.iter().any(|e| e == needed) {
            return Err(fail(format!("{needed} absent de l'archive")));
        }
    }
    Ok(entries.len())
}

/// Vide les tables dérivées de l'instantané et y pose la marque de reconstruction.
///
/// L'instantané est une copie : la base vivante et ses caches ne sont pas touchés (règle
/// des caches de `penelope-archtest`), et la reconstruction passe par les crates qui
/// tiennent chaque table. `VACUUM` rend l'espace : c'est lui qui fait la différence de
/// taille, un `DELETE` seul garderait les pages.
fn strip_derived(snapshot: &Path, created_at: &str) -> penelope_store::Result<()> {
    let mut conn = penelope_store::rusqlite::Connection::open(snapshot)?;
    let tx = conn.transaction()?;
    for table in DERIVED_TABLES {
        tx.execute_batch(&format!("DELETE FROM {table};"))?;
    }
    penelope_store::kv_set(&tx, REBUILD_PENDING_KEY, created_at)?;
    tx.commit()?;
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

/// Base restaurée d'une sauvegarde allégée : reconstruit les index plein texte depuis
/// leurs tables sources et retire la marque. Les vecteurs reviennent par le rattrapage
/// d'embeddings, que la passe de maintenance lance juste après. `None` : rien à faire.
pub async fn rebuild_if_pending(s: &Services) -> anyhow::Result<Option<Value>> {
    let Some(created_at) = s.kv_get(REBUILD_PENDING_KEY).await? else {
        return Ok(None);
    };
    let started = std::time::Instant::now();
    let messages = s.context.history.rebuild_fts().await?;
    let memory = s.memory.rebuild_fts().await?;
    let tools = s.mcp_tools.rebuild_fts().await?;
    s.kv_delete(REBUILD_PENDING_KEY).await?;
    // L'index des messages vient d'être refait en entier : la réindexation partielle
    // demandée par une migration (#300) n'a plus lieu d'être.
    s.kv_delete(crate::session_ops::FTS_REINDEX_PENDING_KEY)
        .await?;
    let report = json!({
        "backup_created_at": created_at,
        "messages_fts": messages,
        "mem_fts": memory,
        "mcp_tools_fts": tools,
        "duration_ms": started.elapsed().as_millis() as u64,
    });
    tracing::info!(report = %report, "index reconstruits après restauration");
    let mut payload = report.clone();
    payload["reason"] = json!("restore");
    s.events
        .append(EventDraft::new("store.rebuilt", payload))
        .await?;
    Ok(Some(report))
}

/// Phrase de passe des sauvegardes, rangée dans le magasin de secrets.
fn passphrase(s: &Services) -> anyhow::Result<String> {
    secret_passphrase(&s.platform)
}

fn secret_passphrase(platform: &penelope_platform::Platform) -> anyhow::Result<String> {
    platform
        .secrets
        .get(PASSPHRASE_SECRET)
        .ok()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "aucune phrase de passe : `penelope backup setup` avant de sauvegarder (sans \
                 elle, l'archive serait lisible par quiconque accède au fournisseur)"
            )
        })
}

/// Sauvegarde complète : archive chiffrée et, si demandé, envoi au fournisseur.
///
/// Le fournisseur est résolu **avant** l'archive : sans lui, rien n'est commencé. Une
/// sauvegarde envoyée devient `backup.last` et efface le dernier échec ; un échec est
/// retenu avec sa cause, pour l'alerte, `doctor` et le digest (#330). Le dossier local
/// ne garde ensuite que `backup.keep_local` archives.
pub async fn run(s: &Services, push: bool, media: Option<bool>) -> anyhow::Result<Value> {
    match run_once(s, push, media).await {
        Ok(report) => {
            if push {
                s.kv_set(LAST_KEY, &report.to_string()).await?;
                s.kv_delete(LAST_ERROR_KEY).await?;
            }
            Ok(report)
        }
        Err(e) => {
            if push {
                let error = json!({"at_ms": s.clock.now_ms(), "error": e.to_string()});
                let _ = s.kv_set(LAST_ERROR_KEY, &error.to_string()).await;
            }
            Err(e)
        }
    }
}

async fn run_once(s: &Services, push: bool, media: Option<bool>) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let media = media.unwrap_or(cfg.backup.include_media);
    let target = if push {
        Some(provider::Target::resolve(
            &cfg.backup,
            s.platform.dirs.as_ref(),
            penelope_platform::dirs::home_dir().as_deref(),
        )?)
    } else {
        None
    };
    let (archive, mut report) = build(s, media).await?;
    let bytes = report["bytes"].as_u64().unwrap_or(0);
    if let Some(target) = &target {
        report["pushed"] = provider::push(s, target, &archive, &report).await?;
    }
    // Le dossier local n'est pas une deuxième rotation (#330) : la dernière archive
    // suffit, sauf s'il est lui-même le dossier du fournisseur.
    let local = archive.parent().map(Path::to_path_buf).unwrap_or_default();
    let is_provider = matches!(&target, Some(provider::Target::Dir { path, .. }) if *path == local);
    let keep = (cfg.backup.keep_local as usize).max(usize::from(!push));
    report["pruned_local"] = json!(alert::prune_local(&local, keep, !is_provider));
    let _ = s
        .events
        .append(EventDraft::new(
            "backup.done",
            json!({
                "bytes": bytes,
                "pushed": push,
                "media": media,
                "provider": target.as_ref().map(|t| t.label()),
            }),
        ))
        .await;
    Ok(report)
}

/// Méthode `backup` : `kit` rend le kit de secours (#328 : il ne passe que par la socket
/// locale, comme `secret.set`) ; `snapshot` pour le seul instantané de la base (ce que
/// `penelope restore <fichier.db>` relit) ; sinon l'archive complète, envoyée au fournisseur sauf
/// `push: false`. `media` absent : `backup.include_media` (#327).
pub async fn rpc(s: &Services, p: &Value) -> anyhow::Result<Value> {
    if p.get("kit").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(json!({"text": kit::of_instance(s)?}));
    }
    if p.get("snapshot").and_then(|v| v.as_bool()) == Some(true) {
        let dest = s.platform.dirs.data().join("backups").join(format!(
            "penelope-{}.db",
            s.clock.now_rfc3339().replace(':', "-")
        ));
        let took = s.store.snapshot_to(dest.clone()).await?;
        let keep = (s.config.config().backup.keep_local as usize).max(1);
        if let Some(dir) = dest.parent() {
            alert::prune_local(dir, keep, false);
        }
        return Ok(json!({"path": dest, "snapshot_ms": took.as_millis() as u64}));
    }
    let push = p.get("push").and_then(|v| v.as_bool()).unwrap_or(true);
    let media = p.get("media").and_then(|v| v.as_bool());
    run(s, push, media).await
}

/// Ce que la rotation retire d'une liste d'archives `penelope-<date>…`, dans n'importe
/// quel ordre : les quotidiennes, hebdomadaires et mensuelles du quota restent, le reste
/// part. La même règle sert au bucket S3 et au dossier du fournisseur.
pub fn rotation_plan(names: &[String], cfg: &penelope_kernel::config::Backup) -> Vec<String> {
    let mut archives = names.to_vec();
    archives.sort();
    archives.reverse(); // plus récentes d'abord

    let mut keep: Vec<String> = Vec::new();
    let mut weeks: Vec<String> = Vec::new();
    let mut months: Vec<String> = Vec::new();
    for (i, name) in archives.iter().enumerate() {
        let day: String = name
            .trim_start_matches("penelope-")
            .chars()
            .take(10)
            .collect();
        if i < cfg.keep_daily as usize {
            keep.push(name.clone());
            continue;
        }
        // Une par semaine, puis une par mois, tant que les quotas le permettent.
        let week = iso_week(&day);
        let month: String = day.chars().take(7).collect();
        if !weeks.contains(&week) && weeks.len() < cfg.keep_weekly as usize {
            weeks.push(week);
            keep.push(name.clone());
            continue;
        }
        if !months.contains(&month) && months.len() < cfg.keep_monthly as usize {
            months.push(month);
            keep.push(name.clone());
        }
    }
    archives.into_iter().filter(|n| !keep.contains(n)).collect()
}

/// Semaine ISO d'une date `AAAA-MM-JJ`, pour la rotation hebdomadaire.
fn iso_week(day: &str) -> String {
    use chrono::Datelike;
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| {
            let w = d.iso_week();
            format!("{}-{}", w.year(), w.week())
        })
        .unwrap_or_else(|_| day.to_string())
}

/// Une sauvegarde par nuit, à l'heure de `backup.cron`, appelée par le superviseur.
pub async fn nightly_tick(
    s: &Services,
    messenger: Option<Arc<dyn Messenger>>,
) -> anyhow::Result<()> {
    if s.config.config().backup.cron.trim().is_empty() {
        return Ok(());
    }
    nightly_run(s, messenger.clone()).await?;
    // Aucune sauvegarde réussie depuis 24 h : dit une fois par jour, après la nuit, dont
    // l'échec vaut l'avis du jour (#330).
    alert::tick(s, messenger.as_ref()).await
}

async fn nightly_run(s: &Services, messenger: Option<Arc<dyn Messenger>>) -> anyhow::Result<()> {
    let cfg = s.config.config();
    let Ok(cron) = penelope_kernel::cron::Cron::parse(&cfg.backup.cron) else {
        tracing::warn!(cron = %cfg.backup.cron, "backup.cron illisible");
        return Ok(());
    };
    let now = s.clock.now_ms();
    let key = "backup.cron.last";
    // Premier passage : on mémorise l'instant sans rien lancer, comme le rêve.
    let Some(last) = s.kv_get(key).await?.and_then(|v| v.parse::<i64>().ok()) else {
        s.kv_set(key, &now.to_string()).await?;
        return Ok(());
    };
    let Some(next) = cron.next_after_ms(last, &cfg.owner.timezone) else {
        return Ok(());
    };
    if now < next {
        return Ok(());
    }
    s.kv_set(key, &now.to_string()).await?;
    let origin = crate::bus::Origin::Internal {
        source: "backup".into(),
    };
    match run(s, true, None).await {
        Ok(r) => tracing::info!(report = %r, "sauvegarde nocturne"),
        Err(e) => {
            // Jamais de silence : une sauvegarde manquée se dit (issue #39).
            tracing::error!(error = %e, "sauvegarde nocturne en échec");
            if let Some(m) = messenger {
                let _ = m
                    .send_text(
                        &origin,
                        &format!(
                            "⚠️ La sauvegarde de cette nuit a échoué : {e}. Relancer : \
                             `penelope backup`."
                        ),
                    )
                    .await;
                // Ce message vaut l'alerte du jour : pas de second avis dans la foulée.
                alert::said(s).await;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ utilitaires

/// Plafond de ce que la collecte copie avant l'archive (base comprise) : la copie se fait
/// sur le disque, mais une collecte sans borne remplirait le disque et l'archive
/// dépasserait de toute façon [`penelope_platform::archive::MAX_ARCHIVE_BYTES`], que le
/// chiffrement vérifie avant de rien charger en mémoire.
pub const MAX_COLLECTED_BYTES: u64 = 4 * penelope_platform::archive::MAX_ARCHIVE_BYTES;

/// Copie d'arbres bornée, qui ne suit aucun lien symbolique.
///
/// Une racine déclarée (le vault, `data/workspace`…) est prise telle que la configuration
/// la désigne ; **sous** elle, un lien symbolique n'est jamais suivi : il est noté
/// (`skipped`), ni copié ni recréé. Rien n'entre ainsi de hors des racines, et une boucle
/// de liens ne peut pas tourner. Les sockets et les tubes (un pont MCP en laisse dans
/// `mcp-data`) sont notés de même, leur serveur les recrée.
pub(crate) struct Collect {
    pub bytes: u64,
    cap: u64,
    pub skipped: Vec<String>,
}

impl Collect {
    pub(crate) fn bounded(cap: u64) -> Self {
        Collect {
            bytes: 0,
            cap,
            skipped: Vec::new(),
        }
    }

    /// Copie la racine `src` vers `dst` ; `label` la nomme dans `skipped`. Seules les
    /// racines que la configuration désigne (le vault de `memory.vault_path`,
    /// `config.toml`) peuvent être elles-mêmes un lien : le propriétaire l'a voulu.
    /// `data/workspace` ou `data/mcp-data` en lien ne sont pas suivis.
    pub(crate) fn copy_root(&mut self, src: &Path, dst: &Path, label: &str) -> anyhow::Result<()> {
        let follow = matches!(label, "vault" | "config.toml");
        let meta = if follow {
            std::fs::metadata(src)
        } else {
            std::fs::symlink_metadata(src)
        };
        let meta = match meta {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(anyhow::anyhow!("copie de {} : {e}", src.display())),
        };
        self.copy(src, meta, dst, label)
            .map_err(|e| anyhow::anyhow!("copie de {} : {e}", src.display()))
    }

    /// `meta` : ce qu'est `src` lui-même, lien compris (`symlink_metadata`) ; un lien,
    /// une socket ou un tube est noté et laissé.
    fn copy(
        &mut self,
        src: &Path,
        meta: std::fs::Metadata,
        dst: &Path,
        rel: &str,
    ) -> anyhow::Result<()> {
        if meta.is_file() {
            // Comptée avant la copie : le plafond franchi, rien de plus n'est lu.
            self.bytes += meta.len();
            if self.bytes > self.cap {
                anyhow::bail!(
                    "plus de {} Mo à sauvegarder : au-delà du plafond, rien n'est archivé \
                     (alléger `data/workspace` ou `data/mcp-data`, ou retirer les médias)",
                    self.cap / (1024 * 1024)
                );
            }
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(src, dst)?;
            return Ok(());
        }
        if !meta.is_dir() {
            self.skipped.push(rel.to_string());
            return Ok(());
        }
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            let path = e.path();
            let name = e.file_name();
            let child_rel = format!("{rel}/{}", name.to_string_lossy());
            match std::fs::symlink_metadata(&path) {
                Ok(m) => self.copy(&path, m, &dst.join(&name), &child_rel)?,
                // Disparu entre la lecture du dossier et la copie : rien à garder.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }
}

/// Copie récursive sans plafond ni lien suivi : la restauration, depuis l'arbre extrait.
pub(crate) fn copy_path(src: &Path, dst: &Path) -> anyhow::Result<()> {
    Collect::bounded(u64::MAX).copy_root(src, dst, "")
}

pub(crate) fn sha256_of(p: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(p)?;
    Ok(penelope_kernel::canonical::sha256_hex(&bytes))
}

#[cfg(test)]
mod fake_s3;

#[cfg(test)]
mod alert_tests;

#[cfg(test)]
mod kit_tests;

#[cfg(test)]
mod links_tests;

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod s3_tests;

#[cfg(test)]
mod tests;
