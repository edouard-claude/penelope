//! Sauvegarde complète chiffrée, et son envoi vers un dépôt privé (issue #42).
//!
//! ```text
//!  base (VACUUM INTO) ─┐
//!  vault/, skills/,    ├─► tar.gz ─► chiffré (phrase de passe) ─► dépôt git privé
//!  workflows/, mcp.d/, │            + MANIFEST.json (jamais de valeur de secret)
//!  gabarits, config    ┘
//! ```
//!
//! Ce qui n'y est **pas** : les valeurs de secrets (seuls leurs noms, pour savoir quoi
//! ressaisir), les artefacts et les médias reçus (sauf `--media`), les journaux, et les
//! index dérivés de la base ([`DERIVED_TABLES`], #289) : l'instantané les vide et porte la
//! marque [`REBUILD_PENDING_KEY`], que [`rebuild_if_pending`] honore au premier passage de
//! maintenance qui suit une restauration.
//!
//! La restauration vit dans la CLI (`penelope restore-all`) : elle se fait daemon arrêté,
//! sur une machine où il n'y a encore rien.

use penelope_app::ports::Messenger;
use penelope_app::services::Services;
use penelope_kernel::event::EventDraft;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub mod s3;
pub mod sigv4;

/// Nom du secret qui porte la phrase de passe des sauvegardes.
pub const PASSPHRASE_SECRET: &str = "backup_passphrase";
/// Clé de suivi de la dernière sauvegarde réussie.
const LAST_KEY: &str = "backup.last";

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

/// Ce qui entre dans l'archive, en plus de l'instantané de la base.
fn entries(s: &Services, media: bool) -> Vec<(PathBuf, String)> {
    let dirs = &s.platform.dirs;
    let mut v: Vec<(PathBuf, String)> = vec![
        (crate::helpers::vault_dir(s), "vault".into()),
        (dirs.skills(), "skills".into()),
        (dirs.data().join("workflows"), "workflows".into()),
        (dirs.data().join("templates"), "templates".into()),
        (dirs.data().join("mcp.d"), "mcp.d".into()),
        (dirs.config_file(), "config.toml".into()),
    ];
    if media {
        v.push((dirs.artifacts(), "artifacts".into()));
        v.push((dirs.data().join("media"), "media".into()));
    }
    v.retain(|(src, _)| src.exists());
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

        let mut contents: Vec<Value> = Vec::new();
        for (src, name) in &self.entries {
            let dst = root.join(name);
            copy_path(src, &dst)
                .map_err(|e| anyhow::anyhow!("copie de {} : {e}", src.display()))?;
            contents.push(json!({"name": name, "bytes": dir_size(&dst)}));
        }

        // Manifeste : ce qu'il y a dedans, et les secrets à ressaisir (noms seulement).
        let secrets: Vec<String> = self.platform.secrets.list().unwrap_or_default();
        let manifest = json!({
            "version": crate::VERSION,
            "created_at": self.created_at,
            "day": self.day,
            "db_bytes": std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0),
            "db_full_bytes": full_bytes,
            "derived_excluded": DERIVED_TABLES,
            "contents": contents,
            "media_included": self.media,
            "secrets_expected": secrets,
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

        let report = json!({
            "archive": sealed.file_name().map(|f| f.to_string_lossy().to_string()),
            "bytes": sealed_bytes,
            "sha256": sha256_of(&sealed)?,
            "manifest": manifest,
        });
        Ok((sealed, report, snapshot))
    }
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
                "aucune phrase de passe : `penelope secret set {PASSPHRASE_SECRET}` avant de \
                 sauvegarder (sans elle, l'archive serait lisible par quiconque accède au dépôt)"
            )
        })
}

/// Où une sauvegarde poussée part : dépôt git privé, stockage S3, ou les deux (#289).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Git(String),
    S3,
}

impl Destination {
    fn label(&self) -> &'static str {
        match self {
            Destination::Git(_) => "git",
            Destination::S3 => "s3",
        }
    }
}

