//! Surveillance des sauvegardes (#330) : alerte au foyer dès 24 h sans sauvegarde
//! réussie, une fois par jour au plus, et rotation du dossier local `backups/`.
//!
//! ```text
//!  chaque passage ─► dernière réussie (ou début de la surveillance) il y a ≥ 24 h ?
//!                    └─ dernier avis il y a ≥ 24 h ? ─► « ⚠️ … cause … `penelope backup` »
//! ```
//!
//! Il avait fallu plusieurs jours d'échecs pour s'en rendre compte : un seul message le
//! matin de l'échec, et `doctor` qui n'alertait qu'au-delà de 48 h.

use super::*;
use penelope_app::backup_state::{ALERT_AFTER_MS, created_ms};

/// Instant du dernier avis (alerte, ou échec de la nuit).
const ALERTED_KEY: &str = "backup.alert.last";
/// Début de la surveillance : sans aucune sauvegarde réussie, l'âge part de là.
const WATCH_KEY: &str = "backup.watch_since";

/// L'avis du jour a été donné (échec de la nuit) : l'alerte attend 24 h.
pub(super) async fn said(s: &Services) {
    let _ = s.kv_set(ALERTED_KEY, &s.clock.now_ms().to_string()).await;
}

async fn kv_ms(s: &Services, key: &str) -> Option<i64> {
    s.kv_get(key).await.ok().flatten()?.parse().ok()
}

/// Un passage de surveillance : alerte si aucune sauvegarde n'a réussi depuis 24 h et
/// qu'aucun avis n'est parti depuis 24 h.
pub(super) async fn tick(
    s: &Services,
    messenger: Option<&Arc<dyn Messenger>>,
) -> anyhow::Result<()> {
    let now = s.clock.now_ms();
    let last: Option<Value> = s
        .kv_get(LAST_KEY)
        .await?
        .and_then(|v| serde_json::from_str(&v).ok());
    let ok_at = last.as_ref().and_then(created_ms);
    let since = match (ok_at, kv_ms(s, WATCH_KEY).await) {
        (Some(ok), _) => ok,
        (None, Some(w)) => w,
        (None, None) => {
            s.kv_set(WATCH_KEY, &now.to_string()).await?;
            return Ok(());
        }
    };
    if now - since < ALERT_AFTER_MS
        || kv_ms(s, ALERTED_KEY)
            .await
            .is_some_and(|a| now - a < ALERT_AFTER_MS)
    {
        return Ok(());
    }
    let Some(m) = messenger else {
        return Ok(());
    };
    let text = alert_text(s, now - since, ok_at.is_some()).await;
    let origin = crate::bus::Origin::Internal {
        source: "backup".into(),
    };
    m.send_text(&origin, &text)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    said(s).await;
    Ok(())
}

/// Le texte de l'alerte : depuis quand, la cause connue, la commande.
async fn alert_text(s: &Services, age_ms: i64, ever: bool) -> String {
    let hours = age_ms / 3_600_000;
    let since = if ever {
        format!("Aucune sauvegarde réussie depuis {hours} h")
    } else {
        format!("Aucune sauvegarde réussie depuis la mise en route de la surveillance ({hours} h)")
    };
    let cause: Option<String> = s
        .kv_get(LAST_ERROR_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<Value>(&v).ok())
        .and_then(|e| e["error"].as_str().map(String::from));
    let cfg = s.config.config();
    let why = match (cause, cfg.backup.effective_provider()) {
        (Some(c), _) => format!(" ; dernier échec : {c}"),
        (None, None) => " ; aucun fournisseur configuré".into(),
        (None, Some(_)) if cfg.backup.cron.trim().is_empty() => {
            " ; `backup.cron` est vide, rien ne part la nuit".into()
        }
        (None, Some(_)) => {
            " ; aucune tentative (daemon arrêté à l'heure de la sauvegarde ?)".into()
        }
    };
    let fix = if cfg.backup.effective_provider().is_none() {
        "`penelope backup setup`"
    } else {
        "`penelope backup`"
    };
    format!("⚠️ {since}{why}. Relancer : {fix}.")
}

/// Garde les `keep` archives les plus récentes du dossier local, et autant d'instantanés
/// de base ; `archives` faux : les archives ne sont pas touchées (le dossier est celui du
/// fournisseur, qui a sa rotation). Renvoie les fichiers effacés.
pub(crate) fn prune_local(dir: &Path, keep: usize, archives: bool) -> Vec<String> {
    let mut removed = Vec::new();
    let all: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("penelope-"))
        .collect();
    let kinds: &[&str] = if archives {
        &[s3::ARCHIVE_SUFFIX, ".db"]
    } else {
        &[".db"]
    };
    for suffix in kinds {
        let mut names: Vec<&String> = all.iter().filter(|n| n.ends_with(suffix)).collect();
        names.sort();
        names.reverse();
        for n in names.into_iter().skip(keep) {
            if std::fs::remove_file(dir.join(n)).is_ok() {
                removed.push(n.clone());
            }
        }
    }
    removed
}
