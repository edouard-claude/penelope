//! Relais de mise à jour (issue #36) : un job launchd éphémère, **hors du job du service**,
//! recharge le service puis vérifie que le nouveau binaire démarre.
//!
//! ```text
//!  daemon (job com.penelope.daemon)
//!     │ launchctl bootstrap relais.plist        (le relais est un job à part)
//!     ▼
//!  relais (job com.penelope.daemon.reloader)
//!     ├─ bootout du service ─► le daemon s'arrête, le relais continue
//!     ├─ bootstrap du service (refusé : fichier d'origine remis, nouvel essai)
//!     ├─ garde-fou : `first_boot_ms` du fichier d'état renseigné avant l'échéance ?
//!     │     non ─► binaire précédent remis au chemin stable, note, rechargement
//!     └─ bootout de lui-même
//! ```
//!
//! `launchctl bootout` rend la main avant la fin de l'arrêt du service : un `bootstrap`
//! lancé aussitôt échoue (« 5: Input/output error », code de sortie non fiable) et le
//! service reste déchargé, ce qu'a produit le rechargement lancé par le daemon lui-même.
//! Le relais attend que le job ait disparu, vérifie le `bootstrap` et le retente ; il vit
//! dans son propre job, à l'abri de l'arrêt du daemon.

use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};

/// Ce que le relais doit faire.
#[derive(Debug, Clone, PartialEq)]
pub struct HandOff {
    /// Domaine launchd (`gui/<uid>`).
    pub domain: String,
    /// Service à recharger ou à surveiller.
    pub service_label: String,
    pub service_plist: PathBuf,
    /// Fichier de service d'origine, remis si launchd refuse le nouveau.
    pub backup_plist: Option<PathBuf>,
    /// Recharger le service (`bootout` puis `bootstrap`) ; sinon, surveiller seulement.
    pub reload: bool,
    /// Binaire lancé par le service, au chemin stable.
    pub binary: PathBuf,
    /// Binaire remis au chemin stable si le nouveau ne démarre jamais.
    pub previous: Option<PathBuf>,
    /// État de la mise à jour : `"first_boot_ms": null` tant que le nouveau binaire n'a pas
    /// démarré ; absent une fois la mise à jour tranchée.
    pub state_file: PathBuf,
    /// Note lue au démarrage suivant pour annoncer le retour arrière.
    pub rolled_back_note: PathBuf,
    /// Version installée, puis version remise en cas de retour arrière.
    pub to_version: String,
    pub from_version: String,
    /// Attente avant d'agir, le temps que la réponse parte.
    pub delay_s: u64,
    /// Délai accordé au nouveau binaire pour démarrer.
    pub guard_s: u64,
    /// Répertoire du script, de sa définition et de son journal.
    pub work_dir: PathBuf,
    /// `launchctl` (remplaçable en test).
    pub launchctl: PathBuf,
}

/// `launchctl` du système.
pub const LAUNCHCTL: &str = "/bin/launchctl";

/// Délai accordé par défaut au nouveau binaire pour démarrer, redémarrages de launchd
/// compris (10 s d'écart entre deux lancements).
pub const GUARD_S: u64 = 120;

/// Domaine launchd de l'utilisateur courant.
pub fn gui_domain() -> Result<String> {
    let out = std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| PlatformError::Service(format!("uid : {e}")))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        return Err(PlatformError::Service("uid introuvable".into()));
    }
    Ok(format!("gui/{uid}"))
}

/// Chaîne entre apostrophes pour `sh`.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn qp(p: &Path) -> String {
    q(&p.to_string_lossy())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

impl HandOff {
    /// Étiquette du job relais.
    pub fn helper_label(&self) -> String {
        format!("{}.reloader", self.service_label)
    }

    pub fn script_path(&self) -> PathBuf {
        self.work_dir.join("reloader.sh")
    }

    pub fn plist_path(&self) -> PathBuf {
        self.work_dir.join(format!("{}.plist", self.helper_label()))
    }

    pub fn log_path(&self) -> PathBuf {
        self.work_dir.join("reloader.log")
    }

