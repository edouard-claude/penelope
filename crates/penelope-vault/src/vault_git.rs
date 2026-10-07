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
    let memory = &s.config.config().memory;
    // Un remote resté configuré n'est plus poussé (#327) : le dire, une fois, sans alarme.
    let push = match (memory.vault_git_remote.trim(), memory.vault_git_push) {
        ("", _) => String::new(),
        (_, true) => " ; poussé en clair vers `memory.vault_git_remote`".into(),
        (_, false) => " ; historique local seulement (`memory.vault_git_remote` n'est plus \
                       poussé sans `memory.vault_git_push = true`)"
            .into(),
    };
    match (autocommit_interval(s), warning(s)) {
        (None, _) => DoctorCheck::ok(ID, LABEL, format!("autocommit désactivé{push}")),
        (Some(_), Some(w)) => DoctorCheck::fail(ID, LABEL, w, Some("penelope vault sync".into())),
        (Some(d), None) => DoctorCheck::ok(
            ID,
            LABEL,
            format!("autocommit toutes les {} min{push}", d.as_secs() / 60),
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

/// Commit du vault s'il est sous git, puis push si un remote est configuré **et**
/// `memory.vault_git_push` le demande : le vault y part en clair (#327).
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
    if !remote.is_empty()
        && cfg.memory.vault_git_push
        && committed["committed"].as_bool() == Some(true)
    {
        match penelope_tools::git::push(&vault, remote, "HEAD").await {
            Ok(v) => out["push"] = v,
            Err(e) => out["push_error"] = json!(e.to_string()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// #327 : un remote configuré n'est plus poussé par défaut, le vault y partirait en
    /// clair ; `memory.vault_git_push = true` le rétablit. L'historique, lui, reste local.
    #[tokio::test]
    async fn the_vault_is_not_pushed_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            Arc::new(penelope_kernel::clock::TestClock::default());
        let s = Services::for_tests(dir.path().join("p"), clock)
            .await
            .unwrap();
        let bare = dir.path().join("vault.git");
        std::fs::create_dir_all(&bare).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .current_dir(&bare)
            .status()
            .unwrap();
        assert!(init.success());
        let remote = bare.display().to_string();
        s.publish_config("test", move |c| {
            c.memory.vault_git_autocommit = "15m".into();
            c.memory.vault_git_remote = remote;
            Ok(vec!["memory.vault_git_remote".into()])
        })
        .unwrap();
        let vault = penelope_app::helpers::vault_dir(&s);
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("memoire.md"), "- un souvenir\n").unwrap();
        let out = vault_sync(&s, "test").await.unwrap();
        assert_eq!(out["git"], true, "{out}");
        assert!(
            out["push"].is_null() && out["push_error"].is_null(),
            "{out}"
        );
        let refs = |bare: &Path| {
            let o = std::process::Command::new("git")
                .args(["for-each-ref"])
                .current_dir(bare)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).to_string()
        };
        assert!(refs(&bare).trim().is_empty(), "rien n'est poussé");
        assert!(
            doctor_check(&s)
                .detail
                .contains("historique local seulement")
        );

        s.publish_config("test", |c| {
            c.memory.vault_git_push = true;
            Ok(vec!["memory.vault_git_push".into()])
        })
        .unwrap();
        std::fs::write(vault.join("memoire.md"), "- un autre souvenir\n").unwrap();
        let out = vault_sync(&s, "test").await.unwrap();
        assert!(out["push_error"].is_null(), "{out}");
        assert!(!refs(&bare).trim().is_empty(), "{out}");
    }
}