/// Dépôt git des sauvegardes : `backup.git_remote`, sinon celui du vault.
fn git_remote(cfg: &penelope_kernel::config::Config) -> String {
    if cfg.backup.git_remote.trim().is_empty() {
        cfg.memory.vault_git_remote.clone()
    } else {
        cfg.backup.git_remote.clone()
    }
}

fn destinations(cfg: &penelope_kernel::config::Config) -> Vec<Destination> {
    let mut v = Vec::new();
    let remote = git_remote(cfg);
    if !remote.trim().is_empty() {
        v.push(Destination::Git(remote));
    }
    if cfg.backup.s3.enabled() {
        v.push(Destination::S3);
    }
    v
}

/// Sauvegarde complète : archive chiffrée, et envoi vers chaque destination si demandé.
///
/// Avec un dépôt git et un bucket S3, les deux reçoivent l'archive ; l'échec de l'un ne
/// retient pas l'autre, et le rapport le dit (`failed`). Tout en échec : erreur.
pub async fn run(s: &Services, push: bool, media: Option<bool>) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let media = media.unwrap_or(cfg.backup.include_media);
    let (archive, mut report) = build(s, media).await?;
    let bytes = report["bytes"].as_u64().unwrap_or(0);

    if push {
        let targets = destinations(&cfg);
        if targets.is_empty() {
            anyhow::bail!(
                "aucun dépôt de sauvegarde ni bucket S3 : `penelope config set \
                 backup.git_remote git@github.com:moi/penelope-backups.git` (dépôt \
                 **privé**), ou une section `[backup.s3]` (endpoint, bucket, clés dans le \
                 magasin de secrets)"
            );
        }
        let mut failed = serde_json::Map::new();
        for target in &targets {
            let result = match target {
                // La limite de taille est celle du dépôt git ; S3 n'en a pas.
                Destination::Git(_) if bytes > cfg.backup.max_push_bytes => Err(anyhow::anyhow!(
                    "archive de {} Mo : au-delà de la limite de {} Mo du dépôt. Relancer sans \
                     les médias (`backup.include_media = false`) ou pousser à la main.",
                    bytes / (1024 * 1024),
                    cfg.backup.max_push_bytes / (1024 * 1024)
                )),
                Destination::Git(remote) => push_archive(s, remote, &archive, &report).await,
                Destination::S3 => push_s3(s, &archive, &report).await,
            };
            match (target, result) {
                (Destination::Git(_), Ok(v)) => report["pushed"] = v,
                (Destination::S3, Ok(v)) => report["pushed_s3"] = v,
                (t, Err(e)) => {
                    failed.insert(t.label().to_string(), json!(e.to_string()));
                }
            }
        }
        if !failed.is_empty() {
            let detail = failed
                .iter()
                .map(|(k, v)| format!("{k} : {}", v.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(" ; ");
            if failed.len() == targets.len() {
                anyhow::bail!("{detail}");
            }
            tracing::warn!(failed = %detail, "sauvegarde partielle");
            report["failed"] = Value::Object(failed);
        }
    }

    s.kv_set(LAST_KEY, &report.to_string()).await?;
    let failed: Vec<String> = report["failed"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let _ = s
        .events
        .append(EventDraft::new(
            "backup.done",
            json!({"bytes": bytes, "pushed": push, "media": media, "failed": failed}),
        ))
        .await;
    Ok(report)
}

/// Envoie l'archive et son manifeste dans le bucket, puis applique la rotation aux
/// objets du préfixe (#289).
async fn push_s3(s: &Services, archive: &Path, report: &Value) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let client = s3::S3Client::from_config(&cfg.backup.s3, s.platform.secrets.as_ref())?;
    let prefix = s3::normalized_prefix(&cfg.backup.s3.prefix);
    let name = archive
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "penelope.tar.gz.enc".into());
    let key = format!("{prefix}{name}");
    let up = client.put_file(&key, archive).await?;
    client
        .put_bytes(&s3::manifest_key(&key), serde_json::to_vec_pretty(report)?)
        .await?;
    // Rotation : la même règle que le dépôt git, sur les objets du préfixe.
    let names: Vec<String> = s3::list_archives(&client, &prefix)
        .await?
        .into_iter()
        .map(|o| o.key.strip_prefix(&prefix).unwrap_or(&o.key).to_string())
        .collect();
    let removed = rotation_plan(&names, &cfg.backup);
    for n in &removed {
        let old = format!("{prefix}{n}");
        client.delete(&old).await?;
        let _ = client.delete(&s3::manifest_key(&old)).await;
    }
    Ok(json!({
        "endpoint": client.endpoint(),
        "bucket": client.bucket(),
        "key": key,
        "bytes": up.bytes,
        "etag": up.etag,
        "parts": up.parts,
        "rotated": removed,
    }))
}

