//! Audio : conversion d'une synthèse vocale au format des vocaux Telegram (issue #41).
//!
//! ```text
//!  WAV (synthèse locale) ─► ffmpeg -c:a libopus -b:a 32k ─► OGG/Opus (sendVoice)
//! ```

use crate::{PlatformError, Result};
use std::path::{Path, PathBuf};

/// `ffmpeg`, s'il est installé.
pub fn ffmpeg() -> Option<PathBuf> {
    crate::which("ffmpeg")
}

/// Convertit un WAV en OGG/Opus, le format des messages vocaux Telegram.
pub fn wav_to_ogg_opus(wav: &Path, ogg: &Path) -> Result<()> {
    let bin = ffmpeg().ok_or_else(|| {
        PlatformError::NotFound(
            "ffmpeg (brew install ffmpeg) : conversion en vocal impossible".into(),
        )
    })?;
    let out = std::process::Command::new(bin)
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(wav)
        .args(["-c:a", "libopus", "-b:a", "32k"])
        .arg(ogg)
        .output()
        .map_err(|e| PlatformError::Process(format!("ffmpeg : {e}")))?;
    if !out.status.success() {
        return Err(PlatformError::Process(format!(
            "ffmpeg : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une seconde de silence, PCM 16 bits mono à 16 kHz.
    fn silence() -> Vec<u8> {
        let data_len: u32 = 32_000;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&16_000u32.to_le_bytes());
        out.extend_from_slice(&32_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(44 + data_len as usize, 0);
        out
    }

    #[test]
    fn a_wav_becomes_an_ogg_opus_voice_note() {
        if ffmpeg().is_none() {
            eprintln!("ffmpeg absent : conversion non testée");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (wav, ogg) = (dir.path().join("in.wav"), dir.path().join("out.ogg"));
        std::fs::write(&wav, silence()).unwrap();
        wav_to_ogg_opus(&wav, &ogg).unwrap();
        let bytes = std::fs::read(&ogg).unwrap();
        assert!(
            bytes.starts_with(b"OggS"),
            "{:?}",
            &bytes[..8.min(bytes.len())]
        );
        assert!(bytes.windows(8).any(|w| w == b"OpusHead"));
    }
}
