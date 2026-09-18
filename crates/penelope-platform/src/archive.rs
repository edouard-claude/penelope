//! Archive chiffrée par phrase de passe (issue #42).
//!
//! Même construction que le magasin de secrets : clé dérivée par Argon2id, chiffrement
//! XChaCha20-Poly1305. Le sel et le nonce voyagent en tête du fichier, si bien qu'une
//! sauvegarde se restaure sur une machine neuve avec la seule phrase de passe.
//!
//! ```text
//!  PNLPBK01 │ sel (16) │ nonce (24) │ chiffré (tar.gz + marque d'authenticité)
//! ```
//!
//! Aucune archive en clair n'est écrite à côté : le chiffrement se fait en mémoire, et le
//! plafond de taille est vérifié avant (une sauvegarde qui dépasse la limite du dépôt est
//! refusée, pas tronquée).

use crate::{PlatformError, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::path::Path;

/// En-tête : format reconnaissable, version comprise.
const MAGIC: &[u8; 8] = b"PNLPBK01";
/// Taille maximale d'une archive chiffrée en mémoire (garde-fou, issue #42).
pub const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

fn derive(passphrase: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(19 * 1024, 2, 1, Some(32))
        .map_err(|e| PlatformError::Secret(format!("paramètres Argon2 invalides : {e}")))?;
    let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; 32];
    a2.hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| PlatformError::Secret(format!("dérivation de clé : {e}")))?;
    Ok(key)
}

/// Chiffre `src` vers `dst`. La phrase de passe vide est refusée : une archive lisible par
/// tous n'est pas une sauvegarde.
pub fn seal(src: &Path, dst: &Path, passphrase: &str) -> Result<u64> {
    if passphrase.trim().is_empty() {
        return Err(PlatformError::Secret(
            "phrase de passe vide : l'archive ne serait pas protégée".into(),
        ));
    }
    let size = std::fs::metadata(src)?.len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(PlatformError::Secret(format!(
            "archive de {size} octets : au-delà de {MAX_ARCHIVE_BYTES}, chiffrer par morceaux"
        )));
    }
    let plain = std::fs::read(src)?;
    let mut salt = [0u8; 16];
    let mut nonce_bytes = [0u8; 24];
    getrandom::getrandom(&mut salt)
        .map_err(|e| PlatformError::Secret(format!("entropie indisponible : {e}")))?;
    getrandom::getrandom(&mut nonce_bytes)
        .map_err(|e| PlatformError::Secret(format!("entropie indisponible : {e}")))?;
    let key = derive(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| PlatformError::Secret(e.to_string()))?;
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plain.as_ref())
        .map_err(|e| PlatformError::Secret(e.to_string()))?;

    let mut out = Vec::with_capacity(48 + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(dst, &out)?;
    crate::secrets::restrict_permissions(dst)?;
    Ok(out.len() as u64)
}

/// Déchiffre `src` vers `dst`. Une phrase de passe incorrecte est dite comme telle.
pub fn open(src: &Path, dst: &Path, passphrase: &str) -> Result<u64> {
    let raw = std::fs::read(src)?;
    if raw.len() < 48 || &raw[..8] != MAGIC {
        return Err(PlatformError::Secret(format!(
            "{} n'est pas une sauvegarde Pénélope",
            src.display()
        )));
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&raw[8..24]);
    let key = derive(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|e| PlatformError::Secret(e.to_string()))?;
    let plain = cipher
        .decrypt(XNonce::from_slice(&raw[24..48]), &raw[48..])
        .map_err(|_| {
            PlatformError::Secret("déchiffrement impossible : phrase de passe incorrecte".into())
        })?;
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(dst, &plain)?;
    Ok(plain.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_archive_reopens_with_its_passphrase_only() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("sauvegarde.tar.gz");
        std::fs::write(&src, b"contenu de la sauvegarde, pas du bruit").unwrap();
        let sealed = dir.path().join("sauvegarde.tar.gz.enc");
        seal(&src, &sealed, "phrase de passe correcte").unwrap();

        // Rien du contenu ne se lit dans le fichier chiffré.
        let raw = std::fs::read(&sealed).unwrap();
        assert!(raw.starts_with(MAGIC));
        assert!(
            !String::from_utf8_lossy(&raw).contains("contenu de la sauvegarde"),
            "l'archive ne doit pas être lisible"
        );

        let back = dir.path().join("rendu.tar.gz");
        open(&sealed, &back, "phrase de passe correcte").unwrap();
        assert_eq!(
            std::fs::read(&back).unwrap(),
            b"contenu de la sauvegarde, pas du bruit"
        );

        let e = open(&sealed, &back, "mauvaise phrase").unwrap_err();
        assert!(e.to_string().contains("phrase de passe incorrecte"), "{e}");
    }

    #[test]
    fn an_empty_passphrase_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.tar.gz");
        std::fs::write(&src, b"x").unwrap();
        let e = seal(&src, &dir.path().join("a.enc"), "   ").unwrap_err();
        assert!(e.to_string().contains("phrase de passe vide"), "{e}");
    }

    #[test]
    fn a_foreign_file_is_not_mistaken_for_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("autre.bin");
        std::fs::write(&src, b"ceci n'est pas une sauvegarde de Penelope du tout").unwrap();
        let e = open(&src, &dir.path().join("out"), "x").unwrap_err();
        assert!(e.to_string().contains("n'est pas une sauvegarde"), "{e}");
    }
}