/// Pousse l'archive dans le dépôt privé, avec son manifeste, et applique la rotation.
async fn push_archive(
    s: &Services,
    remote: &str,
    archive: &Path,
    report: &Value,
) -> anyhow::Result<Value> {
    let cfg = s.config.config();
    let remote = remote.to_string();
    // Dépôt public : refus avant toute écriture (issue #42).
    if let Some(visibility) = repo_visibility(&remote).await
        && visibility != "private"
    {
        anyhow::bail!(
            "`{remote}` est {visibility} : une sauvegarde ne part pas dans un dépôt public, \
             même chiffrée"
        );
    }

    let work = s.platform.dirs.data().join("backups").join("repo");
    let name = archive
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "penelope.tar.gz.enc".into());
    // Clone, copie et push sont des appels bloquants : hors du runtime (#77).
    let (archive, report, retention) = (archive.to_path_buf(), report.clone(), cfg.backup.clone());
    let (remote_c, name_c) = (remote.clone(), name.clone());
    let (removed, pushed) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        penelope_platform::process::git_sync_repo(&work, &remote_c)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        std::fs::copy(&archive, work.join(&name_c))?;
        std::fs::write(
            work.join("MANIFEST.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        let removed = rotate(&work, &retention)?;
        let pushed = penelope_platform::process::git_commit_push(
            &work,
            &format!("Sauvegarde {name_c}"),
            "Penelope",
            "penelope@localhost",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok((removed, pushed))
    })
    .await
    .map_err(|e| anyhow::anyhow!("envoi interrompu : {e}"))??;
    Ok(json!({"remote": remote, "archive": name, "rotated": removed, "commit": pushed}))
}

/// Visibilité d'un dépôt GitHub, quand `gh` est disponible. `None` : inconnue.
async fn repo_visibility(remote: &str) -> Option<String> {
    let slug = github_slug(remote)?;
    let out = tokio::process::Command::new("gh")
        .args(["repo", "view", &slug, "--json", "visibility"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<Value>(&out.stdout)
        .ok()?
        .get("visibility")
        .and_then(|v| v.as_str())
        .map(|v| v.to_lowercase())
}

/// `git@github.com:org/repo.git` et `https://github.com/org/repo` donnent `org/repo`.
pub fn github_slug(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches(".git");
    let rest = r
        .strip_prefix("git@github.com:")
        .or_else(|| r.strip_prefix("https://github.com/"))
        .or_else(|| r.strip_prefix("ssh://git@github.com/"))?;
    let mut parts = rest.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !repo.is_empty()).then(|| format!("{owner}/{repo}"))
}

/// Rotation : 7 quotidiennes, 4 hebdomadaires, 12 mensuelles. Renvoie les archives
/// retirées du dépôt de travail (l'historique git, lui, n'est pas réécrit).
pub fn rotate(dir: &Path, cfg: &penelope_kernel::config::Backup) -> anyhow::Result<Vec<String>> {
    let archives: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(s3::ARCHIVE_SUFFIX).then_some(n)
        })
        .collect();
    let removed = rotation_plan(&archives, cfg);
    for name in &removed {
        std::fs::remove_file(dir.join(name))?;
    }
    Ok(removed)
}

