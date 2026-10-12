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

/// Passe le flux `input` sur l'entrée de `tar` lancé avec `args`, et rend sa sortie
/// standard. Une erreur de lecture du flux (une archive chiffrée coupée ou retouchée,
/// #341) l'emporte sur le code de `tar` : c'est elle qui dit ce qui ne va pas.
fn tar_stdin(args: &[&std::ffi::OsStr], mut input: impl std::io::Read + Send) -> Result<Vec<u8>> {
    let mut child = std::process::Command::new("tar")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // L'écriture se fait à part : `tar` peut remplir sa sortie avant d'avoir tout lu.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| PlatformError::Process("tar : entrée indisponible".into()))?;
    let written = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let copied = std::io::copy(&mut input, &mut stdin);
            drop(stdin);
            copied
        });
        let out = child.wait_with_output();
        (writer.join(), out)
    });
    let out = written.1?;
    match written.0 {
        Ok(Err(e)) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
        Err(_) => return Err(PlatformError::Process("tar : écriture interrompue".into())),
        _ => {}
    }
    if !out.status.success() {
        return Err(PlatformError::Process(format!(
            "tar : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    match written.0 {
        Ok(Ok(_)) => Ok(out.stdout),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(PlatformError::Process("tar : écriture interrompue".into())),
    }
}

/// Extrait une archive `tar.gz` tenue en mémoire, sans l'écrire en clair sur le disque
/// (#329 : la restauration déchiffre en mémoire et passe l'archive à `tar`).
pub fn extract_tar_gz_bytes(bytes: &[u8], dest: &Path) -> Result<()> {
    extract_tar_gz_from(bytes, dest)
}

/// Extrait un flux `tar.gz` dans `dest`, sans clair intermédiaire sur le disque (#341 :
/// la restauration lit l'archive déchiffrée bloc par bloc).
pub fn extract_tar_gz_from(input: impl std::io::Read + Send, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    tar_stdin(
        &[
            "-xzf".as_ref(),
            "-".as_ref(),
            "-C".as_ref(),
            dest.as_os_str(),
        ],
        input,
    )
    .map(|_| ())
}

/// Les entrées d'une archive `tar.gz` tenue en mémoire, sans rien extraire (#328 : le
/// contrôle de déchiffrement de chaque sauvegarde).
pub fn list_tar_gz_bytes(bytes: &[u8]) -> Result<Vec<String>> {
    list_tar_gz_from(bytes)
}

/// Les entrées d'un flux `tar.gz`, sans rien extraire (#341).
pub fn list_tar_gz_from(input: impl std::io::Read + Send) -> Result<Vec<String>> {
    let out = tar_stdin(&["-tzf".as_ref(), "-".as_ref()], input)?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.trim_end_matches('/').to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Les entrées d'une archive `tar.gz` tenue en mémoire avec leur type (`-` fichier, `d`
/// dossier, `l` lien symbolique, `h` lien physique…), sans rien extraire : la
/// restauration refuse ce qui sortirait de sa destination (#329).
pub fn tar_gz_entries(bytes: &[u8]) -> Result<Vec<(char, String)>> {
    tar_gz_entries_from(|| Ok(bytes))
}

/// Comme [`tar_gz_entries`], depuis un flux qu'`open` rouvre : les noms et les types se
/// lisent en deux passes (#341).
pub fn tar_gz_entries_from<R: std::io::Read + Send>(
    mut open: impl FnMut() -> Result<R>,
) -> Result<Vec<(char, String)>> {
    let names = list_tar_gz_from(open()?)?;
    let verbose = tar_stdin(&["-tvzf".as_ref(), "-".as_ref()], open()?)?;
    let kinds: Vec<char> = String::from_utf8_lossy(&verbose)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.chars().next().unwrap_or('?'))
        .collect();
    if kinds.len() != names.len() {
        return Err(PlatformError::Process(format!(
            "tar : {} entrées listées, {} décrites",
            names.len(),
            kinds.len()
        )));
    }
    Ok(kinds.into_iter().zip(names).collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #328, #329 : une archive tenue en mémoire se liste et s'extrait par l'entrée de
    /// `tar`, sans fichier en clair ; un contenu qui n'est pas une archive est refusé.
    #[test]
    fn an_archive_in_memory_is_listed_and_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/penelope");
        std::fs::create_dir_all(src.join("vault")).unwrap();
        std::fs::write(src.join("MANIFEST.json"), "{}").unwrap();
        std::fs::write(src.join("vault/memoire.md"), "souvenir").unwrap();
        let tar = dir.path().join("a.tar.gz");
        create_tar_gz(&tar, &dir.path().join("src"), &["penelope".into()]).unwrap();
        let bytes = std::fs::read(&tar).unwrap();
        let entries = list_tar_gz_bytes(&bytes).unwrap();
        assert!(
            entries.contains(&"penelope/MANIFEST.json".to_string()),
            "{entries:?}"
        );
        assert!(entries.contains(&"penelope/vault/memoire.md".to_string()));
        let out = dir.path().join("out");
        extract_tar_gz_bytes(&bytes, &out).unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("penelope/vault/memoire.md")).unwrap(),
            "souvenir"
        );
        assert!(list_tar_gz_bytes(b"pas une archive").is_err());
        let kinds = tar_gz_entries(&bytes).unwrap();
        assert!(
            kinds.contains(&('d', "penelope/vault".to_string())),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&('-', "penelope/MANIFEST.json".to_string())),
            "{kinds:?}"
        );
    }
}
