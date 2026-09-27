//! Réduction d'une image trop lourde pour le fournisseur du modèle (issue #242).
//!
//! L'outil du système plutôt qu'un crate d'image : `sips` sous macOS, livré avec l'OS ;
//! ailleurs, une erreur `Unsupported`, et la photo part telle quelle (reprise de la
//! 1.0.6 si le fournisseur la refuse).

use crate::{PlatformError, Result};
use std::path::Path;

/// Réduit une image par l'outil du système.
pub trait ImageShrinker: Send + Sync {
    /// Écrit dans `dst` une copie JPEG de `src` dont le grand côté vaut au plus
    /// `max_side` pixels, proportions gardées.
    fn shrink(&self, src: &Path, dst: &Path, max_side: u32) -> Result<()>;
}

/// Aucune réduction : celui des tests, qui ne doivent pas dépendre de l'OS.
pub struct NoShrinker;

impl ImageShrinker for NoShrinker {
    fn shrink(&self, _src: &Path, _dst: &Path, _max_side: u32) -> Result<()> {
        Err(PlatformError::Unsupported(
            "réduction d'image désactivée (tests)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BMP 24 bits non compressé, `w`×`h` pixels gris : l'en-tête se construit à la main.
    fn bmp(w: u32, h: u32) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let data = row * h;
        let mut out = Vec::new();
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + data).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&h.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        out.resize(54 + data as usize, 0x80);
        out
    }

    #[test]
    fn the_test_shrinker_refuses_cleanly() {
        let e = NoShrinker
            .shrink(Path::new("a.png"), Path::new("b.jpg"), 100)
            .unwrap_err();
        assert!(matches!(e, PlatformError::Unsupported(_)), "{e}");
    }

    /// Hors macOS, le stub dit que la réduction n'est pas livrée : la photo part telle
    /// quelle, comme en 1.0.6.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_stub_backend_says_it_cannot_shrink() {
        let e = crate::backend::image_shrinker()
            .shrink(Path::new("a.png"), Path::new("b.jpg"), 100)
            .unwrap_err();
        assert!(matches!(e, PlatformError::Unsupported(_)), "{e}");
    }

    /// `sips` réduit pour de bon : grand côté ramené à la limite, proportions gardées.
    #[cfg(target_os = "macos")]
    #[test]
    fn sips_shrinks_to_the_longest_side() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("in.bmp"), dir.path().join("out.jpg"));
        std::fs::write(&src, bmp(64, 32)).unwrap();
        crate::backend::image_shrinker()
            .shrink(&src, &dst, 16)
            .unwrap();
        let out = std::fs::read(&dst).unwrap();
        assert!(out.starts_with(&[0xFF, 0xD8, 0xFF]), "un JPEG");
        let probe = std::process::Command::new("/usr/bin/sips")
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(&dst)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout);
        assert!(text.contains("pixelWidth: 16"), "{text}");
        assert!(text.contains("pixelHeight: 8"), "{text}");
    }
}