    /// Script du relais.
    pub fn script(&self) -> String {
        let opt = |p: &Option<PathBuf>| p.as_deref().map(qp).unwrap_or_else(|| "''".into());
        format!(
            r#"#!/bin/sh
# Relais de mise à jour de Pénélope (issue #36) : job launchd à part, hors du job du service.
LAUNCHCTL={launchctl}
DOMAIN={domain}
SERVICE="$DOMAIN"/{label}
HELPER="$DOMAIN"/{helper}
PLIST={plist}
BACKUP={backup}
BINARY={binary}
PREVIOUS={previous}
STATE={state}
NOTE={note}
LOG={log}

log() {{ echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" >> "$LOG"; }}
loaded() {{ "$LAUNCHCTL" print "$SERVICE" >/dev/null 2>&1; }}
never_booted() {{ [ -f "$STATE" ] && grep -q '"first_boot_ms": null' "$STATE"; }}
# `bootout` rend la main avant la fin de l'arrêt : attendre que le job ait disparu, puis
# vérifier le `bootstrap` (son code de sortie ne suffit pas) et le retenter.
reload() {{
  "$LAUNCHCTL" bootout "$SERVICE" >> "$LOG" 2>&1
  i=0
  while loaded && [ "$i" -lt 120 ]; do sleep 0.5; i=$((i + 1)); done
  n=0
  while [ "$n" -lt 5 ]; do
    "$LAUNCHCTL" bootstrap "$DOMAIN" "$PLIST" >> "$LOG" 2>&1
    loaded && return 0
    n=$((n + 1))
    sleep 1
  done
  return 1
}}
finish() {{
  log "fin du relais"
  exec "$LAUNCHCTL" bootout "$HELPER"
}}

log "relais démarré"
sleep {delay}
if [ {reload} = 1 ]; then
  log "rechargement de $SERVICE"
  if ! reload; then
    if [ -n "$BACKUP" ] && [ -f "$BACKUP" ]; then
      log "fichier de service refusé par launchd : original remis"
      cp "$BACKUP" "$PLIST"
      reload || log "rechargement de l'original refusé"
    else
      log "rechargement refusé"
    fi
  fi
fi

deadline=$(( $(date +%s) + {guard} ))
while [ "$(date +%s)" -lt "$deadline" ]; do
  if ! never_booted; then
    log "démarrage constaté : mise à jour entre les mains du binaire lancé"
    [ -n "$BACKUP" ] && rm -f "$BACKUP"
    finish
  fi
  sleep 1
done

log "nouveau binaire jamais démarré après {guard} s : retour arrière"
if [ -n "$PREVIOUS" ] && [ -f "$PREVIOUS" ] \
  && cp "$PREVIOUS" "$BINARY.relay" && chmod 755 "$BINARY.relay" \
  && mv -f "$BINARY.relay" "$BINARY"; then
  printf '%s\n%s\n' {to} {from} > "$NOTE"
  log "binaire précédent remis"
else
  printf '%s\n\n%s\n' {to} "binaire précédent introuvable ou non copiable" > "$NOTE"
  log "binaire précédent introuvable ou non copiable"
fi
rm -f "$STATE"
reload || log "rechargement refusé après retour arrière"
[ -n "$BACKUP" ] && rm -f "$BACKUP"
finish
"#,
            launchctl = qp(&self.launchctl),
            domain = q(&self.domain),
            label = q(&self.service_label),
            helper = q(&self.helper_label()),
            plist = qp(&self.service_plist),
            backup = opt(&self.backup_plist),
            binary = qp(&self.binary),
            previous = opt(&self.previous),
            state = qp(&self.state_file),
            note = qp(&self.rolled_back_note),
            log = qp(&self.log_path()),
            delay = self.delay_s,
            reload = u8::from(self.reload),
            guard = self.guard_s,
            to = q(&self.to_version),
            from = q(&self.from_version),
        )
    }

    /// Définition launchd du relais : lancé au chargement, jamais relancé.
    pub fn plist(&self) -> String {
        let log = xml(&self.log_path().to_string_lossy());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>{script}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
            label = xml(&self.helper_label()),
            script = xml(&self.script_path().to_string_lossy()),
        )
    }

    /// Écrit le script et la définition du relais.
    pub fn write(&self) -> Result<()> {
        std::fs::create_dir_all(&self.work_dir)?;
        std::fs::write(self.script_path(), self.script())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(self.script_path(), std::fs::Permissions::from_mode(0o700))?;
        }
        std::fs::write(self.plist_path(), self.plist())?;
        Ok(())
    }

