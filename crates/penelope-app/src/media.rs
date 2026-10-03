//! Pièces jointes reçues : photos et fichiers, rangés hors de la base (§14.4).
//!
//! Les photos vivent sous `{data}/media/photos` : le tour qui les porte les relit pour
//! les montrer au modèle. Les vocaux reçus gardent leur original sous `{data}/media/voice`,
//! à côté de leur transcription (issue #308). Un fichier qui n'est pas un document ingérable est déposé dans
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

/// Conserve l'original d'un vocal reçu sous `{data}/media/voice`, au format d'origine
/// (issue #308). `key` nomme le message d'où il vient : une réception rejouée réécrit le
/// même fichier au lieu d'en ajouter un. L'extension est celle de `file_name`
/// (`audio.ogg`), `ogg` à défaut. Renvoie le chemin.
pub fn save_voice(
    s: &Services,
    key: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let ext = Path::new(file_name)
        .extension()
        .map(|e| safe_file_name(&e.to_string_lossy()))
        .unwrap_or_else(|| "ogg".into());
    let path = s
        .platform
        .dirs
        .data()
        .join("media")
        .join("voice")
        .join(format!("{}.{ext}", safe_file_name(key)));
    write(&path, bytes)?;
    Ok(path)
}

/// Mention d'un vocal conservé, jointe au texte transcrit comme celle d'une photo : le
/// chemin y reste pour qu'un outil réutilise l'audio, et la purge d'une session l'y lit.
pub fn voice_note(path: &Path) -> String {
    format!(
        "\n(vocal enregistré : {} ; audio d'origine, à transmettre tel quel à un outil)",
        path.display()
    )
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

/// Une image réduite pour tenir dans les limites du fournisseur (issue #242).
#[derive(Debug, Clone, PartialEq)]
pub struct Reduction {
    pub before_bytes: usize,
    pub before: Option<(u32, u32)>,
    pub after: Option<(u32, u32)>,
    /// L'image réduite, en JPEG.
    pub bytes: Vec<u8>,
}

/// Grand côté en dessous duquel on ne réduit plus : une capture y deviendrait illisible.
const MIN_SIDE: u32 = 512;

/// Réduit l'image de `path` si elle dépasse `limits` : `None` si elle passe telle quelle.
/// Chaque essai part de l'original ; le suivant vise un grand côté plus petit, à
/// proportion du poids en trop. Une erreur laisse partir l'original (reprise de la 1.0.6
/// si le fournisseur le refuse).
pub fn fit_image(
    shrinker: &dyn penelope_platform::ImageShrinker,
    path: &Path,
    limits: penelope_llm::attachment::ImageLimits,
) -> Result<Option<Reduction>, String> {
    let original =
        std::fs::read(path).map_err(|e| format!("image {} illisible : {e}", path.display()))?;
    let before = image_size(&original);
    let long = before.map(|(w, h)| w.max(h));
    if limits.fits(original.len(), long) {
        return Ok(None);
    }
    let dst = path.with_extension("reduite.jpg");
    let mut side = long.unwrap_or(limits.max_side).min(limits.max_side);
    let outcome = loop {
        if let Err(e) = shrinker.shrink(path, &dst, side) {
            break Err(format!("réduction impossible : {e}"));
        }
        let bytes = match std::fs::read(&dst) {
            Ok(b) => b,
            Err(e) => break Err(format!("image réduite illisible : {e}")),
        };
        let after = image_size(&bytes);
        if image_mime(&bytes).is_none() {
            break Err("réduction impossible : le résultat n'est pas une image".into());
        }
        if limits.fits(bytes.len(), after.map(|(w, h)| w.max(h))) {
            break Ok(Some(Reduction {
                before_bytes: original.len(),
                before,
                after,
                bytes,
            }));
        }
        let excess = limits.max_encoded_bytes as f64
            / penelope_llm::attachment::encoded_len(bytes.len()) as f64;
        let next = (side as f64 * excess.sqrt().min(0.9)) as u32;
        if next < MIN_SIDE {
            break Err(format!(
                "toujours trop lourde à {side} px de côté ({} octets)",
                bytes.len()
            ));
        }
        side = next;
    };
    let _ = std::fs::remove_file(&dst);
    outcome
}

/// L'image de `path` en URI `data:` pour `model_id`, réduite d'abord si elle dépasse les
/// limites de son fournisseur (issue #242). La réduction est journalisée
/// (`media.image_reduced`) ; son échec laisse partir l'original.
pub async fn model_data_url(
    s: &Services,
    path: &Path,
    model_id: &str,
    session_id: &str,
) -> Result<String, String> {
    let limits = penelope_llm::attachment::image_limits(model_id);
    let (platform, owned) = (s.platform.clone(), path.to_path_buf());
    let fitted =
        tokio::task::spawn_blocking(move || fit_image(platform.images.as_ref(), &owned, limits))
            .await
            .unwrap_or_else(|e| Err(format!("réduction interrompue : {e}")));
    sent_data_url(s, path, model_id, session_id, fitted).await
}

/// L'URI envoyée après l'essai de réduction : l'image réduite, journalisée, ou l'original.
async fn sent_data_url(
    s: &Services,
    path: &Path,
    model_id: &str,
    session_id: &str,
    fitted: Result<Option<Reduction>, String>,
) -> Result<String, String> {
    let r = match fitted {
        Ok(Some(r)) => r,
        Ok(None) => return data_url(path),
        Err(e) => {
            tracing::warn!(error = %e, model = model_id, "image non réduite : envoyée telle quelle");
            return data_url(path);
        }
    };
    let dims = |d: Option<(u32, u32)>| d.map(|(w, h)| format!("{w}x{h}"));
    tracing::info!(
        model = model_id,
        before = r.before_bytes,
        after = r.bytes.len(),
        "image réduite pour le fournisseur"
    );
    let _ = s
        .events
        .append(
            penelope_kernel::event::EventDraft::new(
                "media.image_reduced",
                serde_json::json!({
                    "model": model_id,
                    "path": path.display().to_string(),
                    "before_bytes": r.before_bytes,
                    "after_bytes": r.bytes.len(),
                    "before": dims(r.before),
                    "after": dims(r.after),
                }),
            )
            .session(session_id),
        )
        .await;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&r.bytes)
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

    /// Un JPEG de `w`×`h` pixels (en-tête SOF0) complété jusqu'à `len` octets.
    fn jpeg(w: u32, h: u32, len: usize) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        b.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&(h as u16).to_be_bytes());
        b.extend_from_slice(&(w as u16).to_be_bytes());
        b.extend_from_slice(&[3; 13]);
        b.resize(len.max(b.len()), 0);
        b
    }

    /// Réduction déterministe : un quart d'octet par pixel, format 4:3 ; retient les
    /// côtés demandés.
    #[derive(Default)]
    struct FakeShrinker(std::sync::Mutex<Vec<u32>>);

    impl penelope_platform::ImageShrinker for FakeShrinker {
        fn shrink(&self, _src: &Path, dst: &Path, side: u32) -> penelope_platform::Result<()> {
            self.0.lock().unwrap().push(side);
            let h = side * 3 / 4;
            std::fs::write(dst, jpeg(side, h, (side * h / 3) as usize))?;
            Ok(())
        }
    }

    /// #242 : une photo trop lourde pour Anthropic est réduite, au besoin en plusieurs
    /// essais, jusqu'à passer ; l'original reste intact.
    #[test]
    fn an_oversized_image_is_shrunk_until_it_fits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jpg");
        std::fs::write(&path, jpeg(4_000, 3_000, 6_000_000)).unwrap();
        let limits = penelope_llm::attachment::ImageLimits::ANTHROPIC;
        let shrinker = FakeShrinker::default();
        let r = fit_image(&shrinker, &path, limits)
            .unwrap()
            .expect("réduite");
        assert_eq!(*shrinker.0.lock().unwrap(), vec![4_000, 3_600]);
        assert_eq!(r.before, Some((4_000, 3_000)));
        assert_eq!(r.after, Some((3_600, 2_700)));
        assert_eq!(r.before_bytes, 6_000_000);
        assert!(limits.fits(r.bytes.len(), Some(3_600)));
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            6_000_000,
            "original gardé"
        );
        assert!(
            !path.with_extension("reduite.jpg").exists(),
            "copie de travail retirée"
        );
    }

    /// Ce qui passe déjà ne touche pas à l'outil ; un côté trop grand suffit à réduire.
    #[test]
    fn an_image_within_limits_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jpg");
        let limits = penelope_llm::attachment::ImageLimits::ANTHROPIC;
        let shrinker = FakeShrinker::default();
        std::fs::write(&path, jpeg(2_000, 1_500, 1_000_000)).unwrap();
        assert_eq!(fit_image(&shrinker, &path, limits).unwrap(), None);
        assert!(shrinker.0.lock().unwrap().is_empty());
        std::fs::write(&path, jpeg(9_000, 300, 100_000)).unwrap();
        fit_image(&shrinker, &path, limits)
            .unwrap()
            .expect("réduite");
        assert_eq!(
            shrinker.0.lock().unwrap()[0],
            8_000,
            "ramenée au côté permis"
        );
    }

    /// Réduction journalisée avec les tailles avant et après ; échec (stub Linux, `sips`
    /// en panne) : l'original part tel quel, comme en 1.0.6, sans événement.
    #[tokio::test]
    async fn a_reduction_is_journaled_and_a_failure_sends_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            std::sync::Arc::new(penelope_kernel::clock::SystemClock);
        let s = Services::for_tests(dir.path().join("home"), clock)
            .await
            .unwrap();
        let path = dir.path().join("p.jpg");
        std::fs::write(&path, jpeg(4_000, 3_000, 6_000_000)).unwrap();
        let model = "openrouter:anthropic/claude-sonnet-4.5";

        // Services de test : pas d'outil système, l'original part.
        let url = model_data_url(&s, &path, model, "s1").await.unwrap();
        assert_eq!(url, data_url(&path).unwrap());
        let events = s
            .events
            .session_events_of_kind("s1", "media.image_reduced")
            .await
            .unwrap();
        assert!(events.is_empty());

        let limits = penelope_llm::attachment::image_limits(model);
        let fitted = fit_image(&FakeShrinker::default(), &path, limits);
        let url = sent_data_url(&s, &path, model, "s1", fitted).await.unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
        assert!(url.len() <= limits.max_encoded_bytes + 32);
        let events = s
            .events
            .session_events_of_kind("s1", "media.image_reduced")
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        let p = &events[0].payload;
        assert_eq!(p["model"], model);
        assert_eq!(p["before_bytes"], 6_000_000);
        assert_eq!(p["before"], "4000x3000");
        assert_eq!(p["after"], "3600x2700");
        assert_eq!(p["after_bytes"], 3_600 * 2_700 / 3);
    }

    /// Issue #308 : l'original d'un vocal est rangé sous `media/voice`, au format reçu,
    /// sous un nom tiré du message ; la mention cite son chemin.
    #[tokio::test]
    async fn a_voice_note_is_kept_under_a_stable_name() {
        let dir = tempfile::tempdir().unwrap();
        let clock: penelope_kernel::clock::SharedClock =
            std::sync::Arc::new(penelope_kernel::clock::SystemClock);
        let s = Services::for_tests(dir.path().join("home"), clock)
            .await
            .unwrap();
        let path = save_voice(&s, "c1_60", "audio.ogg", b"OggS-1").unwrap();
        assert_eq!(path, s.platform.dirs.data().join("media/voice/c1_60.ogg"));
        let again = save_voice(&s, "c1_60", "audio.ogg", b"OggS-2").unwrap();
        assert_eq!(again, path, "une réception rejouée réécrit le même fichier");
        assert_eq!(std::fs::read(&path).unwrap(), b"OggS-2");
        let mp3 = save_voice(&s, "../x", "audio.mp3", b"ID3").unwrap();
        assert_eq!(mp3.file_name().unwrap(), "x.mp3", "nom sans chemin");
        assert!(voice_note(&path).contains(&path.display().to_string()));
    }
}
