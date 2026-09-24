//! Historique git du vault (issue #27).
//!
//! ```text
//! démarrage ── autocommit actif, vault hors git ─► git init + .gitignore + commit initial
//! rêve ─────── fin de passe ─────────────────────► commit « rêve du AAAA-MM-JJ (d_…) : N promues »
//! maintenance  toutes les `vault_git_autocommit` ► commit des éditions (éditeur, SSH)
//! doctor, digest ─ autocommit actif, vault hors git ─► avertissement
//! penelope mem diff [--since dream] ─► ce que le dernier rêve a changé
//! ```

use penelope_app::services::Services;
use penelope_kernel::api::DoctorCheck;
use penelope_tools::git;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Préfixe des commits de consolidation.
pub const DREAM_PREFIX: &str = "rêve du ";

const GITIGNORE: &str = "# Pénélope : fichiers locaux hors historique\n\
.DS_Store\n\
.*/workspace.json\n\
.*/workspaces.json\n\
.trash/\n\
*.tmp\n\
*.swp\n";

/// Arbre vide de git : base d'un diff quand le rêve est le premier commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Période d'autocommit ; `None` si désactivé (`0s`).
pub fn autocommit_interval(s: &Services) -> Option<Duration> {
    penelope_kernel::config::parse_duration(&s.config.config().memory.vault_git_autocommit)
        .ok()
        .filter(|d| !d.is_zero())
}

pub fn is_repo(vault: &Path) -> bool {
    vault.join(".git").exists()
}

async fn run_ok(vault: &Path, args: &[&str]) -> Result<String, String> {
    let (code, out, err) = git::run(vault, args).await.map_err(|e| e.to_string())?;
    if code != 0 {
        return Err(format!("git {} : {}", args.join(" "), err.trim()));
    }
    Ok(out)
}

/// Fait du vault un dépôt si l'autocommit est actif ; vrai si le dépôt vient d'être créé.
pub async fn ensure_repo(s: &Services) -> Result<bool, String> {
    if autocommit_interval(s).is_none() {
        return Ok(false);
    }
    let vault = penelope_app::helpers::vault_dir(s);
    if is_repo(&vault) {
        return Ok(false);
    }
    if penelope_platform::which("git").is_none() {
        return Err("`git` est absent du PATH".into());
    }
    std::fs::create_dir_all(&vault).map_err(|e| e.to_string())?;
    run_ok(&vault, &["init", "-q"]).await?;
    // Un service n'a souvent ni identité git ni agent de signature : le dépôt du vault
    // porte les siennes, sans toucher à la configuration globale.
    let (_, email, _) = git::run(&vault, &["config", "user.email"])
        .await
        .map_err(|e| e.to_string())?;
    if email.trim().is_empty() {
        run_ok(&vault, &["config", "user.name", "Pénélope"]).await?;
        run_ok(&vault, &["config", "user.email", "penelope@localhost"]).await?;
    }
    run_ok(&vault, &["config", "commit.gpgsign", "false"]).await?;
    let ignore = vault.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, GITIGNORE).map_err(|e| e.to_string())?;
    }
    git::commit(&vault, "vault : dépôt initial", true)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(vault = %vault.display(), "vault initialisé en dépôt git");
    Ok(true)
}

/// Autocommit configuré alors que le vault n'est pas sous git.
pub fn warning(s: &Services) -> Option<String> {
    let vault = penelope_app::helpers::vault_dir(s);
    (autocommit_interval(s).is_some() && vault.exists() && !is_repo(&vault)).then(|| {
        format!(
            "vault_git_autocommit est configuré mais {} n'est pas un dépôt git : aucun historique \
             des rêves (`penelope vault sync` l'initialise)",
            vault.display()
        )
    })
}

pub fn doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "vault_git";
    const LABEL: &str = "Historique git du vault";
    match (autocommit_interval(s), warning(s)) {
        (None, _) => DoctorCheck::ok(ID, LABEL, "autocommit désactivé"),
        (Some(_), Some(w)) => DoctorCheck::fail(ID, LABEL, w, Some("penelope vault sync".into())),
        (Some(d), None) => DoctorCheck::ok(
            ID,
            LABEL,
            format!("autocommit toutes les {} min", d.as_secs() / 60),
        ),
    }
}

