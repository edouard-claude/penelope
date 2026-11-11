//! Les journaux de flux (#469) : la sortie standard et la sortie d'erreur des services
//! (le daemon, un serveur d'inférence locale), que le gestionnaire de services écrit
//! lui-même dans `{logs}/<service>.{out,err}.log`.
//!
//! Les journaux du daemon (`penelope-AAAA-MM-JJ.jsonl`) tournent chaque jour et sont
//! purgés ; ceux-là ne l'étaient par personne. Le 11/11, celui d'un serveur d'inférence
//! pesait 4,9 Mo et ne pouvait que grossir.
//!
//! Le fichier ne peut pas être renommé : le gestionnaire de services garde son
//! descripteur et continuerait d'écrire dans le fichier renommé. Il est donc **copié**
//! vers `<nom>.1`, puis **vidé en place**. launchd ouvre ces fichiers en ajout
//! (`O_APPEND`) : l'écriture suivante repart du début du fichier vidé. Ce qui s'écrit entre
//! la copie et la troncature est perdu : quelques lignes d'un flux de diagnostic, une
//! fois par rotation.

use std::path::{Path, PathBuf};

/// Au-delà, un journal de flux est tourné : au plus une génération gardée, donc deux fois
/// cette taille par flux, à la croissance d'une journée près.
pub const STREAM_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Tourne chaque `*.log` de `dir` qui dépasse `max_bytes`, et rend ceux qui l'ont été.
/// Les `.jsonl` du daemon et les générations `.log.1` ne sont pas concernés ; un lien
/// symbolique n'est pas suivi.
pub fn bound_stream_logs(dir: &Path, max_bytes: u64) -> Vec<PathBuf> {
    let mut rotated = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return rotated;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("log") {
            continue;
        }
        let Ok(meta) = e.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() <= max_bytes {
            continue;
        }
        match rotate(&path) {
            Ok(()) => {
                tracing::info!(
                    fichier = %path.display(),
                    octets = meta.len(),
                    "journal de flux tourné"
                );
                rotated.push(path);
            }
            Err(err) => tracing::warn!(
                fichier = %path.display(),
                error = %err,
                "journal de flux non tourné"
            ),
        }
    }
    rotated.sort();
    rotated
}

/// Copie vers `<nom>.1` (privé comme l'original, #26), puis vide l'original en place.
fn rotate(path: &Path) -> std::io::Result<()> {
    let mut kept = path.as_os_str().to_owned();
    kept.push(".1");
    let kept = PathBuf::from(kept);
    std::fs::copy(path, &kept)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Le test de l'issue : un journal d'enfant au-delà de la borne est tourné. Le service
    /// garde son descripteur ouvert en ajout, comme launchd : ce qu'il écrit ensuite
    /// arrive au début du fichier vidé, pas à l'ancienne position.
    #[test]
    fn a_stream_log_beyond_the_bound_is_rotated_under_its_writer() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("inference.err.log");
        let mut service = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        service.write_all(&[b'a'; 4096]).unwrap();
        std::fs::write(dir.path().join("petit.out.log"), "peu").unwrap();
        std::fs::write(dir.path().join("penelope-2026-11-11.jsonl"), [b'j'; 4096]).unwrap();

        assert_eq!(bound_stream_logs(dir.path(), 1024), vec![log.clone()]);
        let kept = dir.path().join("inference.err.log.1");
        assert_eq!(std::fs::read(&kept).unwrap(), vec![b'a'; 4096]);
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&kept).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        service.write_all(b"suite\n").unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), b"suite\n");

        // Ni le petit flux, ni le journal du daemon, ni la génération gardée.
        assert_eq!(
            std::fs::read(dir.path().join("petit.out.log")).unwrap(),
            b"peu"
        );
        assert!(!dir.path().join("petit.out.log.1").exists());
        assert!(!dir.path().join("penelope-2026-11-11.jsonl.1").exists());
        assert!(bound_stream_logs(dir.path(), 1024).is_empty());

        // La rotation suivante remplace la génération gardée : une seule, jamais deux.
        service.write_all(&[b'b'; 2048]).unwrap();
        assert_eq!(bound_stream_logs(dir.path(), 1024), vec![log.clone()]);
        assert_eq!(std::fs::read(&kept).unwrap().len(), 6 + 2048);
        assert!(!dir.path().join("inference.err.log.2").exists());
        assert!(!dir.path().join("inference.err.log.1.1").exists());
    }

    #[test]
    fn a_missing_directory_or_a_symlink_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        assert!(bound_stream_logs(&dir.path().join("absent"), 1).is_empty());
        #[cfg(unix)]
        {
            let target = dir.path().join("ailleurs.txt");
            std::fs::write(&target, [b'x'; 64]).unwrap();
            std::os::unix::fs::symlink(&target, dir.path().join("lien.log")).unwrap();
            assert!(bound_stream_logs(dir.path(), 1).is_empty());
            assert_eq!(std::fs::metadata(&target).unwrap().len(), 64);
        }
    }
}
