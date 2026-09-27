//! Écriture du fichier de configuration et relevé des chemins modifiés.

use super::{Config, Result};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Écriture atomique : fichier temporaire dans le même répertoire, puis renommage.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    // Nom unique : deux écritures concurrentes ne se renomment pas le fichier l'une de
    // l'autre (« No such file or directory », issue #45).
    let tmp = dir.join(format!(
        ".{}.tmp{}-{}",
        path.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "f".into()),
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Liste grossière des chemins modifiés entre deux configurations (pour `changed_paths`).
pub(super) fn diff_paths(a: &Config, b: &Config) -> Vec<String> {
    let va = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
    let vb = serde_json::to_value(b).unwrap_or(serde_json::Value::Null);
    let mut out = Vec::new();
    diff_value("", &va, &vb, &mut out);
    out.sort();
    out.dedup();
    out
}

fn diff_value(prefix: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    match (a, b) {
        (serde_json::Value::Object(ma), serde_json::Value::Object(mb)) => {
            let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            for k in keys {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match (ma.get(k), mb.get(k)) {
                    (Some(x), Some(y)) => diff_value(&p, x, y, out),
                    _ => out.push(p),
                }
            }
        }
        _ => {
            if a != b {
                out.push(prefix.to_string());
            }
        }
    }
}