/// Commit périodique des éditions du vault, à la période `vault_git_autocommit`.
pub async fn autocommit_tick(s: &Services) {
    let Some(every) = autocommit_interval(s) else {
        return;
    };
    let now = s.clock.now_ms();
    let last = s
        .kv_get("vault.git.autocommit")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < every.as_millis() as i64 {
        return;
    }
    let _ = s.kv_set("vault.git.autocommit", &now.to_string()).await;
    let stamp = s.clock.now_rfc3339();
    if let Err(e) = vault_sync(
        s,
        &format!("autocommit : {}", &stamp[..16.min(stamp.len())]),
    )
    .await
    {
        tracing::warn!(error = %e, "autocommit du vault");
    }
}

/// `penelope mem diff [--since dream]` : changements non commités, ou depuis l'avant-dernier
/// état précédant le dernier rêve.
pub async fn diff(s: &Services, since_dream: bool) -> Result<Value, String> {
    let vault = penelope_app::helpers::vault_dir(s);
    if !is_repo(&vault) {
        return Err(
            "le vault n'est pas un dépôt git : `penelope vault sync` l'initialise quand \
             vault_git_autocommit est actif"
                .into(),
        );
    }
    let (base, label) = if since_dream {
        let log = run_ok(&vault, &["log", "--format=%H %s", "-n", "500"]).await?;
        let Some((sha, subject)) = log
            .lines()
            .filter_map(|l| l.split_once(' '))
            .find(|(_, subject)| subject.starts_with(DREAM_PREFIX))
        else {
            return Err("aucun rêve commité dans le vault".into());
        };
        let parent = format!("{sha}^");
        let (code, _, _) = git::run(&vault, &["rev-parse", "--verify", "-q", &parent])
            .await
            .map_err(|e| e.to_string())?;
        let base = if code == 0 { parent } else { EMPTY_TREE.into() };
        (base, subject.to_string())
    } else {
        ("HEAD".to_string(), "dernier commit".to_string())
    };
    let out = git::diff(&vault, Some(&base), false)
        .await
        .map_err(|e| e.to_string())?;
    let status = git::status(&vault).await.map_err(|e| e.to_string())?;
    let untracked: Vec<Value> = status["files"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f["state"] == "??")
        .map(|f| f["path"].clone())
        .collect();
    let mut text = out["stdout"].as_str().unwrap_or_default().to_string();
    if !untracked.is_empty() {
        text.push_str(&format!(
            "\nNon suivis : {}\n",
            untracked
                .iter()
                .filter_map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if text.trim().is_empty() {
        text = format!("Aucun changement depuis : {label}.");
    }
    Ok(json!({
        "since": label,
        "base": base,
        "added": out["added"],
        "removed": out["removed"],
        "untracked": untracked,
        "text": text,
    }))
}

/// Commit du vault s'il est sous git, puis push si un remote est configuré.
pub async fn vault_sync(s: &Services, message: &str) -> Result<Value, String> {
    let cfg = s.config.config();
    let vault = penelope_app::helpers::vault_dir(s);
    if let Err(e) = crate::vault_git::ensure_repo(s).await {
        tracing::warn!(error = %e, "initialisation git du vault");
    }
    if !vault.join(".git").exists() {
        return Ok(
            json!({"git": false, "note": "le vault n'est pas un dépôt git : `git init` dans le vault pour l'historique"}),
        );
    }
    let committed = penelope_tools::git::commit(&vault, message, true)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = json!({"git": true, "commit": committed});
    let remote = cfg.memory.vault_git_remote.trim();
    if !remote.is_empty() && committed["committed"].as_bool() == Some(true) {
        match penelope_tools::git::push(&vault, remote, "HEAD").await {
            Ok(v) => out["push"] = v,
            Err(e) => out["push_error"] = json!(e.to_string()),
        }
    }
    Ok(out)
}
