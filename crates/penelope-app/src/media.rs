//! Pièces jointes reçues : photos et fichiers, rangés hors de la base (§14.4).
//!
//! Les photos vivent sous `{data}/media/photos` : le tour qui les porte les relit pour
//! les montrer au modèle. Un fichier qui n'est pas un document ingérable est déposé dans
//! le premier workspace, là où les outils de fichiers et le shell peuvent l'atteindre.

use crate::services::Services;
use base64::Engine;
use std::path::{Path, PathBuf};

/// Taille maximale d'une image confiée au modèle, en octets.
pub const IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Type MIME d'une image, d'après ses premiers octets. `None` : pas une image lisible.
pub fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        _ => None,
    }
}

/// Largeur et hauteur d'une image, lues dans son en-tête (PNG, JPEG, GIF, WebP) : des
/// coordonnées rendues par un modèle ne servent qu'avec la taille de l'image qu'il a vue
/// (issue #125).
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let be16 = |b: &[u8], i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let le16 = |b: &[u8], i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    let be32 = |b: &[u8], i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le24 = |b: &[u8], i: usize| {
        Some(u32::from_le_bytes([
            *b.get(i)?,
            *b.get(i + 1)?,
            *b.get(i + 2)?,
            0,
        ]))
    };
    match image_mime(bytes)? {
        "image/png" => Some((be32(bytes, 16)?, be32(bytes, 20)?)),
        "image/gif" => Some((le16(bytes, 6)?, le16(bytes, 8)?)),
        "image/webp" => match bytes.get(12..16)? {
            b"VP8 " => Some((le16(bytes, 26)? & 0x3FFF, le16(bytes, 28)? & 0x3FFF)),
            b"VP8L" => {
                let b = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((b & 0x3FFF) + 1, ((b >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((le24(bytes, 24)? + 1, le24(bytes, 27)? + 1)),
            _ => None,
        },
        _ => {
            // JPEG : segments jusqu'au premier SOFn (hors DHT, JPG, DAC).
            let mut i = 2;
            while i + 9 < bytes.len() {
                if bytes[i] != 0xFF {
                    return None;
                }
                let marker = bytes[i + 1];
                if marker == 0xFF {
                    i += 1;
                    continue;
                }
                let len = be16(bytes, i + 2)? as usize;
                if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                    return Some((be16(bytes, i + 7)?, be16(bytes, i + 5)?));
                }
                i += 2 + len;
            }
            None
        }
    }
}

fn extension_of(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "jpg",
    }
}

/// Enregistre une photo reçue ; renvoie son chemin.
pub fn save_photo(s: &Services, bytes: &[u8]) -> Result<PathBuf, String> {
    let mime = image_mime(bytes).ok_or("ce fichier n'est pas une image reconnue")?;
    if bytes.len() > IMAGE_MAX_BYTES {
        return Err(format!(
            "image trop lourde ({} Mo, {} Mo au plus)",
            bytes.len() / (1024 * 1024),
            IMAGE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let dir = s.platform.dirs.data().join("media").join("photos");
    let path = dir.join(format!(
        "{}.{}",
        penelope_kernel::ids::Ulid::new(),
        extension_of(mime)
    ));
    write(&path, bytes)?;
    Ok(path)
}

/// Relit une image enregistrée et la rend en URI `data:` pour le modèle.
pub fn data_url(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("image {} illisible : {e}", path.display()))?;
    let mime = image_mime(&bytes).ok_or("ce fichier n'est pas une image reconnue")?;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

/// Dépose un fichier reçu dans le workspace (`<workspace>/telegram/`), sous un nom sûr.
pub fn save_attachment(s: &Services, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let workspace = crate::helpers::default_workspaces(s)
        .into_iter()
        .next()
        .ok_or("aucun workspace configuré")?;
    let safe = safe_file_name(name);
    let dir = workspace.join("telegram");
    let mut path = dir.join(&safe);
    let mut n = 2;
    while path.exists() {
        let p = Path::new(&safe);
        let stem = p.file_stem().map(|x| x.to_string_lossy().to_string());
        let ext = p.extension().map(|x| x.to_string_lossy().to_string());
        let candidate = match ext {
            Some(e) => format!("{}-{n}.{e}", stem.unwrap_or_default()),
            None => format!("{safe}-{n}"),
        };
        path = dir.join(candidate);
        n += 1;
    }
    write(&path, bytes)?;
    Ok(path)
}

/// Range l'original d'un document ingéré dans `attachments/` du vault, sous un nom libre
/// dans tout le vault ; il n'est plus jamais modifié. Renvoie son nom de fichier.
pub fn save_document_original(
    vault: &Path,
    slug: &str,
    name: &str,
    bytes: &[u8],
) -> Result<String, String> {
    let ext = penelope_memory::ingest::extension(name);
    let named = |stem: &str| {
        if ext.is_empty() {
            stem.to_string()
        } else {
            format!("{stem}.{ext}")
        }
    };
    let resolver = penelope_memory::wiki::Resolver::scan(vault);
    let mut file = named(slug);
    let mut n = 2;
    while resolver.is_taken(&file) {
        file = named(&format!("{slug}-{n}"));
        n += 1;
    }
    let path = vault
        .join(penelope_memory::wiki::ATTACHMENTS_DIR)
        .join(&file);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    penelope_kernel::config::atomic_write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(file)
}

/// Nom de fichier sans séparateur de chemin ni caractère de contrôle.
pub fn safe_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "fichier".into()
    } else {
        cleaned.chars().take(120).collect()
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #125 : la taille d'une capture se lit dans son en-tête, sans décoder l'image.
    #[test]
    fn image_sizes_are_read_from_headers() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1179u32.to_be_bytes());
        png.extend_from_slice(&2556u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((1179, 2556)));

        let gif = [
            b'G', b'I', b'F', b'8', b'9', b'a', 0x40, 0x01, 0xF0, 0x00, 0, 0,
        ];
        assert_eq!(image_size(&gif), Some((320, 240)));

        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x0A, 0x00, 0x05, 0xA0, 3]);
        jpeg.extend_from_slice(&[0; 12]);
        assert_eq!(image_size(&jpeg), Some((1440, 2560)));

        assert_eq!(image_size(b"pas une image"), None);
    }

    #[test]
    fn images_are_recognised_by_their_magic_bytes() {
        assert_eq!(image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_mime(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(image_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_mime(b"%PDF-1.7"), None);
    }

    #[test]
    fn attachment_names_cannot_escape_their_directory() {
        assert_eq!(safe_file_name("../../.ssh/id_rsa"), "id_rsa");
        assert_eq!(safe_file_name(".env"), "env");
        assert_eq!(safe_file_name("a\u{0}b:c.txt"), "a_b_c.txt");
        assert_eq!(safe_file_name(""), "fichier");
    }
}
