//! Démarrage à l'essai, confirmation de santé et retour arrière (§2.12).

use super::*;

/// Mise à jour en attente de confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub from_version: String,
    pub to_version: String,
    pub binary: PathBuf,
    pub previous: PathBuf,
    pub attempts: u32,
    pub installed_at: String,
    #[serde(default)]
    pub first_boot_ms: Option<i64>,
}

pub fn state_file(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade.json")
}

pub fn rolled_back_note(state_dir: &Path) -> PathBuf {
    state_dir.join("upgrade.rolled-back")
}

pub(super) fn write_pending(state_dir: &Path, p: &Pending) -> Result<(), String> {
    std::fs::create_dir_all(state_dir).map_err(|e| e.to_string())?;
    let raw = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    let tmp = state_dir.join("upgrade.json.tmp");
    std::fs::write(&tmp, raw).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, state_file(state_dir)).map_err(|e| e.to_string())
}

/// Mise à jour en attente, s'il y en a une.
pub fn pending(state_dir: &Path) -> Option<Pending> {
    std::fs::read_to_string(state_file(state_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Décision prise au démarrage de `penelope daemon`.
#[derive(Debug, Clone, PartialEq)]
pub enum Boot {
    /// Pas de mise à jour en cours.
    Normal,
    /// Nouveau binaire à l'essai : le chien de garde doit être armé.
    Trial { attempt: u32 },
    /// Nouveau binaire jamais confirmé : l'ancien est remis au même chemin, le processus
    /// doit s'arrêter pour que le service reparte avec lui (`KeepAlive`), sans toucher au
    /// fichier de service (issue #36).
    RolledBack { from: String, to: String },
}

/// À appeler avant d'ouvrir quoi que ce soit : un binaire qui plante plus loin est compté.
pub fn on_boot(state_dir: &Path, running_version: &str, now_ms: i64) -> Boot {
    let Some(mut p) = pending(state_dir) else {
        return Boot::Normal;
    };
    if running_version != p.to_version {
        // L'ancien binaire tourne : retour arrière fait, ou remplacement à la main.
        let _ = std::fs::remove_file(state_file(state_dir));
        return Boot::Normal;
    }
    p.attempts += 1;
    let first = *p.first_boot_ms.get_or_insert(now_ms);
    let expired = p.attempts > 1 && now_ms - first > HEALTH_WINDOW_MS;
    if expired || p.attempts > MAX_BOOT_ATTEMPTS {
        let _ = std::fs::remove_file(state_file(state_dir));
        let outcome = if p.previous.is_file() {
            replace_with(&p.previous, &p.binary)
        } else {
            Err(format!("{} introuvable", p.previous.display()))
        };
        // Pourquoi l'essai a échoué (issue #153) : la carte disait seulement « n'a pas
        // démarré correctement », et il a fallu lire `daemon.err.log` à la main pour
        // trouver un débordement de pile. La dernière ligne utile y est reprise.
        let why = last_error_line(state_dir).unwrap_or_default();
        let note = match &outcome {
            Ok(()) => format!("{}\n{}\n{why}\n", p.to_version, p.from_version),
            Err(e) => format!("{}\n\n{e}\n", p.to_version),
        };
        let _ = std::fs::write(rolled_back_note(state_dir), note);
        return match outcome {
            Ok(()) => Boot::RolledBack {
                from: p.to_version,
                to: p.from_version,
            },
            Err(_) => Boot::Normal,
        };
    }
    let attempt = p.attempts;
    let _ = write_pending(state_dir, &p);
    Boot::Trial { attempt }
}

/// `on_boot` à l'heure système.
pub fn on_boot_now(state_dir: &Path, running_version: &str) -> Boot {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    on_boot(state_dir, running_version, now_ms)
}

/// Quitte le processus si la santé n'est pas confirmée à temps : un nouveau binaire
/// bloqué finit ainsi en redémarrage, puis en retour arrière.
pub fn arm_watchdog(after: Duration) {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        if !CONFIRMED.load(Ordering::SeqCst) {
            eprintln!(
                "mise à jour : santé non confirmée après {} s, arrêt pour retour arrière",
                after.as_secs()
            );
            std::process::exit(75);
        }
    });
}

/// Ce qu'il faut annoncer au propriétaire après un démarrage sain.
#[derive(Debug, Clone, PartialEq)]
pub enum Confirmation {
    Upgraded {
        from: String,
        to: String,
    },
    RolledBack {
        from: String,
        to: String,
        /// Dernière ligne d'erreur du binaire à l'essai, quand on a pu la lire (#153).
        why: Option<String>,
    },
    RollbackFailed {
        from: String,
        error: String,
    },
}

/// Dernière ligne parlante de la sortie d'erreur du daemon : c'est là qu'un abandon
/// écrit ce qu'il a à dire (`stack overflow`, `fatal runtime error`), sans passer par le
/// journal JSON que le processus meurt avant d'écrire (issue #153).
pub(super) fn last_error_line(state_dir: &Path) -> Option<String> {
    // `<state>/../Logs/Penelope/daemon.err.log` sur macOS, `<state>/daemon.err.log`
    // ailleurs : les deux sont tentés, le premier lisible gagne.
    let candidates = [
        state_dir.join("daemon.err.log"),
        state_dir.join("../Logs/Penelope/daemon.err.log"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Les dernières lignes seulement : le fichier grossit sans fin.
        let line = text
            .lines()
            .rev()
            .take(40)
            .map(str::trim)
            .find(|l| {
                !l.is_empty()
                    && (l.contains("overflow")
                        || l.contains("fatal")
                        || l.contains("panic")
                        || l.contains("erreur")
                        || l.contains("error"))
            })
            .map(|l| l.chars().take(200).collect::<String>());
        if line.is_some() {
            // La ligne part dans un message : elle passe par le rédacteur (#148).
            return line.map(|l| penelope_observe::redact(&l));
        }
    }
    None
}

impl Confirmation {
    pub fn text(&self) -> String {
        match self {
            Confirmation::Upgraded { from, to } => {
                format!("⬆️ Pénélope est passée de {from} à {to}.")
            }
            Confirmation::RolledBack { from, to, why } => match why {
                Some(w) if !w.is_empty() => format!(
                    "⚠️ La version {from} n'a pas démarré : retour automatique à {to}.\n\n\
                     Dernière erreur du binaire à l'essai :\n`{w}`"
                ),
                _ => format!(
                    "⚠️ La version {from} n'a pas démarré correctement : retour automatique \
                     à {to}."
                ),
            },
            Confirmation::RollbackFailed { from, error } => format!(
                "❌ La version {from} ne démarre pas et le retour arrière a échoué ({error}) : \
                 réinstaller à la main."
            ),
        }
    }
}

/// Le daemon est sain : la mise à jour est validée.
pub fn confirm(state_dir: &Path, running_version: &str) -> Option<Confirmation> {
    CONFIRMED.store(true, Ordering::SeqCst);
    if let Ok(note) = std::fs::read_to_string(rolled_back_note(state_dir)) {
        let _ = std::fs::remove_file(rolled_back_note(state_dir));
        let mut lines = note.lines();
        let from = lines.next().unwrap_or("?").to_string();
        let to = lines.next().unwrap_or_default().to_string();
        let rest = lines.collect::<Vec<_>>().join(" ").trim().to_string();
        return Some(if to.is_empty() {
            Confirmation::RollbackFailed { from, error: rest }
        } else {
            Confirmation::RolledBack {
                from,
                to,
                why: (!rest.is_empty()).then_some(rest),
            }
        });
    }
    let p = pending(state_dir)?;
    if p.to_version != running_version {
        return None;
    }
    let _ = std::fs::remove_file(state_file(state_dir));
    Some(Confirmation::Upgraded {
        from: p.from_version,
        to: p.to_version,
    })
}

/// Après la reprise : quelques secondes de fonctionnement, base et journal accessibles,
/// puis confirmation et annonce.
pub async fn confirm_when_healthy(
    s: Arc<Services>,
    handle: Handle,
    messenger: Slot<dyn penelope_app::ports::Messenger>,
) {
    let state_dir = s.platform.dirs.state();
    if pending(&state_dir).is_none() && !rolled_back_note(&state_dir).exists() {
        CONFIRMED.store(true, Ordering::SeqCst);
        return;
    }
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        if handle.is_shutting_down() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Err(e) = s.kv_get("upgrade.health").await {
        tracing::error!(error = %e, "mise à jour : base inaccessible, pas de confirmation");
        return;
    }
    let Some(c) = confirm(&state_dir, crate::VERSION) else {
        return;
    };
    let (kind, payload) = match &c {
        Confirmation::Upgraded { from, to } => {
            ("upgrade.confirmed", json!({"from": from, "to": to}))
        }
        Confirmation::RolledBack { from, to, why } => (
            "upgrade.rolled_back",
            json!({"from": from, "to": to, "automatic": true, "why": why}),
        ),
        Confirmation::RollbackFailed { from, error } => (
            "upgrade.rollback_failed",
            json!({"from": from, "error": error}),
        ),
    };
    if let Err(e) = s.events.append(EventDraft::new(kind, payload)).await {
        tracing::error!(error = %e, "mise à jour : journal inaccessible");
    }
    tracing::info!("{}", c.text());
    // Telegram peut démarrer après la confirmation : on l'attend un peu pour l'annonce.
    for _ in 0..240 {
        if handle.is_shutting_down() {
            return;
        }
        if let Some(m) = messenger.get() {
            let origin = crate::helpers::owner_origin_of(&s);
            let _ = m.send_text(&origin, &c.text()).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Retour manuel au binaire précédent. Le binaire courant devient `.previous` : un second
/// retour arrière revient à la version quittée.
pub fn manual_rollback(binary: &Path, state_dir: &Path) -> Result<Value, String> {
    let previous = previous_path(binary);
    if !previous.is_file() {
        return Err(format!("aucun binaire précédent ({})", previous.display()));
    }
    let target = penelope_platform::process::binary_version(&previous)
        .map_err(|e| format!("binaire précédent inutilisable : {e}"))?;
    let dir = binary.parent().ok_or("binaire sans répertoire")?;
    preflight_writable(dir)?;
    let keep = dir.join(".penelope.rollback");
    std::fs::copy(binary, &keep).map_err(|e| e.to_string())?;
    replace_with(&previous, binary)?;
    std::fs::rename(&keep, &previous).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(state_file(state_dir));
    Ok(json!({"rolled_back": true, "from": crate::VERSION, "to": target, "binary": binary}))
}