/// Ce que la rotation retire d'une liste d'archives `penelope-<date>…`, dans n'importe
/// quel ordre : les quotidiennes, hebdomadaires et mensuelles du quota restent, le reste
/// part. La même règle sert au dépôt git et au bucket S3.
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
    let cfg = s.config.config();
    if cfg.backup.cron.trim().is_empty() {
        return Ok(());
    }
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
        Ok(r) => {
            tracing::info!(report = %r, "sauvegarde nocturne");
            // Une destination sur deux en échec : l'archive est à l'abri, mais le
            // propriétaire doit le savoir (#289).
            if let (Some(failed), Some(m)) = (r["failed"].as_object(), messenger) {
                let received: Vec<&str> = [("pushed", "le dépôt git"), ("pushed_s3", "S3")]
                    .into_iter()
                    .filter(|(k, _)| !r[*k].is_null())
                    .map(|(_, l)| l)
                    .collect();
                let detail = failed
                    .iter()
                    .map(|(k, v)| format!("{k} : {}", v.as_str().unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join(" ; ");
                let _ = m
                    .send_text(
                        &origin,
                        &format!(
                            "⚠️ Sauvegarde de cette nuit partielle : {} l'a reçue ; en échec, \
                             {detail}",
                            received.join(" et ")
                        ),
                    )
                    .await;
            }
        }
        Err(e) => {
            // Jamais de silence : une sauvegarde manquée se dit (issue #39).
            tracing::error!(error = %e, "sauvegarde nocturne en échec");
            if let Some(m) = messenger {
                let _ = m
                    .send_text(
                        &origin,
                        &format!("⚠️ La sauvegarde de cette nuit a échoué : {e}"),
                    )
                    .await;
            }
        }
    }
    Ok(())
}

/// État des sauvegardes, pour `doctor` et `self_status`.
pub async fn status(s: &Services) -> Value {
    let last: Option<Value> = s
        .kv_get(LAST_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok());
    let cfg = s.config.config();
    let s3 = cfg.backup.s3.enabled().then(|| {
        json!({
            "endpoint": cfg.backup.s3.endpoint,
            "bucket": cfg.backup.s3.bucket,
            "prefix": s3::normalized_prefix(&cfg.backup.s3.prefix),
        })
    });
    json!({
        "last": last,
        "remote": git_remote(&cfg),
        "s3": s3,
        "cron": cfg.backup.cron,
        "passphrase": passphrase(s).is_ok(),
        "media_included": cfg.backup.include_media,
    })
}

/// Les contrôles `doctor` des sauvegardes : l'état général, et le bucket S3 s'il y en a un.
pub async fn doctor_checks(s: &Services) -> Vec<penelope_kernel::api::DoctorCheck> {
    let mut checks = vec![doctor_check(s).await];
    if s.config.config().backup.s3.enabled() {
        checks.push(s3_doctor_check(s).await);
    }
    checks
}

