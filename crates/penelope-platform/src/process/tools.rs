use super::*;

/// Résolution d'un exécutable via le PATH effectif (et PATHEXT sous Windows). Jamais de
/// nom en dur : `npx` devient `npx.cmd` sous Windows.
/// Décompresse une archive `.tar.gz` dans `dest` (outil `tar` du système).
pub fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(PlatformError::Process(format!(
            "tar : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Crée une archive `tar.gz` des entrées données, relatives à `root` (issue #42).
pub fn create_tar_gz(dest: &Path, root: &Path, entries: &[String]) -> Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut cmd = std::process::Command::new("tar");
    cmd.arg("-czf").arg(dest).arg("-C").arg(root);
    for e in entries {
        cmd.arg(e);
    }
    let out = cmd.output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(PlatformError::Process(format!(
            "tar : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Clone ou met à jour un dépôt de travail sur `remote` (issue #42).
pub fn git_sync_repo(dir: &Path, remote: &str) -> Result<()> {
    let git = |args: &[&str], cwd: Option<&Path>| -> Result<std::process::Output> {
        let mut c = std::process::Command::new("git");
        c.args(args);
        c.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        Ok(c.output()?)
    };
    if dir.join(".git").is_dir() {
        let _ = git(&["remote", "set-url", "origin", remote], Some(dir))?;
        // Un échec de `pull` n'empêche pas de sauvegarder : le commit suivant le dira.
        let _ = git(&["pull", "--ff-only", "origin", "HEAD"], Some(dir))?;
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let out = git(
        &["clone", remote, dir.to_string_lossy().as_ref()],
        dir.parent(),
    )?;
    if out.status.success() {
        return Ok(());
    }
    // Dépôt vide ou inaccessible en clone : on initialise et on poussera.
    let _ = git(&["init"], Some(dir))?;
    let _ = git(&["remote", "add", "origin", remote], Some(dir))?;
    Ok(())
}

/// Commite et pousse le contenu d'un dépôt de travail. Renvoie le hash du commit.
pub fn git_commit_push(dir: &Path, message: &str, name: &str, email: &str) -> Result<String> {
    let git = |args: &[&str]| -> Result<std::process::Output> {
        let mut c = std::process::Command::new("git");
        c.args(args).current_dir(dir);
        c.env("GIT_TERMINAL_PROMPT", "0");
        Ok(c.output()?)
    };
    let _ = git(&["add", "-A"])?;
    let commit = git(&[
        "-c",
        &format!("user.name={name}"),
        "-c",
        &format!("user.email={email}"),
        "commit",
        "-m",
        message,
    ])?;
    if !commit.status.success() {
        let err = String::from_utf8_lossy(&commit.stdout);
        if !err.contains("nothing to commit") {
            return Err(PlatformError::Process(format!(
                "git commit : {}",
                err.trim()
            )));
        }
    }
    let head = git(&["rev-parse", "HEAD"])?;
    let hash = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let push = git(&["push", "origin", "HEAD"])?;
    if !push.status.success() {
        return Err(PlatformError::Process(format!(
            "git push : {}",
            String::from_utf8_lossy(&push.stderr).trim()
        )));
    }
    Ok(hash)
}

/// Première ligne de `<binaire> --version`, pour vérifier qu'un binaire démarre.
pub fn binary_version(path: &Path) -> Result<String> {
    let out = std::process::Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(PlatformError::Process(format!(
            "{} --version : code {:?}",
            path.display(),
            out.status.code()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string())
}
