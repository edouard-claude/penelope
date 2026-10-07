//! L'état des sauvegardes tel que le digest du matin le lit (#330) : les clés `kv` que
//! `penelope_ops::backup` écrit, et la ligne qui en sort. Ici parce que le digest
//! (orchestrateur) ne dépend pas de `penelope-ops`.

use crate::services::Services;
use serde_json::Value;

/// Rapport de la dernière sauvegarde réussie, envoyée au fournisseur.
pub const LAST_KEY: &str = "backup.last";
/// Dernier échec : `{"at_ms": …, "error": "…"}` ; effacé par une sauvegarde réussie.
pub const LAST_ERROR_KEY: &str = "backup.last_error";
/// Seuil de l'alerte et du rouge de `doctor` : 24 h sans sauvegarde réussie.
pub const ALERT_AFTER_MS: i64 = 24 * 3_600_000;

/// Instant (ms) d'un rapport de sauvegarde : `manifest.created_at`.
pub fn created_ms(report: &Value) -> Option<i64> {
    report["manifest"]["created_at"]
        .as_str()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp_millis())
}

/// Libellé court d'un fournisseur.
pub fn provider_label(provider: &str) -> &str {
    match provider {
        "s3" => "S3",
        "dir" => "dossier",
        "icloud" => "iCloud Drive",
        other => other,
    }
}

/// La ligne du digest : « Sauvegarde : ✅ cette nuit, 137 Mo, S3 », ou « ❌ » avec la
/// cause et la commande. `None` quand aucune sauvegarde n'est attendue (`backup.cron`
/// vide et aucun fournisseur).
pub async fn digest_line(s: &Services) -> Option<String> {
    let cfg = s.config.config();
    if cfg.backup.cron.trim().is_empty() && cfg.backup.effective_provider().is_none() {
        return None;
    }
    let read = |v: Option<String>| v.and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let last = read(s.kv_get(LAST_KEY).await.ok().flatten());
    let error = read(s.kv_get(LAST_ERROR_KEY).await.ok().flatten());
    let now = s.clock.now_ms();
    let ok_at = last.as_ref().and_then(created_ms);
    let err_at = error.as_ref().and_then(|e| e["at_ms"].as_i64());
    let cause = error
        .as_ref()
        .and_then(|e| e["error"].as_str())
        .map(|e| e.chars().take(160).collect::<String>());
    match (ok_at, err_at) {
        (Some(ok), err) if now - ok < ALERT_AFTER_MS && err.is_none_or(|e| e < ok) => {
            let last = last.unwrap_or_default();
            Some(format!(
                "Sauvegarde : ✅ cette nuit, {} Mo, {}",
                last["bytes"].as_u64().unwrap_or(0) / (1024 * 1024),
                provider_label(last["pushed"]["provider"].as_str().unwrap_or("?"))
            ))
        }
        (ok, _) => {
            let since = match ok {
                Some(ok) => format!("aucune réussie depuis {} h", (now - ok) / 3_600_000),
                None => "aucune réussie encore".into(),
            };
            let why = match (cause, cfg.backup.effective_provider()) {
                (Some(c), _) => format!(" ; dernier échec : {c}"),
                (None, None) => " ; aucun fournisseur : `penelope backup setup`".into(),
                (None, Some(_)) => String::new(),
            };
            Some(format!(
                "Sauvegarde : ❌ {since}{why}. Relancer : `penelope backup`"
            ))
        }
    }
}
