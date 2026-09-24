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
//! ressaisir), les artefacts et les médias reçus (sauf `--media`), les journaux.
//!
//! La restauration vit dans la CLI (`penelope restore-all`) : elle se fait daemon arrêté,
//! sur une machine où il n'y a encore rien.

use crate::runtime::Daemon;
use penelope_kernel::event::EventDraft;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Nom du secret qui porte la phrase de passe des sauvegardes.
pub const PASSPHRASE_SECRET: &str = "backup_passphrase";
/// Clé de suivi de la dernière sauvegarde réussie.
const LAST_KEY: &str = "backup.last";

/// Ce qui entre dans l'archive, en plus de l'instantané de la base.
fn entries(d: &Daemon, media: bool) -> Vec<(PathBuf, String)> {
    let dirs = &d.services.platform.dirs;
    let mut v: Vec<(PathBuf, String)> = vec![
        (crate::helpers::vault_dir(&d.services), "vault".into()),
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
pub async fn build(d: &Daemon, media: bool) -> anyhow::Result<(PathBuf, Value)> {
    let s = &d.services;
    let now = s.clock.now_rfc3339();
    let job = BuildJob {
        store: s.store.clone(),
        platform: s.platform.clone(),
        entries: entries(d, media),
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

        // Instantané cohérent de la base (§15), jamais une copie à chaud.
        let db = root.join("penelope.db");
        let t = std::time::Instant::now();
        self.store.backup_to(&db)?;
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

/// Phrase de passe des sauvegardes, rangée dans le magasin de secrets.
fn passphrase(d: &Daemon) -> anyhow::Result<String> {
    secret_passphrase(&d.services.platform)
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

/// Sauvegarde complète : archive chiffrée, et envoi vers le dépôt privé si demandé.
pub async fn run(d: &Daemon, push: bool, media: Option<bool>) -> anyhow::Result<Value> {
    let s = &d.services;
    let cfg = s.config.config();
    let media = media.unwrap_or(cfg.backup.include_media);
    let (archive, mut report) = build(d, media).await?;
    let bytes = report["bytes"].as_u64().unwrap_or(0);

    if push {
        if bytes > cfg.backup.max_push_bytes {
            anyhow::bail!(
                "archive de {} Mo : au-delà de la limite de {} Mo du dépôt. Relancer sans les \
                 médias (`backup.include_media = false`) ou pousser à la main.",
                bytes / (1024 * 1024),
                cfg.backup.max_push_bytes / (1024 * 1024)
            );
        }
        let pushed = push_archive(d, &archive, &report).await?;
        report["pushed"] = pushed;
    }

    s.kv_set(LAST_KEY, &report.to_string()).await?;
    let _ = s
        .events
        .append(EventDraft::new(
            "backup.done",
            json!({"bytes": bytes, "pushed": push, "media": media}),
        ))
        .await;
    Ok(report)
}

/// Pousse l'archive dans le dépôt privé, avec son manifeste, et applique la rotation.
async fn push_archive(d: &Daemon, archive: &Path, report: &Value) -> anyhow::Result<Value> {
    let s = &d.services;
    let cfg = s.config.config();
    let remote = if cfg.backup.git_remote.trim().is_empty() {
        cfg.memory.vault_git_remote.clone()
    } else {
        cfg.backup.git_remote.clone()
    };
    if remote.trim().is_empty() {
        anyhow::bail!(
            "aucun dépôt de sauvegarde : `penelope config set backup.git_remote \
             git@github.com:moi/penelope-backups.git` (dépôt **privé**)"
        );
    }
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
    let mut archives: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.ends_with(".tar.gz.enc").then_some(n)
        })
        .collect();
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
    let mut removed = Vec::new();
    for name in &archives {
        if !keep.contains(name) {
            std::fs::remove_file(dir.join(name))?;
            removed.push(name.clone());
        }
    }
    Ok(removed)
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
pub async fn nightly_tick(d: &Daemon) -> anyhow::Result<()> {
    let s = &d.services;
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
    match run(d, true, None).await {
        Ok(r) => {
            tracing::info!(report = %r, "sauvegarde nocturne");
        }
        Err(e) => {
            // Jamais de silence : une sauvegarde manquée se dit (issue #39).
            tracing::error!(error = %e, "sauvegarde nocturne en échec");
            if let Some(m) = d.hooks.messenger() {
                let _ = m
                    .send_text(
                        &crate::bus::Origin::Internal {
                            source: "backup".into(),
                        },
                        &format!("⚠️ La sauvegarde de cette nuit a échoué : {e}"),
                    )
                    .await;
            }
        }
    }
    Ok(())
}

/// État des sauvegardes, pour `doctor` et `self_status`.
pub async fn status(d: &Daemon) -> Value {
    let last: Option<Value> = d
        .services
        .kv_get(LAST_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok());
    let cfg = d.services.config.config();
    let remote = if cfg.backup.git_remote.trim().is_empty() {
        cfg.memory.vault_git_remote.clone()
    } else {
        cfg.backup.git_remote.clone()
    };
    json!({
        "last": last,
        "remote": remote,
        "cron": cfg.backup.cron,
        "passphrase": passphrase(d).is_ok(),
        "media_included": cfg.backup.include_media,
    })
}

/// Contrôle `doctor` : âge de la dernière sauvegarde, destination, phrase de passe.
pub async fn doctor_check(d: &Daemon) -> penelope_kernel::api::DoctorCheck {
    use penelope_kernel::api::DoctorCheck;
    const ID: &str = "backup";
    const LABEL: &str = "Sauvegarde";
    let st = status(d).await;
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
    let age_h = age_hours(d, created);
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
    let detail = format!(
        "dernière il y a {age_h} h ({} Mo{duration}), vers {}",
        st["last"]["bytes"].as_u64().unwrap_or(0) / (1024 * 1024),
        st["remote"].as_str().unwrap_or("aucun dépôt")
    );
    if age_h > 48 {
        DoctorCheck::fail(ID, LABEL, detail, Some("penelope backup --push".into()))
    } else {
        DoctorCheck::ok(ID, LABEL, detail)
    }
}

fn age_hours(d: &Daemon, created: &str) -> i64 {
    let then = chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    ((d.services.clock.now_ms() - then) / 3_600_000).max(0)
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
mod tests {
    use super::*;
    use penelope_kernel::config::Backup;

    use std::sync::Arc;

    async fn daemon() -> (tempfile::TempDir, Arc<Daemon>) {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::TestClock::default());
        let s = Arc::new(
            crate::runtime::Services::for_tests(dir.path().to_path_buf(), clock)
                .await
                .unwrap(),
        );
        (dir, Arc::new(Daemon::from_services(s)))
    }

    /// #42 : sauvegarde puis restauration dans un répertoire vide : la base et le vault
    /// reviennent identiques, et les valeurs de secrets ne sont jamais dans l'archive.
    #[tokio::test]
    async fn a_backup_restores_the_database_and_the_vault() {
        let (_dir, d) = daemon().await;
        let s = &d.services;
        let vault = crate::helpers::vault_dir(s);
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("memoire.md"), "- un souvenir précis ^01UID\n").unwrap();
        s.platform
            .secrets
            .set(PASSPHRASE_SECRET, "phrase de passe de sauvegarde")
            .unwrap();
        s.platform
            .secrets
            .set("openrouter_api_key", "sk-or-v1-valeur-secrete")
            .unwrap();
        // Une trace dans la base, pour vérifier qu'elle revient.
        s.sessions
            .create(
                penelope_kernel::session::SessionKind::Chat,
                Some("Atlas".into()),
            )
            .await
            .unwrap();

        let (archive, report) = build(&d, false).await.unwrap();
        assert!(archive.is_file());
        assert!(report["bytes"].as_u64().unwrap_or(0) > 0);
        assert!(report["sha256"].as_str().is_some());
        // Le manifeste dit quels secrets ressaisir, jamais leurs valeurs.
        let names: Vec<String> = report["manifest"]["secrets_expected"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"openrouter_api_key".to_string()),
            "{names:?}"
        );
        let raw = std::fs::read(&archive).unwrap();
        assert!(
            !String::from_utf8_lossy(&raw).contains("sk-or-v1-valeur-secrete"),
            "aucune valeur de secret dans l'archive"
        );

        // Restauration dans un répertoire vide.
        let fresh = tempfile::tempdir().unwrap();
        let tar = fresh.path().join("s.tar.gz");
        penelope_platform::archive::open(&archive, &tar, "phrase de passe de sauvegarde").unwrap();
        penelope_platform::process::extract_tar_gz(&tar, fresh.path()).unwrap();
        let root = fresh.path().join("penelope");
        assert_eq!(
            std::fs::read_to_string(root.join("vault/memoire.md")).unwrap(),
            "- un souvenir précis ^01UID\n"
        );
        // La base restaurée porte la session créée.
        let restored = penelope_store::Store::open(root.join("penelope.db")).unwrap();
        let titles: Vec<String> = restored
            .read(|c| {
                let mut st = c.prepare("SELECT title FROM sessions")?;
                let rows = st.query_map([], |r| r.get::<_, Option<String>>(0))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?.unwrap_or_default());
                }
                Ok(out)
            })
            .await
            .unwrap();
        assert!(titles.contains(&"Atlas".to_string()), "{titles:?}");
    }

    /// #77 : sur un runtime à un seul worker, une sauvegarde complète laisse passer les
    /// écritures : l'instantané ne prend pas l'écrivain, et le tar, Argon2 et le
    /// chiffrement ne monopolisent pas le runtime. `doctor` dit sa durée.
    #[tokio::test(flavor = "current_thread")]
    async fn a_backup_neither_blocks_the_runtime_nor_the_writer() {
        let (_dir, d) = daemon().await;
        d.services
            .platform
            .secrets
            .set(PASSPHRASE_SECRET, "phrase de passe")
            .unwrap();
        let job = {
            let d = d.clone();
            tokio::spawn(async move { run(&d, false, None).await })
        };
        let mut during = 0;
        while !job.is_finished() {
            d.services
                .store
                .write(|tx| penelope_store::kv_set(tx, "battement", "1"))
                .await
                .unwrap();
            if !job.is_finished() {
                during += 1;
            }
            tokio::task::yield_now().await;
        }
        let report = job.await.unwrap().unwrap();
        assert!(during > 0, "aucune écriture pendant la sauvegarde");
        assert!(report["snapshot_ms"].is_u64(), "{report}");
        assert!(report["duration_ms"].is_u64(), "{report}");

        let events = d
            .services
            .events
            .range(0, 500)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "store.backup")
            .count();
        assert_eq!(events, 1);
        let check = doctor_check(&d).await;
        assert!(check.detail.contains("d'instantané"), "{}", check.detail);
    }

    /// #42 : sans phrase de passe, rien n'est écrit et le message dit quoi faire.
    #[tokio::test]
    async fn without_a_passphrase_nothing_is_written() {
        let (_dir, d) = daemon().await;
        let e = build(&d, false).await.unwrap_err();
        assert!(e.to_string().contains(PASSPHRASE_SECRET), "{e}");
        let out = d.services.platform.dirs.data().join("backups");
        let archives = std::fs::read_dir(&out)
            .map(|r| {
                r.flatten()
                    .filter(|e| e.file_name().to_string_lossy().ends_with(".enc"))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(archives, 0, "aucune archive ne doit rester");
    }

    /// #42 : une archive au-delà de la limite du dépôt est refusée, avec la marche à suivre.
    #[tokio::test]
    async fn an_oversized_archive_is_refused_before_pushing() {
        let (_dir, d) = daemon().await;
        let s = &d.services;
        s.platform.secrets.set(PASSPHRASE_SECRET, "phrase").unwrap();
        d.publish_config("test", |c| {
            c.backup.max_push_bytes = 64;
            c.backup.git_remote = "git@github.com:moi/sauvegardes.git".into();
            Ok(vec!["backup.max_push_bytes".into()])
        })
        .unwrap();
        let e = run(&d, true, Some(false)).await.unwrap_err();
        assert!(e.to_string().contains("limite"), "{e}");
    }

    /// #42 : l'état des sauvegardes remonte dans `doctor`.
    #[tokio::test]
    async fn doctor_says_when_there_is_no_backup_yet() {
        let (_dir, d) = daemon().await;
        let c = doctor_check(&d).await;
        assert!(!c.ok, "{c:?}");
        assert!(c.detail.contains("phrase de passe"), "{c:?}");

        d.services
            .platform
            .secrets
            .set(PASSPHRASE_SECRET, "phrase")
            .unwrap();
        let c = doctor_check(&d).await;
        assert!(c.detail.contains("aucune sauvegarde"), "{c:?}");
    }

    #[test]
    fn a_github_slug_is_read_from_any_remote_form() {
        for r in [
            "git@github.com:moi/penelope-backups.git",
            "https://github.com/moi/penelope-backups",
            "ssh://git@github.com/moi/penelope-backups.git",
        ] {
            assert_eq!(
                github_slug(r).as_deref(),
                Some("moi/penelope-backups"),
                "{r}"
            );
        }
        assert!(github_slug("git@gitlab.com:moi/x.git").is_none());
    }

    /// #42 : 7 quotidiennes, 4 hebdomadaires, 12 mensuelles ; les autres partent.
    #[test]
    fn rotation_keeps_seven_four_and_twelve() {
        let dir = tempfile::tempdir().unwrap();
        // Trente sauvegardes quotidiennes consécutives.
        for day in 1..=30 {
            let name = format!("penelope-2026-09-{day:02}T04-00-00-000Z.tar.gz.enc");
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let cfg = Backup::default();
        let removed = rotate(dir.path(), &cfg).unwrap();
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        // 7 quotidiennes + une par semaine (4 au plus) + une par mois (1 ici).
        assert!(left.len() >= 7 && left.len() <= 12, "{left:?}");
        assert_eq!(removed.len(), 30 - left.len());
        assert!(
            left.iter().any(|n| n.contains("2026-09-30")),
            "la plus récente reste : {left:?}"
        );
        assert!(
            !left.iter().any(|n| n.contains("2026-09-02")),
            "les vieilles du même mois partent : {left:?}"
        );
    }
}