/// Contrôle `doctor` du bucket (#289) : joignable avec ces clés, dernière sauvegarde S3
/// et son âge.
async fn s3_doctor_check(s: &Services) -> penelope_kernel::api::DoctorCheck {
    use penelope_kernel::api::DoctorCheck;
    const ID: &str = "backup.s3";
    const LABEL: &str = "Sauvegarde S3";
    let cfg = s.config.config();
    let client = match s3::S3Client::from_config(&cfg.backup.s3, s.platform.secrets.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return DoctorCheck::fail(
                ID,
                LABEL,
                e.to_string(),
                Some("penelope secret set s3_access_key_id".into()),
            );
        }
    };
    let bucket = client.bucket().to_string();
    match tokio::time::timeout(std::time::Duration::from_secs(20), client.head_bucket()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return DoctorCheck::fail(ID, LABEL, e.to_string(), None),
        Err(_) => {
            return DoctorCheck::fail(
                ID,
                LABEL,
                format!(
                    "bucket `{bucket}` : {} ne répond pas en 20 s",
                    client.endpoint()
                ),
                None,
            );
        }
    }
    let st = status(s).await;
    let last = &st["last"];
    let (Some(key), Some(created)) = (
        last["pushed_s3"]["key"].as_str(),
        last["manifest"]["created_at"].as_str(),
    ) else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!("bucket `{bucket}` joignable ; aucune sauvegarde S3 encore"),
            Some("penelope backup --push".into()),
        );
    };
    let age_h = age_hours(s, created);
    let detail = format!(
        "bucket `{bucket}` joignable ; dernière sauvegarde S3 il y a {age_h} h ({} Mo, {key})",
        last["pushed_s3"]["bytes"].as_u64().unwrap_or(0) / (1024 * 1024)
    );
    if age_h > 48 {
        DoctorCheck::fail(ID, LABEL, detail, Some("penelope backup --push".into()))
    } else {
        DoctorCheck::ok(ID, LABEL, detail)
    }
}

/// Contrôle `doctor` : âge de la dernière sauvegarde, destination, phrase de passe.
pub async fn doctor_check(s: &Services) -> penelope_kernel::api::DoctorCheck {
    use penelope_kernel::api::DoctorCheck;
    const ID: &str = "backup";
    const LABEL: &str = "Sauvegarde";
    let st = status(s).await;
    if st["passphrase"] != true {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "aucune phrase de passe : rien n'est sauvegardé hors de cette machine",
            Some(format!("penelope secret set {PASSPHRASE_SECRET}")),
        );
    }
    let Some(created) = st["last"]["manifest"]["created_at"].as_str() else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "aucune sauvegarde enregistrée",
            Some("penelope backup --push".into()),
        );
    };
    let age_h = age_hours(s, created);
    // Durée : l'instantané de la base grossit avec elle, sa dérive se voit ici (#77).
    let duration = match (
        st["last"]["duration_ms"].as_u64(),
        st["last"]["snapshot_ms"].as_u64(),
    ) {
        (Some(total), Some(snap)) => format!(
            ", {:.1} s dont {:.1} s d'instantané",
            total as f64 / 1000.0,
            snap as f64 / 1000.0
        ),
        _ => String::new(),
    };
    let mut towards: Vec<String> = Vec::new();
    if let Some(remote) = st["remote"].as_str().filter(|r| !r.trim().is_empty()) {
        towards.push(remote.to_string());
    }
    if let Some(bucket) = st["s3"]["bucket"].as_str() {
        towards.push(format!("S3 `{bucket}`"));
    }
    if towards.is_empty() {
        towards.push("aucun dépôt".into());
    }
    let detail = format!(
        "dernière il y a {age_h} h ({} Mo{duration}), vers {}",
        st["last"]["bytes"].as_u64().unwrap_or(0) / (1024 * 1024),
        towards.join(" et ")
    );
    if age_h > 48 {
        DoctorCheck::fail(ID, LABEL, detail, Some("penelope backup --push".into()))
    } else {
        DoctorCheck::ok(ID, LABEL, detail)
    }
}

fn age_hours(s: &Services, created: &str) -> i64 {
    let then = chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    ((s.clock.now_ms() - then) / 3_600_000).max(0)
}

// ------------------------------------------------------------------ utilitaires

fn copy_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_file() {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(src, dst)?;
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        copy_path(&e.path(), &dst.join(name))?;
    }
    Ok(())
}

fn dir_size(p: &Path) -> u64 {
    if p.is_file() {
        return std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    }
    std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| dir_size(&e.path()))
        .sum()
}

fn sha256_of(p: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(p)?;
    Ok(penelope_kernel::canonical::sha256_hex(&bytes))
}

#[cfg(test)]
mod fake_s3;

#[cfg(test)]
mod push_tests;

#[cfg(test)]
mod s3_tests;

#[cfg(test)]
mod tests;
