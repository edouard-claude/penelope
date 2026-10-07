//! État des sauvegardes pour `doctor` et `self_status` : âge de la dernière, fournisseur,
//! phrase de passe, bucket joignable, clé de configuration retirée (#327).

use super::*;
use penelope_kernel::api::DoctorCheck;

/// État des sauvegardes, pour `doctor` et `self_status`.
pub async fn status(s: &Services) -> Value {
    let last: Option<Value> = s
        .kv_get(LAST_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok());
    let cfg = s.config.config();
    let target = provider::Target::resolve(
        &cfg.backup,
        s.platform.dirs.as_ref(),
        penelope_platform::dirs::home_dir().as_deref(),
    );
    json!({
        "last": last,
        "provider": cfg.backup.effective_provider(),
        "location": target.as_ref().map(|t| t.location()).ok(),
        "provider_error": target.err().map(|e| e.to_string()),
        "cron": cfg.backup.cron,
        "passphrase": passphrase(s).is_ok(),
        "media_included": cfg.backup.include_media,
    })
}

/// Les contrôles `doctor` des sauvegardes : l'état général, le bucket S3 s'il y en a un,
/// et une clé `backup.git_remote` restée dans le fichier.
pub async fn doctor_checks(s: &Services) -> Vec<DoctorCheck> {
    let mut checks = vec![doctor_check(s).await];
    if s.config.config().backup.effective_provider() == Some("s3") {
        checks.push(s3_doctor_check(s).await);
    }
    if let Some(c) = retired_remote_check(s) {
        checks.push(c);
    }
    checks
}

/// `backup.git_remote` encore dans `config.toml` (#327) : ignorée, et dite. Rien ne part
/// plus vers GitHub, ni vers le dépôt du vault.
fn retired_remote_check(s: &Services) -> Option<DoctorCheck> {
    let raw = std::fs::read_to_string(s.platform.dirs.config_file()).ok()?;
    let remote = retired_remote(&raw)?;
    Some(DoctorCheck::fail(
        "backup.git_remote",
        "Sauvegarde : clé retirée",
        format!(
            "`backup.git_remote = \"{remote}\"` est ignorée depuis la 1.0.46 : GitHub ne reçoit \
             plus de sauvegarde. {}",
            if s.config.config().backup.effective_provider().is_some() {
                "Le fournisseur configuré la remplace ; la ligne peut être effacée."
            } else {
                "Aucun fournisseur : rien ne part hors de cette machine."
            }
        ),
        Some("penelope backup setup".into()),
    ))
}

/// La valeur de `git_remote` dans la section `[backup]` d'un `config.toml`, s'il y en a.
pub(super) fn retired_remote(raw: &str) -> Option<String> {
    let mut section = String::new();
    for line in raw.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']', ' ']).to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if (section == "backup" && key == "git_remote") || key == "backup.git_remote" {
            let value = value.trim().trim_matches('"');
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// Contrôle `doctor` du bucket (#289) : joignable avec ces clés, dernière sauvegarde S3
/// et son âge.
async fn s3_doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "backup.s3";
    const LABEL: &str = "Sauvegarde S3";
    let cfg = s.config.config();
    let client = match s3::S3Client::from_config(&cfg.backup.s3, s.platform.secrets.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            return DoctorCheck::fail(
                ID,
                LABEL,
                e.to_string(),
                Some("penelope backup setup".into()),
            );
        }
    };
    let bucket = client.bucket().to_string();
    match tokio::time::timeout(std::time::Duration::from_secs(20), client.head_bucket()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return DoctorCheck::fail(ID, LABEL, e.to_string(), None),
        Err(_) => {
            return DoctorCheck::fail(
                ID,
                LABEL,
                format!(
                    "bucket `{bucket}` : {} ne répond pas en 20 s",
                    client.endpoint()
                ),
                None,
            );
        }
    }
    let st = status(s).await;
    let last = &st["last"];
    let (Some(key), Some(created)) = (
        last["pushed"]["key"].as_str(),
        last["manifest"]["created_at"].as_str(),
    ) else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            format!("bucket `{bucket}` joignable ; aucune sauvegarde S3 encore"),
            Some("penelope backup".into()),
        );
    };
    let age_h = age_hours(s, created);
    let detail = format!(
        "bucket `{bucket}` joignable ; dernière sauvegarde S3 il y a {age_h} h ({} Mo, {key})",
        last["pushed"]["bytes"].as_u64().unwrap_or(0) / (1024 * 1024)
    );
    if age_h > 48 {
        DoctorCheck::fail(ID, LABEL, detail, Some("penelope backup".into()))
    } else {
        DoctorCheck::ok(ID, LABEL, detail)
    }
}

/// Contrôle `doctor` : âge de la dernière sauvegarde, fournisseur, phrase de passe.
pub async fn doctor_check(s: &Services) -> DoctorCheck {
    const ID: &str = "backup";
    const LABEL: &str = "Sauvegarde";
    let st = status(s).await;
    if st["passphrase"] != true {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "aucune phrase de passe : rien n'est sauvegardé hors de cette machine",
            Some("penelope backup setup".into()),
        );
    }
    if let Some(e) = st["provider_error"].as_str() {
        return DoctorCheck::fail(ID, LABEL, e, Some("penelope backup setup".into()));
    }
    let Some(created) = st["last"]["manifest"]["created_at"].as_str() else {
        return DoctorCheck::fail(
            ID,
            LABEL,
            "aucune sauvegarde enregistrée",
            Some("penelope backup".into()),
        );
    };
    let age_h = age_hours(s, created);
    // Durée : l'instantané de la base grossit avec elle, sa dérive se voit ici (#77).
    let duration = match (
        st["last"]["duration_ms"].as_u64(),
        st["last"]["snapshot_ms"].as_u64(),
    ) {
        (Some(total), Some(snap)) => format!(
            ", {:.1} s dont {:.1} s d'instantané",
            total as f64 / 1000.0,
            snap as f64 / 1000.0
        ),
        _ => String::new(),
    };
    let towards = st["location"].as_str().unwrap_or("aucun fournisseur");
    let detail = format!(
        "dernière il y a {age_h} h ({} Mo{duration}), vers {towards}",
        st["last"]["bytes"].as_u64().unwrap_or(0) / (1024 * 1024),
    );
    if age_h > 48 {
        DoctorCheck::fail(ID, LABEL, detail, Some("penelope backup".into()))
    } else {
        DoctorCheck::ok(ID, LABEL, detail)
    }
}

pub(super) fn age_hours(s: &Services, created: &str) -> i64 {
    let then = chrono::DateTime::parse_from_rfc3339(created)
        .map(|t| t.timestamp_millis())
        .unwrap_or(0);
    ((s.clock.now_ms() - then) / 3_600_000).max(0)
}
