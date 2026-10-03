//! Images et sons rendus par un outil MCP (issue #304) : posés sur disque, le transcript
//! ne garde que leur chemin.
//!
//! ```text
//! bloc image/audio (base64) ──► plafond ──► <dossier>/<sha256, 16 car.>.<ext>
//!                                  │           └─ bloc réécrit : mention courte + `saved`
//!                                  └─ au-delà, ou illisible : rien d'écrit, la mention le dit
//! ```
//!
//! Le dossier : `mcp-media/` du répertoire du run pour une étape de workflow (il part avec
//! le workspace du run), sinon `{data}/media/mcp/<session>/` (la purge d'une session et la
//! rétention l'emportent). Le nom suit le contenu : la même image rendue deux fois est le
//! même fichier. Seul ce module écrit `saved` : celui qu'un serveur glisserait dans un
//! bloc est retiré, sinon il ferait montrer au modèle un fichier de son choix.

use base64::Engine;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Plafond d'un média rendu par un outil, en octets décodés : celui d'une photo reçue.
pub const MEDIA_MAX_BYTES: usize = penelope_app::media::IMAGE_MAX_BYTES;

/// Champ du bloc réécrit qui porte le chemin du fichier enregistré.
pub const SAVED: &str = "saved";

/// Sous-dossier du répertoire d'un run.
pub const RUN_DIR: &str = "mcp-media";

/// Dossier des médias d'une session hors workflow, sous `{data}`.
pub fn session_dir(data: &Path, session_id: &str) -> PathBuf {
    root(data).join(penelope_app::media::safe_file_name(session_id))
}

/// Racine des médias MCP hors workflow, que la rétention parcourt.
pub fn root(data: &Path) -> PathBuf {
    penelope_app::media::mcp_root(data)
}

/// Réécrit les blocs `image` et `audio` d'un résultat MCP : la donnée est écrite dans
/// `dir` et remplacée par une mention qui donne le chemin ; aucun base64 ne reste.
pub fn store(v: &mut Value, dir: &Path) {
    let Some(blocks) = v.get_mut("content").and_then(|c| c.as_array_mut()) else {
        return;
    };
    for b in blocks {
        if let Some(o) = b.as_object_mut() {
            o.remove(SAVED);
        }
        let kind = match b.get("type").and_then(Value::as_str) {
            Some("image") => Kind::Image,
            Some("audio") => Kind::Audio,
            _ => continue,
        };
        let Some(data) = b.get("data").and_then(Value::as_str) else {
            continue;
        };
        let mime = b
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or(kind.default_mime())
            .to_string();
        *b = match save(data, &mime, dir) {
            Ok((path, size)) => json!({
                "type": kind.name(),
                "mimeType": mime,
                "bytes": size,
                SAVED: path.to_string_lossy(),
                "text": format!(
                    "[{} {mime}, {}, {} : {}]",
                    kind.name(),
                    human(size),
                    kind.saved(),
                    path.display()
                ),
            }),
            Err(why) => json!({
                "type": kind.name(),
                "mimeType": mime,
                "text": format!("[{} {mime}, {why} : non {}]", kind.name(), kind.saved()),
            }),
        };
    }
}

/// Les images enregistrées d'un résultat réécrit par [`store`], celles qui vivent sous
/// une des `roots` seulement.
pub fn saved_images(v: &Value, roots: &[PathBuf]) -> Vec<PathBuf> {
    let Some(blocks) = v.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|b| b.get(SAVED).and_then(Value::as_str))
        .map(PathBuf::from)
        .filter(|p| roots.iter().any(|r| p.starts_with(r)))
        .collect()
}

#[derive(Clone, Copy)]
enum Kind {
    Image,
    Audio,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Audio => "audio",
        }
    }
    fn saved(self) -> &'static str {
        match self {
            Kind::Image => "enregistrée",
            Kind::Audio => "enregistré",
        }
    }
    fn default_mime(self) -> &'static str {
        match self {
            Kind::Image => "image/png",
            Kind::Audio => "audio/wav",
        }
    }
}

/// Décode et écrit ; le chemin et la taille, ou ce qui l'empêche.
fn save(data: &str, mime: &str, dir: &Path) -> Result<(PathBuf, usize), String> {
    let cap = || format!("au-delà du plafond de {}", human(MEDIA_MAX_BYTES));
    // Le plafond se juge avant de décoder : un base64 de 50 Mo n'est pas alloué deux fois.
    if data.len() / 4 * 3 > MEDIA_MAX_BYTES + 3 {
        return Err(format!("{} environ, {}", human(data.len() / 4 * 3), cap()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| "base64 illisible".to_string())?;
    if bytes.len() > MEDIA_MAX_BYTES {
        return Err(format!("{}, {}", human(bytes.len()), cap()));
    }
    let hash = penelope_kernel::canonical::sha256_hex(&bytes);
    let path = dir.join(format!("{}.{}", &hash[..16], extension(mime)));
    if !path.exists() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
        penelope_kernel::config::atomic_write(&path, &bytes)
            .map_err(|e| format!("{} : {e}", path.display()))?;
    }
    Ok((path, bytes.len()))
}

fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" => "ogg",
        "audio/webm" => "webm",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "m4a",
        "audio/flac" => "flac",
        _ => "bin",
    }
}

fn human(bytes: usize) -> String {
    if bytes < 1024 * 1024 {
        format!("{} Ko", bytes.div_ceil(1024))
    } else {
        format!("{:.1} Mo", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests;
