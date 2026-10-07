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