    /// Écrit puis charge le relais dans launchd : il démarre aussitôt, hors du job qui le
    /// demande.
    pub fn launch(&self) -> Result<()> {
        if !cfg!(target_os = "macos") {
            return Err(PlatformError::Unsupported(
                "relais de mise à jour : macOS seulement".into(),
            ));
        }
        self.write()?;
        let helper = format!("{}/{}", self.domain, self.helper_label());
        // Un relais précédent encore chargé (terminé ou non) laisse la place.
        let _ = std::process::Command::new(&self.launchctl)
            .args(["bootout", &helper])
            .output();
        let out = std::process::Command::new(&self.launchctl)
            .arg("bootstrap")
            .arg(&self.domain)
            .arg(self.plist_path())
            .output()
            .map_err(|e| PlatformError::Service(format!("launchctl bootstrap : {e}")))?;
        if !out.status.success() {
            return Err(PlatformError::Service(format!(
                "launchctl bootstrap du relais : {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// `launchctl` simulé : journal des appels, service « chargé » par un fichier, et un
    /// `bootstrap` qui peut refuser ou « démarrer » le nouveau binaire.
    fn fake_launchctl(dir: &Path, bootstrap: &str) -> PathBuf {
        let path = dir.join("launchctl");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nD={d}\necho \"$*\" >> \"$D/calls\"\ncase \"$1\" in\n\
                 print) [ -f \"$D/loaded\" ] ;;\n\
                 bootout) rm -f \"$D/loaded\" ;;\n\
                 bootstrap) n=$(cat \"$D/boots\" 2>/dev/null || echo 0); n=$((n + 1)); \
                 echo $n > \"$D/boots\"; {bootstrap} ;;\n\
                 esac\n",
                d = qp(dir),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn handoff(dir: &Path, launchctl: PathBuf) -> HandOff {
        std::fs::write(dir.join("service.plist"), "nouveau").unwrap();
        std::fs::write(dir.join("service.plist.sources"), "origine").unwrap();
        std::fs::write(dir.join("penelope"), "binaire neuf").unwrap();
        std::fs::write(dir.join("previous"), "binaire précédent").unwrap();
        std::fs::write(
            dir.join("upgrade.json"),
            "{\n  \"to_version\": \"9.9.9\",\n  \"first_boot_ms\": null\n}",
        )
        .unwrap();
        std::fs::write(dir.join("loaded"), "").unwrap();
        HandOff {
            domain: "gui/501".into(),
            service_label: "com.penelope.test".into(),
            service_plist: dir.join("service.plist"),
            backup_plist: Some(dir.join("service.plist.sources")),
            reload: true,
            binary: dir.join("penelope"),
            previous: Some(dir.join("previous")),
            state_file: dir.join("upgrade.json"),
            rolled_back_note: dir.join("upgrade.rolled-back"),
            to_version: "9.9.9".into(),
            from_version: "0.13.0".into(),
            delay_s: 0,
            guard_s: 3,
            work_dir: dir.join("relais"),
            launchctl,
        }
    }

    fn run(h: &HandOff) -> Vec<String> {
        h.write().unwrap();
        let out = Command::new("/bin/sh")
            .arg(h.script_path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::read_to_string(h.work_dir.parent().unwrap().join("calls"))
            .unwrap()
            .lines()
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_started_binary_ends_the_relay_without_touching_anything() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // Le nouveau binaire démarre : il renseigne `first_boot_ms`.
        let boot = format!(
            "touch \"$D/loaded\"; printf '{{\"first_boot_ms\": 42}}' > {}",
            qp(&d.join("upgrade.json"))
        );
        let h = handoff(d, fake_launchctl(d, &boot));
        let calls = run(&h);
        assert_eq!(
            calls,
            vec![
                "bootout gui/501/com.penelope.test".to_string(),
                "print gui/501/com.penelope.test".into(),
                format!("bootstrap gui/501 {}", d.join("service.plist").display()),
                "print gui/501/com.penelope.test".into(),
                "bootout gui/501/com.penelope.test.reloader".into(),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(d.join("penelope")).unwrap(),
            "binaire neuf"
        );
        assert!(!d.join("upgrade.rolled-back").exists());
        assert!(
            !d.join("service.plist.sources").exists(),
            "sauvegarde retirée"
        );
        let log = std::fs::read_to_string(h.log_path()).unwrap();
        assert!(log.contains("démarrage constaté"), "{log}");
    }

    #[test]
    fn a_binary_that_never_starts_is_replaced_by_the_previous_one() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let h = handoff(d, fake_launchctl(d, "touch \"$D/loaded\""));
        let calls = run(&h);
        assert_eq!(
            std::fs::read_to_string(d.join("penelope")).unwrap(),
            "binaire précédent"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("upgrade.rolled-back")).unwrap(),
            "9.9.9\n0.13.0\n"
        );
        assert!(!d.join("upgrade.json").exists());
        let boots = calls.iter().filter(|c| c.starts_with("bootstrap")).count();
        assert_eq!(boots, 2, "rechargé après le retour arrière : {calls:?}");
        assert_eq!(
            calls.last().unwrap(),
            "bootout gui/501/com.penelope.test.reloader"
        );
    }

    #[test]
    fn a_bootstrap_refused_while_the_job_winds_down_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // Premier essai refusé (job pas encore retiré), le second passe.
        let boot = format!(
            "[ $n -eq 1 ] && exit 0; touch \"$D/loaded\"; printf '{{\"first_boot_ms\": 3}}' > {}",
            qp(&d.join("upgrade.json"))
        );
        let h = handoff(d, fake_launchctl(d, &boot));
        let calls = run(&h);
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("bootstrap")).count(),
            2
        );
        assert_eq!(
            std::fs::read_to_string(d.join("service.plist")).unwrap(),
            "nouveau",
            "le nouveau fichier est gardé"
        );
        assert!(!d.join("upgrade.rolled-back").exists());
    }

    #[test]
    fn a_refused_service_file_is_replaced_by_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // Le nouveau fichier est refusé (rien de chargé, code de sortie 0 comme launchd) ;
        // le fichier d'origine démarre le binaire.
        let boot = format!(
            "grep -q origine \"$3\" || exit 0; touch \"$D/loaded\"; \
             printf '{{\"first_boot_ms\": 7}}' > {}",
            qp(&d.join("upgrade.json"))
        );
        let h = handoff(d, fake_launchctl(d, &boot));
        let calls = run(&h);
        assert_eq!(
            std::fs::read_to_string(d.join("service.plist")).unwrap(),
            "origine"
        );
        assert_eq!(
            calls.iter().filter(|c| c.starts_with("bootstrap")).count(),
            6,
            "cinq essais vérifiés, puis l'original : {calls:?}"
        );
        let log = std::fs::read_to_string(h.log_path()).unwrap();
        assert!(log.contains("original remis"), "{log}");
    }

    #[test]
    fn paths_are_quoted_and_the_helper_is_not_kept_alive() {
        let dir = tempfile::tempdir().unwrap();
        let mut h = handoff(dir.path(), PathBuf::from("/bin/launchctl"));
        h.binary = PathBuf::from("/Users/x/l'appli & co/penelope");
        let script = h.script();
        assert!(script.contains(r#"BINARY='/Users/x/l'\''appli & co/penelope'"#));
        let plist = h.plist();
        assert!(plist.contains("<string>com.penelope.test.reloader</string>"));
        assert!(plist.contains("<key>KeepAlive</key>\n    <false/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        // Le relais se retire lui-même en dernier.
        assert!(script.trim_end().ends_with("finish"));
        assert!(script.contains("exec \"$LAUNCHCTL\" bootout \"$HELPER\""));
    }
}
