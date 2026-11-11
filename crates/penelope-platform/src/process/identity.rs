//! L'identité d'un processus : son pid **et** sa date de lancement (#454).
//!
//! Un pid seul ne désigne personne : le système le redonne. Un fichier pid vieux de six
//! jours désignait un service de macOS, et le ramassage des orphelins aurait tué son
//! groupe au démarrage suivant. La date de lancement, elle, n'est pas redonnée : un pid
//! noté n'est signalé que si elle est restée la même.
//!
//! Linux la lit dans `/proc` (tics depuis l'amorçage, avec l'identifiant de l'amorçage) ;
//! macOS la demande à `ps -o lstart=`, en UTC pour qu'un changement de fuseau ne la
//! change pas. La valeur est opaque : elle ne se compare qu'à elle-même.

use std::path::Path;
use std::process::Stdio;

/// Date de lancement de `pid` ; `None` s'il n'existe pas ou qu'elle ne se lit pas.
pub fn started_at(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap_or_default();
        return start_ticks(&stat).map(|ticks| format!("{}:{ticks}", boot.trim()));
    }
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let at = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!at.is_empty()).then_some(at)
}

/// Le champ 22 de `/proc/<pid>/stat`. Le nom du programme (champ 2, entre parenthèses)
/// peut contenir des espaces et des parenthèses : on compte après la dernière.
fn start_ticks(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)
}

/// Vrai si `pid` est encore le processus lancé à `at`.
pub fn is_same(pid: u32, at: &str) -> bool {
    started_at(pid).as_deref() == Some(at)
}

/// Écrit le fichier pid : le pid, puis sa date de lancement. Rend faux sans rien écrire si
/// la date ne se lit pas : un fichier sans elle ne permettrait de tuer personne.
pub(super) fn note(path: &Path, pid: u32, at: Option<String>) -> std::io::Result<bool> {
    let Some(at) = at else {
        return Ok(false);
    };
    std::fs::write(path, format!("{pid}\n{at}\n"))?;
    Ok(true)
}

/// Relit un fichier pid. `None` pour un fichier illisible ou à l'ancien format (le pid
/// seul) : sans date de lancement, rien ne dit que ce pid est encore le nôtre.
pub(super) fn noted(raw: &str) -> Option<(u32, &str)> {
    let (pid, at) = raw.split_once('\n')?;
    let (pid, at) = (pid.trim().parse().ok()?, at.trim());
    (!at.is_empty()).then_some((pid, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_keeps_its_start_date() {
        let me = std::process::id();
        let at = started_at(me).expect("la date de lancement du test se lit");
        assert_eq!(started_at(me), Some(at.clone()));
        assert!(is_same(me, &at));
        assert!(!is_same(me, "Thu Jan 1 00:00:00 1970"));
        assert_eq!(started_at(999_999_999), None);
        assert_eq!(started_at(0), None);
    }

    #[test]
    fn the_start_field_is_read_after_the_program_name() {
        let stat = "42 (a b) c) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 8675309 1000 10";
        assert_eq!(start_ticks(stat), Some("8675309"));
        assert_eq!(start_ticks("42 (sh"), None);
    }

    #[test]
    fn an_old_pid_file_names_no_one() {
        assert_eq!(noted("8272"), None);
        assert_eq!(noted("8272\n"), None);
        assert_eq!(noted("abc\nWed Nov 11 14:53:58 2026\n"), None);
        assert_eq!(
            noted("8272\nWed Nov 11 14:53:58 2026\n"),
            Some((8272, "Wed Nov 11 14:53:58 2026"))
        );
    }
}
